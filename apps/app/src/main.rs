#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]
#![recursion_limit = "256"]

use anyhow::Context;
use native_dialog::{DialogBuilder, MessageLevel};
use serde::{Deserialize, Serialize};
use std::env;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{Emitter, Listener, Manager};
use tauri_plugin_fs::FsExt;
use theseus::prelude::*;

pub mod dr3sh;
use dr3sh::{
    dr3sh_ack_migration_notice, dr3sh_activate_preload, dr3sh_cancel, dr3sh_cancel_download,
    dr3sh_check_instance_health, dr3sh_check_launcher_update,
    dr3sh_cleanup_deleted_instance, dr3sh_download_modpack, dr3sh_get_diagnostics,
    dr3sh_get_network_time, dr3sh_get_preload_state, dr3sh_get_state, dr3sh_install_or_update,
    dr3sh_link_existing_instance, dr3sh_lock_preload, dr3sh_migrate_local_state,
    dr3sh_open_logs_folder, dr3sh_prepare_launch, dr3sh_repair, dr3sh_wait_for_instance_ready,
};

fn cleanup_dangling_profile_junctions() {
    if let Ok(appdata) = std::env::var("APPDATA") {
        let profiles_dir = PathBuf::from(appdata)
            .join("com.dr3sh.launcher")
            .join("profiles");
        if let Ok(entries) = std::fs::read_dir(&profiles_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if !p.exists() && dr3sh::preflight::is_reparse_point(&p) {
                    let _ = dr3sh::preflight::remove_junction_link_only(&p);
                }
            }
        }
    }
}

#[tauri::command]
fn dr3sh_has_extracted_mods(folder: String) -> bool {
    let p = PathBuf::from(folder.trim());
    let mods_dir = p.join("mods");
    if !mods_dir.is_dir() {
        return false;
    }
    std::fs::read_dir(mods_dir)
        .ok()
        .map(|mut entries| entries.any(|e| e.is_ok()))
        .unwrap_or(false)
}

static PUBLISH_RELEASE_CANCEL: AtomicBool = AtomicBool::new(false);
static PUBLISH_RELEASE_PID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

