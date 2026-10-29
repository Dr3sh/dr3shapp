#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]
#![recursion_limit = "256"]

use anyhow::Context;
use native_dialog::{DialogBuilder, MessageLevel};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::env;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tauri::{Emitter, Listener, Manager};
use tauri_plugin_fs::FsExt;
use theseus::prelude::*;

static DR3SH_CANCEL_DOWNLOAD: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Dr3shProgressPayload {
    server_id: String,
    phase: String,
    percent: u32,
    phase_text: String,
    downloaded_bytes: u64,
    total_bytes: u64,
    downloaded_formatted: String,
    total_formatted: String,
    speed_formatted: String,
}

#[derive(Deserialize)]
struct ManifestRemoveDoc {
    #[serde(default)]
    remove: Vec<String>,
}

fn format_bytes_rust(bytes: u64) -> String {
    if bytes == 0 {
        return "0 MB".to_string();
    }
    let gb = 1024.0 * 1024.0 * 1024.0;
    let mb = 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= gb {
        format!("{:.2} GB", b / gb)
    } else if b >= mb {
        format!("{:.1} MB", b / mb)
    } else {
        format!("{} KB", (b / 1024.0).round() as u64)
    }
}

#[tauri::command]
fn dr3sh_cancel_download() {
    DR3SH_CANCEL_DOWNLOAD.store(true, Ordering::SeqCst);
}

#[tauri::command]
async fn dr3sh_wait_for_instance_ready(instance_id: String) -> api::Result<()> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(180) {
        let jobs = theseus::install::list_jobs(false).await?;
        let active_for_instance = jobs.iter().any(|job| {
            job.instance_id.as_deref() == Some(instance_id.as_str())
        });
        if !active_for_instance {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) {
    if !src.exists() {
        return;
    }
    let _ = std::fs::create_dir_all(dst);
    let Ok(entries) = std::fs::read_dir(src) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let dest_path = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &dest_path);
        } else if !dest_path.exists() {
            let _ = std::fs::copy(&path, &dest_path);
        }
    }
}

fn remove_dir_or_junction(path: &Path) {
    if std::fs::symlink_metadata(path).is_ok() {
        if std::fs::remove_dir(path).is_ok() {
            return;
        }
        let _ = std::fs::remove_file(path);
        if std::fs::symlink_metadata(path).is_ok() {
            let _ = std::fs::remove_dir_all(path);
            let _ = std::fs::remove_dir(path);
        }
    }
}

#[cfg(target_os = "windows")]
fn create_windows_junction(instance_path: &Path, custom_target: &Path) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    if custom_target == instance_path {
        return;
    }
    if instance_path.exists() {
        copy_dir_recursive(instance_path, custom_target);
    }
    remove_dir_or_junction(instance_path);
    if let Some(parent) = instance_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let _ = std::process::Command::new("cmd")
        .creation_flags(CREATE_NO_WINDOW)
        .env("DR3SH_LINK", instance_path)
        .env("DR3SH_TARGET", custom_target)
        .raw_arg("/C mklink /J \"%DR3SH_LINK%\" \"%DR3SH_TARGET%\"")
        .output();

    let target_ok = std::fs::read_link(instance_path)
        .ok()
        .is_some_and(|linked| {
            linked.file_name() == custom_target.file_name() && instance_path.exists()
        });

    if !target_ok {
        remove_dir_or_junction(instance_path);
        let _ = std::process::Command::new("powershell")
            .creation_flags(CREATE_NO_WINDOW)
            .env("DR3SH_LINK", instance_path)
            .env("DR3SH_TARGET", custom_target)
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "New-Item -ItemType Junction -Path $env:DR3SH_LINK -Target $env:DR3SH_TARGET -Force | Out-Null",
            ])
            .output();
    }

    // If the junction still failed, remove any broken reparse point and create a normal folder
    if !instance_path.exists() {
        remove_dir_or_junction(instance_path);
        let _ = std::fs::create_dir_all(instance_path);
    }
}

