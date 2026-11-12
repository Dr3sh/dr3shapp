pub mod download;
pub mod errors;
pub mod logger;
pub mod manifest;
pub mod preflight;
pub mod state;
pub mod transaction;

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tauri::Emitter;
use tauri_plugin_fs::FsExt;
use tauri_plugin_opener::OpenerExt;
use theseus::data::{AppliedContentSetPatch, ModLoader};
use theseus::install::InstallJobStatus;
use theseus::prelude::*;

use crate::dr3sh::download::{
    build_http_client, download_zip_resumable, drive_root_for_path, emit_progress,
    existing_part_size, extract_host, part_file_path, DownloadSpec,
};
use crate::dr3sh::errors::{Dr3shError, Dr3shErrorPayload};
use crate::dr3sh::logger::{
    build_diagnostics, launcher_data_dir, log_event, logs_root_dir, new_op_id, os_version_string,
    prune_logs, register_op_meta, sanitize_url, update_op_stage, OpDiagnosticMeta,
};
use crate::dr3sh::manifest::{
    build_reconciliation_plan, load_baseline, synthesize_manifest_from_zip,
    verify_zip_against_manifest, Manifest,
};
use crate::dr3sh::preflight::{
    ensure_safe_junction, is_launcher_owned_dir, is_reparse_point, remove_junction_link_only,
    run_preflight, sanitize_instance_folder_name,
};
use crate::dr3sh::state::{
    cancel_operation, complete_operation, get_all_server_states, get_cached_remote_gate,
    get_instance_lock, get_server_state, has_active_operations, is_instance_locked,
    migrate_local_storage_state, register_operation, set_server_error, update_cached_remote_gate,
    update_server_state, LegacyLocalStateEntry, ServerInstallState, ServerStateEntry,
};
use crate::dr3sh::transaction::{
    apply_plan_transactional, cleanup_stale_part_downloads, extract_to_staging, recover_instance,
};

const REMOTE_VERSIONS_URL: &str =
    "https://raw.githubusercontent.com/Dr3sh/Dr3sh.github.io/main/versions.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionRefSpec {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, alias = "server_name")]
    pub server_name: Option<String>,
    #[serde(default, alias = "game_version")]
    pub game_version: Option<String>,
    #[serde(default)]
    pub loader: Option<String>,
    #[serde(default, alias = "loader_version")]
    pub loader_version: Option<String>,
    #[serde(default, alias = "download_url", alias = "modpack")]
    pub download_url: Option<String>,
    #[serde(default, alias = "manifest_url", alias = "manifest")]
    pub manifest_url: Option<String>,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default, alias = "size_mb")]
    pub size_mb: Option<f64>,
    #[serde(default)]
    pub icon_url: Option<String>,
    #[serde(default)]
    pub bypass_gate: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoneEventPayload {
    pub op_id: String,
    pub server_id: String,
    pub ok: bool,
    pub version: Option<String>,
    pub instance_id: Option<String>,
    pub instance_path: Option<String>,
    pub migration_notice: bool,
    pub summary: Option<OperationSummary>,
    pub error: Option<Dr3shErrorPayload>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationSummary {
    pub write_count: usize,
    pub backup_then_write_count: usize,
    pub remove_count: usize,
    pub noop_count: usize,
    pub unknown_managed_count: usize,
    pub downloaded_bytes: u64,
    pub avg_speed_bps: u64,
    pub retries: u32,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchPreflightResult {
    pub ok: bool,
    pub instance_id: Option<String>,
    pub instance_path: Option<String>,
    pub refreshed_auth: bool,
}

pub async fn recover_all_on_startup() {
    prune_logs();

    let mut roots: Vec<PathBuf> = Vec::new();
    let mut candidate_dirs: Vec<PathBuf> = Vec::new();

    if let Ok(state) = State::get().await {
        let instances_root = state.directories.instances_dir();
        roots.push(drive_root_for_path(&instances_root));
        if let Ok(entries) = fs::read_dir(&instances_root) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    candidate_dirs.push(p);
                }
            }
        }
    }

    for (_, srv) in get_all_server_states() {
        if let Some(ref p_str) = srv.instance_path {
            let p = PathBuf::from(p_str);
            roots.push(drive_root_for_path(&p));
            if p.is_dir() && !candidate_dirs.contains(&p) {
                candidate_dirs.push(p);
            }
        }
    }

    roots.sort();
    roots.dedup();
    cleanup_stale_part_downloads(&roots);

    for dir in candidate_dirs {
        if let Err(e) = recover_instance(None, &dir) {
            log_event(
                None,
                "RECOVERY",
                "ERROR",
                "crash recovery failed on startup",
                &[
                    ("instance_dir", dir.to_string_lossy().to_string()),
                    ("error", e.to_string()),
                ],
            );
        }
    }
}

pub async fn can_install_updater_safely() -> bool {
    if has_active_operations() {
        return false;
    }
    if let Ok(running) = theseus::process::get_all().await {
        if !running.is_empty() {
            return false;
        }
    }
    true
}

pub async fn verify_remote_gate(server_id: &str, bypass_gate: bool) -> Result<(), Dr3shError> {
    if bypass_gate {
        return Ok(());
    }

    let Ok(client) = build_http_client() else {
        return check_cached_gate_fallback(server_id);
    };

    let url = format!(
        "{}?t={}",
        REMOTE_VERSIONS_URL,
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    );

    let Ok(resp) = client
        .get(&url)
        .timeout(Duration::from_secs(8))
        .send()
        .await
    else {
        return check_cached_gate_fallback(server_id);
    };

    if !resp.status().is_success() {
        return check_cached_gate_fallback(server_id);
    }

    // Use server HTTP Date header, never the local clock
    let server_now: Option<DateTime<Utc>> = resp
        .headers()
        .get(tauri_plugin_http::reqwest::header::DATE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| httpdate::parse_http_date(s).ok())
        .map(DateTime::<Utc>::from);

    let Ok(bytes) = resp.bytes().await else {
        return check_cached_gate_fallback(server_id);
    };
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return check_cached_gate_fallback(server_id);
    };

    let servers_opt = body.get("servers").and_then(|s| s.as_object());
    let Some(servers) = servers_opt else {
        return Ok(());
    };

    let Some(srv) = servers.get(server_id) else {
        return Ok(());
    };

    let status = srv
        .get("status")
        .and_then(|v| v.as_str())
        .map(|s| s.to_ascii_lowercase());
    let maintenance_flag = srv
        .get("maintenance")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || matches!(status.as_deref(), Some("maintenance") | Some("coming_soon"));

    update_cached_remote_gate(server_id, status.clone(), maintenance_flag);

    if maintenance_flag {
        return Err(Dr3shError::unknown(format!(
            "El servidor '{}' se encuentra en mantenimiento o próximamente disponible.",
            server_id
        )));
    }

    if status.as_deref() == Some("testing") {
        if let Some(expires_str) = srv
            .get("testing_expires_at")
            .or_else(|| srv.get("testingExpiresAt"))
            .and_then(|v| v.as_str())
        {
            if let (Ok(exp_dt), Some(now_dt)) = (DateTime::parse_from_rfc3339(expires_str), server_now)
            {
                if now_dt > exp_dt.with_timezone(&Utc) {
                    return Err(Dr3shError::unknown(format!(
                        "El periodo de pruebas para '{}' ha finalizado.",
                        server_id
                    )));
                }
            }
        }
    }

    Ok(())
}