#[tauri::command]
fn dr3sh_cancel_publish_release() {
    PUBLISH_RELEASE_CANCEL.store(true, Ordering::SeqCst);
    let pid = PUBLISH_RELEASE_PID.swap(0, Ordering::SeqCst);
    if pid > 0 {
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            let mut kill_cmd = std::process::Command::new("taskkill");
            kill_cmd.args(["/PID", &pid.to_string(), "/T", "/F"]);
            kill_cmd.creation_flags(0x08000000);
            let _ = kill_cmd.output();
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = std::process::Command::new("kill")
                .args(["-9", &pid.to_string()])
                .output();
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishReleaseResult {
    pub tag_name: String,
    pub archive_name: String,
    pub release_url: String,
    pub archive_url: String,
    pub manifest_url: String,
    pub size_mb: f64,
    #[serde(default)]
    pub sha256: String,
}

#[tauri::command]
async fn dr3sh_publish_release(
    _app: tauri::AppHandle,
    token: String,
    server_id: String,
    version: String,
    title: String,
    archive_path: String,
    description: Option<String>,
    minecraft_version: Option<String>,
    loader: Option<String>,
    loader_version: Option<String>,
    base_manifest_url: Option<String>,
    base_tag: Option<String>,
    fast_patch: Option<bool>,
) -> api::Result<PublishReleaseResult> {
    use std::process::Command;
    #[cfg(target_os = "windows")]
    use std::os::windows::process::CommandExt;

    if PUBLISH_RELEASE_PID.load(Ordering::SeqCst) > 0 {
        return Err(anyhow::anyhow!("Ya hay una subida en progreso. Cancélala o espera a que termine.").into());
    }

    let parts: Vec<&str> = archive_path
        .split('|')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    if parts.is_empty() {
        return Err(anyhow::anyhow!("No se especificó ninguna ruta de origen para empaquetar.").into());
    }

    for sub in &parts {
        let p = PathBuf::from(sub);
        if !p.exists() || (!p.is_file() && !p.is_dir()) {
            return Err(anyhow::anyhow!("El archivo o carpeta no existe en: {sub}").into());
        }
    }

    let mut script_candidates = Vec::new();
    for ancestor in PathBuf::from(env!("CARGO_MANIFEST_DIR")).ancestors() {
        script_candidates.push(ancestor.join("scripts").join("publish_release.cjs"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        for ancestor in cwd.ancestors() {
            script_candidates.push(ancestor.join("scripts").join("publish_release.cjs"));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        for ancestor in exe.ancestors() {
            script_candidates.push(ancestor.join("scripts").join("publish_release.cjs"));
        }
    }
    if let Ok(res_dir) = _app.path().resource_dir() {
        script_candidates.push(res_dir.join("scripts").join("publish_release.cjs"));
        script_candidates.push(res_dir.join("_up_").join("_up_").join("scripts").join("publish_release.cjs"));
        script_candidates.push(res_dir.join("publish_release.cjs"));
    }

    let script_path = script_candidates
        .into_iter()
        .find(|p| p.exists())
        .ok_or_else(|| anyhow::anyhow!("No se encontró el script de publicación publish_release.cjs en las rutas esperadas."))?;

    let mut cmd = Command::new("node");
    cmd.env("GITHUB_TOKEN", &token)
        .arg(&script_path)
        .arg("--server")
        .arg(&server_id)
        .arg("--version")
        .arg(&version)
        .arg("--zip")
        .arg(&archive_path)
        .arg("--title")
        .arg(&title);

    if let Some(desc) = description.filter(|d| !d.trim().is_empty()) {
        cmd.arg("--description").arg(desc);
    }
    if let Some(mc) = minecraft_version.filter(|m| !m.trim().is_empty()) {
        cmd.arg("--mc").arg(mc);
    }
    if let Some(ld) = loader.filter(|l| !l.trim().is_empty()) {
        cmd.arg("--loader").arg(ld);
    }
    if let Some(lv) = loader_version.filter(|v| !v.trim().is_empty()) {
        cmd.arg("--loader-version").arg(lv);
    }
    if let Some(bm) = base_manifest_url.filter(|v| !v.trim().is_empty()) {
        cmd.arg("--base-manifest").arg(bm);
    }
    if let Some(bt) = base_tag.filter(|v| !v.trim().is_empty()) {
        cmd.arg("--base-tag").arg(bt);
    }
    if fast_patch == Some(false) {
        cmd.arg("--no-fast-patch");
    }

    #[cfg(target_os = "windows")]
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW

    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    PUBLISH_RELEASE_CANCEL.store(false, Ordering::SeqCst);

    let app_for_events = _app.clone();
    let (status_code, stdout, stderr) = tokio::task::spawn_blocking(move || -> anyhow::Result<(std::process::ExitStatus, String, String)> {
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow::anyhow!("No se encontró Node.js ('node') en el sistema. Asegúrate de tener Node.js instalado en el PATH para publicar releases.")
            } else {
                anyhow::anyhow!("No se pudo iniciar el proceso node para publicar la release: {e}")
            }
        })?;

        PUBLISH_RELEASE_PID.store(child.id(), Ordering::SeqCst);

        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();

        let stderr_handle = std::thread::spawn(move || {
            let mut buf = String::new();
            if let Some(mut s) = stderr_pipe {
                let _ = s.read_to_string(&mut buf);
            }
            buf
        });

        let mut stdout_acc = String::new();
        if let Some(s) = stdout_pipe {
            let reader = BufReader::new(s);
            for line in reader.lines().map_while(Result::ok) {
                if PUBLISH_RELEASE_CANCEL.load(Ordering::SeqCst) {
                    let _ = child.kill();
                    break;
                }
                if let Some(raw_json) = line.strip_prefix("DR3SH_PROGRESS:") {
                    if let Ok(payload) = serde_json::from_str::<serde_json::Value>(raw_json) {
                        let _ = app_for_events.emit("dr3sh-release-progress", payload);
                    }
                } else {
                    stdout_acc.push_str(&line);
                    stdout_acc.push('\n');
                }
            }
        }

        let status = child.wait()?;
        PUBLISH_RELEASE_PID.store(0, Ordering::SeqCst);
        let stderr_acc = stderr_handle.join().unwrap_or_default();
        Ok((status, stdout_acc, stderr_acc))
    })
    .await
    .map_err(|e| anyhow::anyhow!("Error ejecutando proceso de publicación: {e}"))??;

    if PUBLISH_RELEASE_CANCEL.load(Ordering::SeqCst) {
        return Err(anyhow::anyhow!("Subida cancelada por el usuario.").into());
    }

    if !status_code.success() {
        let err_msg = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else if !stdout.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            format!("El proceso de publicación falló con código {:?}", status_code.code())
        };
        return Err(anyhow::anyhow!("{err_msg}").into());
    }

    let json_str = if let Some(start) = stdout.find("DR3SH_RESULT_JSON_START")
        && let Some(end) = stdout.rfind("DR3SH_RESULT_JSON_END")
    {
        &stdout[start + "DR3SH_RESULT_JSON_START".len()..end]
    } else if let Some(start) = stdout.rfind('{')
        && let Some(end) = stdout.rfind('}')
        && start < end
    {
        &stdout[start..=end]
    } else if let Some(start) = stdout.find('{')
        && let Some(end) = stdout.rfind('}')
        && start < end
    {
        &stdout[start..=end]
    } else {
        return Err(anyhow::anyhow!("Respuesta inesperada al publicar release: {stdout}").into());
    };

    let res: PublishReleaseResult = serde_json::from_str(json_str)
        .with_context(|| format!("Error procesando respuesta JSON de publicación. Contenido recibido: {json_str}"))?;
    Ok(res)
}

#[tauri::command]
fn dr3sh_get_file_size(path: String) -> Option<f64> {
    std::fs::metadata(&path)
        .ok()
        .filter(|m| m.is_file())
        .map(|m| {
            let bytes = m.len();
            if bytes == 0 {
                0.0
            } else {
                (((bytes as f64) / (1024.0 * 1024.0)) * 10.0).round().max(1.0) / 10.0
            }
        })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevSession {
    pub token: String,
    #[serde(default)]
    pub login: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
}

fn dev_session_path(app: &tauri::AppHandle) -> anyhow::Result<PathBuf> {
    let dir = app.path().app_data_dir()?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("dr3sh_dev_session.json"))
}

#[tauri::command]
fn dr3sh_load_dev_session(app: tauri::AppHandle) -> Option<DevSession> {
    let path = dev_session_path(&app).ok()?;
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<DevSession>(&raw).ok()
}

#[tauri::command]
fn dr3sh_save_dev_session(app: tauri::AppHandle, session: DevSession) -> api::Result<()> {
    let path = dev_session_path(&app)?;
    let json = serde_json::to_string_pretty(&session)?;
    std::fs::write(path, json).map_err(|e| anyhow::anyhow!("No se pudo guardar la sesión de desarrollador: {e}"))?;
    Ok(())
}

#[tauri::command]
fn dr3sh_clear_dev_session(app: tauri::AppHandle) -> api::Result<()> {
    if let Ok(path) = dev_session_path(&app)
        && path.exists()
    {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn resolve_windows_desktop_dir(app: &tauri::AppHandle) -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{
        FOLDERID_Desktop, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
    };

    if let Ok(pwstr) = unsafe { SHGetKnownFolderPath(&FOLDERID_Desktop, KF_FLAG_DEFAULT, None) } {
        let converted = unsafe { pwstr.to_string() };
        unsafe {
            CoTaskMemFree(Some(pwstr.0 as *const _));
        }
        if let Ok(s) = converted {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(PathBuf::from(trimmed));
            }
        }
    }

    if let Ok(p) = app.path().desktop_dir() {
        return Some(p);
    }

    std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join("Desktop"))
}