fn cleanup_dangling_profile_junctions() {
    if let Ok(appdata) = std::env::var("APPDATA") {
        let profiles_dir = PathBuf::from(appdata)
            .join("com.dr3sh.launcher")
            .join("profiles");
        if let Ok(entries) = std::fs::read_dir(&profiles_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if !p.exists() && std::fs::symlink_metadata(&p).is_ok() {
                    let _ = std::fs::remove_dir(&p)
                        .or_else(|_| std::fs::remove_file(&p));
                }
            }
        }
    }
}

#[tauri::command]
fn dr3sh_cleanup_deleted_instance(custom_folder: Option<String>) {
    cleanup_dangling_profile_junctions();
    if let Some(folder) = custom_folder
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        let p = PathBuf::from(folder);
        if p.exists() && p.join("mods").exists() {
            let _ = std::fs::remove_dir_all(&p);
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

#[tauri::command]
async fn dr3sh_link_existing_instance(
    app: tauri::AppHandle,
    instance_id: String,
    instance_dir: String,
    existing_folder: String,
) -> api::Result<bool> {
    let existing_path = PathBuf::from(existing_folder.trim());
    if !existing_path.exists() {
        return Ok(false);
    }
    let _ = dr3sh_wait_for_instance_ready(instance_id).await;
    let instance_path = PathBuf::from(&instance_dir);

    #[cfg(target_os = "windows")]
    create_windows_junction(&instance_path, &existing_path);

    let _ = app.fs_scope().allow_directory(&existing_path, true);
    Ok(true)
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
) -> api::Result<PublishReleaseResult> {
    use std::process::Command;
    #[cfg(target_os = "windows")]
    use std::os::windows::process::CommandExt;

    let p = PathBuf::from(&archive_path);
    if !p.exists() || !p.is_file() {
        return Err(anyhow::anyhow!("El archivo ZIP no existe en: {archive_path}").into());
    }

    let script_candidates = [
        PathBuf::from("E:\\launcehr\\Dr3sh app\\scripts\\publish_release.cjs"),
        PathBuf::from("E:\\launcehr\\scripts\\publish_release.cjs"),
    ];
    let script_path = script_candidates
        .into_iter()
        .find(|p| p.exists())
        .ok_or_else(|| anyhow::anyhow!("No se encontró el script de publicación publish_release.cjs"))?;

    let mut cmd = Command::new("node");
    cmd.arg(&script_path)
        .arg("--server")
        .arg(&server_id)
        .arg("--version")
        .arg(&version)
        .arg("--zip")
        .arg(&archive_path)
        .arg("--token")
        .arg(&token)
        .arg("--title")
        .arg(&title);

    if let Some(desc) = description.filter(|d| !d.trim().is_empty()) {
        cmd.arg("--description").arg(desc);
    }

    #[cfg(target_os = "windows")]
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW

    let output = tokio::task::spawn_blocking(move || cmd.output())
        .await
        .map_err(|e| anyhow::anyhow!("Error ejecutando proceso de publicación: {e}"))?
        .context("No se pudo iniciar el proceso node para publicar la release")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        let err_msg = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else if !stdout.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            format!("El proceso de publicación falló con código {:?}", output.status.code())
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
        .map(|m| ((m.len() as f64 / (1024.0 * 1024.0)) * 10.0).round() / 10.0)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn dr3sh_download_modpack(
    app: tauri::AppHandle,
    server_id: String,
    server_title: String,
    download_url: String,
    manifest_url: Option<String>,
    instance_dir: String,
    custom_dir: Option<String>,
    expected_size_mb: Option<f64>,
    instance_id: Option<String>,
) -> api::Result<String> {
    DR3SH_CANCEL_DOWNLOAD.store(false, Ordering::SeqCst);

    // Wait for any active Theseus background install job on this instance BEFORE touching its directory
    // so Theseus never fails with os error 2 or rolls back the instance database record.
    if let Some(ref inst_id) = instance_id
        && !inst_id.trim().is_empty()
    {
        let _ = dr3sh_wait_for_instance_ready(inst_id.clone()).await;
    }

    let instance_path = PathBuf::from(&instance_dir);
    let final_target_path = if let Some(custom) = custom_dir
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        let sanitized_title: String = server_title
            .chars()
            .map(|c| {
                if ['/', '\\', '?', '*', ':', '\'', '\"', '|', '<', '>', '!']
                    .contains(&c)
                {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        let custom_base = PathBuf::from(custom);
        let custom_target = if custom_base
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case(&sanitized_title))
        {
            custom_base
        } else {
            custom_base.join(&sanitized_title)
        };
        std::fs::create_dir_all(&custom_target)
            .context("No se pudo crear la carpeta personalizada")?;

        #[cfg(target_os = "windows")]
        create_windows_junction(&instance_path, &custom_target);
        custom_target
    } else {
        if !instance_path.exists() && std::fs::symlink_metadata(&instance_path).is_ok() {
            remove_dir_or_junction(&instance_path);
        }
        std::fs::create_dir_all(&instance_path)
            .context("No se pudo crear el directorio de la instancia")?;
        instance_path.clone()
    };

    let _ = app
        .fs_scope()
        .allow_directory(&final_target_path, true);

    let client = tauri_plugin_http::reqwest::Client::builder()
        .user_agent("Dr3sh-Launcher/0.2.16")
        .tcp_nodelay(true)
        .connect_timeout(Duration::from_secs(20))
        .build()
        .context("Error creando cliente HTTP")?;

    // 1. Optional delta manifest remove list
    if let Some(m_url) = manifest_url
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| s.starts_with("http://") || s.starts_with("https://"))
        && let Ok(resp) = client.get(m_url).send().await
        && resp.status().is_success()
        && let Ok(bytes) = resp.bytes().await
        && let Ok(doc) = serde_json::from_slice::<ManifestRemoveDoc>(&bytes)
    {
        for rel in doc.remove {
            let clean = rel.replace('\\', "/");
            let clean = clean.trim_start_matches('/');
            if clean.starts_with("mods/") && !clean.contains("..") {
                let target_remove = final_target_path.join(clean);
                if target_remove.exists() {
                    let _ = std::fs::remove_file(target_remove);
                }
            }
        }
    }

    // 2. High-speed streamed download directly to temporary file on target drive
    let temp_zip_path = final_target_path.join(".dr3sh_modpack_temp.zip");
    let mut res = client
        .get(&download_url)
        .send()
        .await
        .context("No se pudo conectar al servidor de descargas")?;

    if !res.status().is_success() {
        return Err(anyhow::anyhow!(
            "Error al descargar el modpack: HTTP {}",
            res.status()
        )
        .into());
    }

    let fallback_total =
        ((expected_size_mb.unwrap_or(220.0)) * 1024.0 * 1024.0) as u64;
    let total_bytes = res.content_length().unwrap_or(fallback_total).max(1);
    let total_formatted = format_bytes_rust(total_bytes);

    let _ = app.emit(
        "dr3sh-download-progress",
        Dr3shProgressPayload {
            server_id: server_id.clone(),
            phase: "downloading".to_string(),
            percent: 2,
            phase_text: format!("Descargando {server_title}..."),
            downloaded_bytes: 0,
            total_bytes,
            downloaded_formatted: "0 MB".to_string(),
            total_formatted: total_formatted.clone(),
            speed_formatted: "Conectando...".to_string(),
        },
    );

    {
        let file = std::fs::File::create(&temp_zip_path)
            .context("No se pudo crear el archivo temporal de descarga")?;
        let mut writer = BufWriter::with_capacity(1024 * 1024, file);

        let mut received_bytes: u64 = 0;
        let start_time = Instant::now();
        let mut last_emit = Instant::now();
        let mut last_bytes: u64 = 0;
        let mut smoothed_speed: f64 = 0.0;

        while let Some(chunk) = res
            .chunk()
            .await
            .context("Error leyendo flujo de descarga")?
        {
            if DR3SH_CANCEL_DOWNLOAD.load(Ordering::Relaxed) {
                drop(writer);
                let _ = std::fs::remove_file(&temp_zip_path);
                return Err(
                    anyhow::anyhow!("Descarga cancelada por el usuario").into(),
                );
            }

            writer
                .write_all(&chunk)
                .context("Error escribiendo datos en disco")?;
            received_bytes += chunk.len() as u64;

            let elapsed_since_emit = last_emit.elapsed().as_secs_f64();
            if elapsed_since_emit >= 0.08 || received_bytes >= total_bytes {
                let delta_bytes = received_bytes.saturating_sub(last_bytes);
                let instant_speed =
                    (delta_bytes as f64) / elapsed_since_emit.max(0.001);
                smoothed_speed = if smoothed_speed <= 0.0 {
                    instant_speed
                } else {
                    smoothed_speed * 0.65 + instant_speed * 0.35
                };
                let avg_speed = if smoothed_speed > 0.0 {
                    smoothed_speed as u64
                } else {
                    let total_secs = start_time.elapsed().as_secs_f64().max(0.01);
                    (received_bytes as f64 / total_secs) as u64
                };

                let ratio =
                    (received_bytes as f64 / total_bytes as f64).clamp(0.0, 1.0);
                let pct = (2.0 + ratio * 80.0).round() as u32;
                let dl_fmt = format_bytes_rust(received_bytes);
                let spd_fmt = format!("{}/s", format_bytes_rust(avg_speed));

                let _ = app.emit(
                    "dr3sh-download-progress",
                    Dr3shProgressPayload {
                        server_id: server_id.clone(),
                        phase: "downloading".to_string(),
                        percent: pct.min(82),
                        phase_text: format!("Descargando {server_title}..."),
                        downloaded_bytes: received_bytes,
                        total_bytes,
                        downloaded_formatted: dl_fmt,
                        total_formatted: total_formatted.clone(),
                        speed_formatted: spd_fmt,
                    },
                );

                last_emit = Instant::now();
                last_bytes = received_bytes;
            }
        }

        writer
            .flush()
            .context("Error al volcar el archivo descargado a disco")?;
    }

    // 3. Fast native Rust ZIP extraction on blocking thread
    let app_clone = app.clone();
    let server_id_clone = server_id.clone();
    let temp_zip_for_extract = temp_zip_path.clone();
    let extract_target_dir = final_target_path.clone();
    let total_fmt_clone = total_formatted.clone();

    let extract_res = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let zip_file = std::fs::File::open(&temp_zip_for_extract)
            .context("No se pudo abrir el paquete ZIP descargado")?;
        let mut archive = zip::ZipArchive::new(zip_file)
            .context("El archivo descargado no es un ZIP válido")?;

        let total_entries = archive.len();
        if total_entries == 0 {
            return Ok(());
        }

        // Detect single top-level wrapper folder
        let mut common_prefix: Option<String> = None;
        for i in 0..total_entries {
            if let Ok(entry) = archive.by_index_raw(i) {
                let name = entry.name()?.replace('\\', "/");
                let trimmed = name.trim_start_matches('/');
                if trimmed.is_empty() {
                    continue;
                }
                if let Some((first_seg, _)) = trimmed.split_once('/') {
                    let candidate = format!("{first_seg}/");
                    match &common_prefix {
                        None => common_prefix = Some(candidate),
                        Some(existing) if existing != &candidate => {
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

        // Do not strip standard Minecraft folders if the whole zip happens to be just "mods/"
        if let Some(ref prefix) = common_prefix {
            let folder = prefix.trim_end_matches('/').to_ascii_lowercase();
            let standard_mc_dirs = [
                "mods",
                "config",
                "resourcepacks",
                "shaderpacks",
                "kubejs",
                "defaultconfigs",
                "saves",
                "datapacks",
            ];
            if standard_mc_dirs.contains(&folder.as_str()) {
                common_prefix = None;
            }
        }

        let protected_dirs: HashSet<&'static str> = HashSet::from([
            "saves",
            "screenshots",
            "logs",
            "crash-reports",
            "backups",
            "xaerowaypoints",
            "xaeroworldmap",
            "schematics",
            "replay_recordings",
            "replay_videos",
        ]);

        let seed_files: HashSet<&'static str> = HashSet::from([
            "options.txt",
            "optionsof.txt",
            "optionsshaders.txt",
            "servers.dat",
            "servers.dat_old",
        ]);

        let mut last_emit = Instant::now();

        for i in 0..total_entries {
            if DR3SH_CANCEL_DOWNLOAD.load(Ordering::Relaxed) {
                anyhow::bail!("Instalación cancelada por el usuario");
            }

            let mut entry = archive.by_index(i)?;
            if entry.is_dir() {
                continue;
            }

            let raw_name = entry.name()?.replace('\\', "/");
            let mut rel = raw_name.trim_start_matches('/');
            if let Some(ref prefix) = common_prefix
                && let Some(stripped) = rel.strip_prefix(prefix.as_str())
            {
                rel = stripped.trim_start_matches('/');
            }

            if rel.is_empty() || rel.contains("..") {
                continue;
            }

            let top_dir = rel
                .split('/')
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if protected_dirs.contains(top_dir.as_str()) {
                continue;
            }

            let file_name = Path::new(rel)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let rel_lower = rel.to_ascii_lowercase();

            let dest_path = extract_target_dir.join(rel);

            if (seed_files.contains(file_name.as_str())
                || seed_files.contains(rel_lower.as_str()))
                && dest_path.exists()
            {
                continue;
            }

            if let Some(parent) = dest_path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            let out_file = std::fs::File::create(&dest_path)?;
            let mut out_writer =
                BufWriter::with_capacity(256 * 1024, out_file);
            std::io::copy(&mut entry, &mut out_writer)?;
            out_writer.flush()?;

            if last_emit.elapsed() >= Duration::from_millis(70)
                || i + 1 == total_entries
            {
                let ratio = (i + 1) as f64 / (total_entries.max(1) as f64);
                let pct = (83.0 + ratio * 16.0).round() as u32;
                let _ = app_clone.emit(
                    "dr3sh-download-progress",
                    Dr3shProgressPayload {
                        server_id: server_id_clone.clone(),
                        phase: "extracting".to_string(),
                        percent: pct.min(99),
                        phase_text: "Extrayendo archivos...".to_string(),
                        downloaded_bytes: total_bytes,
                        total_bytes,
                        downloaded_formatted: total_fmt_clone.clone(),
                        total_formatted: total_fmt_clone.clone(),
                        speed_formatted: "Instalando...".to_string(),
                    },
                );
                last_emit = Instant::now();
            }
        }

        Ok(())
    })
    .await
    .context("Error en el hilo de extracción")?;

    let _ = std::fs::remove_file(&temp_zip_path);
    extract_res?;

    let _ = app.emit(
        "dr3sh-download-progress",
        Dr3shProgressPayload {
            server_id,
            phase: "finishing".to_string(),
            percent: 100,
            phase_text: "¡Instalación completada con éxito!".to_string(),
            downloaded_bytes: total_bytes,
            total_bytes,
            downloaded_formatted: total_formatted.clone(),
            total_formatted,
            speed_formatted: "Listo".to_string(),
        },
    );

    Ok(final_target_path.to_string_lossy().to_string())
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
                remove_dir_or_junction(&p);
                let _ = std::fs::create_dir_all(&p);
            }
        }
    }
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

    tracing::info!("Initialized tracing subscriber. Loading Modrinth App!");

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
        use windows::Win32::UI::WindowsAndMessaging::{
            SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
            SWP_NOZORDER, SetWindowPos,
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
            dr3sh_download_modpack,
            dr3sh_cancel_download,
            dr3sh_wait_for_instance_ready,
            dr3sh_has_extracted_mods,
            dr3sh_link_existing_instance,
            dr3sh_cleanup_deleted_instance,
            dr3sh_publish_release,
            dr3sh_get_file_size,
        ]);

    tracing::info!("Initializing app...");
    let app = builder.build(tauri_context);

    match app {
        Ok(app) => {
            app.run(|app, event| {
                #[cfg(not(any(feature = "updater", target_os = "macos")))]
                let _ = app;

                if matches!(&event, tauri::RunEvent::ExitRequested { .. })
                    && let Err(error) = tauri::async_runtime::block_on(
                        theseus::minecraft_skins::flush_pending_skin_change(),
                    )
                {
                    tracing::warn!(
                        "Failed to flush pending Minecraft skin change before exit: {error}"
                    );
                }

                #[cfg(feature = "updater")]
                if matches!(&event, tauri::RunEvent::Exit) {
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
                        .set_text("Your Microsoft Edge WebView2 installation is corrupt.\n\nMicrosoft Edge WebView2 is required to run Modrinth App.\n\nLearn how to repair it at https://support.modrinth.com/en/articles/8797765-corrupted-microsoft-edge-webview2-installation")
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
