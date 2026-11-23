pub mod delta;
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
use tauri_plugin_http::reqwest;
use tauri_plugin_opener::OpenerExt;
use theseus::data::{AppliedContentSetPatch, ModLoader};
use theseus::install::InstallJobStatus;
use theseus::prelude::*;

use crate::dr3sh::delta::{
    download_and_extract_partial_zip, should_use_partial_zip, PartialZipAttempt,
};
use crate::dr3sh::download::{
    build_http_client, download_zip_resumable, drive_root_for_path, emit_progress,
    emit_progress_detailed, existing_part_size, extract_host, part_file_path, DownloadSpec,
};
use crate::dr3sh::errors::{Dr3shError, Dr3shErrorPayload};
use crate::dr3sh::logger::{
    build_diagnostics, launcher_data_dir, log_event, logs_root_dir, new_op_id, os_version_string,
    prune_logs, register_op_meta, sanitize_url, update_op_stage, OpDiagnosticMeta,
};
use crate::dr3sh::manifest::{
    build_reconciliation_plan, load_baseline, synthesize_manifest_from_zip_cancellable,
    verify_zip_against_manifest_cancellable, Manifest,
};
use crate::dr3sh::preflight::{
    ensure_safe_junction, ensure_single_entry_user_deletable,
    ensure_user_deletable_permissions, is_launcher_owned_dir, is_reparse_point,
    remove_junction_link_only, run_preflight, sanitize_instance_folder_name,
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
const GITHUB_LAUNCHER_LATEST_API_URL: &str =
    "https://api.github.com/repos/Dr3sh/dr3shapp/releases/latest";
const GITHUB_LAUNCHER_LATEST_JSON_URL: &str =
    "https://github.com/Dr3sh/dr3shapp/releases/latest/download/latest.json";
const GITHUB_LAUNCHER_RELEASES_LATEST_URL: &str =
    "https://github.com/Dr3sh/dr3shapp/releases/latest";
const GITHUB_LAUNCHER_RELEASES_BASE_URL: &str =
    "https://github.com/Dr3sh/dr3shapp/releases";

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LauncherUpdateCheckResult {
    pub current_version: String,
    pub latest_version: String,
    pub tag_name: String,
    pub update_available: bool,
    pub release_name: String,
    pub release_notes: String,
    pub download_url: String,
    pub release_page_url: String,
    pub source: String,
}

#[derive(Debug, Deserialize)]
struct GithubReleaseAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Deserialize)]
struct GithubLatestReleaseResponse {
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GithubReleaseAsset>,
}

#[derive(Debug, Deserialize)]
struct TauriLatestJsonPlatform {
    url: String,
}

#[derive(Debug, Deserialize)]
struct TauriLatestJsonResponse {
    version: String,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    platforms: HashMap<String, TauriLatestJsonPlatform>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedSemver {
    major: u64,
    minor: u64,
    patch: u64,
    pre: Option<String>,
}

fn parse_semver_str(raw: &str) -> Option<ParsedSemver> {
    let trimmed = raw.trim().trim_start_matches(['v', 'V']);
    if trimmed.is_empty() {
        return None;
    }
    let without_build = trimmed.split('+').next().unwrap_or(trimmed);
    let mut pre_split = without_build.splitn(2, '-');
    let core = pre_split.next().unwrap_or("");
    let pre = pre_split
        .next()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let parts: Vec<&str> = core.split('.').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    let major = parts[0].parse::<u64>().ok()?;
    let minor = parts.get(1).copied().unwrap_or("0").parse::<u64>().ok()?;
    let patch = parts.get(2).copied().unwrap_or("0").parse::<u64>().ok()?;
    Some(ParsedSemver {
        major,
        minor,
        patch,
        pre,
    })
}

fn compare_prerelease(a: &str, b: &str) -> std::cmp::Ordering {
    let a_parts: Vec<&str> = a.split('.').collect();
    let b_parts: Vec<&str> = b.split('.').collect();
    let max_len = a_parts.len().max(b_parts.len());
    for i in 0..max_len {
        match (a_parts.get(i), b_parts.get(i)) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) => {
                let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(nx), Ok(ny)) => nx.cmp(&ny),
                    (Ok(_), Err(_)) => std::cmp::Ordering::Less,
                    (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
                    (Err(_), Err(_)) => x.cmp(y),
                };
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
            }
        }
    }
    std::cmp::Ordering::Equal
}

pub fn is_semver_newer(current: &str, candidate: &str) -> bool {
    let Some(cur) = parse_semver_str(current) else {
        return false;
    };
    let Some(cand) = parse_semver_str(candidate) else {
        return false;
    };

    let core_ord = (cand.major, cand.minor, cand.patch)
        .cmp(&(cur.major, cur.minor, cur.patch));
    if core_ord != std::cmp::Ordering::Equal {
        return core_ord == std::cmp::Ordering::Greater;
    }

    match (cur.pre.as_deref(), cand.pre.as_deref()) {
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (None, None) => false,
        (Some(c_pre), Some(n_pre)) => {
            compare_prerelease(n_pre, c_pre) == std::cmp::Ordering::Greater
        }
    }
}

fn pick_windows_setup_asset_url(
    assets: &[GithubReleaseAsset],
    latest_version: &str,
) -> Option<String> {
    let ver_clean = latest_version.trim().trim_start_matches(['v', 'V']).to_lowercase();
    let exe_assets: Vec<&GithubReleaseAsset> = assets
        .iter()
        .filter(|a| {
            let lower = a.name.to_lowercase();
            lower.ends_with(".exe")
                && !lower.ends_with(".sig")
                && a.browser_download_url.starts_with("http")
        })
        .collect();

    if let Some(a) = exe_assets.iter().find(|a| {
        let lower = a.name.to_lowercase();
        lower.contains("setup") && !ver_clean.is_empty() && lower.contains(&ver_clean)
    }) {
        return Some(a.browser_download_url.clone());
    }
    if let Some(a) = exe_assets.iter().find(|a| {
        let lower = a.name.to_lowercase();
        lower.contains("setup") && lower.contains("dr3sh")
    }) {
        return Some(a.browser_download_url.clone());
    }
    if let Some(a) = exe_assets
        .iter()
        .find(|a| a.name.to_lowercase().contains("setup"))
    {
        return Some(a.browser_download_url.clone());
    }
    exe_assets.first().map(|a| a.browser_download_url.clone())
}