#[cfg(not(target_os = "windows"))]
fn resolve_windows_desktop_dir(app: &tauri::AppHandle) -> Option<PathBuf> {
    app.path().desktop_dir().ok()
}

fn build_ico_from_image_bytes(raw_bytes: &[u8]) -> Option<Vec<u8>> {
    use image::imageops::FilterType;
    use image::{DynamicImage, GenericImageView, ImageFormat, RgbaImage};
    use std::io::Cursor;

    let img = image::load_from_memory(raw_bytes).ok()?;
    let sizes: &[u32] = &[256, 64, 48, 32, 16];
    let mut frames: Vec<(u32, Vec<u8>)> = Vec::with_capacity(sizes.len());

    for &size in sizes {
        let resized = img.resize(size, size, FilterType::Lanczos3);
        let (rw, rh) = resized.dimensions();
        let mut canvas = RgbaImage::new(size, size);
        let x_off = ((size.saturating_sub(rw)) / 2) as i64;
        let y_off = ((size.saturating_sub(rh)) / 2) as i64;
        image::imageops::overlay(&mut canvas, &resized.to_rgba8(), x_off, y_off);

        let mut png_buf = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(canvas)
            .write_to(&mut png_buf, ImageFormat::Png)
            .ok()?;
        frames.push((size, png_buf.into_inner()));
    }

    let count = frames.len() as u16;
    let mut out = Vec::new();
    // ICONDIR header: reserved=0, type=1 (.ico), count
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());

    let mut data_offset = 6u32 + (frames.len() as u32) * 16;
    for (size, png_bytes) in &frames {
        let dim_byte = if *size >= 256 { 0u8 } else { *size as u8 };
        let byte_len = u32::try_from(png_bytes.len()).ok()?;
        out.push(dim_byte);
        out.push(dim_byte);
        out.push(0); // color count
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bit count
        out.extend_from_slice(&byte_len.to_le_bytes());
        out.extend_from_slice(&data_offset.to_le_bytes());
        data_offset = data_offset.checked_add(byte_len)?;
    }

    for (_, png_bytes) in frames {
        out.extend_from_slice(&png_bytes);
    }

    Some(out)
}

