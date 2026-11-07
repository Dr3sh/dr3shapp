use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::dr3sh::download::emit_progress;
use crate::dr3sh::errors::Dr3shError;
use crate::dr3sh::logger::{format_log_line, log_event, log_op_debug_batch};
use crate::dr3sh::manifest::{
    file_size_and_mtime, save_baseline_atomic, validate_relative_path, FilePolicy, Manifest,
    ManifestFileEntry, PlanActionKind, ReconciliationPlan,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalOp {
    pub op: String,
    pub path: String,
    pub done: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Journal {
    pub txn_id: String,
    pub pack_id: String,
    pub from_version: Option<String>,
    pub to_version: String,
    pub state: String,
    pub preserve_backup: bool,
    pub ops: Vec<JournalOp>,
}

impl Journal {
    pub fn write_flushed(&self, instance_dir: &Path) -> Result<(), Dr3shError> {
        let dr3sh_dir = instance_dir.join(".dr3sh");
        fs::create_dir_all(&dr3sh_dir).map_err(|e| {
            Dr3shError::FsPermission {
                path: dr3sh_dir.to_string_lossy().to_string(),
                os_code: e.raw_os_error().unwrap_or(-1).to_string(),
            }
        })?;
        let txn_path = dr3sh_dir.join("txn.json");
        let tmp_path = dr3sh_dir.join("txn.json.tmp");

        let serialized = serde_json::to_vec_pretty(self)
            .map_err(|e| Dr3shError::unknown(format!("Failed to serialize txn.json: {}", e)))?;

        {
            let mut file = File::create(&tmp_path).map_err(|e| Dr3shError::FsPermission {
                path: tmp_path.to_string_lossy().to_string(),
                os_code: e.raw_os_error().unwrap_or(-1).to_string(),
            })?;
            use std::io::Write;
            file.write_all(&serialized).map_err(|e| Dr3shError::FsPermission {
                path: tmp_path.to_string_lossy().to_string(),
                os_code: e.raw_os_error().unwrap_or(-1).to_string(),
            })?;
            let _ = file.sync_all();
        }

        rename_with_lock_retries(&tmp_path, &txn_path)?;
        Ok(())
    }
}

pub fn rename_with_lock_retries(from: &Path, to: &Path) -> Result<(), Dr3shError> {
    let delays_ms = [200u64, 300, 400, 500, 500];
    let mut last_err: Option<std::io::Error> = None;

    for (attempt, delay_ms) in std::iter::once(0u64).chain(delays_ms.into_iter()).enumerate() {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(delay_ms));
        }

        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => {
                let os_code = e.raw_os_error().unwrap_or(-1);
                let is_lock_or_access = matches!(os_code, 5 | 32 | 33)
                    || e.kind() == std::io::ErrorKind::PermissionDenied;
                last_err = Some(e);
                if !is_lock_or_access {
                    break;
                }
            }
        }
    }

    let err = last_err.unwrap_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "rename failed"));
    let os_code = err.raw_os_error().unwrap_or(-1);
    let path_str = format!("{} -> {}", from.display(), to.display());

    if matches!(os_code, 32 | 33 | 5) || err.kind() == std::io::ErrorKind::PermissionDenied {
        Err(Dr3shError::FsLocked {
            path: path_str,
            os_code: os_code.to_string(),
        })
    } else {
        Err(Dr3shError::FsPermission {
            path: path_str,
            os_code: os_code.to_string(),
        })
    }
}