fn check_cached_gate_fallback(server_id: &str) -> Result<(), Dr3shError> {
    if let Some(cached) = get_cached_remote_gate(server_id) {
        if cached.maintenance {
            return Err(Dr3shError::unknown(format!(
                "El servidor '{}' continúa en mantenimiento (sin conexión para verificar estado).",
                server_id
            )));
        }
    }
    Ok(())
}

fn parse_mod_loader(loader_str: Option<&str>) -> ModLoader {
    match loader_str
        .unwrap_or("fabric")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "forge" => ModLoader::Forge,
        "neoforge" => ModLoader::NeoForge,
        "quilt" => ModLoader::Quilt,
        "vanilla" => ModLoader::Vanilla,
        _ => ModLoader::Fabric,
    }
}

pub async fn wait_for_theseus_base(
    app: Option<&tauri::AppHandle>,
    op_id: &str,
    server_id: &str,
    instance_id: &str,
    target_job_id: Option<uuid::Uuid>,
    cancel_token: &Arc<AtomicBool>,
) -> Result<(), Dr3shError> {
    let mut last_progress_change = Instant::now();
    let mut last_progress_signature = String::new();
    let timeout = Duration::from_secs(600); // 10 minutes of no progress change

    // Brief initial pause so Theseus can register any newly spawned install job
    tokio::time::sleep(Duration::from_millis(120)).await;

    loop {
        if cancel_token.load(Ordering::SeqCst) {
            if let Some(jid) = target_job_id {
                let _ = theseus::install::cancel_job(jid).await;
            }
            return Err(Dr3shError::Cancelled {
                stage: "base".to_string(),
            });
        }

        let matching_job = if let Some(jid) = target_job_id {
            theseus::install::get_job(jid).await.ok()
        } else {
            let jobs = theseus::install::list_jobs(true)
                .await
                .map_err(|e| Dr3shError::TheseusBase {
                    theseus_error: e.to_string(),
                })?;

            jobs.into_iter()
                .filter(|j| j.instance_id.as_deref() == Some(instance_id))
                .max_by_key(|j| j.modified.clone())
        };

        if let Some(job) = matching_job {
            match job.status {
                InstallJobStatus::Succeeded => {
                    if let Some(app_handle) = app {
                        emit_progress(
                            app_handle,
                            op_id,
                            server_id,
                            "base",
                            "Base de Minecraft lista.",
                            100,
                            100,
                            0,
                            None,
                            Some(10),
                        );
                    }
                    return Ok(());
                }
                InstallJobStatus::Failed | InstallJobStatus::Interrupted => {
                    let err_msg = job
                        .error
                        .or(job.rollback_error)
                        .map(|v| format!("[{}] {}", v.code, v.message))
                        .unwrap_or_else(|| format!("Theseus job {:?} failed", job.status));
                    return Err(Dr3shError::TheseusBase {
                        theseus_error: err_msg,
                    });
                }
                InstallJobStatus::Canceled => {
                    return Err(Dr3shError::Cancelled {
                        stage: "base".to_string(),
                    });
                }
                InstallJobStatus::Queued | InstallJobStatus::Running => {
                    let cur = job.progress.as_ref().map(|p| p.current).unwrap_or(0);
                    let tot = job.progress.as_ref().map(|p| p.total).unwrap_or(100);
                    let sig = format!("{:?}:{}/{}", job.phase, cur, tot);
                    if sig != last_progress_signature {
                        last_progress_signature = sig.clone();
                        last_progress_change = Instant::now();
                    } else if last_progress_change.elapsed() > timeout {
                        return Err(Dr3shError::TheseusTimeout {
                            last_progress: sig,
                        });
                    }

                    let ratio = if tot > 0 {
                        (cur as f64 / tot as f64).clamp(0.0, 1.0)
                    } else {
                        0.2
                    };
                    let pct = (1.0 + ratio * 9.0).round() as u32;
                    if let Some(app_handle) = app {
                        emit_progress(
                            app_handle,
                            op_id,
                            server_id,
                            "base",
                            format!("Instalando Minecraft/Loader ({:?})...", job.phase),
                            (ratio * 100.0) as u64,
                            100,
                            0,
                            None,
                            Some(pct),
                        );
                    }
                }
            }
        } else {
            // Check if the instance is already in Installed stage without an active job
            if let Ok(Some(inst)) = theseus::instance::get(instance_id).await {
                if inst.instance.install_stage.as_str() == "installed" {
                    return Ok(());
                }
            }
            if last_progress_change.elapsed() > timeout {
                return Err(Dr3shError::TheseusTimeout {
                    last_progress: "waiting for instance creation".to_string(),
                });
            }
        }

        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn resolve_version_spec_from_remote(
    server_id: &str,
    version_id: &str,
) -> Result<VersionRefSpec, Dr3shError> {
    let client = build_http_client()?;
    let resp = client
        .get(REMOTE_VERSIONS_URL)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| Dr3shError::ManifestFetch {
            status: format!("network: {e}"),
            host: extract_host(REMOTE_VERSIONS_URL),
        })?;

    if !resp.status().is_success() {
        return Err(Dr3shError::ManifestFetch {
            status: resp.status().as_u16().to_string(),
            host: extract_host(REMOTE_VERSIONS_URL),
        });
    }

    let bytes = resp.bytes().await.map_err(|e| Dr3shError::ManifestInvalid {
        reason: format!("Invalid versions.json body: {}", e),
    })?;
    let doc: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| Dr3shError::ManifestInvalid {
            reason: format!("Invalid versions.json: {}", e),
        })?;

    let srv = doc
        .get("servers")
        .and_then(|s| s.get(server_id))
        .ok_or_else(|| Dr3shError::ManifestInvalid {
            reason: format!("Server '{}' not found in versions.json", server_id),
        })?;

    let server_name = srv
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let versions = srv
        .get("versions")
        .and_then(|v| v.as_array())
        .ok_or_else(|| Dr3shError::ManifestInvalid {
            reason: format!("Server '{}' has no versions array", server_id),
        })?;

    let ver_obj = if version_id.is_empty() || version_id == "latest" {
        versions.first()
    } else {
        versions
            .iter()
            .find(|v| v.get("id").and_then(|x| x.as_str()) == Some(version_id))
            .or_else(|| versions.first())
    }
    .ok_or_else(|| Dr3shError::ManifestInvalid {
        reason: format!("Version '{}' not found for '{}'", version_id, server_id),
    })?;

    let id = ver_obj
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or(version_id)
        .to_string();
    let download_url = ver_obj
        .get("modpack")
        .or_else(|| ver_obj.get("download_url"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let manifest_url = ver_obj
        .get("manifest")
        .or_else(|| ver_obj.get("manifest_url"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let game_version = ver_obj
        .get("game_version")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let loader = ver_obj
        .get("loader")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let loader_version = ver_obj
        .get("loader_version")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let sha256 = ver_obj
        .get("sha256")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let size_mb = ver_obj.get("size_mb").and_then(|v| v.as_f64());

    Ok(VersionRefSpec {
        id,
        name: ver_obj
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        server_name,
        game_version,
        loader,
        loader_version,
        download_url,
        manifest_url,
        sha256,
        size: ver_obj.get("size").and_then(|v| v.as_u64()),
        size_mb,
        icon_url: None,
        bypass_gate: false,
    })
}

async fn fetch_manifest_if_available(
    op_id: &str,
    server_id: &str,
    spec: &VersionRefSpec,
) -> Result<Option<Manifest>, Dr3shError> {
    let zip_url = spec.download_url.clone().unwrap_or_default();
    let explicit_manifest_url = spec
        .manifest_url
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let candidate_url = if let Some(ref url) = explicit_manifest_url {
        Some((url.clone(), true))
    } else if let Some(pos) = zip_url.rfind('/') {
        Some((format!("{}/manifest.json", &zip_url[..pos]), false))
    } else {
        None
    };

    let Some((m_url, is_explicit)) = candidate_url else {
        return Ok(None);
    };

    let client = build_http_client()?;
    const MAX_MANIFEST_RETRIES: u32 = 3;
    let mut last_error_status: Option<String> = None;
    let mut bytes_opt: Option<Vec<u8>> = None;

    for attempt in 1..=MAX_MANIFEST_RETRIES {
        let send_res = client
            .get(&m_url)
            .timeout(Duration::from_secs(15))
            .send()
            .await;

        let resp = match send_res {
            Ok(r) => r,
            Err(e) => {
                let err_msg = format!("network: {e}");
                last_error_status = Some(err_msg.clone());
                if attempt < MAX_MANIFEST_RETRIES {
                    let delay = Duration::from_millis(600 * attempt as u64);
                    log_event(
                        Some(op_id),
                        "MANIFEST",
                        "WARN",
                        &format!("manifest fetch transient error on attempt {attempt}/{MAX_MANIFEST_RETRIES}: {err_msg}; retrying..."),
                        &[("url", sanitize_url(&m_url)), ("attempt", attempt.to_string())],
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
                break;
            }
        };

        let status = resp.status();
        if !status.is_success() {
            let status_str = status.as_u16().to_string();
            last_error_status = Some(status_str.clone());
            let is_transient = status.is_server_error() || status.as_u16() == 429 || status.as_u16() == 408;
            if is_transient && attempt < MAX_MANIFEST_RETRIES {
                let delay = Duration::from_millis(600 * attempt as u64);
                log_event(
                    Some(op_id),
                    "MANIFEST",
                    "WARN",
                    &format!("manifest fetch status {status_str} on attempt {attempt}/{MAX_MANIFEST_RETRIES}; retrying..."),
                    &[("url", sanitize_url(&m_url)), ("attempt", attempt.to_string())],
                );
                tokio::time::sleep(delay).await;
                continue;
            }
            break;
        }

        match resp.bytes().await {
            Ok(b) => {
                bytes_opt = Some(b.to_vec());
                break;
            }
            Err(e) => {
                let err_msg = format!("read: {e}");
                last_error_status = Some(err_msg.clone());
                if attempt < MAX_MANIFEST_RETRIES {
                    let delay = Duration::from_millis(600 * attempt as u64);
                    log_event(
                        Some(op_id),
                        "MANIFEST",
                        "WARN",
                        &format!("manifest body read error on attempt {attempt}/{MAX_MANIFEST_RETRIES}: {err_msg}; retrying..."),
                        &[("url", sanitize_url(&m_url)), ("attempt", attempt.to_string())],
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
                break;
            }
        }
    }

    let bytes = match bytes_opt {
        Some(b) => b,
        None => {
            let err_status = last_error_status.unwrap_or_else(|| "unknown".to_string());
            if !zip_url.is_empty() {
                log_event(
                    Some(op_id),
                    "MANIFEST",
                    "WARN",
                    "failed to fetch remote manifest; falling back to full modpack ZIP download and synthesis",
                    &[
                        ("url", sanitize_url(&m_url)),
                        ("status", err_status),
                        ("is_explicit", is_explicit.to_string()),
                    ],
                );
                return Ok(None);
            }

            if is_explicit {
                return Err(Dr3shError::ManifestFetch {
                    status: err_status,
                    host: extract_host(&m_url),
                });
            }
            return Ok(None);
        }
    };

    let fallback_mc_ver = spec.game_version.as_deref().unwrap_or("1.21.1");
    let fallback_loader = spec.loader.as_deref().unwrap_or("fabric");
    let fallback_loader_ver = spec.loader_version.as_deref().unwrap_or("");

    let parsed = match Manifest::from_json_or_legacy(
        &bytes,
        server_id,
        fallback_mc_ver,
        fallback_loader,
        fallback_loader_ver,
        &zip_url,
        Some(op_id),
    ) {
        Ok(p) => p,
        Err(e) => {
            if !zip_url.is_empty() {
                log_event(
                    Some(op_id),
                    "MANIFEST",
                    "WARN",
                    "fetched manifest bytes failed to parse; falling back to full modpack ZIP download and synthesis",
                    &[
                        ("url", sanitize_url(&m_url)),
                        ("parse_error", e.to_string()),
					],
                );
                return Ok(None);
            }
            return Err(e);
        }
    };

    // Only treat as authoritative full manifest if it has files[] (schema 1)
    if parsed.files.is_empty() {
        log_event(
            Some(op_id),
            "MANIFEST",
            "INFO",
            "fetched legacy delta manifest without files[]; will synthesize full manifest from verified ZIP",
            &[("url", sanitize_url(&m_url))],
        );
        return Ok(None);
    }

    log_event(
        Some(op_id),
        "MANIFEST",
        "INFO",
        "validated remote schema-1 manifest",
        &[
            ("url", sanitize_url(&m_url)),
            ("files", parsed.files.len().to_string()),
            ("zip_sha256", parsed.zip.sha256.clone()),
        ],
    );

    Ok(Some(parsed))
}

async fn ensure_theseus_instance_and_base(
    app: Option<&tauri::AppHandle>,
    op_id: &str,
    server_id: &str,
    spec: &VersionRefSpec,
    cancel_token: &Arc<AtomicBool>,
) -> Result<(String, PathBuf), Dr3shError> {
    let state = State::get()
        .await
        .map_err(|e| Dr3shError::TheseusBase {
            theseus_error: e.to_string(),
        })?;
    let instances_dir = state.directories.instances_dir();

    let current_state = get_server_state(server_id);
    let desired_game_ver = spec
        .game_version
        .clone()
        .unwrap_or_else(|| "1.21.1".to_string());
    let desired_loader = parse_mod_loader(spec.loader.as_deref());
    let desired_loader_ver = spec.loader_version.clone();

    let mut matched_instance = None;
    if let Some(ref existing_id) = current_state.instance_id {
        if let Ok(Some(inst)) = theseus::instance::get(existing_id).await {
            matched_instance = Some(inst);
        }
    }

    if matched_instance.is_none() {
        if let Ok(all) = theseus::instance::list().await {
            let folder_title = spec
                .server_name
                .as_deref()
                .unwrap_or(server_id);
            if let Ok(sanitized) = sanitize_instance_folder_name(folder_title) {
                matched_instance = all.into_iter().find(|item| {
                    item.instance.path.eq_ignore_ascii_case(&sanitized)
                        || item.instance.name.eq_ignore_ascii_case(folder_title)
                        || item.instance.path.eq_ignore_ascii_case(server_id)
                });
            }
        }
    }

    let (instance_id, rel_folder) = if let Some(existing) = matched_instance {
        let id = existing.instance.id.clone();
        let rel_path = existing.instance.path.clone();

        // Check if there is already an active job running for this instance
        let active_job_id = theseus::install::list_jobs(false)
            .await
            .ok()
            .and_then(|jobs| {
                jobs.into_iter()
                    .find(|j| j.instance_id.as_deref() == Some(id.as_str()))
                    .and_then(|j| uuid::Uuid::parse_str(&j.job_id).ok())
            });

        if let Some(active_jid) = active_job_id {
            wait_for_theseus_base(app, op_id, server_id, &id, Some(active_jid), cancel_token)
                .await?;
        } else {
            let needs_reinstall = existing.applied_content_set.game_version != desired_game_ver
                || existing.applied_content_set.loader != desired_loader
                || (desired_loader_ver.is_some()
                    && existing.applied_content_set.loader_version != desired_loader_ver)
                || existing.instance.install_stage.as_str() != "installed";

            if needs_reinstall {
                // Dismiss or clear any failed/interrupted jobs for this instance so install_existing_instance can proceed
                if let Ok(all_jobs) = theseus::install::list_jobs(true).await {
                    for job in all_jobs
                        .into_iter()
                        .filter(|j| j.instance_id.as_deref() == Some(id.as_str()))
                    {
                        if let Ok(jid) = uuid::Uuid::parse_str(&job.job_id) {
                            let _ = theseus::install::dismiss_job(jid).await;
                        }
                    }
                }

                let _ = theseus::instance::edit(
                    &id,
                    theseus::data::EditInstance {
                        content_set_patch: Some(AppliedContentSetPatch {
                            game_version: Some(desired_game_ver.clone()),
                            loader: Some(desired_loader),
                            loader_version: desired_loader_ver.clone().map(Some),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                )
                .await;
                let snap = theseus::install::install_existing_instance(id.clone(), true)
                    .await
                    .map_err(|e| Dr3shError::TheseusBase {
                        theseus_error: e.to_string(),
                    })?;
                let jid = uuid::Uuid::parse_str(&snap.job_id).ok();
                wait_for_theseus_base(app, op_id, server_id, &id, jid, cancel_token).await?;
            } else if let Some(app_handle) = app {
                emit_progress(
                    app_handle,
                    op_id,
                    server_id,
                    "base",
                    "Base de Minecraft lista.",
                    100,
                    100,
                    0,
                    None,
                    Some(10),
                );
            }
        }

        (id, rel_path)
    } else {
        let display_name = spec
            .server_name
            .clone()
            .unwrap_or_else(|| server_id.to_string());
        let sanitized_name = sanitize_instance_folder_name(&display_name)?;
        let snapshot = theseus::install::create_instance(
            sanitized_name.clone(),
            desired_game_ver,
            desired_loader,
            desired_loader_ver,
            spec.icon_url.clone(),
            None,
            theseus::data::InstanceLink::Unmanaged,
        )
        .await
        .map_err(|e| Dr3shError::TheseusBase {
            theseus_error: e.to_string(),
        })?;

        let id = snapshot
            .instance_id
            .clone()
            .unwrap_or_else(|| sanitized_name.clone());
        let jid = uuid::Uuid::parse_str(&snapshot.job_id).ok();
        wait_for_theseus_base(app, op_id, server_id, &id, jid, cancel_token).await?;

        let rel_path = theseus::instance::get(&id)
            .await
            .ok()
            .flatten()
            .map(|inst| inst.instance.path)
            .unwrap_or(sanitized_name);
        (id, rel_path)
    };

    let default_instance_dir = instances_dir.join(&rel_folder);
    Ok((instance_id, default_instance_dir))
}

#[allow(clippy::too_many_arguments)]
pub async fn execute_pipeline(
    app: Option<tauri::AppHandle>,
    op_id: String,
    server_id: String,
    version_ref: serde_json::Value,
    custom_dir: Option<String>,
    ignore_cache: bool,
    explicit_instance_id: Option<String>,
    explicit_instance_dir: Option<String>,
    cancel_token: Arc<AtomicBool>,
) -> Result<DoneEventPayload, Dr3shErrorPayload> {
    let op_start = Instant::now();
    let lock = get_instance_lock(&server_id);
    let _guard = lock.lock().await;

    let run_res: Result<DoneEventPayload, Dr3shError> = (|| async {
        update_server_state(app.as_ref(), &server_id, |st| {
            st.state = ServerInstallState::Preparing;
            st.active_op_id = Some(op_id.clone());
            st.last_error = None;
        });

        let mut spec: VersionRefSpec = if let Some(ver_str) = version_ref.as_str() {
            resolve_version_spec_from_remote(&server_id, ver_str).await?
        } else {
            serde_json::from_value::<VersionRefSpec>(version_ref.clone()).map_err(|e| {
                Dr3shError::ManifestInvalid {
                    reason: format!("Invalid version_ref payload: {}", e),
                }
            })?
        };

        if spec.download_url.as_deref().unwrap_or("").is_empty() {
            let remote = resolve_version_spec_from_remote(&server_id, &spec.id).await?;
            if spec.download_url.is_none() {
                spec.download_url = remote.download_url;
            }
            if spec.manifest_url.is_none() {
                spec.manifest_url = remote.manifest_url;
            }
            if spec.game_version.is_none() {
                spec.game_version = remote.game_version;
            }
            if spec.loader.is_none() {
                spec.loader = remote.loader;
            }
            if spec.loader_version.is_none() {
                spec.loader_version = remote.loader_version;
            }
            if spec.sha256.is_none() {
                spec.sha256 = remote.sha256;
            }
            if spec.size.is_none() {
                spec.size = remote.size;
            }
            if spec.size_mb.is_none() {
                spec.size_mb = remote.size_mb;
            }
            if spec.server_name.is_none() {
                spec.server_name = remote.server_name;
            }
        }

        register_op_meta(OpDiagnosticMeta {
            op_id: op_id.clone(),
            server_id: server_id.clone(),
            from_version: get_server_state(&server_id).installed_version,
            to_version: spec.id.clone(),
            stage: "BASE".to_string(),
            error_code: None,
            launcher_version: env!("CARGO_PKG_VERSION").to_string(),
            os_version: os_version_string(),
        });

        log_event(
            Some(&op_id),
            "START",
            "INFO",
            "pipeline initiated",
            &[
                ("server_id", server_id.clone()),
                ("to_version", spec.id.clone()),
                ("game_version", spec.game_version.clone().unwrap_or_default()),
                ("loader", spec.loader.clone().unwrap_or_default()),
            ],
        );

        // Section 13.3: Remote maintenance/testing gate check in Rust
        verify_remote_gate(&server_id, spec.bypass_gate).await?;

        let base_start = Instant::now();
        let (instance_id, default_instance_dir) = if let (Some(id), Some(dir)) =
            (explicit_instance_id.clone(), explicit_instance_dir.clone())
        {
            wait_for_theseus_base(app.as_ref(), &op_id, &server_id, &id, None, &cancel_token)
                .await?;
            (id, PathBuf::from(dir))
        } else {
            ensure_theseus_instance_and_base(
                app.as_ref(),
                &op_id,
                &server_id,
                &spec,
                &cancel_token,
            )
            .await?
        };
        log_event(
            Some(&op_id),
            "STAGE",
            "INFO",
            "base stage finished",
            &[
                ("instance_id", instance_id.clone()),
                ("duration_ms", base_start.elapsed().as_millis().to_string()),
            ],
        );

        // Resolve custom directory and sanitize folder name (Section 10.2)
        let folder_label = spec
            .server_name
            .as_deref()
            .unwrap_or(&server_id);
        let sanitized_folder = sanitize_instance_folder_name(folder_label)?;

        let custom_target_dir: Option<PathBuf> = custom_dir
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|raw| {
                let base = PathBuf::from(raw);
                if base
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.eq_ignore_ascii_case(&sanitized_folder))
                {
                    base
                } else {
                    base.join(&sanitized_folder)
                }
            });

        let effective_instance_dir = custom_target_dir
            .clone()
            .unwrap_or_else(|| default_instance_dir.clone());

        // Crash recovery check on this instance before any operation
        recover_instance(Some(&op_id), &effective_instance_dir)?;

        let baseline = load_baseline(&effective_instance_dir);
        let from_version = baseline
            .as_ref()
            .map(|b| b.version.clone())
            .or_else(|| get_server_state(&server_id).installed_version);

        // Fetch manifest if available
        let stage_manifest_start = Instant::now();
        let remote_manifest = fetch_manifest_if_available(&op_id, &server_id, &spec).await?;
        log_event(
            Some(&op_id),
            "STAGE",
            "INFO",
            "manifest stage finished",
            &[(
                "duration_ms",
                stage_manifest_start.elapsed().as_millis().to_string(),
            )],
        );

        let zip_url = remote_manifest
            .as_ref()
            .map(|m| m.zip.url.clone())
            .or_else(|| spec.download_url.clone())
            .ok_or_else(|| Dr3shError::ManifestInvalid {
                reason: "Missing modpack ZIP URL".to_string(),
            })?;

        let expected_zip_size = remote_manifest
            .as_ref()
            .map(|m| m.zip.size)
            .or(spec.size)
            .unwrap_or_else(|| (spec.size_mb.unwrap_or(0.0) * 1024.0 * 1024.0) as u64);

        let expected_zip_sha = remote_manifest
            .as_ref()
            .map(|m| m.zip.sha256.clone())
            .or_else(|| spec.sha256.clone());

        let already_downloaded_bytes = existing_part_size(
            &effective_instance_dir,
            &server_id,
            &spec.id,
            expected_zip_sha.as_deref(),
        );
        let zip_remaining = expected_zip_size.saturating_sub(already_downloaded_bytes);

        let est_write_bytes = remote_manifest
            .as_ref()
            .map(|m| m.files.iter().map(|f| f.size).sum::<u64>())
            .unwrap_or(expected_zip_size);

        // Section 10: Run preflight BEFORE creating junctions or downloading
        let preflight_report = run_preflight(
            &op_id,
            &default_instance_dir,
            custom_target_dir.as_deref(),
            zip_remaining,
            est_write_bytes,
        )?;

        log_event(
            Some(&op_id),
            "START",
            "INFO",
            "operation started",
            &[
                ("launcher_version", env!("CARGO_PKG_VERSION").to_string()),
                ("os", os_version_string()),
                ("server_id", server_id.clone()),
                ("from_version", from_version.clone().unwrap_or_else(|| "none".to_string())),
                ("to_version", spec.id.clone()),
                ("url", sanitize_url(&zip_url)),
                ("instance_path", effective_instance_dir.to_string_lossy().to_string()),
                ("fs", preflight_report.instance_fs.clone()),
                ("free_bytes", preflight_report.free_bytes.to_string()),
                ("migration_mode", baseline.is_none().to_string()),
            ],
        );

        register_op_meta(OpDiagnosticMeta {
            op_id: op_id.clone(),
            server_id: server_id.clone(),
            from_version: from_version.clone(),
            to_version: spec.id.clone(),
            stage: "PREFLIGHT".to_string(),
            error_code: None,
            launcher_version: env!("CARGO_PKG_VERSION").to_string(),
            os_version: os_version_string(),
        });

        let instance_folder_name = default_instance_dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if !instance_folder_name.is_empty() {
            let _ = theseus::instance::unwatch_instance(&instance_folder_name).await;
        }

        let inner_res: Result<DoneEventPayload, Dr3shError> = (|| async {
            // Section 11: Safe NTFS junction if custom directory is used
            if let Some(ref custom_target) = custom_target_dir {
                ensure_safe_junction(Some(&op_id), &default_instance_dir, custom_target)?;
            } else {
                fs::create_dir_all(&default_instance_dir).map_err(|e| Dr3shError::FsPermission {
                    path: default_instance_dir.to_string_lossy().to_string(),
                    os_code: e.raw_os_error().unwrap_or(-1).to_string(),
                })?;
                let _ = fs::write(default_instance_dir.join(".dr3sh_instance"), b"dr3sh");
            }

            if let Some(ref app_handle) = app {
                let _ = app_handle
                    .fs_scope()
                    .allow_directory(&effective_instance_dir, true);
            }

            // If we already have a schema-1 remote manifest, we can compute the plan BEFORE downloading!
            // That satisfies Section 15 #15: "No changes between versions -> the ZIP is not downloaded and the baseline is updated."
            let mut downloaded_zip_path: Option<PathBuf> = None;
            let mut dl_bytes = 0u64;
            let mut dl_speed = 0u64;
            let mut dl_retries = 0u32;

            let (manifest, plan) = if let Some(m) = remote_manifest {
                let mut plan = build_reconciliation_plan(
                    &effective_instance_dir,
                    baseline.as_ref(),
                    &m,
                    ignore_cache,
                );

                if plan.needs_zip() {
                    update_op_stage(&op_id, "DOWNLOAD", None);
                    update_server_state(app.as_ref(), &server_id, |st| {
                        st.state = ServerInstallState::Downloading;
                    });
                    let dl_stage_start = Instant::now();
                    let part_path = part_file_path(
                        &effective_instance_dir,
                        &m.pack_id,
                        &m.version,
                        &m.zip.sha256,
                    );
                    let outcome = download_zip_resumable(DownloadSpec {
                        app: app.as_ref(),
                        op_id: &op_id,
                        server_id: &server_id,
                        server_title: folder_label,
                        url: &m.zip.url,
                        part_path: &part_path,
                        expected_size: Some(m.zip.size),
                        expected_sha256: Some(&m.zip.sha256),
                        cancel_flag: &cancel_token,
                    })
                    .await?;
                    log_event(
                        Some(&op_id),
                        "STAGE",
                        "INFO",
                        "download and verify stage finished",
                        &[(
                            "duration_ms",
                            dl_stage_start.elapsed().as_millis().to_string(),
                        )],
                    );
                    update_op_stage(&op_id, "VERIFY", None);
                    update_server_state(app.as_ref(), &server_id, |st| {
                        st.state = ServerInstallState::Verifying;
                    });
                    let resolved_map = verify_zip_against_manifest(&outcome.part_path, &m)?;
                    for item in &mut plan.items {
                        if item.archive_entry_name.is_none()
                            && let Some(actual) = resolved_map.get(&item.path.to_ascii_lowercase())
                        {
                            item.archive_entry_name = Some(actual.clone());
                        }
                    }
                    downloaded_zip_path = Some(outcome.part_path);
                    dl_bytes = outcome.total_bytes;
                    dl_speed = outcome.avg_speed_bps;
                    dl_retries = outcome.retries_used;
                } else {
                    log_event(
                        Some(&op_id),
                        "DOWNLOAD",
                        "INFO",
                        "no file writes required by plan; skipping ZIP download",
                        &[("version", m.version.clone())],
                    );
                }
                (m, plan)
            } else {
                // Legacy release without schema-1 manifest: download ZIP first, then synthesize & validate schema-1 manifest
                update_op_stage(&op_id, "DOWNLOAD", None);
                update_server_state(app.as_ref(), &server_id, |st| {
                    st.state = ServerInstallState::Downloading;
                });
                let dl_stage_start = Instant::now();
                let part_path = part_file_path(
                    &effective_instance_dir,
                    &server_id,
                    &spec.id,
                    expected_zip_sha.as_deref().unwrap_or(""),
                );
                let outcome = download_zip_resumable(DownloadSpec {
                    app: app.as_ref(),
                    op_id: &op_id,
                    server_id: &server_id,
                    server_title: folder_label,
                    url: &zip_url,
                    part_path: &part_path,
                    expected_size: if expected_zip_size > 0 {
                        Some(expected_zip_size)
                    } else {
                        None
                    },
                    expected_sha256: expected_zip_sha.as_deref(),
                    cancel_flag: &cancel_token,
                })
                .await?;
                log_event(
                    Some(&op_id),
                    "STAGE",
                    "INFO",
                    "download stage finished",
                    &[(
                        "duration_ms",
                        dl_stage_start.elapsed().as_millis().to_string(),
                    )],
                );

                update_op_stage(&op_id, "VERIFY", None);
                update_server_state(app.as_ref(), &server_id, |st| {
                    st.state = ServerInstallState::Verifying;
                });
                let synth = synthesize_manifest_from_zip(
                    &outcome.part_path,
                    &server_id,
                    &spec.id,
                    spec.game_version.as_deref().unwrap_or("1.21.1"),
                    spec.loader.as_deref().unwrap_or("fabric"),
                    spec.loader_version.as_deref().unwrap_or(""),
                    &zip_url,
                )?;
                let plan = build_reconciliation_plan(
                    &effective_instance_dir,
                    baseline.as_ref(),
                    &synth,
                    ignore_cache,
                );
                downloaded_zip_path = Some(outcome.part_path);
                dl_bytes = outcome.total_bytes;
                dl_speed = outcome.avg_speed_bps;
                dl_retries = outcome.retries_used;
                (synth, plan)
            };

            // Log plan summary and per-file DEBUG entries (Section 8.3)
            log_event(
                Some(&op_id),
                "PLAN",
                "INFO",
                "reconciliation plan computed",
                &[
                    ("migration_mode", plan.migration_mode.to_string()),
                    ("write", plan.write_count().to_string()),
                    ("backup_then_write", plan.backup_then_write_count().to_string()),
                    ("remove", plan.remove_count().to_string()),
                    ("noop", plan.noop_count().to_string()),
                    ("unknown_managed", plan.unknown_managed_count().to_string()),
                    ("write_bytes", plan.write_bytes_total.to_string()),
                ],
            );
            let mut plan_debug_buf = String::new();
            for entry in &plan.items {
                plan_debug_buf.push_str(&logger::format_log_line(
                    Some(&op_id),
                    "PLAN",
                    "DEBUG",
                    "file action",
                    &[
                        ("path", entry.path.clone()),
                        ("policy", entry.policy.as_str().to_string()),
                        ("action", format!("{:?}", entry.action)),
                        ("tag", entry.tag.clone().unwrap_or_default()),
                    ],
                ));
            }
            logger::log_op_debug_batch(Some(&op_id), &plan_debug_buf);

            let txn_id = format!("txn_{}", op_id);

            // Stage 5: Extract needed files into staging/<txn_id>
            if let Some(ref zip_path) = downloaded_zip_path {
                if plan.needs_zip() {
                    update_op_stage(&op_id, "EXTRACT", None);
                    update_server_state(app.as_ref(), &server_id, |st| {
                        st.state = ServerInstallState::Extracting;
                    });
                    let extract_start = Instant::now();
                    let app_clone = app.clone();
                    let op_id_clone = op_id.clone();
                    let srv_clone = server_id.clone();
                    let zip_clone = zip_path.clone();
                    let inst_clone = effective_instance_dir.clone();
                    let txn_clone = txn_id.clone();
                    let plan_clone = plan.clone();
                    let cancel_clone = cancel_token.clone();

                    tokio::task::spawn_blocking(move || {
                        extract_to_staging(
                            app_clone.as_ref(),
                            &op_id_clone,
                            &srv_clone,
                            &zip_clone,
                            &inst_clone,
                            &txn_clone,
                            &plan_clone,
                            &cancel_clone,
                        )
                    })
                    .await
                    .map_err(|e| Dr3shError::unknown(format!("Extract task join error: {}", e)))??;

                    log_event(
                        Some(&op_id),
                        "STAGE",
                        "INFO",
                        "extract stage finished",
                        &[("duration_ms", extract_start.elapsed().as_millis().to_string())],
                    );
                }
            }

            // Stage 6: Transactional Apply
            update_op_stage(&op_id, "APPLY", None);
            update_server_state(app.as_ref(), &server_id, |st| {
                st.state = ServerInstallState::Applying;
            });
            let apply_start = Instant::now();
            {
                let app_clone = app.clone();
                let op_id_clone = op_id.clone();
                let srv_clone = server_id.clone();
                let inst_clone = effective_instance_dir.clone();
                let txn_clone = txn_id.clone();
                let from_ver_clone = from_version.clone();
                let manifest_clone = manifest.clone();
                let plan_clone = plan.clone();

                tokio::task::spawn_blocking(move || {
                    apply_plan_transactional(
                        app_clone.as_ref(),
                        &op_id_clone,
                        &srv_clone,
                        &inst_clone,
                        &txn_clone,
                        from_ver_clone,
                        &manifest_clone,
                        &plan_clone,
                    )
                })
                .await
                .map_err(|e| Dr3shError::unknown(format!("Apply task join error: {}", e)))??;
            }
            log_event(
                Some(&op_id),
                "STAGE",
                "INFO",
                "apply stage finished",
                &[("duration_ms", apply_start.elapsed().as_millis().to_string())],
            );

            // Remove .part file after commit
            if let Some(ref zip_path) = downloaded_zip_path {
                let _ = fs::remove_file(zip_path);
            }

            let migration_notice = plan.has_migration_backups();
            let inst_path_str = effective_instance_dir.to_string_lossy().to_string();

            update_op_stage(&op_id, "DONE", None);
            update_server_state(app.as_ref(), &server_id, |st| {
                st.state = ServerInstallState::Installed;
                st.instance_id = Some(instance_id.clone());
                st.instance_path = Some(inst_path_str.clone());
                st.installed_version = Some(manifest.version.clone());
                st.active_op_id = None;
                st.last_error = None;
                if migration_notice {
                    st.migration_notice_pending = true;
                }
            });

            let total_duration_ms = op_start.elapsed().as_millis() as u64;
            let summary = OperationSummary {
                write_count: plan.write_count(),
                backup_then_write_count: plan.backup_then_write_count(),
                remove_count: plan.remove_count(),
                noop_count: plan.noop_count(),
                unknown_managed_count: plan.unknown_managed_count(),
                downloaded_bytes: dl_bytes,
                avg_speed_bps: dl_speed,
                retries: dl_retries,
                duration_ms: total_duration_ms,
            };

            log_event(
                Some(&op_id),
                "DONE",
                "INFO",
                "OK",
                &[
                    ("version", manifest.version.clone()),
                    ("write", summary.write_count.to_string()),
                    ("backup_then_write", summary.backup_then_write_count.to_string()),
                    ("remove", summary.remove_count.to_string()),
                    ("noop", summary.noop_count.to_string()),
                    ("bytes", summary.downloaded_bytes.to_string()),
                    ("avg_speed_bps", summary.avg_speed_bps.to_string()),
                    ("retries", summary.retries.to_string()),
                    ("duration_ms", summary.duration_ms.to_string()),
                ],
            );

            Ok(DoneEventPayload {
                op_id: op_id.clone(),
                server_id: server_id.clone(),
                ok: true,
                version: Some(manifest.version),
                instance_id: Some(instance_id.clone()),
                instance_path: Some(inst_path_str),
                migration_notice,
                summary: Some(summary),
                error: None,
            })
        })()
        .await;

        if !instance_folder_name.is_empty() {
            let _ = theseus::instance::watch_instance(&instance_id, &instance_folder_name).await;
        }

        inner_res
    })()
    .await;

    complete_operation(&op_id);

    match run_res {
        Ok(done) => {
            if let Some(ref app_handle) = app {
                let _ = app_handle.emit("dr3sh://done", &done);
            }
            Ok(done)
        }
        Err(err) => {
            let payload = err.to_payload(&op_id, "PIPELINE");
            update_op_stage(&op_id, &payload.stage, Some(&payload.code));
            log_event(
                Some(&op_id),
                "DONE",
                "ERROR",
                &format!("ERROR {}", payload.code),
                &[
                    ("message", payload.message.clone()),
                    ("details", payload.details.clone()),
                ],
            );
            set_server_error(app.as_ref(), &server_id, &payload);
            let done = DoneEventPayload {
                op_id: op_id.clone(),
                server_id: server_id.clone(),
                ok: false,
                version: None,
                instance_id: None,
                instance_path: None,
                migration_notice: false,
                summary: None,
                error: Some(payload.clone()),
            };
            if let Some(ref app_handle) = app {
                let _ = app_handle.emit("dr3sh://done", &done);
            }
            Err(payload)
        }
    }
}

#[tauri::command]
pub async fn dr3sh_install_or_update(
    app: tauri::AppHandle,
    server_id: String,
    version_ref: serde_json::Value,
    custom_dir: Option<String>,
) -> Result<String, Dr3shErrorPayload> {
    let op_id = new_op_id();
    let cancel_token = register_operation(&op_id);

    let op_id_ret = op_id.clone();
    tauri::async_runtime::spawn(async move {
        let _ = execute_pipeline(
            Some(app),
            op_id,
            server_id,
            version_ref,
            custom_dir,
            false,
            None,
            None,
            cancel_token,
        )
        .await;
    });

    Ok(op_id_ret)
}

#[tauri::command]
pub async fn dr3sh_repair(
    app: tauri::AppHandle,
    server_id: String,
    version_ref: Option<serde_json::Value>,
) -> Result<String, Dr3shErrorPayload> {
    let op_id = new_op_id();
    let cancel_token = register_operation(&op_id);

    let st = get_server_state(&server_id);
    let ver_val = version_ref.unwrap_or_else(|| {
        serde_json::Value::String(
            st.installed_version
                .unwrap_or_else(|| "latest".to_string()),
        )
    });

    let op_id_ret = op_id.clone();
    tauri::async_runtime::spawn(async move {
        let _ = execute_pipeline(
            Some(app),
            op_id,
            server_id,
            ver_val,
            None,
            true,
            st.instance_id,
            None,
            cancel_token,
        )
        .await;
    });

    Ok(op_id_ret)
}

#[tauri::command]
pub fn dr3sh_cancel(op_id: Option<String>) -> bool {
    let target = op_id.unwrap_or_else(|| "*".to_string());
    log_event(
        Some(&target),
        "CANCEL",
        "WARN",
        "cancellation requested by user",
        &[],
    );
    cancel_operation(&target)
}

#[tauri::command]
pub fn dr3sh_get_state() -> HashMap<String, ServerStateEntry> {
    get_all_server_states()
}

#[tauri::command]
pub fn dr3sh_migrate_local_state(
    app: tauri::AppHandle,
    entries: HashMap<String, LegacyLocalStateEntry>,
) -> HashMap<String, ServerStateEntry> {
    migrate_local_storage_state(Some(&app), entries)
}

#[tauri::command]
pub fn dr3sh_ack_migration_notice(app: tauri::AppHandle, server_id: String) -> ServerStateEntry {
    update_server_state(Some(&app), &server_id, |st| {
        st.migration_notice_pending = false;
    })
}

#[tauri::command]
pub fn dr3sh_get_diagnostics(
    op_id: Option<String>,
    server_id: Option<String>,
) -> String {
    let srv_id = server_id.as_deref().unwrap_or("unknown");
    let st = get_server_state(srv_id);
    let resolved_op_id = op_id
        .or_else(|| st.last_error.as_ref().map(|e| e.op_id.clone()))
        .or(st.active_op_id);

    build_diagnostics(resolved_op_id.as_deref())
}

#[tauri::command]
pub fn dr3sh_open_logs_folder(app: tauri::AppHandle) -> Result<(), String> {
    let dir = logs_root_dir();
    let _ = fs::create_dir_all(&dir);
    app.opener()
        .open_path(dir.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn dr3sh_prepare_launch(
    server_id: String,
    is_offline: bool,
    offline_username: Option<String>,
    bypass_gate: Option<bool>,
) -> Result<LaunchPreflightResult, Dr3shErrorPayload> {
    let op_id = new_op_id();
    if is_instance_locked(&server_id) {
        return Err(Dr3shError::unknown(
            "La instancia tiene una operación en curso. Espera a que termine antes de jugar.",
        )
        .to_payload(&op_id, "LAUNCH"));
    }

    let lock = get_instance_lock(&server_id);
    let _guard = lock.lock().await;

    if let Err(e) = verify_remote_gate(&server_id, bypass_gate.unwrap_or(false)).await {
        return Err(e.to_payload(&op_id, "LAUNCH"));
    }

    let mut refreshed_auth = false;
    if is_offline {
        let name = offline_username.unwrap_or_default();
        let trimmed = name.trim();
        let valid = trimmed.len() >= 3
            && trimmed.len() <= 16
            && trimmed
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            return Err(Dr3shError::unknown(
                "El nombre de usuario offline debe tener entre 3 y 16 caracteres (solo letras, números y _)",
            )
            .to_payload(&op_id, "AUTH"));
        }
        let _ = theseus::minecraft_auth::add_offline_user(trimmed)
            .await
            .map_err(|e| Dr3shError::unknown(e.to_string()).to_payload(&op_id, "AUTH"))?;
    } else if theseus::minecraft_auth::get_default_user().await.is_ok() {
        // Refresh Microsoft token if expiring in < 2 hours (handled inside get_default_user -> get_default_credential)
        refreshed_auth = true;
    }

    let st = get_server_state(&server_id);
    Ok(LaunchPreflightResult {
        ok: true,
        instance_id: st.instance_id,
        instance_path: st.instance_path,
        refreshed_auth,
    })
}

// Backward-compatible command shims delegating to the hardened engine
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn dr3sh_download_modpack(
    app: tauri::AppHandle,
    server_id: String,
    server_title: String,
    download_url: String,
    manifest_url: Option<String>,
    instance_dir: String,
    custom_dir: Option<String>,
    expected_size_mb: Option<f64>,
    expected_sha256: Option<String>,
    instance_id: Option<String>,
) -> crate::api::Result<String> {
    let op_id = new_op_id();
    let cancel_token = register_operation(&op_id);
    let spec = VersionRefSpec {
        id: "latest".to_string(),
        name: Some(server_title.clone()),
        server_name: Some(server_title),
        game_version: None,
        loader: None,
        loader_version: None,
        download_url: Some(download_url),
        manifest_url,
        sha256: expected_sha256,
        size: None,
        size_mb: expected_size_mb,
        icon_url: None,
        bypass_gate: true,
    };

    let res = execute_pipeline(
        Some(app),
        op_id,
        server_id,
        serde_json::to_value(spec).unwrap_or_default(),
        custom_dir,
        false,
        instance_id,
        Some(instance_dir.clone()),
        cancel_token,
    )
    .await
    .map_err(|e| anyhow::anyhow!("{} ({})", e.message, e.code))?;

    Ok(res.instance_path.unwrap_or(instance_dir))
}

#[tauri::command]
pub fn dr3sh_cancel_download() {
    cancel_operation("*");
}

#[tauri::command]
pub async fn dr3sh_wait_for_instance_ready(instance_id: String) -> crate::api::Result<()> {
    let op_id = new_op_id();
    let token = Arc::new(AtomicBool::new(false));
    wait_for_theseus_base(None, &op_id, "wait", &instance_id, None, &token)
        .await
        .map_err(|e| anyhow::anyhow!("{}", e.ui_message_es()))?;
    Ok(())
}

#[tauri::command]
pub async fn dr3sh_link_existing_instance(
    app: tauri::AppHandle,
    instance_id: String,
    instance_dir: String,
    existing_folder: String,
) -> crate::api::Result<bool> {
    let existing_path = PathBuf::from(existing_folder.trim());
    if !existing_path.exists() {
        return Ok(false);
    }
    let op_id = new_op_id();
    let token = Arc::new(AtomicBool::new(false));
    let _ = wait_for_theseus_base(Some(&app), &op_id, &instance_id, &instance_id, None, &token)
        .await;
    let instance_path = PathBuf::from(&instance_dir);
    let instance_folder_name = instance_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    if !instance_folder_name.is_empty() {
        let _ = theseus::instance::unwatch_instance(&instance_folder_name).await;
    }
    let junc_res = ensure_safe_junction(Some(&op_id), &instance_path, &existing_path);
    if !instance_folder_name.is_empty() {
        let _ = theseus::instance::watch_instance(&instance_id, &instance_folder_name).await;
    }
    junc_res.map_err(|e| anyhow::anyhow!("{}", e.ui_message_es()))?;
    let _ = app.fs_scope().allow_directory(&existing_path, true);
    Ok(true)
}

#[tauri::command]
pub fn dr3sh_cleanup_deleted_instance(
    app: tauri::AppHandle,
    server_id: Option<String>,
    custom_folder: Option<String>,
) {
    if let Ok(appdata) = std::env::var("APPDATA") {
        let profiles_dir = PathBuf::from(appdata)
            .join("com.dr3sh.launcher")
            .join("profiles");
        if let Ok(entries) = fs::read_dir(&profiles_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if !p.exists() && is_reparse_point(&p) {
                    let _ = remove_junction_link_only(&p);
                }
            }
        }
    }

    if let Some(ref sid) = server_id {
        update_server_state(Some(&app), sid, |st| {
            st.state = ServerInstallState::NotInstalled;
            st.installed_version = None;
            st.instance_id = None;
            st.instance_path = None;
            st.last_error = None;
        });
    }

    if let Some(folder) = custom_folder
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        let p = PathBuf::from(folder);
        if is_reparse_point(&p) {
            let _ = remove_junction_link_only(&p);
            return;
        }
        if let Ok(canon) = p.canonicalize() {
            if canon.components().count() >= 3 && is_launcher_owned_dir(&canon) {
                let backup_name = format!(
                    "{}.deleted_{}",
                    canon
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("instance"),
                    Utc::now().timestamp()
                );
                let backup_dest = launcher_data_dir().join("deleted_instances").join(backup_name);
                if let Some(parent) = backup_dest.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                if fs::rename(&canon, &backup_dest).is_err() {
                    let _ = fs::remove_dir_all(&canon);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn sample_schema_1_manifest_json(zip_url: &str) -> String {
        format!(
            r#"{{
                "schema": 1,
                "pack_id": "cobbleverse",
                "version": "9",
                "minecraft_version": "1.21.1",
                "loader": "fabric",
                "loader_version": "0.16.9",
                "zip": {{
                    "url": "{zip_url}",
                    "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "size": 1024
                }},
                "rules": [
                    {{
                        "pattern": "**",
                        "policy": "managed"
                    }}
                ],
                "files": [
                    {{
                        "path": "mods/example.jar",
                        "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                        "size": 512,
                        "policy": "managed"
                    }}
                ]
            }}"#
        )
    }

    #[tokio::test]
    async fn test_manifest_fetch_retries_on_504_and_succeeds() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let req_count = Arc::new(AtomicUsize::new(0));
        let req_count_clone = req_count.clone();

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let count = req_count_clone.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let _ = socket.read(&mut buf).await;
                    if count == 0 {
                        let resp = "HTTP/1.1 504 Gateway Time-out\r\nContent-Length: 0\r\n\r\n";
                        let _ = socket.write_all(resp.as_bytes()).await;
                    } else {
                        let body = sample_schema_1_manifest_json("https://github.com/Dr3sh/cobbleverse-modpack/releases/download/v9/cobbleverse.zip");
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = socket.write_all(resp.as_bytes()).await;
                    }
                    let _ = socket.shutdown().await;
                });
            }
        });

        let spec = VersionRefSpec {
            id: "9".to_string(),
            manifest_url: Some(format!("http://{addr}/manifest.json")),
            download_url: Some("https://github.com/Dr3sh/cobbleverse-modpack/releases/download/v9/cobbleverse.zip".to_string()),
            ..Default::default()
        };

        let result = fetch_manifest_if_available("sim_op_retry", "cobbleverse", &spec).await;
        assert!(result.is_ok(), "fetch should succeed after retry: {result:?}");
        let opt = result.unwrap();
        assert!(opt.is_some(), "manifest should be parsed");
        let manifest = opt.unwrap();
        assert_eq!(manifest.pack_id, "cobbleverse");
        assert_eq!(manifest.version, "9");
        assert_eq!(manifest.files.len(), 1);
        assert_eq!(req_count.load(Ordering::SeqCst), 2, "must have made 2 attempts");
    }

    #[tokio::test]
    async fn test_manifest_fetch_fallback_to_none_when_504_persists() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let req_count = Arc::new(AtomicUsize::new(0));
        let req_count_clone = req_count.clone();

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                req_count_clone.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let _ = socket.read(&mut buf).await;
                    let resp = "HTTP/1.1 504 Gateway Time-out\r\nContent-Length: 0\r\n\r\n";
                    let _ = socket.write_all(resp.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });

        let spec = VersionRefSpec {
            id: "9".to_string(),
            manifest_url: Some(format!("http://{addr}/manifest.json")),
            download_url: Some(format!("http://{addr}/pack.zip")),
            ..Default::default()
        };

        let result = fetch_manifest_if_available("sim_op_fallback", "cobbleverse", &spec).await;
        assert!(result.is_ok(), "should not return Err when fallback ZIP is available: {result:?}");
        let opt = result.unwrap();
        assert!(opt.is_none(), "must fall back to Ok(None) to let ZIP pipeline synthesize manifest");
        assert_eq!(req_count.load(Ordering::SeqCst), 3, "must have exhausted 3 retries");
    }

    #[tokio::test]
    async fn test_manifest_fetch_fails_when_no_download_url_and_504() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let _ = socket.read(&mut buf).await;
                    let resp = "HTTP/1.1 504 Gateway Time-out\r\nContent-Length: 0\r\n\r\n";
                    let _ = socket.write_all(resp.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });

        let spec = VersionRefSpec {
            id: "9".to_string(),
            manifest_url: Some(format!("http://{addr}/manifest.json")),
            download_url: None,
            ..Default::default()
        };

        let result = fetch_manifest_if_available("sim_op_fail", "cobbleverse", &spec).await;
        assert!(result.is_err(), "must return Err when no fallback download_url is present");
        match result.unwrap_err() {
            Dr3shError::ManifestFetch { status, .. } => {
                assert_eq!(status, "504");
            }
            other => panic!("expected ManifestFetch error, got {other:?}"),
        }
    }
}