async fn load_shortcut_icon_bytes(
    instance_id: &str,
    server_id: Option<&str>,
    icon_url: Option<&str>,
) -> Option<Vec<u8>> {
    let candidate = icon_url.map(str::trim).filter(|s| !s.is_empty());

    if let Some(raw_url) = candidate {
        if raw_url.starts_with("http://") || raw_url.starts_with("https://") {
            if let Ok(resp) = dr3sh::download::shared_http_client()
                .get(raw_url)
                .timeout(std::time::Duration::from_secs(12))
                .send()
                .await
                && resp.status().is_success()
                && let Ok(bytes) = resp.bytes().await
                && !bytes.is_empty()
            {
                return Some(bytes.to_vec());
            }
        } else if let Some(b64_part) = raw_url.strip_prefix("data:")
            && let Some((_, encoded)) = b64_part.split_once(";base64,")
        {
            use base64::Engine;
            if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded.trim())
                && !decoded.is_empty()
            {
                return Some(decoded);
            }
        } else {
            let clean = raw_url.strip_prefix("file:///").unwrap_or(raw_url);
            let local = PathBuf::from(clean);
            if local.is_file()
                && let Ok(bytes) = std::fs::read(&local)
            {
                return Some(bytes);
            }
            if raw_url.starts_with('/') {
                let fallback_gh = format!(
                    "https://raw.githubusercontent.com/Dr3sh/Dr3sh.github.io/main{}",
                    raw_url
                );
                if let Ok(resp) = dr3sh::download::shared_http_client()
                    .get(&fallback_gh)
                    .timeout(std::time::Duration::from_secs(12))
                    .send()
                    .await
                    && resp.status().is_success()
                    && let Ok(bytes) = resp.bytes().await
                    && !bytes.is_empty()
                {
                    return Some(bytes.to_vec());
                }
            }
        }
    }

    if let Ok(Some(inst)) = theseus::instance::get(instance_id).await
        && let Some(ref icon_path) = inst.instance.icon_path
    {
        let p = PathBuf::from(icon_path);
        if p.is_file()
            && let Ok(bytes) = std::fs::read(&p)
        {
            return Some(bytes);
        }
    }

    if let Some(sid) = server_id {
        let st = dr3sh::state::get_server_state(sid);
        if let Some(ref dir) = st.instance_path {
            for rel in ["icon.png", "server-icon.png", ".dr3sh/icon.png"] {
                let p = PathBuf::from(dir).join(rel);
                if p.is_file()
                    && let Ok(bytes) = std::fs::read(&p)
                {
                    return Some(bytes);
                }
            }
        }
    }

    None
}

async fn resolve_instance_shortcut_ico(
    instance_id: &str,
    server_id: Option<&str>,
    icon_url: Option<&str>,
) -> Option<PathBuf> {
    let raw_bytes = load_shortcut_icon_bytes(instance_id, server_id, icon_url).await?;
    let key = server_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(instance_id)
        .to_string();

    tokio::task::spawn_blocking(move || {
        use sha2::{Digest, Sha256};

        let mut hasher = Sha256::new();
        hasher.update(&raw_bytes);
        let hash_hex: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
        let hash8 = &hash_hex[..8];

        let safe_key: String = key
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let safe_key = if safe_key.is_empty() {
            "instance".to_string()
        } else {
            safe_key
        };

        let ico_dir = dr3sh::logger::launcher_data_dir().join("icons");
        let _ = std::fs::create_dir_all(&ico_dir);
        let file_name = format!("{safe_key}-{hash8}.ico");
        let ico_path = ico_dir.join(&file_name);

        if ico_path.is_file() {
            return Some(ico_path);
        }

        let ico_bytes = build_ico_from_image_bytes(&raw_bytes)?;
        let prefix = format!("{safe_key}-");
        if let Ok(entries) = std::fs::read_dir(&ico_dir) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str()
                    && name.starts_with(&prefix)
                    && name.ends_with(".ico")
                    && name != file_name
                {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }

        let tmp_path = ico_dir.join(format!("{file_name}.tmp"));
        if std::fs::write(&tmp_path, &ico_bytes).is_ok()
            && std::fs::rename(&tmp_path, &ico_path).is_ok()
        {
            Some(ico_path)
        } else {
            let _ = std::fs::remove_file(&tmp_path);
            None
        }
    })
    .await
    .ok()
    .flatten()
}