fn extract_tag_from_redirect_location(location: &str) -> Option<(String, String)> {
    let clean_loc = location.split(['?', '#']).next()?.trim_end_matches('/');
    let marker = "/releases/tag/";
    let idx = clean_loc.find(marker)?;
    let tag = clean_loc[idx + marker.len()..].trim();
    if tag.is_empty() || tag.contains('/') {
        return None;
    }
    let parsed = parse_semver_str(tag)?;
    let clean_ver = format!("{}.{}.{}", parsed.major, parsed.minor, parsed.patch);
    let clean_ver = if let Some(pre) = parsed.pre {
        format!("{clean_ver}-{pre}")
    } else {
        clean_ver
    };
    Some((tag.to_string(), clean_ver))
}

fn format_clean_version(raw: &str) -> Option<String> {
    let parsed = parse_semver_str(raw)?;
    let base = format!("{}.{}.{}", parsed.major, parsed.minor, parsed.patch);
    Some(match parsed.pre {
        Some(pre) => format!("{base}-{pre}"),
        None => base,
    })
}

pub async fn check_launcher_update_with_endpoints(
    client: &reqwest::Client,
    no_redirect_client: &reqwest::Client,
    current_version: &str,
    api_url: &str,
    latest_json_url: &str,
    releases_latest_url: &str,
    releases_base_url: &str,
) -> Result<LauncherUpdateCheckResult, String> {
    let cur_clean = format_clean_version(current_version)
        .unwrap_or_else(|| current_version.trim().trim_start_matches(['v', 'V']).to_string());
    let default_releases_page = releases_base_url.trim_end_matches('/').to_string();
    let mut errors: Vec<String> = Vec::new();

    // Layer 1: GitHub Releases REST API (with 2 attempts for transient 5xx)
    for attempt in 0..2 {
        let req = client
            .get(api_url)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", format!("Dr3shLauncher/{cur_clean}"))
            .timeout(Duration::from_secs(8));
        match req.send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    match resp.text().await {
                        Ok(body) => {
                            let trimmed = body.trim();
                            if trimmed.starts_with('{') {
                                match serde_json::from_str::<GithubLatestReleaseResponse>(trimmed) {
                                    Ok(rel) if !rel.draft && !rel.prerelease => {
                                        if let Some(latest_ver) = format_clean_version(&rel.tag_name) {
                                            let update_available = is_semver_newer(&cur_clean, &latest_ver);
                                            let release_page_url = rel
                                                .html_url
                                                .filter(|u| u.starts_with("http"))
                                                .unwrap_or_else(|| {
                                                    format!("{default_releases_page}/tag/{}", rel.tag_name)
                                                });
                                            let download_url = pick_windows_setup_asset_url(&rel.assets, &latest_ver)
                                                .unwrap_or_else(|| release_page_url.clone());
                                            return Ok(LauncherUpdateCheckResult {
                                                current_version: cur_clean,
                                                latest_version: latest_ver.clone(),
                                                tag_name: rel.tag_name,
                                                update_available,
                                                release_name: rel
                                                    .name
                                                    .filter(|s| !s.trim().is_empty())
                                                    .unwrap_or_else(|| format!("Dr3sh Launcher v{latest_ver}")),
                                                release_notes: rel.body.unwrap_or_default(),
                                                download_url,
                                                release_page_url,
                                                source: "github-api".to_string(),
                                            });
                                        }
                                        errors.push("github-api invalid tag_name".to_string());
                                        break;
                                    }
                                    Ok(_) => {
                                        errors.push("github-api returned draft/prerelease".to_string());
                                        break;
                                    }
                                    Err(e) => {
                                        errors.push(format!("github-api parse error: {e}"));
                                        break;
                                    }
                                }
                            } else {
                                errors.push("github-api returned non-JSON body".to_string());
                                break;
                            }
                        }
                        Err(e) => {
                            errors.push(format!("github-api read error: {e}"));
                            break;
                        }
                    }
                } else {
                    let retryable = matches!(status.as_u16(), 502 | 503 | 504);
                    errors.push(format!("github-api HTTP {}", status.as_u16()));
                    if !retryable || attempt == 1 {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
            Err(e) => {
                errors.push(format!("github-api network error: {e}"));
                if attempt == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    }

    // Layer 2: latest.json asset (no API rate limit)
    if let Ok(resp) = client
        .get(latest_json_url)
        .header("User-Agent", format!("Dr3shLauncher/{cur_clean}"))
        .timeout(Duration::from_secs(8))
        .send()
        .await
    {
        if resp.status().is_success() {
            if let Ok(body) = resp.text().await {
                let trimmed = body.trim();
                if trimmed.starts_with('{') {
                    if let Ok(parsed) = serde_json::from_str::<TauriLatestJsonResponse>(trimmed) {
                        if let Some(latest_ver) = format_clean_version(&parsed.version) {
                            let tag_name = if parsed.version.trim().starts_with(['v', 'V']) {
                                parsed.version.trim().to_string()
                            } else {
                                format!("v{latest_ver}")
                            };
                            let release_page_url = format!("{default_releases_page}/tag/{tag_name}");
                            let download_url = parsed
                                .platforms
                                .get("windows-x86_64")
                                .map(|p| p.url.clone())
                                .filter(|u| u.starts_with("http"))
                                .unwrap_or_else(|| release_page_url.clone());
                            let update_available = is_semver_newer(&cur_clean, &latest_ver);
                            return Ok(LauncherUpdateCheckResult {
                                current_version: cur_clean,
                                latest_version: latest_ver.clone(),
                                tag_name,
                                update_available,
                                release_name: format!("Dr3sh Launcher v{latest_ver}"),
                                release_notes: parsed.notes.unwrap_or_default(),
                                download_url,
                                release_page_url,
                                source: "latest-json".to_string(),
                            });
                        }
                    }
                }
            }
            errors.push("latest-json invalid format".to_string());
        } else {
            errors.push(format!("latest-json HTTP {}", resp.status().as_u16()));
        }
    } else {
        errors.push("latest-json unreachable".to_string());
    }

    // Layer 3: HTML /releases/latest 302 Location redirect (zero API rate limit, works without latest.json)
    if let Ok(resp) = no_redirect_client
        .get(releases_latest_url)
        .header("User-Agent", format!("Dr3shLauncher/{cur_clean}"))
        .timeout(Duration::from_secs(8))
        .send()
        .await
    {
        if resp.status().is_redirection() {
            if let Some(loc) = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
            {
                if let Some((tag_name, latest_ver)) = extract_tag_from_redirect_location(loc) {
                    let release_page_url = format!("{default_releases_page}/tag/{tag_name}");
                    let candidate_exe_url = format!(
                        "{default_releases_page}/download/{tag_name}/Dr3sh-Launcher-Setup-{latest_ver}.exe"
                    );
                    let exe_ok = match client
                        .head(&candidate_exe_url)
                        .header("User-Agent", format!("Dr3shLauncher/{cur_clean}"))
                        .timeout(Duration::from_secs(6))
                        .send()
                        .await
                    {
                        Ok(head_resp) => {
                            head_resp.status().is_success() || head_resp.status().is_redirection()
                        }
                        Err(_) => false,
                    };
                    let download_url = if exe_ok {
                        candidate_exe_url
                    } else {
                        release_page_url.clone()
                    };
                    let update_available = is_semver_newer(&cur_clean, &latest_ver);
                    return Ok(LauncherUpdateCheckResult {
                        current_version: cur_clean,
                        latest_version: latest_ver.clone(),
                        tag_name,
                        update_available,
                        release_name: format!("Dr3sh Launcher v{latest_ver}"),
                        release_notes: String::new(),
                        download_url,
                        release_page_url,
                        source: "redirect-tag".to_string(),
                    });
                }
            }
        }
        errors.push(format!("redirect-tag HTTP {}", resp.status().as_u16()));
    } else {
        errors.push("redirect-tag unreachable".to_string());
    }

    Err(format!(
        "No se pudo consultar actualizaciones en GitHub: {}",
        errors.join("; ")
    ))
}

#[tauri::command]
pub async fn dr3sh_check_launcher_update(
    app: tauri::AppHandle,
) -> Result<LauncherUpdateCheckResult, String> {
    let client = build_http_client().map_err(|e| e.to_string())?;
    let no_redirect_client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;
    let current_version = app.package_info().version.to_string();
    check_launcher_update_with_endpoints(
        &client,
        &no_redirect_client,
        &current_version,
        GITHUB_LAUNCHER_LATEST_API_URL,
        GITHUB_LAUNCHER_LATEST_JSON_URL,
        GITHUB_LAUNCHER_RELEASES_LATEST_URL,
        GITHUB_LAUNCHER_RELEASES_BASE_URL,
    )
    .await
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
        .timeout(Duration::from_secs(3))
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
                            Some(6),
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
                    let pct = (1.0 + ratio * 5.0).round() as u32;
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
                    Some(6),
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
        {
            let op_c = op_id.clone();
            let dir_c = effective_instance_dir.clone();
            tokio::task::spawn_blocking(move || recover_instance(Some(&op_c), &dir_c))
                .await
                .map_err(|e| Dr3shError::unknown(format!("Recovery task join error: {e}")))??;
        }

        let baseline = {
            let dir_c = effective_instance_dir.clone();
            tokio::task::spawn_blocking(move || load_baseline(&dir_c))
                .await
                .map_err(|e| Dr3shError::unknown(format!("Baseline task join error: {e}")))?
        };
        let from_version = baseline
            .as_ref()
            .map(|b| b.version.clone())
            .or_else(|| get_server_state(&server_id).installed_version);

        // Fetch manifest if available
        let stage_manifest_start = Instant::now();
        let remote_manifest = fetch_manifest_if_available(&op_id, &server_id, &spec).await?;
        if cancel_token.load(Ordering::SeqCst) {
            return Err(Dr3shError::Cancelled {
                stage: "manifest".to_string(),
            });
        }
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

        let est_write_bytes = remote_manifest
            .as_ref()
            .map(|m| m.files.iter().map(|f| f.size).sum::<u64>())
            .unwrap_or(expected_zip_size);

        // Section 10: Run preflight BEFORE creating junctions or downloading
        let preflight_report = {
            let op_c = op_id.clone();
            let def_c = default_instance_dir.clone();
            let cust_c = custom_target_dir.clone();
            let eff_c = effective_instance_dir.clone();
            let srv_c = server_id.clone();
            let spec_id_c = spec.id.clone();
            let sha_c = expected_zip_sha.clone();
            tokio::task::spawn_blocking(move || {
                let already_downloaded_bytes = existing_part_size(
                    &eff_c,
                    &srv_c,
                    &spec_id_c,
                    sha_c.as_deref(),
                );
                let zip_remaining = expected_zip_size.saturating_sub(already_downloaded_bytes);
                run_preflight(
                    &op_c,
                    &def_c,
                    cust_c.as_deref(),
                    zip_remaining,
                    est_write_bytes,
                )
            })
            .await
            .map_err(|e| Dr3shError::unknown(format!("Preflight task join error: {e}")))??
        };

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
            {
                let op_c = op_id.clone();
                let def_c = default_instance_dir.clone();
                let cust_c = custom_target_dir.clone();
                tokio::task::spawn_blocking(move || -> Result<(), Dr3shError> {
                    if let Some(ref custom_target) = cust_c {
                        ensure_safe_junction(Some(&op_c), &def_c, custom_target)?;
                    } else {
                        fs::create_dir_all(&def_c).map_err(|e| Dr3shError::FsPermission {
                            path: def_c.to_string_lossy().to_string(),
                            os_code: e.raw_os_error().unwrap_or(-1).to_string(),
                        })?;
                        let _ = fs::write(def_c.join(".dr3sh_instance"), b"dr3sh");
                        ensure_single_entry_user_deletable(&def_c);
                    }
                    Ok(())
                })
                .await
                .map_err(|e| Dr3shError::unknown(format!("Junction setup join error: {e}")))??;
            }

            if let Some(ref app_handle) = app {
                let _ = app_handle
                    .fs_scope()
                    .allow_directory(&effective_instance_dir, true);
            }

            if cancel_token.load(Ordering::SeqCst) {
                return Err(Dr3shError::Cancelled {
                    stage: "preflight".to_string(),
                });
            }

            // If we already have a schema-1 remote manifest, we can compute the plan BEFORE downloading!
            // That satisfies Section 15 #15: "No changes between versions -> the ZIP is not downloaded and the baseline is updated."
            let txn_id = format!("txn_{}", op_id);
            let mut downloaded_zip_path: Option<PathBuf> = None;
            let mut dl_bytes = 0u64;
            let mut dl_speed = 0u64;
            let mut dl_retries = 0u32;

            let (manifest, plan) = if let Some(m) = remote_manifest {
                let mut plan = {
                    let eff_c = effective_instance_dir.clone();
                    let base_c = baseline.clone();
                    let m_c = m.clone();
                    tokio::task::spawn_blocking(move || {
                        build_reconciliation_plan(&eff_c, base_c.as_ref(), &m_c, ignore_cache)
                    })
                    .await
                    .map_err(|e| Dr3shError::unknown(format!("Plan task join error: {e}")))?
                };

                if plan.needs_zip() {
                    update_op_stage(&op_id, "DOWNLOAD", None);
                    update_server_state(app.as_ref(), &server_id, |st| {
                        st.state = ServerInstallState::Downloading;
                    });
                    let dl_stage_start = Instant::now();
                    let remote_zip_bytes = if m.zip.size > 0 {
                        m.zip.size
                    } else {
                        expected_zip_size
                    };
                    let mut used_delta = false;
                    if should_use_partial_zip(remote_zip_bytes, &plan, 4 * 1024 * 1024) {
                        match download_and_extract_partial_zip(
                            app.as_ref(),
                            &op_id,
                            &server_id,
                            &m.zip.url,
                            remote_zip_bytes,
                            &effective_instance_dir,
                            &txn_id,
                            &plan,
                            &cancel_token,
                        )
                        .await?
                        {
                            PartialZipAttempt::Completed(outcome) => {
                                dl_bytes = outcome.downloaded_bytes;
                                dl_speed = outcome.avg_speed_bps;
                                used_delta = true;
                                log_event(
                                    Some(&op_id),
                                    "DOWNLOAD",
                                    "INFO",
                                    "delta partial ZIP completed",
                                    &[
                                        ("downloaded_bytes", outcome.downloaded_bytes.to_string()),
                                        ("full_zip_bytes", remote_zip_bytes.to_string()),
                                        (
                                            "saved_bytes",
                                            remote_zip_bytes
                                                .saturating_sub(outcome.downloaded_bytes)
                                                .to_string(),
                                        ),
                                        ("files_extracted", outcome.files_extracted.to_string()),
                                        (
                                            "duration_ms",
                                            dl_stage_start.elapsed().as_millis().to_string(),
                                        ),
                                    ],
                                );
                            }
                            PartialZipAttempt::FallbackToFull(reason) => {
                                log_event(
                                    Some(&op_id),
                                    "DOWNLOAD",
                                    "WARN",
                                    "partial ZIP fallback to full download",
                                    &[("reason", reason)],
                                );
                            }
                        }
                    }

                    if !used_delta {
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
                        if cancel_token.load(Ordering::SeqCst) {
                            return Err(Dr3shError::Cancelled {
                                stage: "verify".to_string(),
                            });
                        }
                        update_op_stage(&op_id, "VERIFY", None);
                        update_server_state(app.as_ref(), &server_id, |st| {
                            st.state = ServerInstallState::Verifying;
                        });
                        let verify_total = m.files.len() as u64;
                        if let Some(ref app_handle) = app {
                            emit_progress_detailed(
                                app_handle,
                                &op_id,
                                &server_id,
                                "verify",
                                format!("Verificando archivos (0/{verify_total})..."),
                                0,
                                0,
                                0,
                                None,
                                Some(70),
                                None,
                                Some(0),
                                Some(verify_total),
                            );
                        }
                        let resolved_map = {
                            let p_c = outcome.part_path.clone();
                            let m_c = m.clone();
                            let c_c = cancel_token.clone();
                            let app_c = app.clone();
                            let op_c = op_id.clone();
                            let srv_c = server_id.clone();
                            tokio::task::spawn_blocking(move || {
                                verify_zip_against_manifest_cancellable(
                                    &p_c,
                                    &m_c,
                                    &c_c,
                                    |done, total| {
                                        if let Some(ref app_handle) = app_c {
                                            let ratio = if total > 0 {
                                                done as f64 / total as f64
                                            } else {
                                                1.0
                                            };
                                            let pct = (70.0 + ratio * 2.0).round() as u32;
                                            emit_progress_detailed(
                                                app_handle,
                                                &op_c,
                                                &srv_c,
                                                "verify",
                                                format!("Verificando archivos ({done}/{total})..."),
                                                0,
                                                0,
                                                0,
                                                None,
                                                Some(pct),
                                                None,
                                                Some(done),
                                                Some(total),
                                            );
                                        }
                                    },
                                )
                            })
                            .await
                            .map_err(|e| Dr3shError::unknown(format!("Verify task join error: {e}")))??
                        };
                        if let Some(ref app_handle) = app {
                            emit_progress_detailed(
                                app_handle,
                                &op_id,
                                &server_id,
                                "verify",
                                "Paquete verificado.",
                                0,
                                0,
                                0,
                                None,
                                Some(72),
                                None,
                                Some(verify_total),
                                Some(verify_total),
                            );
                        }
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
                    }
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

                if cancel_token.load(Ordering::SeqCst) {
                    return Err(Dr3shError::Cancelled {
                        stage: "verify".to_string(),
                    });
                }

                update_op_stage(&op_id, "VERIFY", None);
                update_server_state(app.as_ref(), &server_id, |st| {
                    st.state = ServerInstallState::Verifying;
                });
                if let Some(ref app_handle) = app {
                    emit_progress_detailed(
                        app_handle,
                        &op_id,
                        &server_id,
                        "verify",
                        "Analizando archivos del paquete...",
                        0,
                        0,
                        0,
                        None,
                        Some(70),
                        None,
                        None,
                        None,
                    );
                }
                let (synth, plan) = {
                    let p_c = outcome.part_path.clone();
                    let eff_c = effective_instance_dir.clone();
                    let srv_c = server_id.clone();
                    let spec_id_c = spec.id.clone();
                    let mc_c = spec
                        .game_version
                        .clone()
                        .unwrap_or_else(|| "1.21.1".to_string());
                    let ld_c = spec.loader.clone().unwrap_or_else(|| "fabric".to_string());
                    let ld_ver_c = spec.loader_version.clone().unwrap_or_default();
                    let url_c = zip_url.clone();
                    let base_c = baseline.clone();
                    let c_c = cancel_token.clone();
                    let app_c = app.clone();
                    let op_c = op_id.clone();
                    tokio::task::spawn_blocking(move || -> Result<(Manifest, manifest::ReconciliationPlan), Dr3shError> {
                        let synth = synthesize_manifest_from_zip_cancellable(
                            &p_c,
                            &srv_c,
                            &spec_id_c,
                            &mc_c,
                            &ld_c,
                            &ld_ver_c,
                            &url_c,
                            &c_c,
                            |done, total| {
                                if let Some(ref app_handle) = app_c {
                                    let ratio = if total > 0 {
                                        done as f64 / total as f64
                                    } else {
                                        0.0
                                    };
                                    let pct = (70.0 + ratio * 2.0).round() as u32;
                                    emit_progress_detailed(
                                        app_handle,
                                        &op_c,
                                        &srv_c,
                                        "verify",
                                        format!("Analizando archivos ({done}/{total})..."),
                                        0,
                                        0,
                                        0,
                                        None,
                                        Some(pct),
                                        None,
                                        Some(done),
                                        Some(total),
                                    );
                                }
                            },
                        )?;
                        let plan = build_reconciliation_plan(
                            &eff_c,
                            base_c.as_ref(),
                            &synth,
                            ignore_cache,
                        );
                        Ok((synth, plan))
                    })
                    .await
                    .map_err(|e| Dr3shError::unknown(format!("Synthesize task join error: {e}")))??
                };
                let synth_total = synth.files.len() as u64;
                if let Some(ref app_handle) = app {
                    emit_progress_detailed(
                        app_handle,
                        &op_id,
                        &server_id,
                        "verify",
                        "Paquete verificado.",
                        0,
                        0,
                        0,
                        None,
                        Some(72),
                        None,
                        Some(synth_total),
                        Some(synth_total),
                    );
                }
                downloaded_zip_path = Some(outcome.part_path);
                dl_bytes = outcome.total_bytes;
                dl_speed = outcome.avg_speed_bps;
                dl_retries = outcome.retries_used;
                (synth, plan)
            };

            if cancel_token.load(Ordering::SeqCst) {
                return Err(Dr3shError::Cancelled {
                    stage: "extract".to_string(),
                });
            }

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
            {
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
                let op_c = op_id.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    logger::log_op_debug_batch(Some(&op_c), &plan_debug_buf);
                })
                .await;
            }

            // Stage 5: Extract needed files into staging/<txn_id>
            if let Some(ref zip_path) = downloaded_zip_path {
                if plan.needs_zip() {
                    if cancel_token.load(Ordering::SeqCst) {
                        return Err(Dr3shError::Cancelled {
                            stage: "extract".to_string(),
                        });
                    }
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
            if let Some(zip_path) = downloaded_zip_path {
                let _ = tokio::task::spawn_blocking(move || {
                    let _ = fs::remove_file(zip_path);
                })
                .await;
            }

            {
                let eff_c = effective_instance_dir.clone();
                let srv_c = server_id.clone();
                let inst_c = instance_id.clone();
                let ver_c = manifest.version.clone();
                tokio::task::spawn_blocking(move || {
                    if let Err(e) = write_instance_marker(&eff_c, &srv_c, &inst_c, &ver_c) {
                        tracing::warn!("No se pudo escribir el marcador de instancia: {e}");
                    }
                    ensure_user_deletable_permissions(&eff_c);
                })
                .await
                .map_err(|e| Dr3shError::unknown(format!("Marker/ACL task join error: {e}")))?;
            }

            let migration_notice = plan.has_migration_backups();
            let inst_path_str = effective_instance_dir.to_string_lossy().to_string();

            update_op_stage(&op_id, "DONE", None);
            update_server_state(app.as_ref(), &server_id, |st| {
                st.state = ServerInstallState::Installed;
                st.instance_id = Some(instance_id.clone());
                st.instance_path = Some(inst_path_str.clone());
                st.installed_version = Some(manifest.version.clone());
                st.marker_written = true;
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

pub const INSTANCE_MARKER_REL_PATH: &str = ".dr3sh/instance_marker.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceMarker {
    pub marker: String,
    pub server_id: String,
    pub instance_id: String,
    pub version: String,
    pub installed_at: String,
    #[serde(default)]
    pub vital_dirs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct InstanceHealthStatus {
    pub healthy: bool,
    pub needs_repair: bool,
    pub reason: Option<String>,
    pub resolved_path: Option<String>,
}

pub fn write_instance_marker(
    instance_dir: &PathBuf,
    server_id: &str,
    instance_id: &str,
    version: &str,
) -> Result<(), Dr3shError> {
    let dr3sh_dir = instance_dir.join(".dr3sh");
    fs::create_dir_all(&dr3sh_dir).map_err(|e| Dr3shError::FsPermission {
        path: dr3sh_dir.to_string_lossy().to_string(),
        os_code: e.raw_os_error().unwrap_or(-1).to_string(),
    })?;

    let payload = InstanceMarker {
        marker: "dr3sh_instance_present".to_string(),
        server_id: server_id.to_string(),
        instance_id: instance_id.to_string(),
        version: version.to_string(),
        installed_at: Utc::now().to_rfc3339(),
        vital_dirs: vec!["mods".to_string(), ".dr3sh".to_string()],
    };

    let final_path = dr3sh_dir.join("instance_marker.json");
    let tmp_path = dr3sh_dir.join("instance_marker.json.tmp");
    let bytes = serde_json::to_vec_pretty(&payload).map_err(|e| {
        Dr3shError::unknown(format!("No se pudo serializar el marcador: {e}"))
    })?;
    fs::write(&tmp_path, bytes).map_err(|e| Dr3shError::FsPermission {
        path: tmp_path.to_string_lossy().to_string(),
        os_code: e.raw_os_error().unwrap_or(-1).to_string(),
    })?;
    fs::rename(&tmp_path, &final_path).map_err(|e| Dr3shError::FsPermission {
        path: final_path.to_string_lossy().to_string(),
        os_code: e.raw_os_error().unwrap_or(-1).to_string(),
    })?;
    Ok(())
}

fn dir_has_any_entry(dir: &PathBuf) -> bool {
    dir.is_dir()
        && fs::read_dir(dir)
            .ok()
            .and_then(|mut rd| rd.next())
            .is_some()
}

pub fn evaluate_instance_health(
    server_id: &str,
    fallback_instance_path: Option<&str>,
) -> InstanceHealthStatus {
    let st = get_server_state(server_id);
    let state_path = st
        .instance_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let fallback_path = fallback_instance_path
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let has_install_record = st
        .installed_version
        .as_deref()
        .is_some_and(|v| !v.trim().is_empty())
        || state_path.is_some()
        || fallback_path.is_some();

    if !has_install_record {
        return InstanceHealthStatus {
            healthy: false,
            needs_repair: false,
            reason: None,
            resolved_path: None,
        };
    }

    let Some(dir_str) = state_path.or(fallback_path) else {
        return InstanceHealthStatus {
            healthy: false,
            needs_repair: true,
            reason: Some(
                "No se encontró la carpeta configurada para esta instancia."
                    .to_string(),
            ),
            resolved_path: None,
        };
    };

    let dir = PathBuf::from(dir_str);
    let resolved_path = Some(dir_str.to_string());

    if !dir.exists() || !dir.is_dir() {
        return InstanceHealthStatus {
            healthy: false,
            needs_repair: true,
            reason: Some(
                "No se encontró la carpeta de la instancia en disco.".to_string(),
            ),
            resolved_path,
        };
    }

    let marker_path = dir.join(".dr3sh").join("instance_marker.json");
    if marker_path.is_file() {
        let parsed = fs::read_to_string(&marker_path)
            .ok()
            .and_then(|raw| serde_json::from_str::<InstanceMarker>(&raw).ok());
        let valid_marker = parsed.as_ref().is_some_and(|m| {
            m.marker == "dr3sh_instance_present"
                && m.server_id.eq_ignore_ascii_case(server_id)
        });
        if !valid_marker {
            return InstanceHealthStatus {
                healthy: false,
                needs_repair: true,
                reason: Some(
                    "El marcador de verificación (.dr3sh/instance_marker.json) está dañado o no coincide."
                        .to_string(),
                ),
                resolved_path,
            };
        }
    } else if st.marker_written {
        return InstanceHealthStatus {
            healthy: false,
            needs_repair: true,
            reason: Some(
                "Falta el archivo marcador (.dr3sh/instance_marker.json)."
                    .to_string(),
            ),
            resolved_path,
        };
    } else {
        let has_legacy_marker = dir.join(".dr3sh").join("baseline.json").is_file()
            || dir.join(".dr3sh_instance").is_file()
            || dir.join("profile.json").is_file();
        if !has_legacy_marker || !dir_has_any_entry(&dir.join("mods")) {
            return InstanceHealthStatus {
                healthy: false,
                needs_repair: true,
                reason: Some(
                    "Faltan archivos de integridad de la instancia.".to_string(),
                ),
                resolved_path,
            };
        }
        let inst_id = st
            .instance_id
            .as_deref()
            .unwrap_or(server_id);
        let ver = st.installed_version.as_deref().unwrap_or("1");
        if write_instance_marker(&dir, server_id, inst_id, ver).is_ok() {
            update_server_state(None, server_id, |entry| {
                entry.marker_written = true;
            });
        }
    }

    if let Some(baseline) = load_baseline(&dir) {
        let mut expected_mod_files = 0usize;
        for mf in &baseline.files {
            let lower = mf.path.to_ascii_lowercase();
            if lower.starts_with("mods/") {
                expected_mod_files += 1;
                if mf.policy == "managed" && !dir.join(&mf.path).exists() {
                    return InstanceHealthStatus {
                        healthy: false,
                        needs_repair: true,
                        reason: Some(format!(
                            "Falta un archivo vital del modpack: {}",
                            mf.path
                        )),
                        resolved_path,
                    };
                }
            }
        }
        if expected_mod_files > 0 && !dir_has_any_entry(&dir.join("mods")) {
            return InstanceHealthStatus {
                healthy: false,
                needs_repair: true,
                reason: Some("Falta la carpeta mods o está vacía.".to_string()),
                resolved_path,
            };
        }
    } else if !dir_has_any_entry(&dir.join("mods")) {
        return InstanceHealthStatus {
            healthy: false,
            needs_repair: true,
            reason: Some(
                "Falta la carpeta mods o el registro de archivos de la instancia."
                    .to_string(),
            ),
            resolved_path,
        };
    }

    InstanceHealthStatus {
        healthy: true,
        needs_repair: false,
        reason: None,
        resolved_path,
    }
}

#[tauri::command]
pub async fn dr3sh_check_instance_health(
    server_id: String,
    fallback_instance_path: Option<String>,
) -> InstanceHealthStatus {
    tokio::task::spawn_blocking(move || {
        evaluate_instance_health(&server_id, fallback_instance_path.as_deref())
    })
    .await
    .unwrap_or_else(|_| InstanceHealthStatus {
        healthy: true,
        needs_repair: false,
        reason: None,
        resolved_path: None,
    })
}

fn preserved_custom_dir_for_repair(saved_path: Option<&str>) -> Option<String> {
    let raw = saved_path?.trim();
    if raw.is_empty() {
        return None;
    }
    let normalized = raw.replace('/', "\\").to_ascii_lowercase();
    if normalized.contains(r"\com.dr3sh.launcher\profiles\") {
        return None;
    }
    let root = drive_root_for_path(std::path::Path::new(raw));
    if root.exists() {
        Some(raw.to_string())
    } else {
        None
    }
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
    let preserved_custom_dir =
        preserved_custom_dir_for_repair(st.instance_path.as_deref());
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
            preserved_custom_dir,
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
    let existing_for_acl = existing_path.clone();
    let _ = tokio::task::spawn_blocking(move || {
        ensure_user_deletable_permissions(&existing_for_acl);
    })
    .await;
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
            st.marker_written = false;
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

    #[test]
    fn test_launcher_update_semver_comparison_cases() {
        assert!(is_semver_newer("1.1.0", "1.2.0"));
        assert!(is_semver_newer("1.1.0", "v1.1.1"));
        assert!(is_semver_newer("1.1.0", "2.0.0"));
        assert!(is_semver_newer("1.0.5", "1.1.0"));
        assert!(is_semver_newer("1.2.0-beta.1", "1.2.0"));
        assert!(is_semver_newer("1.2.0-beta.1", "1.2.0-beta.2"));
        assert!(!is_semver_newer("1.1.0", "1.1.0"));
        assert!(!is_semver_newer("1.1.0", "v1.1.0"));
        assert!(!is_semver_newer("1.2.0", "1.1.9"));
        assert!(!is_semver_newer("1.2.0", "1.2.0-beta.1"));
        assert!(!is_semver_newer("1.1.0", "not-a-version"));
    }

    #[test]
    fn test_pick_windows_setup_asset_prefers_matching_version_and_skips_sig() {
        let assets = vec![
            GithubReleaseAsset {
                name: "Dr3sh-Launcher-Setup-1.2.0.exe.sig".to_string(),
                browser_download_url: "https://example.test/1.2.0.exe.sig".to_string(),
            },
            GithubReleaseAsset {
                name: "latest.json".to_string(),
                browser_download_url: "https://example.test/latest.json".to_string(),
            },
            GithubReleaseAsset {
                name: "Dr3sh-Launcher-Setup-1.0.5.exe".to_string(),
                browser_download_url: "https://example.test/old.exe".to_string(),
            },
            GithubReleaseAsset {
                name: "Dr3sh.Launcher_1.2.0_x64-setup.exe".to_string(),
                browser_download_url: "https://example.test/alt-1.2.0.exe".to_string(),
            },
            GithubReleaseAsset {
                name: "Dr3sh-Launcher-Setup-1.2.0.exe".to_string(),
                browser_download_url: "https://example.test/Dr3sh-Launcher-Setup-1.2.0.exe".to_string(),
            },
        ];
        let picked = pick_windows_setup_asset_url(&assets, "1.2.0");
        assert!(
            picked.as_deref() == Some("https://example.test/alt-1.2.0.exe")
                || picked.as_deref() == Some("https://example.test/Dr3sh-Launcher-Setup-1.2.0.exe"),
            "picked={picked:?}"
        );
        let sig_only = vec![GithubReleaseAsset {
            name: "Dr3sh-Launcher-Setup-1.2.0.exe.sig".to_string(),
            browser_download_url: "https://example.test/1.2.0.exe.sig".to_string(),
        }];
        assert_eq!(pick_windows_setup_asset_url(&sig_only, "1.2.0"), None);
    }

    #[tokio::test]
    async fn test_launcher_update_api_detects_newer_same_and_older_releases() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let _ = socket.read(&mut buf).await;
                    let body = format!(
                        r#"{{
                            "tag_name": "v1.2.0",
                            "name": "Dr3sh Launcher v1.2.0",
                            "body": "- Mejora de estabilidad\n- Nuevo instalador",
                            "html_url": "http://{addr}/releases/tag/v1.2.0",
                            "draft": false,
                            "prerelease": false,
                            "assets": [
                                {{ "name": "Dr3sh-Launcher-Setup-1.2.0.exe.sig", "browser_download_url": "http://{addr}/releases/download/v1.2.0/setup.exe.sig" }},
                                {{ "name": "Dr3sh-Launcher-Setup-1.2.0.exe", "browser_download_url": "http://{addr}/releases/download/v1.2.0/Dr3sh-Launcher-Setup-1.2.0.exe" }}
                            ]
                        }}"#
                    );
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });

        let client = build_http_client().unwrap();
        let no_redir = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let api_url = format!("http://{addr}/api/releases/latest");
        let json_url = format!("http://{addr}/latest.json");
        let latest_url = format!("http://{addr}/releases/latest");
        let base_url = format!("http://{addr}/releases");

        let newer = check_launcher_update_with_endpoints(
            &client, &no_redir, "1.1.0", &api_url, &json_url, &latest_url, &base_url,
        )
        .await
        .unwrap();
        assert!(newer.update_available);
        assert_eq!(newer.latest_version, "1.2.0");
        assert_eq!(newer.source, "github-api");
        assert!(newer.download_url.ends_with("Dr3sh-Launcher-Setup-1.2.0.exe"));
        assert!(newer.release_notes.contains("Nuevo instalador"));

        let same = check_launcher_update_with_endpoints(
            &client, &no_redir, "1.2.0", &api_url, &json_url, &latest_url, &base_url,
        )
        .await
        .unwrap();
        assert!(!same.update_available);

        let older_remote = check_launcher_update_with_endpoints(
            &client, &no_redir, "1.5.0", &api_url, &json_url, &latest_url, &base_url,
        )
        .await
        .unwrap();
        assert!(!older_remote.update_available);
    }

    #[tokio::test]
    async fn test_launcher_update_api_retries_504_then_succeeds() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let count_cl = count.clone();

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let n = count_cl.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let _ = socket.read(&mut buf).await;
                    if n == 0 {
                        let _ = socket
                            .write_all(b"HTTP/1.1 504 Gateway Time-out\r\nContent-Length: 0\r\n\r\n")
                            .await;
                    } else {
                        let body = format!(
                            r#"{{"tag_name":"v1.2.1","html_url":"http://{addr}/releases/tag/v1.2.1","assets":[{{"name":"Dr3sh-Launcher-Setup-1.2.1.exe","browser_download_url":"http://{addr}/Dr3sh-Launcher-Setup-1.2.1.exe"}}]}}"#
                        );
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

        let client = build_http_client().unwrap();
        let no_redir = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let res = check_launcher_update_with_endpoints(
            &client,
            &no_redir,
            "1.1.0",
            &format!("http://{addr}/api"),
            &format!("http://{addr}/latest.json"),
            &format!("http://{addr}/releases/latest"),
            &format!("http://{addr}/releases"),
        )
        .await
        .unwrap();
        assert!(res.update_available);
        assert_eq!(res.latest_version, "1.2.1");
        assert_eq!(res.source, "github-api");
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_launcher_update_rate_limit_or_html_falls_back_to_latest_json() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let n = socket.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    if req.starts_with("GET /api") {
                        let _ = socket
                            .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 10\r\n\r\nrate limit")
                            .await;
                    } else if req.starts_with("GET /api-html") {
                        let _ = socket
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 16\r\n\r\n<html>err</html>")
                            .await;
                    } else {
                        let body = format!(
                            r#"{{"version":"1.2.5","notes":"Parche urgente","platforms":{{"windows-x86_64":{{"url":"http://{addr}/Dr3sh-Launcher-Setup-1.2.5.exe"}}}}}}"#
                        );
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

        let client = build_http_client().unwrap();
        let no_redir = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();

        for api_path in ["/api", "/api-html"] {
            let res = check_launcher_update_with_endpoints(
                &client,
                &no_redir,
                "1.1.0",
                &format!("http://{addr}{api_path}"),
                &format!("http://{addr}/latest.json"),
                &format!("http://{addr}/releases/latest"),
                &format!("http://{addr}/releases"),
            )
            .await
            .unwrap();
            assert!(res.update_available);
            assert_eq!(res.latest_version, "1.2.5");
            assert_eq!(res.source, "latest-json");
            assert_eq!(res.release_notes, "Parche urgente");
        }
    }

    #[tokio::test]
    async fn test_launcher_update_falls_back_to_redirect_tag_and_verifies_exe() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let n = socket.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    if req.starts_with("GET /releases/latest ") {
                        let resp = format!(
                            "HTTP/1.1 302 Found\r\nLocation: http://{addr}/releases/tag/v1.4.0\r\nContent-Length: 0\r\n\r\n"
                        );
                        let _ = socket.write_all(resp.as_bytes()).await;
                    } else if req.starts_with("HEAD /releases/download/v1.4.0/Dr3sh-Launcher-Setup-1.4.0.exe ") {
                        let _ = socket
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n")
                            .await;
                    } else if req.starts_with("GET /alt/releases/latest ") {
                        let resp = format!(
                            "HTTP/1.1 302 Found\r\nLocation: http://{addr}/alt/releases/tag/v1.5.0\r\nContent-Length: 0\r\n\r\n"
                        );
                        let _ = socket.write_all(resp.as_bytes()).await;
                    } else {
                        let _ = socket
                            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                            .await;
                    }
                    let _ = socket.shutdown().await;
                });
            }
        });

        let client = build_http_client().unwrap();
        let no_redir = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();

        // Case 1: redirect tag + HEAD 200 OK -> direct .exe URL
        let res1 = check_launcher_update_with_endpoints(
            &client,
            &no_redir,
            "1.1.0",
            &format!("http://{addr}/api"),
            &format!("http://{addr}/latest.json"),
            &format!("http://{addr}/releases/latest"),
            &format!("http://{addr}/releases"),
        )
        .await
        .unwrap();
        assert!(res1.update_available);
        assert_eq!(res1.source, "redirect-tag");
        assert_eq!(res1.latest_version, "1.4.0");
        assert_eq!(
            res1.download_url,
            format!("http://{addr}/releases/download/v1.4.0/Dr3sh-Launcher-Setup-1.4.0.exe")
        );

        // Case 2: redirect tag + HEAD 404 -> safe fallback to release page URL (no 404 link!)
        let res2 = check_launcher_update_with_endpoints(
            &client,
            &no_redir,
            "1.1.0",
            &format!("http://{addr}/api"),
            &format!("http://{addr}/latest.json"),
            &format!("http://{addr}/alt/releases/latest"),
            &format!("http://{addr}/alt/releases"),
        )
        .await
        .unwrap();
        assert!(res2.update_available);
        assert_eq!(res2.source, "redirect-tag");
        assert_eq!(res2.latest_version, "1.5.0");
        assert_eq!(res2.download_url, format!("http://{addr}/alt/releases/tag/v1.5.0"));
    }

    #[tokio::test]
    async fn test_launcher_update_all_endpoints_unreachable_returns_err_cleanly() {
        let client = build_http_client().unwrap();
        let no_redir = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let err = check_launcher_update_with_endpoints(
            &client,
            &no_redir,
            "1.1.0",
            "http://127.0.0.1:1/api",
            "http://127.0.0.1:1/latest.json",
            "http://127.0.0.1:1/releases/latest",
            "http://127.0.0.1:1/releases",
        )
        .await
        .unwrap_err();
        assert!(err.contains("No se pudo consultar actualizaciones en GitHub"), "err={err}");
    }

    #[test]
    fn test_instance_marker_and_health_detection() {
        let srv_id = format!("test_srv_{}", uuid::Uuid::new_v4());
        let base = std::env::temp_dir().join(format!("dr3sh_health_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(base.join("mods")).unwrap();
        fs::write(base.join("mods").join("example.jar"), b"jar").unwrap();
        fs::create_dir_all(base.join(".dr3sh")).unwrap();
        fs::write(
            base.join(".dr3sh").join("baseline.json"),
            r#"{"schema":1,"pack_id":"p","version":"1","minecraft_version":"1.21.1","loader":"fabric","loader_version":"0.16.0","zip":{"url":"","size":0,"sha256":""},"rules":[],"files":[{"path":"mods/example.jar","sha256":"abc","size":3,"policy":"managed"}]}"#,
        )
        .unwrap();
        write_instance_marker(&base, &srv_id, "inst_1", "1").unwrap();

        update_server_state(None, &srv_id, |st| {
            st.state = ServerInstallState::Installed;
            st.installed_version = Some("1".to_string());
            st.instance_id = Some("inst_1".to_string());
            st.instance_path = Some(base.to_string_lossy().to_string());
            st.marker_written = true;
        });

        let h1 = evaluate_instance_health(&srv_id, None);
        assert!(h1.healthy && !h1.needs_repair, "expected healthy: {h1:?}");

        // Delete marker JSON -> needs_repair
        fs::remove_file(base.join(".dr3sh").join("instance_marker.json")).unwrap();
        let h2 = evaluate_instance_health(&srv_id, None);
        assert!(!h2.healthy && h2.needs_repair, "expected needs_repair without marker: {h2:?}");

        // Restore marker, delete vital mod -> needs_repair
        write_instance_marker(&base, &srv_id, "inst_1", "1").unwrap();
        fs::remove_file(base.join("mods").join("example.jar")).unwrap();
        let h3 = evaluate_instance_health(&srv_id, None);
        assert!(!h3.healthy && h3.needs_repair, "expected needs_repair without vital mod: {h3:?}");

        // Delete entire directory -> needs_repair
        let _ = fs::remove_dir_all(&base);
        let h4 = evaluate_instance_health(&srv_id, None);
        assert!(!h4.healthy && h4.needs_repair, "expected needs_repair without dir: {h4:?}");
    }
}