pub fn extract_to_staging(
    app: Option<&tauri::AppHandle>,
    op_id: &str,
    server_id: &str,
    zip_path: &Path,
    instance_dir: &Path,
    txn_id: &str,
    plan: &ReconciliationPlan,
    cancel_token: &Arc<AtomicBool>,
) -> Result<PathBuf, Dr3shError> {
    let staging_dir = instance_dir.join(".dr3sh").join("staging").join(txn_id);
    if staging_dir.exists() {
        let _ = fs::remove_dir_all(&staging_dir);
    }
    fs::create_dir_all(&staging_dir).map_err(|e| Dr3shError::FsPermission {
        path: staging_dir.to_string_lossy().to_string(),
        os_code: e.raw_os_error().unwrap_or(-1).to_string(),
    })?;

    let mut needed: HashMap<String, (String, String)> = HashMap::new();
    for entry in &plan.items {
        if matches!(
            entry.action,
            PlanActionKind::Write | PlanActionKind::BackupThenWrite
        ) {
            if let Some(sha) = &entry.expected_sha256 {
                let lookup_key = entry
                    .archive_entry_name
                    .as_deref()
                    .unwrap_or(&entry.path)
                    .replace('\\', "/")
                    .to_ascii_lowercase();
                needed.insert(lookup_key, (entry.path.clone(), sha.clone()));
            }
        }
    }

    if needed.is_empty() {
        log_event(
            Some(op_id),
            "EXTRACT",
            "INFO",
            "no files need extraction",
            &[("txn_id", txn_id.to_string())],
        );
        return Ok(staging_dir);
    }

    let result = (|| -> Result<(), Dr3shError> {
        let file = File::open(zip_path).map_err(|e| Dr3shError::ZipCorrupt {
            entry: "<archive>".to_string(),
            error: e.to_string(),
        })?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| Dr3shError::ZipCorrupt {
            entry: "<archive>".to_string(),
            error: e.to_string(),
        })?;

        let total_needed = needed.len() as u64;
        let mut extracted_count = 0u64;
        let mut last_emit = Instant::now();

        let mut common_prefix: Option<String> = None;
        for i in 0..archive.len() {
            if let Ok(entry) = archive.by_index_raw(i)
                && !entry.is_dir()
                && let Ok(raw_n) = entry.name()
            {
                let norm = raw_n.replace('\\', "/");
                if let Some((first_seg, _)) = norm.split_once('/') {
                    let candidate = format!("{first_seg}/");
                    match &common_prefix {
                        None => common_prefix = Some(candidate),
                        Some(existing) if !existing.eq_ignore_ascii_case(&candidate) => {
                            common_prefix = None;
                            break;
                        }
                        _ => {}
                    }
                } else {
                    common_prefix = None;
                    break;
                }
            }
        }
        if let Some(ref prefix) = common_prefix {
            let folder = prefix.trim_end_matches('/').to_ascii_lowercase();
            let standard_dirs = [
                "mods",
                "config",
                "resourcepacks",
                "shaderpacks",
                "kubejs",
                "defaultconfigs",
                "saves",
                "datapacks",
            ];
            if standard_dirs.contains(&folder.as_str()) {
                common_prefix = None;
            }
        }

        for i in 0..archive.len() {
            if cancel_token.load(Ordering::SeqCst) {
                return Err(Dr3shError::Cancelled {
                    stage: "extract".to_string(),
                });
            }

            let mut entry = archive.by_index(i).map_err(|e| Dr3shError::ZipCorrupt {
                entry: format!("index {}", i),
                error: e.to_string(),
            })?;

            let raw_name = entry
                .name()
                .map_err(|e| Dr3shError::ZipCorrupt {
                    entry: format!("index {}", i),
                    error: e.to_string(),
                })?
                .replace('\\', "/");
            if entry.is_dir() || raw_name.ends_with('/') {
                continue;
            }

            validate_relative_path(&raw_name)?;
            let key = raw_name.to_ascii_lowercase();

            let matched = needed.remove(&key).or_else(|| {
                if let Some(ref prefix) = common_prefix
                    && raw_name.len() > prefix.len()
                {
                    let stripped = raw_name[prefix.len()..]
                        .trim_start_matches('/')
                        .to_ascii_lowercase();
                    needed.remove(&stripped)
                } else {
                    None
                }
            });

            let Some((canonical_rel_path, expected_sha)) = matched else {
                continue;
            };

            let out_path = staging_dir.join(&canonical_rel_path);
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent).map_err(|e| Dr3shError::FsPermission {
                    path: parent.to_string_lossy().to_string(),
                    os_code: e.raw_os_error().unwrap_or(-1).to_string(),
                })?;
            }

            let mut out_file = File::create(&out_path).map_err(|e| Dr3shError::FsPermission {
                path: out_path.to_string_lossy().to_string(),
                os_code: e.raw_os_error().unwrap_or(-1).to_string(),
            })?;

            let mut hasher = Sha256::new();
            let mut buf = [0u8; 65536];
            loop {
                if cancel_token.load(Ordering::SeqCst) {
                    return Err(Dr3shError::Cancelled {
                        stage: "extract".to_string(),
                    });
                }
                let n = entry.read(&mut buf).map_err(|e| Dr3shError::ZipCorrupt {
                    entry: canonical_rel_path.clone(),
                    error: e.to_string(),
                })?;
                if n == 0 {
                    break;
                }
                use std::io::Write;
                out_file.write_all(&buf[..n]).map_err(|e| Dr3shError::FsPermission {
                    path: out_path.to_string_lossy().to_string(),
                    os_code: e.raw_os_error().unwrap_or(-1).to_string(),
                })?;
                hasher.update(&buf[..n]);
            }

            let actual_sha: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
            if !actual_sha.eq_ignore_ascii_case(&expected_sha) {
                return Err(Dr3shError::ZipCorrupt {
                    entry: canonical_rel_path,
                    error: format!("SHA-256 mismatch (expected {}, got {})", expected_sha, actual_sha),
                });
            }

            extracted_count += 1;
            if let Some(app_handle) = app
                && (last_emit.elapsed() >= Duration::from_millis(100)
                    || extracted_count == total_needed)
            {
                let percent = (70.0 + (extracted_count as f64 / total_needed.max(1) as f64) * 20.0)
                    .round() as u32;
                emit_progress(
                    app_handle,
                    op_id,
                    server_id,
                    "extract",
                    format!("Extrayendo archivos ({}/{})...", extracted_count, total_needed),
                    extracted_count,
                    total_needed,
                    0,
                    None,
                    Some(percent),
                );
                last_emit = Instant::now();
            }
        }

        if !needed.is_empty() {
            let missing: Vec<String> = needed.into_values().map(|(p, _)| p).collect();
            return Err(Dr3shError::ZipContentMismatch {
                missing_entries: missing,
            });
        }

        if let Some(app_handle) = app {
            emit_progress(
                app_handle,
                op_id,
                server_id,
                "extract",
                "Extracción completada.",
                total_needed,
                total_needed,
                0,
                None,
                Some(90),
            );
        }

        Ok(())
    })();

    if let Err(err) = result {
        let _ = fs::remove_dir_all(&staging_dir);
        return Err(err);
    }

    Ok(staging_dir)
}