#[tauri::command]
async fn dr3sh_create_desktop_shortcut(
    app: tauri::AppHandle,
    instance_name: String,
    instance_id: String,
    server_id: Option<String>,
    icon_url: Option<String>,
) -> api::Result<String> {
    let desktop = resolve_windows_desktop_dir(&app)
        .ok_or_else(|| anyhow::anyhow!("No se pudo obtener la ruta del escritorio"))?;
    let _ = std::fs::create_dir_all(&desktop);
    let safe_name: String = instance_name
        .chars()
        .filter(|c| !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .collect();
    let safe_name = if safe_name.trim().is_empty() {
        "Modpack".to_string()
    } else {
        safe_name.trim().to_string()
    };
    let out_path = desktop.join(format!("{safe_name}.lnk"));
    let icon_path = resolve_instance_shortcut_ico(
        &instance_id,
        server_id.as_deref(),
        icon_url.as_deref(),
    )
    .await;

    tracing::info!(
        instance_id = %instance_id,
        out_path = %out_path.display(),
        icon_path = ?icon_path,
        "Creating desktop shortcut"
    );
    api::shortcuts::create_instance_shortcut(
        instance_name,
        instance_id,
        out_path.clone(),
        None,
        None,
        icon_path,
    )
    .await
    .map_err(|e| {
        tracing::error!(out_path = %out_path.display(), "Desktop shortcut failed: {e:?}");
        e
    })?;
    Ok(out_path.to_string_lossy().to_string())
}

#[tauri::command]
async fn dr3sh_detect_github_token() -> Option<String> {
    tokio::task::spawn_blocking(|| {
        use std::process::Command;
        #[cfg(target_os = "windows")]
        use std::os::windows::process::CommandExt;

        // 1. Try GitHub CLI (`gh auth token`)
        let mut gh_cmd = Command::new("gh");
        gh_cmd.args(["auth", "token"]);
        #[cfg(target_os = "windows")]
        gh_cmd.creation_flags(0x08000000);
        if let Ok(out) = gh_cmd.output()
            && out.status.success()
        {
            let tok = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !tok.is_empty() {
                return Some(tok);
            }
        }

        // 2. Try Git Credential Manager (`git credential fill`)
        let mut git_cred = Command::new("git");
        git_cred
            .args(["credential", "fill"])
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(target_os = "windows")]
        git_cred.creation_flags(0x08000000);
        if let Ok(mut child) = git_cred.spawn() {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(b"protocol=https\nhost=github.com\n\n");
            }
            if let Ok(out) = child.wait_with_output()
                && out.status.success()
            {
                let text = String::from_utf8_lossy(&out.stdout);
                for line in text.lines() {
                    if let Some(pass) = line.strip_prefix("password=") {
                        let tok = pass.trim().to_string();
                        if !tok.is_empty() {
                            return Some(tok);
                        }
                    }
                }
            }
        }

        // 3. Environment variables
        for key in ["GITHUB_TOKEN", "GH_TOKEN"] {
            if let Ok(val) = std::env::var(key) {
                let tok = val.trim().to_string();
                if !tok.is_empty() {
                    return Some(tok);
                }
            }
        }

        // 4. Git remote origin embedded token if configured locally
        let mut git_remote = Command::new("git");
        git_remote.args(["config", "--get", "remote.origin.url"]);
        #[cfg(target_os = "windows")]
        git_remote.creation_flags(0x08000000);
        if let Ok(out) = git_remote.output()
            && out.status.success()
        {
            let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
            for prefix in ["https://x-access-token:", "https://oauth2:"] {
                if let Some(rest) = url.strip_prefix(prefix)
                    && let Some((tok, _)) = rest.split_once('@')
                {
                    let clean = tok.trim().to_string();
                    if !clean.is_empty() {
                        return Some(clean);
                    }
                }
            }
        }

        None
    })
    .await
    .ok()
    .flatten()
}

mod api;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(feature = "updater")]
mod updater_impl;
#[cfg(not(feature = "updater"))]
mod updater_impl_noop;

// Should be called in launcher initialization
#[tracing::instrument(skip_all)]
#[tauri::command]
async fn initialize_state(
    app: tauri::AppHandle,
    events: tauri::ipc::Channel<tauri::ipc::InvokeResponseBody>,
) -> api::Result<()> {
    cleanup_dangling_profile_junctions();

    tracing::info!("Initializing app event state...");
    theseus::EventState::init(app.clone(), events).await?;

    tracing::info!("Initializing app state...");
    State::init(app.config().identifier.clone()).await?;

    let state = State::get().await?;
    if let Ok(instances) = theseus::instance::list().await {
        for inst in instances {
            let p = state.directories.instances_dir().join(&inst.instance.path);
            if !p.exists() {
                if dr3sh::preflight::is_reparse_point(&p) {
                    let _ = dr3sh::preflight::remove_junction_link_only(&p);
                }
                let _ = std::fs::create_dir_all(&p);
            }
        }
    }

    dr3sh::recover_all_on_startup().await;

    app.asset_protocol_scope()
        .allow_directory(state.directories.caches_dir(), true)?;
    app.asset_protocol_scope()
        .allow_directory(state.directories.caches_dir().join("icons"), true)?;
    app.asset_protocol_scope()
        .allow_directory(state.directories.icon_dir(), true)?;
    app.fs_scope()
        .allow_directory(state.directories.instances_dir(), true)?;

    Ok(())
}

// Should be call once Vue has mounted the app
#[tracing::instrument(skip_all)]
#[tauri::command]
fn show_window(app: tauri::AppHandle) {
    let win = app.get_window("main").unwrap();
    if let Err(e) = win.show() {
        DialogBuilder::message()
            .set_level(MessageLevel::Error)
            .set_title("Initialization error")
            .set_text(format!(
                "Cannot display application window due to an error:\n{e}"
            ))
            .alert()
            .show()
            .unwrap();
        panic!("cannot display application window")
    } else {
        let _ = win.set_focus();
    }
}

#[tauri::command]
fn is_dev() -> bool {
    cfg!(debug_assertions)
}

