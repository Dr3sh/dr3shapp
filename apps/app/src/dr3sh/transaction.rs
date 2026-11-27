use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::dr3sh::download::emit_progress_detailed;
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

    let mut needed: HashMap<String, (String, String, u64)> = HashMap::new();
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
                needed.insert(
                    lookup_key,
                    (entry.path.clone(), sha.clone(), entry.expected_size),
                );
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
        if cancel_token.load(Ordering::SeqCst) {
            return Err(Dr3shError::Cancelled {
                stage: "extract".to_string(),
            });
        }

        let total_needed = needed.len() as u64;
        let total_extract_bytes: u64 = needed
            .values()
            .map(|(_, _, sz)| *sz)
            .sum::<u64>()
            .max(1);

        if let Some(app_handle) = app {
            emit_progress_detailed(
                app_handle,
                op_id,
                server_id,
                "extract",
                format!("Descomprimiendo archivos (0/{})...", total_needed),
                0,
                total_extract_bytes,
                0,
                None,
                Some(72),
                None,
                Some(0),
                Some(total_needed),
            );
        }

        // Open File directly without BufReader: ZipArchive seeks per Central Directory entry,
        // so wrapping File in a large BufReader thrashes page reads on large packs.
        let file = File::open(zip_path).map_err(|e| Dr3shError::ZipCorrupt {
            entry: "<archive>".to_string(),
            error: e.to_string(),
        })?;
        let archive = zip::ZipArchive::new(file).map_err(|e| Dr3shError::ZipCorrupt {
            entry: "<archive>".to_string(),
            error: e.to_string(),
        })?;
        let archive_metadata = archive.metadata();

        let extract_start = Instant::now();
        let mut common_prefix: Option<String> = None;
        let mut raw_entries: Vec<(usize, String)> = Vec::with_capacity(archive.len());

        for (idx, name_res) in archive.file_names().enumerate() {
            if cancel_token.load(Ordering::Relaxed) {
                return Err(Dr3shError::Cancelled {
                    stage: "extract".to_string(),
                });
            }
            let raw_n = name_res.map_err(|e| Dr3shError::ZipCorrupt {
                entry: format!("index {}", idx),
                error: e.to_string(),
            })?;
            if raw_n.ends_with('/') || raw_n.ends_with('\\') {
                continue;
            }
            let norm = raw_n.replace('\\', "/");
            validate_relative_path(&norm)?;
            if let Some((first_seg, _)) = norm.split_once('/') {
                let candidate = format!("{first_seg}/");
                match &common_prefix {
                    None => common_prefix = Some(candidate),
                    Some(existing) if !existing.eq_ignore_ascii_case(&candidate) => {
                        common_prefix = None;
                    }
                    _ => {}
                }
            } else {
                common_prefix = None;
            }
            raw_entries.push((idx, norm));
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

        let mut work_items: Vec<(usize, String, String, u64)> = Vec::with_capacity(needed.len());
        for (idx, raw_name) in raw_entries {
            if cancel_token.load(Ordering::Relaxed) {
                return Err(Dr3shError::Cancelled {
                    stage: "extract".to_string(),
                });
            }
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
            if let Some((canonical_rel_path, expected_sha, expected_sz)) = matched {
                work_items.push((idx, canonical_rel_path, expected_sha, expected_sz));
            }
        }

        if !needed.is_empty() {
            let missing: Vec<String> = needed.into_values().map(|(p, _, _)| p).collect();
            return Err(Dr3shError::ZipContentMismatch {
                missing_entries: missing,
            });
        }

        let mut created_parents: HashSet<PathBuf> = HashSet::new();
        for (_, canonical_rel_path, _, _) in &work_items {
            if let Some(parent) = staging_dir.join(canonical_rel_path).parent() {
                if created_parents.insert(parent.to_path_buf()) {
                    fs::create_dir_all(parent).map_err(|e| Dr3shError::FsPermission {
                        path: parent.to_string_lossy().to_string(),
                        os_code: e.raw_os_error().unwrap_or(-1).to_string(),
                    })?;
                }
            }
        }

        let worker_count = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 6)
            .min(work_items.len().max(1));

        let next_idx = AtomicUsize::new(0);
        let extracted_count = AtomicU64::new(0);
        let extracted_bytes = AtomicU64::new(0);
        let abort_err = AtomicBool::new(false);
        let first_error: Mutex<Option<Dr3shError>> = Mutex::new(None);
        let last_emit: Mutex<Instant> = Mutex::new(extract_start);

        let record_err = |err: Dr3shError| {
            abort_err.store(true, Ordering::Release);
            if let Ok(mut guard) = first_error.lock()
                && guard.is_none()
            {
                *guard = Some(err);
            }
        };

        let emit_extract_progress = |force: bool| {
            let Some(app_handle) = app else {
                return;
            };
            let Ok(mut guard) = last_emit.try_lock() else {
                return;
            };
            if !force && guard.elapsed() < Duration::from_millis(150) {
                return;
            }
            let done_cnt = extracted_count.load(Ordering::Relaxed);
            let shown_bytes = extracted_bytes
                .load(Ordering::Relaxed)
                .min(total_extract_bytes);
            let ratio = (shown_bytes as f64 / total_extract_bytes as f64).clamp(0.0, 1.0);
            let percent = (72.0 + ratio * 22.0).round() as u32;
            let elapsed_secs = extract_start.elapsed().as_secs_f64().max(0.2);
            let speed_bps = (shown_bytes as f64 / elapsed_secs) as u64;
            let eta_seconds = if total_extract_bytes > shown_bytes && speed_bps > 1024 {
                Some(((total_extract_bytes - shown_bytes) / speed_bps).max(1))
            } else {
                None
            };
            emit_progress_detailed(
                app_handle,
                op_id,
                server_id,
                "extract",
                format!("Descomprimiendo archivos ({}/{})...", done_cnt, total_needed),
                shown_bytes,
                total_extract_bytes,
                speed_bps,
                None,
                Some(percent),
                eta_seconds,
                Some(done_cnt),
                Some(total_needed),
            );
            *guard = Instant::now();
        };

        drop(archive);

        std::thread::scope(|scope| {
            for _ in 0..worker_count {
                scope.spawn(|| {
                    let w_file = match File::open(zip_path) {
                        Ok(f) => f,
                        Err(e) => {
                            record_err(Dr3shError::ZipCorrupt {
                                entry: "<archive>".to_string(),
                                error: e.to_string(),
                            });
                            return;
                        }
                    };
                    let mut w_archive = unsafe {
                        zip::ZipArchive::unsafe_new_with_metadata(
                            w_file,
                            Arc::clone(&archive_metadata),
                        )
                    };
                    let mut buf = vec![0u8; 256 * 1024];

                    loop {
                        if cancel_token.load(Ordering::Relaxed) {
                            record_err(Dr3shError::Cancelled {
                                stage: "extract".to_string(),
                            });
                            return;
                        }
                        if abort_err.load(Ordering::Acquire) {
                            return;
                        }

                        let idx = next_idx.fetch_add(1, Ordering::Relaxed);
                        if idx >= work_items.len() {
                            return;
                        }

                        let (zip_idx, ref canonical_rel_path, ref expected_sha, _) =
                            work_items[idx];

                        let mut entry = match w_archive.by_index(zip_idx) {
                            Ok(e) => e,
                            Err(e) => {
                                record_err(Dr3shError::ZipCorrupt {
                                    entry: canonical_rel_path.clone(),
                                    error: e.to_string(),
                                });
                                return;
                            }
                        };

                        let out_path = staging_dir.join(canonical_rel_path);
                        let out_file = match File::create(&out_path) {
                            Ok(f) => f,
                            Err(e) => {
                                record_err(Dr3shError::FsPermission {
                                    path: out_path.to_string_lossy().to_string(),
                                    os_code: e.raw_os_error().unwrap_or(-1).to_string(),
                                });
                                return;
                            }
                        };
                        let mut out_writer = BufWriter::with_capacity(512 * 1024, out_file);
                        let mut hasher = Sha256::new();

                        loop {
                            if cancel_token.load(Ordering::Relaxed) {
                                record_err(Dr3shError::Cancelled {
                                    stage: "extract".to_string(),
                                });
                                return;
                            }
                            if abort_err.load(Ordering::Acquire) {
                                return;
                            }
                            let n = match entry.read(&mut buf) {
                                Ok(n) => n,
                                Err(e) => {
                                    record_err(Dr3shError::ZipCorrupt {
                                        entry: canonical_rel_path.clone(),
                                        error: e.to_string(),
                                    });
                                    return;
                                }
                            };
                            if n == 0 {
                                break;
                            }
                            if let Err(e) = out_writer.write_all(&buf[..n]) {
                                record_err(Dr3shError::FsPermission {
                                    path: out_path.to_string_lossy().to_string(),
                                    os_code: e.raw_os_error().unwrap_or(-1).to_string(),
                                });
                                return;
                            }
                            hasher.update(&buf[..n]);
                            extracted_bytes.fetch_add(n as u64, Ordering::Relaxed);
                            emit_extract_progress(false);
                        }

                        if let Err(e) = out_writer.flush() {
                            record_err(Dr3shError::FsPermission {
                                path: out_path.to_string_lossy().to_string(),
                                os_code: e.raw_os_error().unwrap_or(-1).to_string(),
                            });
                            return;
                        }

                        let actual_sha: String =
                            hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
                        if !actual_sha.eq_ignore_ascii_case(expected_sha) {
                            record_err(Dr3shError::ZipCorrupt {
                                entry: canonical_rel_path.clone(),
                                error: format!(
                                    "SHA-256 mismatch (expected {}, got {})",
                                    expected_sha, actual_sha
                                ),
                            });
                            return;
                        }

                        let done_cnt = extracted_count.fetch_add(1, Ordering::Relaxed) + 1;
                        emit_extract_progress(done_cnt == total_needed);
                    }
                });
            }
        });

        if cancel_token.load(Ordering::SeqCst) {
            return Err(Dr3shError::Cancelled {
                stage: "extract".to_string(),
            });
        }
        if let Some(err) = first_error.into_inner().ok().flatten() {
            return Err(err);
        }

        if let Some(app_handle) = app {
            emit_progress_detailed(
                app_handle,
                op_id,
                server_id,
                "extract",
                "Descompresión completada.",
                total_extract_bytes,
                total_extract_bytes,
                0,
                None,
                Some(94),
                None,
                Some(total_needed),
                Some(total_needed),
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
                && (last_progress_emit.elapsed() >= Duration::from_millis(120)
                    || (idx + 1) as u64 == total_ops)
            {
                let done_count = (idx + 1) as u64;
                let percent = (94.0 + (done_count as f64 / total_ops.max(1) as f64) * 5.0)
                    .round() as u32;
                emit_progress_detailed(
                    app_handle,
                    op_id,
                    server_id,
                    "apply",
                    format!("Instalando archivos ({}/{})...", done_count, total_ops),
                    0,
                    0,
                    0,
                    None,
                    Some(percent),
                    None,
                    Some(done_count),
                    Some(total_ops),
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
            emit_progress_detailed(
                app_handle,
                op_id,
                server_id,
                "commit",
                "¡Instalación completada!",
                0,
                0,
                0,
                None,
                Some(100),
                None,
                Some(total_ops),
                Some(total_ops),
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

pub fn cleanup_old_pack_part_downloads(
    op_id: Option<&str>,
    instance_dir: &Path,
    pack_id: &str,
) {
    let safe_pack: String = pack_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let prefix = format!("{safe_pack}-");

    let mut search_dirs = vec![
        crate::dr3sh::download::drive_root_for_path(instance_dir).join(".dr3sh_downloads"),
        instance_dir.join(".dr3sh").join("downloads"),
    ];
    search_dirs.dedup();

    for dl_dir in search_dirs {
        let Ok(entries) = fs::read_dir(&dl_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("part") {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !name.starts_with(&prefix) {
                continue;
            }
            if fs::remove_file(&path).is_ok() {
                log_event(
                    op_id,
                    "CLEANUP",
                    "INFO",
                    "deleted obsolete .part download for pack after successful commit",
                    &[("path", path.to_string_lossy().to_string())],
                );
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AppliedOptionsPatch {
    sha256: String,
    version: String,
}

pub fn apply_options_patch_if_needed(
    op_id: Option<&str>,
    instance_dir: &Path,
    pack_version: &str,
) -> Result<bool, Dr3shError> {
    let patch_path = instance_dir.join("dr3sh_options_patch.txt");
    if !patch_path.is_file() {
        return Ok(false);
    }

    let patch_bytes = fs::read(&patch_path).map_err(|e| Dr3shError::FsPermission {
        path: patch_path.to_string_lossy().to_string(),
        os_code: e.raw_os_error().unwrap_or(-1).to_string(),
    })?;
    let patch_sha256: String = Sha256::digest(&patch_bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();

    let dr3sh_dir = instance_dir.join(".dr3sh");
    let state_path = dr3sh_dir.join("options_patch_applied.json");
    if let Ok(raw) = fs::read_to_string(&state_path)
        && let Ok(prev) = serde_json::from_str::<AppliedOptionsPatch>(&raw)
        && prev.sha256.eq_ignore_ascii_case(&patch_sha256)
    {
        return Ok(false);
    }

    let options_path = instance_dir.join("options.txt");
    if !options_path.is_file() {
        log_event(
            op_id,
            "APPLY",
            "INFO",
            "options.txt not found yet; skipping dr3sh_options_patch.txt so YOSBR defaults remain intact",
            &[("version", pack_version.to_string())],
        );
        return Ok(false);
    }

    let patch_text = String::from_utf8_lossy(&patch_bytes);
    let mut ordered_keys: Vec<String> = Vec::new();
    let mut patch_map: HashMap<String, String> = HashMap::new();
    for raw_line in patch_text.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("//") {
            continue;
        }
        let Some((k_raw, v_raw)) = trimmed.split_once(':') else {
            continue;
        };
        let key = k_raw.trim();
        if key.is_empty() {
            continue;
        }
        let val = v_raw.trim();
        if !patch_map.contains_key(key) {
            ordered_keys.push(key.to_string());
        }
        patch_map.insert(key.to_string(), val.to_string());
    }

    if !ordered_keys.is_empty() {
        let existing_raw = fs::read_to_string(&options_path).map_err(|e| Dr3shError::FsPermission {
            path: options_path.to_string_lossy().to_string(),
            os_code: e.raw_os_error().unwrap_or(-1).to_string(),
        })?;
        let line_ending = if existing_raw.contains("\r\n") { "\r\n" } else { "\n" };
        let mut applied_keys: HashSet<String> = HashSet::new();
        let mut out_lines: Vec<String> = Vec::new();

        for line in existing_raw.lines() {
            if let Some((k, _)) = line.split_once(':') {
                let trimmed_k = k.trim();
                if let Some(new_val) = patch_map.get(trimmed_k) {
                    out_lines.push(format!("{trimmed_k}:{new_val}"));
                    applied_keys.insert(trimmed_k.to_string());
                    continue;
                }
            }
            out_lines.push(line.to_string());
        }

        for key in &ordered_keys {
            if !applied_keys.contains(key)
                && let Some(val) = patch_map.get(key)
            {
                out_lines.push(format!("{key}:{val}"));
            }
        }

        let mut new_content = out_lines.join(line_ending);
        if existing_raw.ends_with('\n') || !new_content.is_empty() {
            new_content.push_str(line_ending);
        }

        let tmp_options = instance_dir.join("options.txt.tmp");
        {
            let mut f = File::create(&tmp_options).map_err(|e| Dr3shError::FsPermission {
                path: tmp_options.to_string_lossy().to_string(),
                os_code: e.raw_os_error().unwrap_or(-1).to_string(),
            })?;
            f.write_all(new_content.as_bytes()).map_err(|e| Dr3shError::FsPermission {
                path: tmp_options.to_string_lossy().to_string(),
                os_code: e.raw_os_error().unwrap_or(-1).to_string(),
            })?;
            let _ = f.sync_all();
        }
        rename_with_lock_retries(&tmp_options, &options_path)?;
    }

    fs::create_dir_all(&dr3sh_dir).map_err(|e| Dr3shError::FsPermission {
        path: dr3sh_dir.to_string_lossy().to_string(),
        os_code: e.raw_os_error().unwrap_or(-1).to_string(),
    })?;
    let state_tmp = dr3sh_dir.join("options_patch_applied.json.tmp");
    let state_bytes = serde_json::to_vec_pretty(&AppliedOptionsPatch {
        sha256: patch_sha256,
        version: pack_version.to_string(),
    })
    .map_err(|e| Dr3shError::unknown(format!("Failed to serialize options patch state: {e}")))?;
    {
        let mut f = File::create(&state_tmp).map_err(|e| Dr3shError::FsPermission {
            path: state_tmp.to_string_lossy().to_string(),
            os_code: e.raw_os_error().unwrap_or(-1).to_string(),
        })?;
        f.write_all(&state_bytes).map_err(|e| Dr3shError::FsPermission {
            path: state_tmp.to_string_lossy().to_string(),
            os_code: e.raw_os_error().unwrap_or(-1).to_string(),
        })?;
        let _ = f.sync_all();
    }
    rename_with_lock_retries(&state_tmp, &state_path)?;

    log_event(
        op_id,
        "APPLY",
        "INFO",
        "applied surgical options.txt patch",
        &[
            ("version", pack_version.to_string()),
            ("keys_updated", ordered_keys.len().to_string()),
        ],
    );
    Ok(true)
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
            ("servers.dat", b"srv_v1", "seed"),
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
        assert_eq!(fs::read(inst_dir.join("servers.dat")).unwrap(), b"srv_v1");

        // Player customizes config/pack.toml and servers.dat
        fs::write(inst_dir.join("config/pack.toml"), b"player_custom_cfg_longer").unwrap();
        fs::write(inst_dir.join("servers.dat"), b"player_srv_custom").unwrap();

        // v1.1.0: config/pack.toml is UNCHANGED in pack ("cfg_v1"), mods/b.jar removed, mods/c.jar added, servers.dat changed in pack
        let v2_entries: Vec<(&str, &[u8], &str)> = vec![
            ("mods/a.jar", b"mod_a_v1", "managed"),
            ("mods/c.jar", b"mod_c_v2", "managed"),
            ("config/pack.toml", b"cfg_v1", "update_on_change"),
            ("servers.dat", b"srv_v2", "seed"),
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
        assert_eq!(fs::read(inst_dir.join("servers.dat")).unwrap(), b"player_srv_custom");

        // v1.2.0: config/pack.toml IS updated in pack ("cfg_v3").
        // Even with mtime cache enabled (ignore_mtime_cache = false), it MUST detect player edit from v1.0.0 and do BackupThenWrite!
        let v3_entries: Vec<(&str, &[u8], &str)> = vec![
            ("mods/a.jar", b"mod_a_v1", "managed"),
            ("mods/c.jar", b"mod_c_v2", "managed"),
            ("config/pack.toml", b"cfg_v3_updated_by_author", "update_on_change"),
            ("servers.dat", b"srv_v2", "seed"),
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

    #[test]
    fn test_parallel_extract_and_cancel_immediate() {
        let temp_dir = std::env::temp_dir().join(format!("dr3sh_par_ext_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&temp_dir).unwrap();
        let zip_path = temp_dir.join("pack.zip");
        let inst_dir = temp_dir.join("inst");
        fs::create_dir_all(&inst_dir).unwrap();

        let mut plan_items = Vec::new();
        {
            let f = File::create(&zip_path).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for i in 0..16 {
                let rel = format!("mods/mod_{i:02}.jar");
                let payload = vec![(i as u8).wrapping_mul(17).wrapping_add(5); 32 * 1024];
                let sha: String = Sha256::digest(&payload)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                zw.start_file(&rel, options).unwrap();
                zw.write_all(&payload).unwrap();
                plan_items.push(crate::dr3sh::manifest::FilePlanItem {
                    path: rel.clone(),
                    policy: FilePolicy::Managed,
                    action: PlanActionKind::Write,
                    tag: None,
                    expected_sha256: Some(sha),
                    expected_size: payload.len() as u64,
                    archive_entry_name: Some(rel),
                });
            }
            zw.finish().unwrap();
        }

        let plan = ReconciliationPlan {
            migration_mode: false,
            items: plan_items,
            counts_by_action: HashMap::new(),
            counts_by_tag: HashMap::new(),
            write_bytes_total: 16 * 32 * 1024,
        };

        // 1. Pre-cancelled token should abort cleanly before extracting
        let pre_cancel = Arc::new(AtomicBool::new(true));
        let cancel_res = extract_to_staging(
            None,
            "op_par_cancel",
            "srv",
            &zip_path,
            &inst_dir,
            "txn_cancel",
            &plan,
            &pre_cancel,
        );
        assert!(matches!(cancel_res, Err(Dr3shError::Cancelled { .. })));

        // 2. Non-cancelled run should extract all 16 files accurately in parallel
        let ok_cancel = Arc::new(AtomicBool::new(false));
        let staging = extract_to_staging(
            None,
            "op_par_ok",
            "srv",
            &zip_path,
            &inst_dir,
            "txn_ok",
            &plan,
            &ok_cancel,
        )
        .expect("parallel extraction should succeed");
        for i in 0..16 {
            let rel = format!("mods/mod_{i:02}.jar");
            let data = fs::read(staging.join(&rel)).unwrap();
            assert_eq!(data.len(), 32 * 1024);
            assert_eq!(data[0], (i as u8).wrapping_mul(17).wrapping_add(5));
        }

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_surgical_options_patch_applied_once_and_preserves_other_settings() {
        let temp_dir =
            std::env::temp_dir().join(format!("dr3sh_opt_patch_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&temp_dir).unwrap();

        let opts_path = temp_dir.join("options.txt");
        let patch_path = temp_dir.join("dr3sh_options_patch.txt");

        fs::write(&opts_path, "fov:1.0\nsoundCategory_master:0.4\nlang:es_es\n").unwrap();
        fs::write(&patch_path, "# Cambio puntual\nfov:0.8\nao:false\n").unwrap();

        assert!(apply_options_patch_if_needed(None, &temp_dir, "1.1.0").unwrap());
        let after_first = fs::read_to_string(&opts_path).unwrap();
        assert!(after_first.contains("fov:0.8\n"));
        assert!(after_first.contains("soundCategory_master:0.4\n"));
        assert!(after_first.contains("lang:es_es\n"));
        assert!(after_first.contains("ao:false\n"));

        // Player changes fov back; re-running same patch hash must NOT overwrite
        fs::write(&opts_path, after_first.replace("fov:0.8", "fov:0.95")).unwrap();
        assert!(!apply_options_patch_if_needed(None, &temp_dir, "1.1.0").unwrap());
        assert!(fs::read_to_string(&opts_path).unwrap().contains("fov:0.95"));

        // New patch content DOES apply
        fs::write(&patch_path, "renderDistance:12\n").unwrap();
        assert!(apply_options_patch_if_needed(None, &temp_dir, "1.2.0").unwrap());
        let after_second = fs::read_to_string(&opts_path).unwrap();
        assert!(after_second.contains("fov:0.95"));
        assert!(after_second.contains("renderDistance:12"));

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