pub fn apply_plan_transactional(
    app: Option<&tauri::AppHandle>,
    op_id: &str,
    server_id: &str,
    instance_dir: &Path,
    txn_id: &str,
    from_version: Option<String>,
    manifest: &Manifest,
    plan: &ReconciliationPlan,
) -> Result<(), Dr3shError> {
    let dr3sh_dir = instance_dir.join(".dr3sh");
    let staging_dir = dr3sh_dir.join("staging").join(txn_id);
    let backup_dir = dr3sh_dir.join("backup").join(txn_id);
    fs::create_dir_all(&backup_dir).map_err(|e| Dr3shError::FsPermission {
        path: backup_dir.to_string_lossy().to_string(),
        os_code: e.raw_os_error().unwrap_or(-1).to_string(),
    })?;

    let mut ops: Vec<JournalOp> = Vec::new();
    let mut preserve_backup = false;

    for entry in &plan.items {
        if let Some(tag) = &entry.tag {
            if matches!(
                tag.as_str(),
                "OVERWROTE_PLAYER_CHANGE" | "OVERWROTE_NO_BASELINE" | "UNKNOWN_MANAGED_REMOVED"
            ) {
                preserve_backup = true;
            }
        }
    }

    // Build ordered journal operations according to Section 6.4:
    // 1. Every REMOVE (except UNKNOWN_MANAGED_REMOVED), BACKUP_THEN_WRITE, and WRITE whose target exists -> backup
    for entry in &plan.items {
        let is_unknown_managed = entry.tag.as_deref() == Some("UNKNOWN_MANAGED_REMOVED");
        if is_unknown_managed {
            continue;
        }
        let target_path = instance_dir.join(&entry.path);
        let needs_backup = match entry.action {
            PlanActionKind::Remove | PlanActionKind::BackupThenWrite => target_path.exists(),
            PlanActionKind::Write => target_path.exists(),
            PlanActionKind::Skip | PlanActionKind::Keep => false,
        };
        if needs_backup {
            ops.push(JournalOp {
                op: "backup".to_string(),
                path: entry.path.clone(),
                done: false,
                tag: entry.tag.clone(),
            });
        }
    }

    // 2. Every WRITE / BACKUP_THEN_WRITE -> place from staging
    for entry in &plan.items {
        if matches!(
            entry.action,
            PlanActionKind::Write | PlanActionKind::BackupThenWrite
        ) {
            ops.push(JournalOp {
                op: "place".to_string(),
                path: entry.path.clone(),
                done: false,
                tag: entry.tag.clone(),
            });
        }
    }

    // 3. UNKNOWN_MANAGED_REMOVED -> backup
    for entry in &plan.items {
        if entry.tag.as_deref() == Some("UNKNOWN_MANAGED_REMOVED") {
            let target_path = instance_dir.join(&entry.path);
            if target_path.exists() {
                ops.push(JournalOp {
                    op: "backup".to_string(),
                    path: entry.path.clone(),
                    done: false,
                    tag: entry.tag.clone(),
                });
            }
        }
    }

    let mut journal = Journal {
        txn_id: txn_id.to_string(),
        pack_id: manifest.pack_id.clone(),
        from_version,
        to_version: manifest.version.clone(),
        state: "applying".to_string(),
        preserve_backup,
        ops,
    };

    // Write txn.json BEFORE the first change to the instance
    journal.write_flushed(instance_dir)?;

    let total_ops = journal.ops.len() as u64;
    let mut debug_log_buf = String::new();
    let mut last_journal_flush = Instant::now();
    let mut last_progress_emit = Instant::now();

    let apply_result = (|| -> Result<(), Dr3shError> {
        for idx in 0..journal.ops.len() {
            let (op_kind, rel_path, tag) = {
                let item = &journal.ops[idx];
                (item.op.clone(), item.path.clone(), item.tag.clone())
            };

            if op_kind == "backup" {
                let src = instance_dir.join(&rel_path);
                let dst = backup_dir.join(&rel_path);
                if src.exists() {
                    if let Some(parent) = dst.parent() {
                        fs::create_dir_all(parent).map_err(|e| Dr3shError::FsPermission {
                            path: parent.to_string_lossy().to_string(),
                            os_code: e.raw_os_error().unwrap_or(-1).to_string(),
                        })?;
                    }
                    rename_with_lock_retries(&src, &dst)?;
                    debug_log_buf.push_str(&format_log_line(
                        Some(op_id),
                        "APPLY",
                        "DEBUG",
                        "backed up file",
                        &[
                            ("path", rel_path.clone()),
                            ("tag", tag.unwrap_or_default()),
                        ],
                    ));
                }
            } else if op_kind == "place" {
                let src = staging_dir.join(&rel_path);
                let dst = instance_dir.join(&rel_path);
                if let Some(parent) = dst.parent() {
                    fs::create_dir_all(parent).map_err(|e| Dr3shError::FsPermission {
                        path: parent.to_string_lossy().to_string(),
                        os_code: e.raw_os_error().unwrap_or(-1).to_string(),
                    })?;
                }
                rename_with_lock_retries(&src, &dst)?;
                debug_log_buf.push_str(&format_log_line(
                    Some(op_id),
                    "APPLY",
                    "DEBUG",
                    "placed file from staging",
                    &[("path", rel_path.clone())],
                ));
            }

            journal.ops[idx].done = true;
            if (idx + 1) % 128 == 0
                || last_journal_flush.elapsed() >= Duration::from_millis(250)
                || idx + 1 == journal.ops.len()
            {
                journal.write_flushed(instance_dir)?;
                last_journal_flush = Instant::now();
            }

            if let Some(app_handle) = app
                && (last_progress_emit.elapsed() >= Duration::from_millis(100)
                    || (idx + 1) as u64 == total_ops)
            {
                let done_count = (idx + 1) as u64;
                let percent = (90.0 + (done_count as f64 / total_ops.max(1) as f64) * 9.0)
                    .round() as u32;
                emit_progress(
                    app_handle,
                    op_id,
                    server_id,
                    "apply",
                    format!("Aplicando cambios ({}/{})...", done_count, total_ops),
                    done_count,
                    total_ops,
                    0,
                    None,
                    Some(percent),
                );
                last_progress_emit = Instant::now();
            }
        }

        log_op_debug_batch(Some(op_id), &debug_log_buf);
        debug_log_buf.clear();

        // Step 4: Write the new baseline atomically
        let plan_actions_by_lower: HashMap<String, (PlanActionKind, FilePolicy)> = plan
            .items
            .iter()
            .map(|i| (i.path.to_ascii_lowercase(), (i.action, i.policy)))
            .collect();

        let mut baseline_files = Vec::with_capacity(manifest.files.len());
        for mf in &manifest.files {
            let full_path = instance_dir.join(&mf.path);
            if full_path.exists() {
                let (action, policy) = plan_actions_by_lower
                    .get(&mf.path.to_ascii_lowercase())
                    .copied()
                    .unwrap_or((PlanActionKind::Skip, FilePolicy::Managed));
                let disk_matches_pack = matches!(
                    action,
                    PlanActionKind::Write | PlanActionKind::BackupThenWrite
                ) || (action == PlanActionKind::Skip && policy != FilePolicy::Seed);

                let (sz, mtime_opt) = if disk_matches_pack {
                    let (sz, mt) = file_size_and_mtime(&full_path).unwrap_or((mf.size, 0));
                    (sz, Some(mt))
                } else {
                    (mf.size, None)
                };

                baseline_files.push(ManifestFileEntry {
                    path: mf.path.clone(),
                    sha256: mf.sha256.clone(),
                    size: sz,
                    policy: mf.policy.clone(),
                    archive_path: mf.archive_path.clone(),
                    mtime: mtime_opt,
                });
            }
        }

        let mut baseline = manifest.clone();
        baseline.files = baseline_files;
        save_baseline_atomic(instance_dir, &baseline)?;

        // Step 5: Set journal state to committed, then delete txn.json and staging/<txn_id>
        journal.state = "committed".to_string();
        journal.write_flushed(instance_dir)?;

        finalize_committed_journal(Some(op_id), instance_dir, &journal);

        if let Some(app_handle) = app {
            emit_progress(
                app_handle,
                op_id,
                server_id,
                "commit",
                "Instalación completada.",
                total_ops,
                total_ops,
                0,
                None,
                Some(100),
            );
        }

        Ok(())
    })();

    if !debug_log_buf.is_empty() {
        log_op_debug_batch(Some(op_id), &debug_log_buf);
    }

    if let Err(apply_err) = apply_result {
        log_event(
            Some(op_id),
            "APPLY",
            "ERROR",
            "apply failed; starting rollback",
            &[("error", apply_err.to_string()), ("txn_id", txn_id.to_string())],
        );
        if let Err(rb_err) = rollback_from_journal(Some(op_id), instance_dir, &journal) {
            return Err(rb_err);
        }
        return Err(apply_err);
    }

    Ok(())
}