#[tauri::command]
fn are_updates_enabled() -> bool {
    cfg!(feature = "updater")
        && env::var("MODRINTH_EXTERNAL_UPDATE_PROVIDER").is_err()
}

#[cfg(feature = "updater")]
pub use updater_impl::*;

#[cfg(not(feature = "updater"))]
pub use updater_impl_noop::*;

// Toggles decorations
#[tauri::command]
async fn toggle_decorations(b: bool, window: tauri::Window) -> api::Result<()> {
    window
        .set_decorations(b)
        .context("failed to toggle decorations")?;
    Ok(())
}

#[tauri::command]
fn restart_app(app: tauri::AppHandle) {
    app.restart();
}

#[derive(Default)]
struct RestartAfterPendingUpdate(AtomicBool);

#[tauri::command]
async fn set_restart_after_pending_update(
    should_restart: bool,
    restart: tauri::State<'_, RestartAfterPendingUpdate>,
) -> api::Result<()> {
    restart.0.store(should_restart, Ordering::Relaxed);
    Ok(())
}

// if Tauri app is called with arguments, then those arguments will be treated as commands
// ie: deep links or filepaths for .mrpacks
fn main() {
    dr3sh::logger::install_panic_hook();
    dr3sh::logger::prune_logs();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to create app async runtime");
    tauri::async_runtime::set(runtime.handle().clone());

    #[cfg(feature = "export-app-events")]
    theseus::export_app_event_bindings(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../app-frontend/src/generated/app-events"),
    )
    .expect("failed to export app event TypeScript bindings");

    /*
        tracing is set basd on the environment variable RUST_LOG=xxx, depending on the amount of logs to show
            ERROR > WARN > INFO > DEBUG > TRACE
        eg. RUST_LOG=info will show info, warn, and error logs
            RUST_LOG="theseus=trace" will show *all* messages but from theseus only (and not dependencies using similar crates)
            RUST_LOG="theseus=trace" will show *all* messages but from theseus only (and not dependencies using similar crates)

        Error messages returned to Tauri will display as traced error logs if they return an error.
        This will also include an attached span trace if the error is from a tracing error, and the level is set to info, debug, or trace

        on unix:
            RUST_LOG="theseus=trace" {run command}

    */

    let tauri_context = tauri::generate_context!();

    let _log_guard = theseus::start_logger(&tauri_context.config().identifier);

    tracing::info!("Initialized tracing subscriber. Loading Dr3sh App!");

    let mut builder = tauri::Builder::default();

    #[cfg(target_os = "macos")]
    {
        builder = builder
            .menu(macos::menu::create)
            .on_menu_event(macos::menu::handle_event);
    }

    #[cfg(feature = "updater")]
    {
        use tauri_plugin_http::reqwest::header::{HeaderValue, USER_AGENT};
        use theseus::launcher_user_agent;
        builder = builder.plugin(
            tauri_plugin_updater::Builder::new()
                .header(
                    USER_AGENT,
                    HeaderValue::from_str(&launcher_user_agent()).unwrap(),
                )
                .unwrap()
                .build(),
        );
    }

    builder = builder
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if let Some(payload) = args.get(1) {
                tracing::info!("Handling command-line deep link");
                let payload = payload.clone();
                tauri::async_runtime::spawn(api::utils::handle_command(
                    payload,
                ));
            }

            if let Some(win) = app.get_window("main") {
                let _ = win.set_focus();
            }
        }))
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_opener::init());

    #[cfg(target_os = "windows")]
    {
        use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
        use windows::Win32::UI::WindowsAndMessaging::{
            SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
            SWP_NOZORDER, SetWindowPos,
        };

        let _ = unsafe {
            SetCurrentProcessExplicitAppUserModelID(windows_core::w!(
                "com.dr3sh.launcher"
            ))
        };

        // Refresh the hidden window's frame before window-state measures its client area.
        builder = builder.plugin(
            tauri::plugin::Builder::<_, ()>::new("window-frame")
                .on_window_ready(|window| {
                    if window.label() != "main" {
                        return;
                    }

                    let hwnd = match window.hwnd() {
                        Ok(hwnd) => hwnd,
                        Err(e) => {
                            tracing::warn!("Failed to get window handle: {e}");
                            return;
                        }
                    };

                    if let Err(e) = unsafe {
                        SetWindowPos(
                            hwnd,
                            None,
                            0,
                            0,
                            0,
                            0,
                            SWP_FRAMECHANGED
                                | SWP_NOACTIVATE
                                | SWP_NOMOVE
                                | SWP_NOSIZE
                                | SWP_NOZORDER,
                        )
                    } {
                        tracing::warn!("Failed to refresh window frame: {e}");
                    }
                })
                .build(),
        );
    }

    let window_state_flags = tauri_plugin_window_state::StateFlags::POSITION
        | tauri_plugin_window_state::StateFlags::SIZE
        | tauri_plugin_window_state::StateFlags::MAXIMIZED;
    #[cfg(target_os = "windows")]
    let window_state_flags =
        window_state_flags | tauri_plugin_window_state::StateFlags::DECORATIONS;

    builder = builder
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_filename("app-window-state.json")
                .with_denylist(&["signin"])
                // Restoring VISIBLE would override the hidden startup window.
                .with_state_flags(window_state_flags)
                .build(),
        )
        .setup(|app| {
            #[cfg(target_os = "macos")]
            {
                let payload = macos::deep_link::get_or_init_payload(app);

                let mtx_copy = payload.payload;
                app.listen("deep-link://new-url", move |url| {
                    let mtx_copy_copy = mtx_copy.clone();
                    let request = url.payload().to_owned();

                    let actual_request =
                        serde_json::from_str::<Vec<String>>(&request)
                            .ok()
                            .map(|mut x| x.remove(0))
                            .unwrap_or(request);

                    tauri::async_runtime::spawn(async move {
                        tracing::info!("Handling macOS deep link");

                        let mut payload = mtx_copy_copy.lock().await;
                        if payload.is_none() {
                            *payload = Some(actual_request.clone());
                        }

                        let _ =
                            api::utils::handle_command(actual_request).await;
                    });
                });
            };

            #[cfg(not(target_os = "macos"))]
            app.listen("deep-link://new-url", |url| {
                let payload = url.payload().to_owned();
                tracing::info!("Handling deep link");
                tauri::async_runtime::spawn(api::utils::handle_command(
                    payload,
                ));
            });

            #[cfg(not(target_os = "linux"))]
            if let Some(window) = app.get_window("main")
                && let Err(e) = window.set_shadow(true)
            {
                tracing::warn!("Failed to set window shadow: {e}");
            }

            Ok(())
        });

    builder = builder
        .plugin(api::auth::init())
        .plugin(api::mr_auth::init())
        .plugin(api::onboarding_checklist::init())
        .plugin(api::import::init())
        .plugin(api::install::init())
        .plugin(api::instance::init())
        .plugin(api::logs::init())
        .plugin(api::jre::init())
        .plugin(api::metadata::init())
        .plugin(api::minecraft_skins::init())
        .plugin(api::process::init())
        .plugin(api::reports::init())
        .plugin(api::settings::init())
        .plugin(api::shortcuts::init())
        .plugin(api::tags::init())
        .plugin(api::users::init())
        .plugin(api::utils::init())
        .plugin(api::cache::init())
        .plugin(api::files::init())
        .plugin(api::ads::init())
        .plugin(api::friends::init())
        .plugin(api::worlds::init())
        .manage(PendingUpdateData::default())
        .manage(RestartAfterPendingUpdate::default())
        .invoke_handler(tauri::generate_handler![
            initialize_state,
            is_dev,
            are_updates_enabled,
            get_update_size,
            enqueue_update_for_installation,
            remove_enqueued_update,
            set_restart_after_pending_update,
            toggle_decorations,
            show_window,
            restart_app,
            dr3sh_install_or_update,
            dr3sh_check_launcher_update,
            dr3sh_cancel,
            dr3sh_repair,
            dr3sh_get_state,
            dr3sh_migrate_local_state,
            dr3sh_ack_migration_notice,
            dr3sh_get_diagnostics,
            dr3sh_open_logs_folder,
            dr3sh_prepare_launch,
            dr3sh_download_modpack,
            dr3sh_cancel_download,
            dr3sh_wait_for_instance_ready,
            dr3sh_has_extracted_mods,
            dr3sh_link_existing_instance,
            dr3sh_cleanup_deleted_instance,
            dr3sh_publish_release,
            dr3sh_cancel_publish_release,
            dr3sh_get_file_size,
            dr3sh_load_dev_session,
            dr3sh_save_dev_session,
            dr3sh_clear_dev_session,
            dr3sh_detect_github_token,
            dr3sh_create_desktop_shortcut,
            dr3sh_check_instance_health,
            dr3sh_get_network_time,
            dr3sh_get_preload_state,
            dr3sh_lock_preload,
            dr3sh_activate_preload,
        ]);

    tracing::info!("Initializing app...");
    let app = builder.build(tauri_context);

    static PENDING_EXIT_AFTER_LAUNCH: AtomicBool = AtomicBool::new(false);

    fn trigger_deferred_exit_after_launch(app: &tauri::AppHandle) {
        if !PENDING_EXIT_AFTER_LAUNCH.swap(true, Ordering::SeqCst) {
            let app_handle = app.clone();
            tauri::async_runtime::spawn(async move {
                for _ in 0..300 {
                    if theseus::active_launch_count() == 0 {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                }
                app_handle.exit(0);
            });
        }
    }

    match app {
        Ok(app) => {
            app.run(|app, event| {
                if let tauri::RunEvent::WindowEvent {
                    label,
                    event: tauri::WindowEvent::CloseRequested { api, .. },
                    ..
                } = &event
                    && label == "main"
                    && theseus::active_launch_count() > 0
                {
                    api.prevent_close();
                    if let Some(window) = app.get_webview_window(label) {
                        let _ = window.hide();
                    }
                    trigger_deferred_exit_after_launch(app);
                    return;
                }

                if let tauri::RunEvent::ExitRequested { api, .. } = &event {
                    if theseus::active_launch_count() > 0 {
                        api.prevent_exit();
                        trigger_deferred_exit_after_launch(app);
                        return;
                    }
                    if let Err(error) = tauri::async_runtime::block_on(
                        theseus::minecraft_skins::flush_pending_skin_change(),
                    ) {
                        tracing::warn!(
                            "Failed to flush pending Minecraft skin change before exit: {error}"
                        );
                    }
                }

                #[cfg(feature = "updater")]
                if matches!(&event, tauri::RunEvent::Exit)
                    && tauri::async_runtime::block_on(dr3sh::can_install_updater_safely())
                {
                    let update_data = app.state::<PendingUpdateData>().inner();
                    let should_restart = app
                        .state::<RestartAfterPendingUpdate>()
                        .0
                        .load(Ordering::Relaxed);
                    if let Some((update, data)) = &*update_data.0.lock().unwrap()
                    {
                        fn set_changelog_toast(version: Option<String>) {
                            let toast_result: theseus::Result<()> = tauri::async_runtime::block_on(async move {
                                let mut settings = settings::get().await?;
                                settings.pending_update_toast_for_version = version;
                                settings::set(settings).await?;
                                Ok(())
                            });
                            if let Err(e) = toast_result {
                                tracing::warn!(
                                    "Failed to set pending_update_toast: {e}"
                                )
                            }
                        }

						let current_version = &app.package_info().version;
						let mut version_parts = update.version.split('.');
						let major = version_parts.next().and_then(|part| part.parse::<u64>().ok());
						let minor = version_parts.next().and_then(|part| part.parse::<u64>().ok());
						let is_major_update = major.zip(minor).is_some_and(|version| {
							version > (current_version.major, current_version.minor)
						});
						set_changelog_toast(is_major_update.then(|| update.version.clone()));
                        let update = if should_restart {
                            (**update).clone()
                        } else {
                            (**update).clone().restart_after_install(false)
                        };
                        match update.install(data) {
                            Ok(()) => {
                                if should_restart {
                                    tracing::info!(
                                        "Pending update installed successfully (version {}); restarting because user requested reload",
                                        update.version
                                    );
                                    app.restart();
                                } else {
                                    tracing::info!(
                                        "Pending update installed successfully (version {}); exiting without relaunch (user did not request reload)",
                                        update.version
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::error!(
                                    "Pending update install failed (version {}): {e}",
                                    update.version
                                );
                                set_changelog_toast(None);

                                DialogBuilder::message()
                                    .set_level(MessageLevel::Error)
                                    .set_title("Update error")
                                    .set_text(format!("Failed to install update due to an error:\n{e}"))
                                    .alert()
                                    .show()
                                    .unwrap();
                            }
                        }
                    }
                }
                #[cfg(target_os = "macos")]
                if let tauri::RunEvent::Opened { urls } = event {
                    tracing::info!("Handling webview open {urls:?}");

                    let file = urls
                        .into_iter()
                        .find_map(|url| url.to_file_path().ok());

                    if let Some(file) = file {
                        let payload =
                            macos::deep_link::get_or_init_payload(app);

                        let mtx_copy = payload.payload;
                        let request = file.to_string_lossy().to_string();
                        tauri::async_runtime::spawn(async move {
                            let mut payload = mtx_copy.lock().await;
                            if payload.is_none() {
                                *payload = Some(request.clone());
                            }

                            let _ = api::utils::handle_command(request).await;
                        });
                    }
                }
            });
        }
        Err(e) => {
            tracing::error!("Error while running tauri application: {:?}", e);

            #[cfg(target_os = "windows")]
            {
                // tauri doesn't expose runtime errors, so matching a string representation seems like the only solution
                if format!("{e:?}").contains(
                    "Runtime(CreateWebview(WebView2Error(WindowsError",
                ) {
                    DialogBuilder::message()
                        .set_level(MessageLevel::Error)
                        .set_title("Initialization error")
                        .set_text("Your Microsoft Edge WebView2 installation is corrupt.\n\nMicrosoft Edge WebView2 is required to run Dr3sh App.")
                        .alert()
                        .show()
                        .unwrap();

                    panic!("webview2 initialization failed")
                }
            }

            DialogBuilder::message()
                .set_level(MessageLevel::Error)
                .set_title("Initialization error")
                .set_text(format!(
                    "Cannot initialize application due to an error:\n{e:?}"
                ))
                .alert()
                .show()
                .unwrap();

            panic!("{1}: {:?}", e, "error while running tauri application")
        }
    }
}