pub fn finalize_committed_journal(op_id: Option<&str>, instance_dir: &Path, journal: &Journal) {
    let dr3sh_dir = instance_dir.join(".dr3sh");
    let txn_path = dr3sh_dir.join("txn.json");
    let staging_dir = dr3sh_dir.join("staging").join(&journal.txn_id);
    let backup_dir = dr3sh_dir.join("backup").join(&journal.txn_id);

    let _ = fs::remove_file(&txn_path);
    if staging_dir.exists() {
        let _ = fs::remove_dir_all(&staging_dir);
    }

    if journal.preserve_backup && backup_dir.exists() {
        let marker = backup_dir.join(".retained_backup");
        let _ = fs::write(&marker, journal.txn_id.as_bytes());
        log_event(
            op_id,
            "APPLY",
            "INFO",
            "retained player backup",
            &[("backup_dir", backup_dir.to_string_lossy().to_string())],
        );
    } else if backup_dir.exists() {
        let _ = fs::remove_dir_all(&backup_dir);
    }

    prune_backups(op_id, instance_dir, 5);
}

pub fn prune_backups(op_id: Option<&str>, instance_dir: &Path, keep_newest: usize) {
    let backup_root = instance_dir.join(".dr3sh").join("backup");
    let Ok(read_dir) = fs::read_dir(&backup_root) else {
        return;
    };

    let mut dirs: Vec<(PathBuf, SystemTime)> = Vec::new();
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            dirs.push((path, modified));
        }
    }

    if dirs.len() <= keep_newest {
        return;
    }

    dirs.sort_by(|a, b| b.1.cmp(&a.1));
    for (old_dir, _) in dirs.into_iter().skip(keep_newest) {
        if fs::remove_dir_all(&old_dir).is_ok() {
            log_event(
                op_id,
                "CLEANUP",
                "INFO",
                "pruned old backup directory",
                &[("path", old_dir.to_string_lossy().to_string())],
            );
        }
    }
}

pub fn rollback_from_journal(
    op_id: Option<&str>,
    instance_dir: &Path,
    journal: &Journal,
) -> Result<(), Dr3shError> {
    let dr3sh_dir = instance_dir.join(".dr3sh");
    let backup_dir = dr3sh_dir.join("backup").join(&journal.txn_id);
    let staging_dir = dr3sh_dir.join("staging").join(&journal.txn_id);

    for item in journal.ops.iter().rev() {
        let executed_on_disk = item.done
            || if item.op == "place" {
                !staging_dir.join(&item.path).exists() && instance_dir.join(&item.path).exists()
            } else {
                backup_dir.join(&item.path).exists()
            };
        if !executed_on_disk {
            continue;
        }
        if item.op == "place" {
            let target = instance_dir.join(&item.path);
            if target.exists() {
                if let Err(e) = fs::remove_file(&target) {
                    log_event(
                        op_id,
                        "ROLLBACK",
                        "ERROR",
                        "failed to remove placed file during rollback",
                        &[
                            ("path", item.path.clone()),
                            ("error", e.to_string()),
                        ],
                    );
                    return Err(Dr3shError::RollbackFailed {
                        path: backup_dir.to_string_lossy().to_string(),
                        failed_op: format!("remove placed file {}: {}", item.path, e),
                    });
                }
                log_event(
                    op_id,
                    "ROLLBACK",
                    "INFO",
                    "removed placed file",
                    &[("path", item.path.clone())],
                );
            }
        } else if item.op == "backup" {
            let backed_up = backup_dir.join(&item.path);
            let original = instance_dir.join(&item.path);
            if backed_up.exists() {
                if let Some(parent) = original.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                if let Err(e) = rename_with_lock_retries(&backed_up, &original) {
                    log_event(
                        op_id,
                        "ROLLBACK",
                        "ERROR",
                        "failed to restore backed up file during rollback",
                        &[
                            ("path", item.path.clone()),
                            ("error", e.to_string()),
                        ],
                    );
                    return Err(Dr3shError::RollbackFailed {
                        path: backup_dir.to_string_lossy().to_string(),
                        failed_op: format!("restore backup {}: {}", item.path, e),
                    });
                }
                log_event(
                    op_id,
                    "ROLLBACK",
                    "INFO",
                    "restored file from backup",
                    &[("path", item.path.clone())],
                );
            }
        }
    }

    let txn_path = dr3sh_dir.join("txn.json");
    let _ = fs::remove_file(&txn_path);
    if staging_dir.exists() {
        let _ = fs::remove_dir_all(&staging_dir);
    }
    if backup_dir.exists() {
        let _ = fs::remove_dir_all(&backup_dir);
    }

    Ok(())
}

pub fn recover_instance(op_id: Option<&str>, instance_dir: &Path) -> Result<(), Dr3shError> {
    let dr3sh_dir = instance_dir.join(".dr3sh");
    if !dr3sh_dir.exists() {
        return Ok(());
    }

    let txn_path = dr3sh_dir.join("txn.json");
    if txn_path.exists() {
        let raw = fs::read_to_string(&txn_path).map_err(|e| Dr3shError::RollbackFailed {
            path: dr3sh_dir.join("backup").to_string_lossy().to_string(),
            failed_op: format!("read txn.json: {}", e),
        })?;
        let journal: Journal = serde_json::from_str(&raw).map_err(|e| Dr3shError::RollbackFailed {
            path: dr3sh_dir.join("backup").to_string_lossy().to_string(),
            failed_op: format!("parse txn.json: {}", e),
        })?;

        if journal.state == "committed" {
            log_event(
                op_id,
                "RECOVERY",
                "INFO",
                "finishing committed transaction cleanup",
                &[
                    ("instance", instance_dir.to_string_lossy().to_string()),
                    ("txn_id", journal.txn_id.clone()),
                ],
            );
            finalize_committed_journal(op_id, instance_dir, &journal);
        } else {
            log_event(
                op_id,
                "RECOVERY",
                "WARN",
                "RECOVERY_ROLLBACK triggered for uncommitted transaction",
                &[
                    ("instance", instance_dir.to_string_lossy().to_string()),
                    ("txn_id", journal.txn_id.clone()),
                ],
            );
            rollback_from_journal(op_id, instance_dir, &journal)?;
        }
    }

    // Any staging/* without a journal: delete it
    let staging_root = dr3sh_dir.join("staging");
    if staging_root.exists() && !txn_path.exists() {
        if let Ok(entries) = fs::read_dir(&staging_root) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    let _ = fs::remove_dir_all(&p);
                    log_event(
                        op_id,
                        "RECOVERY",
                        "INFO",
                        "deleted orphaned staging directory",
                        &[("path", p.to_string_lossy().to_string())],
                    );
                }
            }
        }
    }

    Ok(())
}

pub fn cleanup_stale_part_downloads(roots: &[PathBuf]) {
    let max_age = Duration::from_secs(7 * 24 * 3600);
    let now = SystemTime::now();

    for root in roots {
        let dl_dir = root.join(".dr3sh_downloads");
        let Ok(entries) = fs::read_dir(&dl_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("part") {
                continue;
            }
            let is_stale = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|m| now.duration_since(m).ok())
                .map(|age| age > max_age)
                .unwrap_or(false);

            if is_stale && fs::remove_file(&path).is_ok() {
                log_event(
                    None,
                    "RECOVERY",
                    "INFO",
                    "deleted stale .part download (>7 days)",
                    &[("path", path.to_string_lossy().to_string())],
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dr3sh::manifest::{FilePlanItem, FilePolicy, ManifestRule, ManifestZip};

    #[test]
    fn test_crash_recovery_rolls_back_interrupted_apply() {
        let temp_dir = std::env::temp_dir().join(format!("dr3sh_txn_test_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(temp_dir.join("mods")).unwrap();
        fs::write(temp_dir.join("mods/old.jar"), b"old_mod_bytes").unwrap();

        let txn_id = "txn_crash_test";
        let backup_dir = temp_dir.join(".dr3sh/backup").join(txn_id);
        let staging_dir = temp_dir.join(".dr3sh/staging").join(txn_id);
        fs::create_dir_all(backup_dir.join("mods")).unwrap();
        fs::create_dir_all(staging_dir.join("mods")).unwrap();

        // Simulate mid-apply crash: old.jar was moved to backup, new.jar was placed from staging
        fs::rename(
            temp_dir.join("mods/old.jar"),
            backup_dir.join("mods/old.jar"),
        )
        .unwrap();
        fs::write(temp_dir.join("mods/new.jar"), b"new_mod_bytes").unwrap();

        let journal = Journal {
            txn_id: txn_id.to_string(),
            pack_id: "test".to_string(),
            from_version: Some("1.0.0".to_string()),
            to_version: "1.1.0".to_string(),
            state: "applying".to_string(),
            preserve_backup: false,
            ops: vec![
                JournalOp {
                    op: "backup".to_string(),
                    path: "mods/old.jar".to_string(),
                    done: true,
                    tag: None,
                },
                JournalOp {
                    op: "place".to_string(),
                    path: "mods/new.jar".to_string(),
                    done: true,
                    tag: None,
                },
            ],
        };
        journal.write_flushed(&temp_dir).unwrap();

        // Run crash recovery
        recover_instance(Some("test_op"), &temp_dir).unwrap();

        assert!(temp_dir.join("mods/old.jar").exists());
        assert_eq!(
            fs::read(temp_dir.join("mods/old.jar")).unwrap(),
            b"old_mod_bytes"
        );
        assert!(!temp_dir.join("mods/new.jar").exists());
        assert!(!temp_dir.join(".dr3sh/txn.json").exists());

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_transactional_apply_retains_player_change_backup() {
        let temp_dir = std::env::temp_dir().join(format!("dr3sh_apply_test_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(temp_dir.join("config")).unwrap();
        fs::write(temp_dir.join("config/test.toml"), b"player_modified").unwrap();

        let txn_id = "txn_apply_1";
        let staging_dir = temp_dir.join(".dr3sh/staging").join(txn_id);
        fs::create_dir_all(staging_dir.join("config")).unwrap();
        let new_bytes = b"pack_new_content";
        fs::write(staging_dir.join("config/test.toml"), new_bytes).unwrap();
        let new_sha: String = Sha256::digest(new_bytes).iter().map(|b| format!("{b:02x}")).collect();

        let manifest = Manifest {
            schema: 1,
            pack_id: "testpack".to_string(),
            version: "1.1.0".to_string(),
            minecraft_version: "1.21.1".to_string(),
            loader: "fabric".to_string(),
            loader_version: String::new(),
            zip: ManifestZip {
                url: "https://example.com/pack.zip".to_string(),
                size: 100,
                sha256: "a".repeat(64),
            },
            rules: vec![ManifestRule {
                pattern: "**".to_string(),
                policy: "update_on_change".to_string(),
            }],
            files: vec![ManifestFileEntry {
                path: "config/test.toml".to_string(),
                sha256: new_sha.clone(),
                size: new_bytes.len() as u64,
                policy: "update_on_change".to_string(),
                archive_path: None,
                mtime: None,
            }],
        };

        let plan = ReconciliationPlan {
            migration_mode: false,
            items: vec![FilePlanItem {
                path: "config/test.toml".to_string(),
                policy: FilePolicy::UpdateOnChange,
                action: PlanActionKind::BackupThenWrite,
                tag: Some("OVERWROTE_PLAYER_CHANGE".to_string()),
                expected_sha256: Some(new_sha),
                expected_size: new_bytes.len() as u64,
                archive_entry_name: None,
            }],
            counts_by_action: HashMap::new(),
            counts_by_tag: HashMap::new(),
            write_bytes_total: new_bytes.len() as u64,
        };

        apply_plan_transactional(
            None,
            "op_apply",
            "srv",
            &temp_dir,
            txn_id,
            Some("1.0.0".to_string()),
            &manifest,
            &plan,
        )
        .unwrap();

        assert_eq!(
            fs::read(temp_dir.join("config/test.toml")).unwrap(),
            new_bytes
        );
        // Player's copy is preserved in backup/<txn_id>/config/test.toml
        assert_eq!(
            fs::read(
                temp_dir
                    .join(".dr3sh/backup")
                    .join(txn_id)
                    .join("config/test.toml")
            )
            .unwrap(),
            b"player_modified"
        );
        assert!(temp_dir.join(".dr3sh/installed_manifest.json").exists());

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_sim_three_version_lifecycle_and_wrapped_zip_extraction() {
        use crate::dr3sh::manifest::{
            build_reconciliation_plan, default_rules, load_baseline, verify_zip_against_manifest,
        };
        use std::io::Write;

        let temp_dir = std::env::temp_dir().join(format!("dr3sh_3ver_sim_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&temp_dir).unwrap();

        let make_zip = |prefix: &str, entries: &[(&str, &[u8])]| -> (PathBuf, String, u64) {
            let zip_path = temp_dir.join(format!("pack_{}.zip", uuid::Uuid::new_v4()));
            let f = File::create(&zip_path).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (rel, data) in entries {
                let full_entry = if prefix.is_empty() {
                    rel.to_string()
                } else {
                    format!("{prefix}/{rel}")
                };
                zw.start_file(full_entry, opts).unwrap();
                zw.write_all(data).unwrap();
            }
            zw.finish().unwrap();
            let meta = fs::metadata(&zip_path).unwrap();
            let sha = crate::dr3sh::manifest::hash_local_file(&zip_path).unwrap();
            (zip_path, sha, meta.len())
        };

        let make_manifest = |ver: &str, zip_sha: &str, zip_sz: u64, entries: &[(&str, &[u8], &str)]| -> Manifest {
            let files = entries
                .iter()
                .map(|(p, data, pol)| {
                    let sha: String = Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect();
                    ManifestFileEntry {
                        path: p.to_string(),
                        sha256: sha,
                        size: data.len() as u64,
                        policy: pol.to_string(),
                        archive_path: None,
                        mtime: None,
                    }
                })
                .collect();
            Manifest {
                schema: 1,
                pack_id: "cobbleverse".to_string(),
                version: ver.to_string(),
                minecraft_version: "1.21.1".to_string(),
                loader: "fabric".to_string(),
                loader_version: "0.16.9".to_string(),
                zip: ManifestZip {
                    url: "https://github.com/Dr3sh/modpack-versions/releases/download/v1/pack.zip".to_string(),
                    size: zip_sz,
                    sha256: zip_sha.to_string(),
                },
                rules: default_rules(),
                files,
            }
        };

        let inst_dir = temp_dir.join("instance");
        fs::create_dir_all(inst_dir.join("saves/MyWorld")).unwrap();
        fs::write(inst_dir.join("saves/MyWorld/level.dat"), b"world_save_data").unwrap();

        // v1.0.0: Wrapped ZIP ("Desgraciamon-v1/...") without archive_path in manifest
        let v1_entries: Vec<(&str, &[u8], &str)> = vec![
            ("mods/a.jar", b"mod_a_v1", "managed"),
            ("mods/b.jar", b"mod_b_v1", "managed"),
            ("config/pack.toml", b"cfg_v1", "update_on_change"),
            ("options.txt", b"fov:70", "seed"),
        ];
        let v1_zip_raw: Vec<(&str, &[u8])> = v1_entries.iter().map(|(p, d, _)| (*p, *d)).collect();
        let (z1_path, z1_sha, z1_sz) = make_zip("Desgraciamon-v1", &v1_zip_raw);
        let m1 = make_manifest("1.0.0", &z1_sha, z1_sz, &v1_entries);

        let resolved1 = verify_zip_against_manifest(&z1_path, &m1).unwrap();
        let mut plan1 = build_reconciliation_plan(&inst_dir, None, &m1, false);
        for item in &mut plan1.items {
            if item.archive_entry_name.is_none()
                && let Some(act) = resolved1.get(&item.path.to_ascii_lowercase())
            {
                item.archive_entry_name = Some(act.clone());
            }
        }
        let cancel = Arc::new(AtomicBool::new(false));
        extract_to_staging(None, "op1", "srv", &z1_path, &inst_dir, "txn1", &plan1, &cancel).unwrap();
        apply_plan_transactional(None, "op1", "srv", &inst_dir, "txn1", None, &m1, &plan1).unwrap();

        assert_eq!(fs::read(inst_dir.join("mods/a.jar")).unwrap(), b"mod_a_v1");
        assert_eq!(fs::read(inst_dir.join("config/pack.toml")).unwrap(), b"cfg_v1");
        assert_eq!(fs::read(inst_dir.join("options.txt")).unwrap(), b"fov:70");

        // Player customizes config/pack.toml and options.txt
        fs::write(inst_dir.join("config/pack.toml"), b"player_custom_cfg_longer").unwrap();
        fs::write(inst_dir.join("options.txt"), b"fov:110").unwrap();

        // v1.1.0: config/pack.toml is UNCHANGED in pack ("cfg_v1"), mods/b.jar removed, mods/c.jar added, options.txt changed in pack
        let v2_entries: Vec<(&str, &[u8], &str)> = vec![
            ("mods/a.jar", b"mod_a_v1", "managed"),
            ("mods/c.jar", b"mod_c_v2", "managed"),
            ("config/pack.toml", b"cfg_v1", "update_on_change"),
            ("options.txt", b"fov:90", "seed"),
        ];
        let v2_zip_raw: Vec<(&str, &[u8])> = v2_entries.iter().map(|(p, d, _)| (*p, *d)).collect();
        let (z2_path, z2_sha, z2_sz) = make_zip("", &v2_zip_raw);
        let m2 = make_manifest("1.1.0", &z2_sha, z2_sz, &v2_entries);

        let b1 = load_baseline(&inst_dir).expect("baseline 1.0.0 must exist");
        let plan2 = build_reconciliation_plan(&inst_dir, Some(&b1), &m2, false);
        extract_to_staging(None, "op2", "srv", &z2_path, &inst_dir, "txn2", &plan2, &cancel).unwrap();
        apply_plan_transactional(
            None,
            "op2",
            "srv",
            &inst_dir,
            "txn2",
            Some("1.0.0".to_string()),
            &m2,
            &plan2,
        )
        .unwrap();

        assert!(!inst_dir.join("mods/b.jar").exists());
        assert_eq!(fs::read(inst_dir.join("mods/c.jar")).unwrap(), b"mod_c_v2");
        assert_eq!(
            fs::read(inst_dir.join("config/pack.toml")).unwrap(),
            b"player_custom_cfg_longer"
        );
        assert_eq!(fs::read(inst_dir.join("options.txt")).unwrap(), b"fov:110");

        // v1.2.0: config/pack.toml IS updated in pack ("cfg_v3").
        // Even with mtime cache enabled (ignore_mtime_cache = false), it MUST detect player edit from v1.0.0 and do BackupThenWrite!
        let v3_entries: Vec<(&str, &[u8], &str)> = vec![
            ("mods/a.jar", b"mod_a_v1", "managed"),
            ("mods/c.jar", b"mod_c_v2", "managed"),
            ("config/pack.toml", b"cfg_v3_updated_by_author", "update_on_change"),
            ("options.txt", b"fov:90", "seed"),
        ];
        let v3_zip_raw: Vec<(&str, &[u8])> = v3_entries.iter().map(|(p, d, _)| (*p, *d)).collect();
        let (z3_path, z3_sha, z3_sz) = make_zip("", &v3_zip_raw);
        let m3 = make_manifest("1.2.0", &z3_sha, z3_sz, &v3_entries);

        let b2 = load_baseline(&inst_dir).expect("baseline 1.1.0 must exist");
        let plan3 = build_reconciliation_plan(&inst_dir, Some(&b2), &m3, false);
        let cfg_item = plan3
            .items
            .iter()
            .find(|i| i.path == "config/pack.toml")
            .unwrap();
        assert_eq!(cfg_item.action, PlanActionKind::BackupThenWrite);
        assert_eq!(cfg_item.tag.as_deref(), Some("OVERWROTE_PLAYER_CHANGE"));

        extract_to_staging(None, "op3", "srv", &z3_path, &inst_dir, "txn3", &plan3, &cancel).unwrap();
        apply_plan_transactional(
            None,
            "op3",
            "srv",
            &inst_dir,
            "txn3",
            Some("1.1.0".to_string()),
            &m3,
            &plan3,
        )
        .unwrap();

        assert_eq!(
            fs::read(inst_dir.join("config/pack.toml")).unwrap(),
            b"cfg_v3_updated_by_author"
        );
        assert_eq!(
            fs::read(inst_dir.join(".dr3sh/backup/txn3/config/pack.toml")).unwrap(),
            b"player_custom_cfg_longer"
        );
        assert_eq!(
            fs::read(inst_dir.join("saves/MyWorld/level.dat")).unwrap(),
            b"world_save_data"
        );

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_sim_unflushed_batch_crash_recovery_and_backup_pruning() {
        let temp_dir = std::env::temp_dir().join(format!("dr3sh_batch_crash_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(temp_dir.join("mods")).unwrap();

        let txn_id = "txn_unflushed";
        let backup_dir = temp_dir.join(".dr3sh/backup").join(txn_id);
        let staging_dir = temp_dir.join(".dr3sh/staging").join(txn_id);
        fs::create_dir_all(backup_dir.join("mods")).unwrap();
        fs::create_dir_all(staging_dir.join("mods")).unwrap();

        // Simulate crash where ops happened on disk BEFORE journal.ops[i].done was flushed to txn.json
        fs::write(backup_dir.join("mods/old.jar"), b"original_mod").unwrap();
        fs::write(temp_dir.join("mods/new.jar"), b"new_placed_mod").unwrap();

        let journal = Journal {
            txn_id: txn_id.to_string(),
            pack_id: "test".to_string(),
            from_version: Some("1.0.0".to_string()),
            to_version: "1.1.0".to_string(),
            state: "applying".to_string(),
            preserve_backup: false,
            ops: vec![
                JournalOp {
                    op: "backup".to_string(),
                    path: "mods/old.jar".to_string(),
                    done: false,
                    tag: None,
                },
                JournalOp {
                    op: "place".to_string(),
                    path: "mods/new.jar".to_string(),
                    done: false,
                    tag: None,
                },
            ],
        };
        journal.write_flushed(&temp_dir).unwrap();

        recover_instance(Some("sim_rec"), &temp_dir).unwrap();
        assert_eq!(
            fs::read(temp_dir.join("mods/old.jar")).unwrap(),
            b"original_mod"
        );
        assert!(!temp_dir.join("mods/new.jar").exists());

        // Simulate 7 backup folders and verify prune_backups keeps exactly 5
        for i in 0..7 {
            let b = temp_dir.join(".dr3sh/backup").join(format!("b_{i}"));
            fs::create_dir_all(&b).unwrap();
            std::thread::sleep(Duration::from_millis(15));
        }
        prune_backups(Some("sim_prune"), &temp_dir, 5);
        let remaining = fs::read_dir(temp_dir.join(".dr3sh/backup"))
            .unwrap()
            .count();
        assert_eq!(remaining, 5);

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
