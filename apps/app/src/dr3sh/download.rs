use crate::dr3sh::errors::Dr3shError;
use crate::dr3sh::logger::{log_event, sanitize_url};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tauri_plugin_http::reqwest;
use tokio::sync::{mpsc, Mutex, Notify, RwLock};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryInfo {
    pub attempt: u32,
    pub max: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dr3shProgressEvent {
    pub op_id: String,
    pub server_id: String,
    pub stage: String,
    pub stage_text: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub speed_bps: u64,
    pub eta_seconds: Option<u64>,
    pub files_done: Option<u64>,
    pub files_total: Option<u64>,
    pub retry: Option<RetryInfo>,
    pub percent: Option<u32>,
    pub phase: String,
    pub phase_text: String,
    pub downloaded_formatted: String,
    pub total_formatted: String,
    pub speed_formatted: String,
}

pub fn format_bytes(bytes: u64) -> String {
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

#[allow(clippy::too_many_arguments)]
pub fn emit_progress_detailed(
    app: &AppHandle,
    op_id: &str,
    server_id: &str,
    stage: &str,
    stage_text: impl Into<String>,
    downloaded_bytes: u64,
    total_bytes: u64,
    speed_bps: u64,
    retry: Option<RetryInfo>,
    percent: Option<u32>,
    eta_seconds: Option<u64>,
    files_done: Option<u64>,
    files_total: Option<u64>,
) {
    let text = stage_text.into();
    let dl_fmt = format_bytes(downloaded_bytes);
    let tot_fmt = if total_bytes > 0 {
        format_bytes(total_bytes)
    } else {
        String::new()
    };
    let spd_fmt = if speed_bps > 0 {
        format!("{}/s", format_bytes(speed_bps))
    } else if let Some(ref r) = retry {
        format!("Reintento {}/{}...", r.attempt, r.max)
    } else {
        String::new()
    };
    let legacy_phase = match stage {
        "preflight" | "base" => "preparing",
        "download" => "downloading",
        "verify" => "verifying",
        "extract" => "extracting",
        "apply" => "applying",
        "commit" => "finishing",
        other => other,
    };
    let payload = Dr3shProgressEvent {
        op_id: op_id.to_string(),
        server_id: server_id.to_string(),
        stage: stage.to_string(),
        stage_text: text.clone(),
        downloaded_bytes,
        total_bytes,
        speed_bps,
        eta_seconds,
        files_done,
        files_total,
        retry,
        percent,
        phase: legacy_phase.to_string(),
        phase_text: text,
        downloaded_formatted: dl_fmt,
        total_formatted: tot_fmt,
        speed_formatted: spd_fmt,
    };
    let _ = app.emit("dr3sh://progress", &payload);
}

#[allow(clippy::too_many_arguments)]
pub fn emit_progress(
    app: &AppHandle,
    op_id: &str,
    server_id: &str,
    stage: &str,
    stage_text: impl Into<String>,
    downloaded_bytes: u64,
    total_bytes: u64,
    speed_bps: u64,
    retry: Option<RetryInfo>,
    percent: Option<u32>,
) {
    emit_progress_detailed(
        app,
        op_id,
        server_id,
        stage,
        stage_text,
        downloaded_bytes,
        total_bytes,
        speed_bps,
        retry,
        percent,
        None,
        None,
        None,
    );
}

static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    let ua = format!(
        "Dr3shLauncher/{} (+https://github.com/Dr3sh/dr3shapp)",
        env!("CARGO_PKG_VERSION")
    );
    reqwest::Client::builder()
        .user_agent(ua)
        .tcp_nodelay(true)
        .pool_max_idle_per_host(12)
        .connect_timeout(Duration::from_secs(20))
        .tcp_keepalive(Some(Duration::from_secs(10)))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .expect("failed to build shared Dr3sh HTTP client")
});

static MULTIPART_HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    let ua = format!(
        "Dr3shLauncher/{} (+https://github.com/Dr3sh/dr3shapp)",
        env!("CARGO_PKG_VERSION")
    );
    reqwest::Client::builder()
        .user_agent(ua)
        .http1_only()
        .tcp_nodelay(true)
        .pool_max_idle_per_host(16)
        .connect_timeout(Duration::from_secs(20))
        .tcp_keepalive(Some(Duration::from_secs(10)))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .expect("failed to build multipart Dr3sh HTTP client")
});

pub fn shared_http_client() -> &'static reqwest::Client {
    &HTTP_CLIENT
}

pub fn multipart_http_client() -> &'static reqwest::Client {
    &MULTIPART_HTTP_CLIENT
}

pub fn build_http_client() -> Result<&'static reqwest::Client, Dr3shError> {
    Ok(&HTTP_CLIENT)
}

pub fn extract_host(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "unknown".to_string())
}

pub fn drive_root_of(path: &Path) -> PathBuf {
    let mut root = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(p) => {
                let s = p.as_os_str().to_string_lossy();
                let clean = s.strip_prefix(r"\\?\").unwrap_or(&s);
                root.push(format!("{clean}\\"));
                return root;
            }
            Component::RootDir => {
                root.push(comp.as_os_str());
                return root;
            }
            _ => break,
        }
    }
    PathBuf::from(".")
}

pub fn drive_root_for_path(path: &Path) -> PathBuf {
    drive_root_of(path)
}

pub fn existing_part_size(
    instance_path: &Path,
    pack_id: &str,
    version: &str,
    expected_sha256: Option<&str>,
) -> u64 {
    let part = part_file_path(
        instance_path,
        pack_id,
        version,
        expected_sha256.unwrap_or(""),
    );
    fs::metadata(&part).map(|m| m.len()).unwrap_or(0)
}

pub fn part_file_path(
    instance_path: &Path,
    pack_id: &str,
    version: &str,
    zip_sha256: &str,
) -> PathBuf {
    let root = drive_root_of(instance_path);
    let root_dl_dir = root.join(".dr3sh_downloads");
    let dl_dir = if fs::create_dir_all(&root_dl_dir).is_ok()
        && fs::write(root_dl_dir.join(".dr3sh_instance"), "dr3sh_downloads").is_ok()
    {
        root_dl_dir
    } else {
        let fallback = instance_path.join(".dr3sh").join("downloads");
        let _ = fs::create_dir_all(&fallback);
        fallback
    };
    let safe_pack: String = pack_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let safe_ver: String = version
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let hash_prefix = if zip_sha256.len() >= 16 {
        &zip_sha256[..16]
    } else if !zip_sha256.is_empty() {
        zip_sha256
    } else {
        "nohash0000000000"
    };
    dl_dir.join(format!("{safe_pack}-{safe_ver}-{hash_prefix}.part"))
}

fn parse_total_from_content_range(header_val: &str) -> Option<u64> {
    let (_, total_str) = header_val.rsplit_once('/')?;
    let trimmed = total_str.trim();
    if trimmed == "*" {
        return None;
    }
    trimmed.parse::<u64>().ok()
}

fn sanitize_snippet(bytes: &[u8]) -> String {
    let limit = bytes.len().min(2048);
    let s = String::from_utf8_lossy(&bytes[..limit]);
    s.chars()
        .map(|c| if c.is_ascii_graphic() || c == ' ' { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let val = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?.trim();
    if let Ok(secs) = val.parse::<u64>() {
        return Some(Duration::from_secs(secs.min(60)));
    }
    if let Ok(date) = httpdate::parse_http_date(val)
        && let Ok(dur) = date.duration_since(std::time::SystemTime::now())
    {
        return Some(dur.min(Duration::from_secs(60)));
    }
    None
}

fn hash_existing_part_file(
    path: &Path,
    cancel_flag: Option<&AtomicBool>,
) -> Result<(Sha256, u64, Option<[u8; 4]>), Dr3shError> {
    let file = File::open(path).map_err(|e| map_io_error(path, e))?;
    let mut reader = BufReader::with_capacity(512 * 1024, file);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut first_four: Option<[u8; 4]> = None;
    let mut total_read: u64 = 0;
    loop {
        if cancel_flag.is_some_and(|f| f.load(Ordering::Relaxed)) {
            return Err(Dr3shError::Cancelled {
                stage: "DOWNLOAD".to_string(),
            });
        }
        let n = reader.read(&mut buf).map_err(|e| map_io_error(path, e))?;
        if n == 0 {
            break;
        }
        if first_four.is_none() && total_read == 0 && n >= 4 {
            first_four = Some([buf[0], buf[1], buf[2], buf[3]]);
        }
        hasher.update(&buf[..n]);
        total_read += n as u64;
    }
    Ok((hasher, total_read, first_four))
}

fn backoff_with_jitter(retry_idx: u32) -> Duration {
    const BASE_SECS: [u64; 6] = [1, 2, 4, 8, 16, 30];
    let idx = (retry_idx.saturating_sub(1) as usize).min(BASE_SECS.len() - 1);
    let base_ms = (BASE_SECS[idx] * 1000) as f64;
    let mut rng = rand::rng();
    let factor: f64 = rng.random_range(0.80..=1.20);
    Duration::from_millis((base_ms * factor).round() as u64)
}

#[derive(Debug, Clone)]
pub struct DownloadOutcome {
    pub part_path: PathBuf,
    pub sha256: String,
    pub total_bytes: u64,
    pub avg_speed_bps: u64,
    pub retries_used: u32,
}

pub struct DownloadSpec<'a> {
    pub app: Option<&'a AppHandle>,
    pub op_id: &'a str,
    pub server_id: &'a str,
    pub server_title: &'a str,
    pub url: &'a str,
    pub part_path: &'a Path,
    pub expected_size: Option<u64>,
    pub expected_sha256: Option<&'a str>,
    pub cancel_flag: &'a Arc<AtomicBool>,
}

pub async fn download_zip_resumable(spec: DownloadSpec<'_>) -> Result<DownloadOutcome, Dr3shError> {
    let mut redownload_after_hash_mismatch = false;

    loop {
        if spec.cancel_flag.load(Ordering::Relaxed) {
            return Err(Dr3shError::Cancelled {
                stage: "DOWNLOAD".to_string(),
            });
        }

        let (final_size, actual_hash, avg_speed_bps, retries_used) =
            run_download_attempts(&spec).await?;

        if let Some(exp_size) = spec.expected_size.filter(|&s| s > 0)
            && final_size != exp_size
        {
            let _ = fs::remove_file(spec.part_path);
            return Err(Dr3shError::SizeMismatch {
                expected: exp_size,
                actual: final_size,
            });
        }

        if let Some(exp_hash) = spec.expected_sha256.map(str::trim).filter(|s| !s.is_empty()) {
            if !actual_hash.eq_ignore_ascii_case(exp_hash) {
                let _ = fs::remove_file(spec.part_path);
                if !redownload_after_hash_mismatch {
                    redownload_after_hash_mismatch = true;
                    log_event(
                        Some(spec.op_id),
                        "VERIFY",
                        "WARN",
                        "SHA-256 mismatch on first download; deleting .part and redownloading once from 0",
                        &[
                            ("expected", exp_hash.to_string()),
                            ("actual", actual_hash),
                        ],
                    );
                    continue;
                }
                return Err(Dr3shError::HashMismatch {
                    expected: exp_hash.to_lowercase(),
                    actual: actual_hash,
                });
            }
            log_event(
                Some(spec.op_id),
                "VERIFY",
                "INFO",
                "ZIP SHA-256 verified",
                &[
                    ("sha256", actual_hash.clone()),
                    ("size", final_size.to_string()),
                ],
            );
        } else {
            log_event(
                Some(spec.op_id),
                "VERIFY",
                "WARN",
                "ZIP downloaded without expected SHA-256; verified ZIP signature only",
                &[
                    ("computed_sha256", actual_hash.clone()),
                    ("size", final_size.to_string()),
                ],
            );
        }

        return Ok(DownloadOutcome {
            part_path: spec.part_path.to_path_buf(),
            sha256: actual_hash,
            total_bytes: final_size,
            avg_speed_bps,
            retries_used,
        });
    }
}

#[derive(Clone, Copy)]
struct MultipartConfig {
    threshold: u64,
    chunk_size: u64,
    concurrency: usize,
    max_reorder_ahead: u64,
}

impl Default for MultipartConfig {
    fn default() -> Self {
        Self {
            threshold: 8 * 1024 * 1024,
            chunk_size: 16 * 1024 * 1024,
            concurrency: 8,
            max_reorder_ahead: 12,
        }
    }
}

fn update_speed_window_and_eta(
    window: &mut VecDeque<(Instant, u64)>,
    now: Instant,
    current_bytes: u64,
    total_bytes: u64,
    smoothed_speed: &mut f64,
) -> (u64, Option<u64>) {
    window.push_back((now, current_bytes));
    while window.len() > 2
        && window
            .front()
            .is_some_and(|(t, _)| now.duration_since(*t) > Duration::from_secs(10))
    {
        window.pop_front();
    }
    let speed_bps = if let Some(&(oldest_t, oldest_b)) = window.front() {
        let dt = now.duration_since(oldest_t).as_secs_f64();
        if dt >= 0.4 && current_bytes >= oldest_b {
            let win_speed = (current_bytes - oldest_b) as f64 / dt;
            *smoothed_speed = if *smoothed_speed <= 0.0 {
                win_speed
            } else {
                *smoothed_speed * 0.78 + win_speed * 0.22
            };
        }
        (*smoothed_speed).max(0.0) as u64
    } else {
        0
    };
    let eta_seconds = if total_bytes > current_bytes && speed_bps > 1024 {
        Some(((total_bytes - current_bytes) / speed_bps).max(1))
    } else {
        None
    };
    (speed_bps, eta_seconds)
}

async fn hash_existing_part_async(
    part_path: &Path,
    cancel_flag: &Arc<AtomicBool>,
) -> Result<(Sha256, u64, Option<[u8; 4]>), Dr3shError> {
    let p = part_path.to_path_buf();
    let c = Arc::clone(cancel_flag);
    tokio::task::spawn_blocking(move || hash_existing_part_file(&p, Some(c.as_ref())))
        .await
        .map_err(|e| Dr3shError::unknown(format!("Hash task join error: {e}")))?
}

struct RetryActiveGuard {
    counter: Arc<AtomicU32>,
}

impl Drop for RetryActiveGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

async fn sleep_with_retry_tracking(
    wait: Duration,
    abort_notify: &Notify,
    cancel_flag: &Arc<AtomicBool>,
    active_retrying: &Arc<AtomicU32>,
    max_attempt: &Arc<AtomicU32>,
    attempt: u32,
) -> bool {
    active_retrying.fetch_add(1, Ordering::Relaxed);
    max_attempt.fetch_max(attempt, Ordering::Relaxed);
    let _guard = RetryActiveGuard {
        counter: Arc::clone(active_retrying),
    };
    tokio::select! {
        _ = tokio::time::sleep(wait) => false,
        _ = abort_notify.notified() => true,
        _ = wait_until_cancelled(cancel_flag) => true,
    }
}

async fn try_multipart_download(
    spec: &DownloadSpec<'_>,
) -> Result<Option<(u64, String, u64, u32)>, Dr3shError> {
    try_multipart_download_with_config(spec, MultipartConfig::default()).await
}

async fn try_multipart_download_with_config(
    spec: &DownloadSpec<'_>,
    cfg: MultipartConfig,
) -> Result<Option<(u64, String, u64, u32)>, Dr3shError> {
    const CHUNK_MAX_RETRIES: u32 = 5;
    const CHUNK_READ_TIMEOUT: Duration = Duration::from_secs(20);

    if spec
        .expected_size
        .is_some_and(|s| s > 0 && s < cfg.threshold)
    {
        return Ok(None);
    }

    let client = multipart_http_client();
    let probe_res = tokio::select! {
        r = client.get(spec.url).header(reqwest::header::RANGE, "bytes=0-0").send() => r,
        _ = wait_until_cancelled(spec.cancel_flag) => {
            return Err(Dr3shError::Cancelled { stage: "DOWNLOAD".to_string() });
        }
    };

    let Ok(probe) = probe_res else {
        return Ok(None);
    };

    let probe_status = probe.status();
    if probe_status == reqwest::StatusCode::NOT_FOUND || probe_status == reqwest::StatusCode::GONE {
        return Err(Dr3shError::HttpNotFound {
            status: probe_status.as_u16(),
            url: sanitize_url(spec.url),
        });
    }
    if probe_status != reqwest::StatusCode::PARTIAL_CONTENT {
        return Ok(None);
    }

    let final_url = probe.url().clone();
    let total_bytes = probe
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_total_from_content_range)
        .unwrap_or(0);

    if total_bytes < cfg.threshold {
        return Ok(None);
    }

    if let Some(exp) = spec.expected_size.filter(|&s| s > 0)
        && exp != total_bytes
    {
        return Err(Dr3shError::SizeMismatch {
            expected: exp,
            actual: total_bytes,
        });
    }

    let first_chunk = match tokio::select! {
        r = probe.bytes() => r,
        _ = wait_until_cancelled(spec.cancel_flag) => {
            return Err(Dr3shError::Cancelled { stage: "DOWNLOAD".to_string() });
        }
    } {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };
    if first_chunk.first().copied() != Some(0x50) {
        return Ok(None);
    }

    if let Some(parent) = spec.part_path.parent() {
        fs::create_dir_all(parent).map_err(|e| map_io_error(parent, e))?;
    }

    let (resume_hasher, resume_offset) = if spec.part_path.exists() {
        if let Some(app_handle) = spec.app {
            emit_progress_detailed(
                app_handle,
                spec.op_id,
                spec.server_id,
                "download",
                "Verificando descarga parcial existente...",
                0,
                total_bytes,
                0,
                None,
                Some(6),
                None,
                None,
                None,
            );
        }
        match hash_existing_part_async(spec.part_path, spec.cancel_flag).await {
            Ok((h, len, first4)) => {
                if first4 == Some([0x50, 0x4B, 0x03, 0x04]) && len == total_bytes {
                    let digest = h.finalize();
                    let actual_hash: String = digest.iter().map(|b| format!("{b:02x}")).collect();
                    return Ok(Some((total_bytes, actual_hash, 0, 0)));
                } else if first4 == Some([0x50, 0x4B, 0x03, 0x04])
                    && len > 0
                    && len < total_bytes
                {
                    (h, len)
                } else {
                    let _ = fs::remove_file(spec.part_path);
                    (Sha256::new(), 0)
                }
            }
            Err(e @ Dr3shError::Cancelled { .. }) => return Err(e),
            Err(_) => {
                let _ = fs::remove_file(spec.part_path);
                (Sha256::new(), 0)
            }
        }
    } else {
        (Sha256::new(), 0)
    };

    let remaining_bytes = total_bytes - resume_offset;
    let total_chunks = remaining_bytes.div_ceil(cfg.chunk_size);
    let concurrency = cfg.concurrency.min(total_chunks as usize).max(1);

    log_event(
        Some(spec.op_id),
        "DOWNLOAD",
        "INFO",
        "Iniciando descarga dinámica multiparte",
        &[
            ("workers", concurrency.to_string()),
            ("chunk_size", cfg.chunk_size.to_string()),
            ("total_chunks", total_chunks.to_string()),
            ("resume_from", resume_offset.to_string()),
            ("total_bytes", total_bytes.to_string()),
            ("host", extract_host(final_url.as_str())),
        ],
    );

    let next_dispatch_idx = Arc::new(AtomicU64::new(0));
    let next_write_idx = Arc::new(AtomicU64::new(0));
    let window_notify = Arc::new(Notify::new());
    let abort_notify = Arc::new(Notify::new());
    let worker_abort = Arc::new(AtomicBool::new(false));
    let live_downloaded = Arc::new(AtomicU64::new(resume_offset));
    let total_retries = Arc::new(AtomicU32::new(0));
    let shared_url = Arc::new(RwLock::new(final_url));
    let url_refresh_lock = Arc::new(Mutex::new(()));
    let (tx, mut rx) = mpsc::channel::<(u64, Vec<u8>)>(16);

    let writer_part_path = spec.part_path.to_path_buf();
    let writer_next_idx = Arc::clone(&next_write_idx);
    let writer_window_notify = Arc::clone(&window_notify);
    let writer_abort_flag = Arc::clone(&worker_abort);
    let writer_abort_notify = Arc::clone(&abort_notify);
    let writer_handle = tokio::task::spawn_blocking(move || -> Result<Option<(u64, String)>, Dr3shError> {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .append(resume_offset > 0)
            .truncate(resume_offset == 0)
            .open(&writer_part_path)
            .map_err(|e| map_io_error(&writer_part_path, e))?;
        let mut writer = BufWriter::with_capacity(2 * 1024 * 1024, file);
        let mut hasher = resume_hasher;
        let mut pending: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
        let mut expected_idx: u64 = 0;
        let mut written_bytes = resume_offset;

        while let Some((chunk_idx, data)) = rx.blocking_recv() {
            pending.insert(chunk_idx, data);
            while let Some(buf) = pending.remove(&expected_idx) {
                if expected_idx == 0
                    && resume_offset == 0
                    && (buf.len() < 4 || buf[..4] != [0x50, 0x4B, 0x03, 0x04])
                {
                    let snippet = sanitize_snippet(&buf);
                    drop(writer);
                    let _ = fs::remove_file(&writer_part_path);
                    writer_abort_flag.store(true, Ordering::Release);
                    writer_abort_notify.notify_waiters();
                    return Err(Dr3shError::HtmlInsteadOfZip {
                        content_type: "unknown".to_string(),
                        snippet,
                        host: "release-assets".to_string(),
                    });
                }
                if let Err(e) = writer.write_all(&buf) {
                    drop(writer);
                    writer_abort_flag.store(true, Ordering::Release);
                    writer_abort_notify.notify_waiters();
                    return Err(map_io_error(&writer_part_path, e));
                }
                hasher.update(&buf);
                written_bytes += buf.len() as u64;
                expected_idx += 1;
                writer_next_idx.store(expected_idx, Ordering::Release);
                writer_window_notify.notify_waiters();
            }
        }

        if let Err(e) = writer.flush() {
            return Err(map_io_error(&writer_part_path, e));
        }
        drop(writer);

        if expected_idx == total_chunks && written_bytes == total_bytes {
            let digest = hasher.finalize();
            let actual_hash: String = digest.iter().map(|b| format!("{b:02x}")).collect();
            Ok(Some((written_bytes, actual_hash)))
        } else {
            Ok(None)
        }
    });

    let start_time = Instant::now();
    let initial_pct = if total_bytes > 0 {
        let ratio = (resume_offset as f64 / total_bytes as f64).clamp(0.0, 1.0);
        Some((6.0 + ratio * 62.0).round() as u32)
    } else {
        Some(6)
    };
    if let Some(app_handle) = spec.app {
        emit_progress_detailed(
            app_handle,
            spec.op_id,
            spec.server_id,
            "download",
            format!("Descargando {}...", spec.server_title),
            resume_offset,
            total_bytes,
            0,
            None,
            initial_pct,
            None,
            None,
            None,
        );
    }

    let active_retrying_workers = Arc::new(AtomicU32::new(0));
    let max_active_attempt = Arc::new(AtomicU32::new(0));
    let mut handles = Vec::with_capacity(concurrency);
    for worker_id in 0..concurrency {
        let tx_w = tx.clone();
        let next_dispatch = Arc::clone(&next_dispatch_idx);
        let next_write = Arc::clone(&next_write_idx);
        let win_notify = Arc::clone(&window_notify);
        let abt_notify = Arc::clone(&abort_notify);
        let abt_flag = Arc::clone(&worker_abort);
        let cancel_flag = Arc::clone(spec.cancel_flag);
        let dl_counter = Arc::clone(&live_downloaded);
        let retry_counter = Arc::clone(&total_retries);
        let active_retrying_w = Arc::clone(&active_retrying_workers);
        let max_attempt_w = Arc::clone(&max_active_attempt);
        let shared_url_w = Arc::clone(&shared_url);
        let refresh_lock = Arc::clone(&url_refresh_lock);
        let spec_url = spec.url.to_string();
        let chunk_sz = cfg.chunk_size;
        let max_ahead = cfg.max_reorder_ahead;

        handles.push(tokio::spawn(async move {
            loop {
                if cancel_flag.load(Ordering::Relaxed) {
                    abt_flag.store(true, Ordering::Release);
                    abt_notify.notify_waiters();
                    return Err(Dr3shError::Cancelled {
                        stage: "DOWNLOAD".to_string(),
                    });
                }
                if abt_flag.load(Ordering::Acquire) {
                    return Ok(());
                }

                let my_idx = next_dispatch.fetch_add(1, Ordering::Relaxed);
                if my_idx >= total_chunks {
                    return Ok(());
                }

                while my_idx >= next_write.load(Ordering::Acquire) + max_ahead {
                    if cancel_flag.load(Ordering::Relaxed) {
                        abt_flag.store(true, Ordering::Release);
                        abt_notify.notify_waiters();
                        return Err(Dr3shError::Cancelled {
                            stage: "DOWNLOAD".to_string(),
                        });
                    }
                    if abt_flag.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    tokio::select! {
                        _ = win_notify.notified() => {}
                        _ = abt_notify.notified() => {}
                        _ = tokio::time::sleep(Duration::from_millis(150)) => {}
                    }
                }

                let start = resume_offset + my_idx * chunk_sz;
                let end = (start + chunk_sz - 1).min(total_bytes - 1);
                let expected_len = ((end - start) + 1) as usize;

                let mut attempt = 0u32;
                let mut url_refreshed_for_chunk = false;

                let chunk_buf: Vec<u8> = loop {
                    if cancel_flag.load(Ordering::Relaxed) {
                        abt_flag.store(true, Ordering::Release);
                        abt_notify.notify_waiters();
                        return Err(Dr3shError::Cancelled {
                            stage: "DOWNLOAD".to_string(),
                        });
                    }
                    if abt_flag.load(Ordering::Acquire) {
                        return Ok(());
                    }

                    let current_url = shared_url_w.read().await.clone();
                    let send_res = tokio::select! {
                        r = multipart_http_client()
                            .get(current_url.clone())
                            .header(reqwest::header::RANGE, format!("bytes={start}-{end}"))
                            .send() => r,
                        _ = abt_notify.notified() => {
                            return Ok(());
                        }
                        _ = wait_until_cancelled(&cancel_flag) => {
                            abt_flag.store(true, Ordering::Release);
                            abt_notify.notify_waiters();
                            return Err(Dr3shError::Cancelled { stage: "DOWNLOAD".to_string() });
                        }
                    };

                    let mut resp = match send_res {
                        Ok(r) => r,
                        Err(e) => {
                            if attempt >= CHUNK_MAX_RETRIES {
                                abt_flag.store(true, Ordering::Release);
                                abt_notify.notify_waiters();
                                return Err(Dr3shError::Network {
                                    last_cause: format!("Error en bloque {my_idx} (worker {worker_id}): {e}"),
                                    retries: attempt,
                                    offset: start,
                                });
                            }
                            attempt += 1;
                            retry_counter.fetch_add(1, Ordering::Relaxed);
                            let wait = backoff_with_jitter(attempt);
                            if sleep_with_retry_tracking(
                                wait,
                                &abt_notify,
                                &cancel_flag,
                                &active_retrying_w,
                                &max_attempt_w,
                                attempt,
                            )
                            .await
                            {
                                if cancel_flag.load(Ordering::Relaxed) {
                                    abt_flag.store(true, Ordering::Release);
                                    abt_notify.notify_waiters();
                                    return Err(Dr3shError::Cancelled { stage: "DOWNLOAD".to_string() });
                                }
                                return Ok(());
                            }
                            continue;
                        }
                    };

                    let status = resp.status();
                    let code = status.as_u16();
                    if code == 404 || code == 410 {
                        abt_flag.store(true, Ordering::Release);
                        abt_notify.notify_waiters();
                        return Err(Dr3shError::HttpNotFound {
                            status: code,
                            url: sanitize_url(&spec_url),
                        });
                    }

                    if (code == 401 || code == 403) && !url_refreshed_for_chunk {
                        url_refreshed_for_chunk = true;
                        let _guard = refresh_lock.lock().await;
                        let cur_shared = shared_url_w.read().await.clone();
                        if cur_shared == current_url {
                            if let Ok(re_probe) = multipart_http_client()
                                .get(&spec_url)
                                .header(reqwest::header::RANGE, "bytes=0-0")
                                .send()
                                .await
                                && re_probe.status() == reqwest::StatusCode::PARTIAL_CONTENT
                            {
                                *shared_url_w.write().await = re_probe.url().clone();
                            }
                        }
                        retry_counter.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }

                    if code == 401 || code == 403 {
                        abt_flag.store(true, Ordering::Release);
                        abt_notify.notify_waiters();
                        return Err(Dr3shError::HttpForbidden {
                            status: code,
                            host: extract_host(current_url.as_str()),
                        });
                    }

                    if status != reqwest::StatusCode::PARTIAL_CONTENT {
                        if attempt >= CHUNK_MAX_RETRIES {
                            abt_flag.store(true, Ordering::Release);
                            abt_notify.notify_waiters();
                            return Err(Dr3shError::Network {
                                last_cause: format!("HTTP {code} en bloque {my_idx}"),
                                retries: attempt,
                                offset: start,
                            });
                        }
                        attempt += 1;
                        retry_counter.fetch_add(1, Ordering::Relaxed);
                        let wait = parse_retry_after(resp.headers())
                            .unwrap_or_else(|| backoff_with_jitter(attempt));
                        if sleep_with_retry_tracking(
                            wait,
                            &abt_notify,
                            &cancel_flag,
                            &active_retrying_w,
                            &max_attempt_w,
                            attempt,
                        )
                        .await
                        {
                            if cancel_flag.load(Ordering::Relaxed) {
                                abt_flag.store(true, Ordering::Release);
                                abt_notify.notify_waiters();
                                return Err(Dr3shError::Cancelled { stage: "DOWNLOAD".to_string() });
                            }
                            return Ok(());
                        }
                        continue;
                    }

                    let mut buf = Vec::with_capacity(expected_len);
                    let mut chunk_added: u64 = 0;
                    let mut chunk_failed = false;

                    loop {
                        if cancel_flag.load(Ordering::Relaxed) {
                            abt_flag.store(true, Ordering::Release);
                            abt_notify.notify_waiters();
                            return Err(Dr3shError::Cancelled {
                                stage: "DOWNLOAD".to_string(),
                            });
                        }
                        if abt_flag.load(Ordering::Acquire) {
                            return Ok(());
                        }
                        match tokio::select! {
                            r = tokio::time::timeout(CHUNK_READ_TIMEOUT, resp.chunk()) => r,
                            _ = abt_notify.notified() => return Ok(()),
                            _ = wait_until_cancelled(&cancel_flag) => {
                                abt_flag.store(true, Ordering::Release);
                                abt_notify.notify_waiters();
                                return Err(Dr3shError::Cancelled { stage: "DOWNLOAD".to_string() });
                            }
                        } {
                            Ok(Ok(Some(slice))) => {
                                let rem = expected_len.saturating_sub(buf.len());
                                if rem == 0 {
                                    break;
                                }
                                let keep = &slice[..slice.len().min(rem)];
                                buf.extend_from_slice(keep);
                                let add = keep.len() as u64;
                                chunk_added += add;
                                dl_counter.fetch_add(add, Ordering::Relaxed);
                                if buf.len() >= expected_len {
                                    break;
                                }
                            }
                            Ok(Ok(None)) => {
                                if buf.len() != expected_len {
                                    chunk_failed = true;
                                }
                                break;
                            }
                            Ok(Err(_)) | Err(_) => {
                                chunk_failed = true;
                                break;
                            }
                        }
                    }

                    if !chunk_failed && buf.len() == expected_len {
                        break buf;
                    }

                    if chunk_added > 0 {
                        dl_counter.fetch_sub(chunk_added, Ordering::Relaxed);
                    }
                    if attempt >= CHUNK_MAX_RETRIES {
                        abt_flag.store(true, Ordering::Release);
                        abt_notify.notify_waiters();
                        return Err(Dr3shError::Network {
                            last_cause: format!("Bloque {my_idx} incompleto o tiempo agotado"),
                            retries: attempt,
                            offset: start,
                        });
                    }
                    attempt += 1;
                    retry_counter.fetch_add(1, Ordering::Relaxed);
                    let wait = backoff_with_jitter(attempt);
                    if sleep_with_retry_tracking(
                        wait,
                        &abt_notify,
                        &cancel_flag,
                        &active_retrying_w,
                        &max_attempt_w,
                        attempt,
                    )
                    .await
                    {
                        if cancel_flag.load(Ordering::Relaxed) {
                            abt_flag.store(true, Ordering::Release);
                            abt_notify.notify_waiters();
                            return Err(Dr3shError::Cancelled { stage: "DOWNLOAD".to_string() });
                        }
                        return Ok(());
                    }
                };

                if tx_w.send((my_idx, chunk_buf)).await.is_err() {
                    return Ok(());
                }
            }
        }));
    }

    drop(tx);

    let mut smoothed_speed: f64 = 0.0;
    let mut speed_window: VecDeque<(Instant, u64)> = VecDeque::new();
    speed_window.push_back((start_time, resume_offset));
    let mut last_reported_bytes = resume_offset;
    let mut last_progress_at = start_time;

    loop {
        if spec.cancel_flag.load(Ordering::Relaxed) {
            worker_abort.store(true, Ordering::Release);
            abort_notify.notify_waiters();
            break;
        }

        if handles.iter().all(|h| h.is_finished()) || writer_handle.is_finished() {
            break;
        }

        tokio::time::sleep(Duration::from_millis(150)).await;
        let now = Instant::now();
        let current = live_downloaded.load(Ordering::Relaxed).min(total_bytes);
        let (mut speed_bps, mut eta_seconds) = update_speed_window_and_eta(
            &mut speed_window,
            now,
            current,
            total_bytes,
            &mut smoothed_speed,
        );
        let pct = Some((6.0 + (current as f64 / total_bytes as f64) * 62.0).round() as u32);
        if current > last_reported_bytes {
            last_reported_bytes = current;
            last_progress_at = now;
            max_active_attempt.store(0, Ordering::Relaxed);
        }
        let retrying_now = active_retrying_workers.load(Ordering::Relaxed);
        let (stage_text, retry_badge) = if retrying_now > 0
            && now.duration_since(last_progress_at) >= Duration::from_secs(2)
        {
            let att = max_active_attempt
                .load(Ordering::Relaxed)
                .max(1)
                .min(CHUNK_MAX_RETRIES);
            speed_bps = 0;
            eta_seconds = None;
            (
                format!("Conexión inestable, reconectando ({att}/{CHUNK_MAX_RETRIES})..."),
                Some(RetryInfo {
                    attempt: att,
                    max: CHUNK_MAX_RETRIES,
                }),
            )
        } else {
            (format!("Descargando {}...", spec.server_title), None)
        };
        if let Some(app_handle) = spec.app {
            emit_progress_detailed(
                app_handle,
                spec.op_id,
                spec.server_id,
                "download",
                stage_text,
                current,
                total_bytes,
                speed_bps,
                retry_badge,
                pct,
                eta_seconds,
                None,
                None,
            );
        }
    }

    let mut fatal_err: Option<Dr3shError> = None;
    let mut had_transient_err = false;
    for h in handles {
        match h.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                if matches!(
                    e,
                    Dr3shError::Cancelled { .. }
                        | Dr3shError::HttpNotFound { .. }
                        | Dr3shError::HttpForbidden { .. }
                        | Dr3shError::HtmlInsteadOfZip { .. }
                        | Dr3shError::SizeMismatch { .. }
                ) {
                    if fatal_err.is_none() {
                        fatal_err = Some(e);
                    }
                } else {
                    had_transient_err = true;
                }
            }
            Err(_) => {
                had_transient_err = true;
            }
        }
    }

    let writer_outcome = writer_handle
        .await
        .map_err(|e| Dr3shError::unknown(format!("Writer task join error: {e}")))??;

    if let Some(err) = fatal_err {
        return Err(err);
    }

    if spec.cancel_flag.load(Ordering::Relaxed) {
        return Err(Dr3shError::Cancelled {
            stage: "DOWNLOAD".to_string(),
        });
    }

    if had_transient_err {
        return Ok(None);
    }

    let Some((written_bytes, actual_hash)) = writer_outcome else {
        return Ok(None);
    };

    let elapsed_secs = start_time.elapsed().as_secs_f64().max(0.05);
    let newly_downloaded = written_bytes.saturating_sub(resume_offset);
    let avg_speed = if newly_downloaded > 0 {
        (newly_downloaded as f64 / elapsed_secs) as u64
    } else {
        0
    };
    let retries_used = total_retries.load(Ordering::Relaxed);

    if let Some(app_handle) = spec.app {
        emit_progress_detailed(
            app_handle,
            spec.op_id,
            spec.server_id,
            "download",
            format!("Descargando {}...", spec.server_title),
            total_bytes,
            total_bytes,
            avg_speed,
            None,
            Some(68),
            None,
            None,
            None,
        );
    }

    Ok(Some((written_bytes, actual_hash, avg_speed, retries_used)))
}

async fn sleep_or_cancel(
    wait: Duration,
    cancel_flag: &Arc<AtomicBool>,
) -> Result<(), Dr3shError> {
    tokio::select! {
        _ = tokio::time::sleep(wait) => Ok(()),
        _ = wait_until_cancelled(cancel_flag) => Err(Dr3shError::Cancelled {
            stage: "DOWNLOAD".to_string(),
        }),
    }
}

async fn run_download_attempts(
    spec: &DownloadSpec<'_>,
) -> Result<(u64, String, u64, u32), Dr3shError> {
    const MAX_RETRIES: u32 = 6;
    let mut retry_count: u32 = 0;
    let mut total_retries_used: u32 = 0;
    let mut forbidden_retried = false;
    let mut smoothed_speed: f64 = 0.0;

    loop {
        if spec.cancel_flag.load(Ordering::Relaxed) {
            return Err(Dr3shError::Cancelled {
                stage: "DOWNLOAD".to_string(),
            });
        }

        if retry_count == 0
            && let Some(multipart_result) = try_multipart_download(spec).await?
        {
            return Ok(multipart_result);
        }

        let (mut hasher, mut received_bytes, mut magic_checked) = if spec.part_path.exists() {
            match hash_existing_part_async(spec.part_path, spec.cancel_flag).await {
                Ok((h, len, first4)) => {
                    if let Some(m) = first4
                        && m != [0x50, 0x4B, 0x03, 0x04]
                    {
                        let _ = fs::remove_file(spec.part_path);
                        (Sha256::new(), 0, false)
                    } else {
                        (h, len, first4.is_some())
                    }
                }
                Err(e @ Dr3shError::Cancelled { .. }) => return Err(e),
                Err(_) => (Sha256::new(), 0, false),
            }
        } else {
            (Sha256::new(), 0, false)
        };

        if let Some(exp_size) = spec.expected_size.filter(|&s| s > 0)
            && received_bytes == exp_size
            && magic_checked
        {
            let digest = hasher.finalize();
            let actual_hash: String = digest.iter().map(|b| format!("{b:02x}")).collect();
            return Ok((
                received_bytes,
                actual_hash,
                smoothed_speed.max(0.0) as u64,
                total_retries_used,
            ));
        }

        let client = shared_http_client();
        let mut req = client.get(spec.url);
        if received_bytes > 0 {
            req = req.header(reqwest::header::RANGE, format!("bytes={received_bytes}-"));
        }

        let retry_badge = if retry_count > 0 {
            Some(RetryInfo {
                attempt: retry_count,
                max: MAX_RETRIES,
            })
        } else {
            None
        };

        let send_res = tokio::select! {
            r = req.send() => r,
            _ = wait_until_cancelled(spec.cancel_flag) => {
                return Err(Dr3shError::Cancelled { stage: "DOWNLOAD".to_string() });
            }
        };

        let mut resp = match send_res {
            Ok(r) => r,
            Err(e) => {
                if retry_count >= MAX_RETRIES {
                    return Err(Dr3shError::Network {
                        last_cause: e.to_string(),
                        retries: retry_count,
                        offset: received_bytes,
                    });
                }
                retry_count += 1;
                total_retries_used += 1;
                let wait = backoff_with_jitter(retry_count);
                log_event(
                    Some(spec.op_id),
                    "DOWNLOAD",
                    "WARN",
                    &format!("retry {retry_count}/{MAX_RETRIES}"),
                    &[
                        ("cause", e.to_string()),
                        ("at_byte", received_bytes.to_string()),
                        ("wait_ms", wait.as_millis().to_string()),
                    ],
                );
                if let Some(app_handle) = spec.app {
                    emit_progress(
                        app_handle,
                        spec.op_id,
                        spec.server_id,
                        "download",
                        format!("Reconectando ({retry_count}/{MAX_RETRIES})..."),
                        received_bytes,
                        spec.expected_size.unwrap_or(0),
                        0,
                        Some(RetryInfo {
                            attempt: retry_count,
                            max: MAX_RETRIES,
                        }),
                        None,
                    );
                }
                sleep_or_cancel(wait, spec.cancel_flag).await?;
                continue;
            }
        };

        let status = resp.status();
        let status_code = status.as_u16();
        let final_host = resp
            .url()
            .host_str()
            .unwrap_or("unknown")
            .to_string();

        log_event(
            Some(spec.op_id),
            "DOWNLOAD",
            "INFO",
            "HTTP response received",
            &[
                ("status", status_code.to_string()),
                ("host", final_host.clone()),
                ("resume_from", received_bytes.to_string()),
            ],
        );

        if status_code == 416 && received_bytes > 0 {
            let digest = hasher.finalize();
            let actual_hash: String = digest.iter().map(|b| format!("{b:02x}")).collect();
            return Ok((
                received_bytes,
                actual_hash,
                smoothed_speed.max(0.0) as u64,
                total_retries_used,
            ));
        }

        if status_code == 404 || status_code == 410 {
            return Err(Dr3shError::HttpNotFound {
                status: status_code,
                url: sanitize_url(spec.url),
            });
        }

        if status_code == 401 {
            return Err(Dr3shError::HttpForbidden {
                status: status_code,
                host: final_host,
            });
        }

        if status_code == 403 {
            if !forbidden_retried {
                forbidden_retried = true;
                retry_count += 1;
                total_retries_used += 1;
                let wait = backoff_with_jitter(retry_count);
                log_event(
                    Some(spec.op_id),
                    "DOWNLOAD",
                    "WARN",
                    "HTTP 403 received; retrying once",
                    &[("host", final_host), ("at_byte", received_bytes.to_string())],
                );
                sleep_or_cancel(wait, spec.cancel_flag).await?;
                continue;
            }
            return Err(Dr3shError::HttpForbidden {
                status: status_code,
                host: final_host,
            });
        }

        if status_code == 429 || status.is_server_error() {
            if retry_count >= MAX_RETRIES {
                return Err(Dr3shError::Network {
                    last_cause: format!("HTTP {status_code}"),
                    retries: retry_count,
                    offset: received_bytes,
                });
            }
            retry_count += 1;
            total_retries_used += 1;
            let wait = parse_retry_after(resp.headers())
                .unwrap_or_else(|| backoff_with_jitter(retry_count));
            log_event(
                Some(spec.op_id),
                "DOWNLOAD",
                "WARN",
                &format!("retry {retry_count}/{MAX_RETRIES}"),
                &[
                    ("cause", format!("HTTP {status_code}")),
                    ("at_byte", received_bytes.to_string()),
                    ("host", final_host),
                    ("wait_ms", wait.as_millis().to_string()),
                ],
            );
            sleep_or_cancel(wait, spec.cancel_flag).await?;
            continue;
        }

        if !status.is_success() {
            return Err(Dr3shError::Network {
                last_cause: format!("HTTP {status_code}"),
                retries: retry_count,
                offset: received_bytes,
            });
        }

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        if content_type.to_ascii_lowercase().contains("text/html") {
            let body_bytes = resp.bytes().await.unwrap_or_default();
            let snippet = sanitize_snippet(&body_bytes);
            if received_bytes == 0 {
                let _ = fs::remove_file(spec.part_path);
            }
            return Err(Dr3shError::HtmlInsteadOfZip {
                content_type,
                snippet,
                host: final_host,
            });
        }

        let is_partial = status_code == 206;
        if !is_partial && received_bytes > 0 {
            log_event(
                Some(spec.op_id),
                "DOWNLOAD",
                "INFO",
                "Server returned 200 OK instead of 206; restarting file from byte 0",
                &[("host", final_host.clone())],
            );
            hasher = Sha256::new();
            received_bytes = 0;
            magic_checked = false;
        }

        let total_from_headers = if is_partial {
            resp.headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_total_from_content_range)
                .or_else(|| resp.content_length().map(|cl| received_bytes + cl))
        } else {
            resp.content_length()
        };

        if let (Some(exp), Some(actual_tot)) =
            (spec.expected_size.filter(|&s| s > 0), total_from_headers)
            && exp != actual_tot
        {
            return Err(Dr3shError::SizeMismatch {
                expected: exp,
                actual: actual_tot,
            });
        }

        let total_bytes = total_from_headers
            .or(spec.expected_size.filter(|&s| s > 0))
            .unwrap_or(0);

        let (chunk_tx, mut chunk_rx) = mpsc::channel::<Vec<u8>>(16);
        let writer_path = spec.part_path.to_path_buf();
        let initial_hasher = hasher;
        let initial_received = received_bytes;
        let append_mode = is_partial && received_bytes > 0;
        let writer_handle =
            tokio::task::spawn_blocking(move || -> Result<(u64, Sha256), Dr3shError> {
                let file = OpenOptions::new()
                    .create(true)
                    .write(true)
                    .append(append_mode)
                    .truncate(!append_mode)
                    .open(&writer_path)
                    .map_err(|e| map_io_error(&writer_path, e))?;
                let mut writer = BufWriter::with_capacity(2 * 1024 * 1024, file);
                let mut h = initial_hasher;
                let mut total_written = initial_received;
                while let Some(buf) = chunk_rx.blocking_recv() {
                    if let Err(e) = writer.write_all(&buf) {
                        drop(writer);
                        return Err(map_io_error(&writer_path, e));
                    }
                    h.update(&buf);
                    total_written += buf.len() as u64;
                }
                if let Err(e) = writer.flush() {
                    return Err(map_io_error(&writer_path, e));
                }
                Ok((total_written, h))
            });

        let attempt_start = Instant::now();
        let bytes_at_attempt_start = received_bytes;
        let mut bytes_since_retry_reset: u64 = 0;
        let mut last_emit = Instant::now();
        let mut stall_window_30s: VecDeque<(Instant, u64)> = VecDeque::new();
        stall_window_30s.push_back((attempt_start, received_bytes));
        let mut speed_window_10s: VecDeque<(Instant, u64)> = VecDeque::new();
        speed_window_10s.push_back((attempt_start, received_bytes));
        let mut header_buf: Vec<u8> = Vec::new();
        let mut attempt_retry_cause: Option<String> = None;

        loop {
            if spec.cancel_flag.load(Ordering::Relaxed) {
                drop(chunk_tx);
                let _ = writer_handle.await;
                return Err(Dr3shError::Cancelled {
                    stage: "DOWNLOAD".to_string(),
                });
            }

            let chunk_fut = tokio::time::timeout(Duration::from_secs(20), resp.chunk());
            let chunk_res = tokio::select! {
                r = chunk_fut => r,
                _ = wait_until_cancelled(spec.cancel_flag) => {
                    drop(chunk_tx);
                    let _ = writer_handle.await;
                    return Err(Dr3shError::Cancelled { stage: "DOWNLOAD".to_string() });
                }
            };

            let chunk_opt = match chunk_res {
                Ok(Ok(c)) => c,
                Ok(Err(e)) => {
                    attempt_retry_cause = Some(format!("stream error: {e}"));
                    break;
                }
                Err(_) => {
                    attempt_retry_cause = Some("no bytes received for 20s (stall)".to_string());
                    break;
                }
            };

            let Some(chunk) = chunk_opt else {
                break;
            };

            if !magic_checked {
                header_buf.extend_from_slice(&chunk[..chunk.len().min(2048 - header_buf.len())]);
                if header_buf.len() >= 4 {
                    if header_buf[..4] != [0x50, 0x4B, 0x03, 0x04] {
                        drop(chunk_tx);
                        let _ = writer_handle.await;
                        let _ = fs::remove_file(spec.part_path);
                        let snippet = sanitize_snippet(&header_buf);
                        return Err(Dr3shError::HtmlInsteadOfZip {
                            content_type: if content_type.is_empty() {
                                "unknown".to_string()
                            } else {
                                content_type
                            },
                            snippet,
                            host: final_host,
                        });
                    }
                    magic_checked = true;
                }
            }

            let chunk_len = chunk.len() as u64;
            if chunk_tx.send(chunk.to_vec()).await.is_err() {
                attempt_retry_cause = Some("writer channel closed".to_string());
                break;
            }
            received_bytes += chunk_len;
            bytes_since_retry_reset += chunk_len;

            if retry_count > 0 && bytes_since_retry_reset >= 5 * 1024 * 1024 {
                retry_count = 0;
                bytes_since_retry_reset = 0;
            }

            let now = Instant::now();
            stall_window_30s.push_back((now, received_bytes));
            while stall_window_30s
                .front()
                .is_some_and(|(t, _)| now.duration_since(*t) > Duration::from_secs(30))
            {
                stall_window_30s.pop_front();
            }

            let attempt_elapsed = now.duration_since(attempt_start).as_secs_f64();
            if attempt_elapsed >= 30.0
                && let Some(&(oldest_t, oldest_b)) = stall_window_30s.front()
            {
                let win_secs = now.duration_since(oldest_t).as_secs_f64();
                if win_secs >= 25.0 {
                    let overall_speed =
                        (received_bytes.saturating_sub(bytes_at_attempt_start)) as f64
                            / attempt_elapsed;
                    let recent_speed =
                        (received_bytes.saturating_sub(oldest_b)) as f64 / win_secs;
                    if overall_speed > 10_240.0 && recent_speed < overall_speed * 0.05 {
                        attempt_retry_cause = Some(format!(
                            "speed stall: recent {:.0} B/s < 5% of average {:.0} B/s",
                            recent_speed, overall_speed
                        ));
                        break;
                    }
                }
            }

            let elapsed_emit = now.duration_since(last_emit).as_secs_f64();
            if elapsed_emit >= 0.15 || (total_bytes > 0 && received_bytes >= total_bytes) {
                let (speed_bps, eta_seconds) = update_speed_window_and_eta(
                    &mut speed_window_10s,
                    now,
                    received_bytes,
                    total_bytes,
                    &mut smoothed_speed,
                );
                let pct = if total_bytes > 0 {
                    let ratio = (received_bytes as f64 / total_bytes as f64).clamp(0.0, 1.0);
                    Some((6.0 + ratio * 62.0).round() as u32)
                } else {
                    None
                };

                if let Some(app_handle) = spec.app {
                    emit_progress_detailed(
                        app_handle,
                        spec.op_id,
                        spec.server_id,
                        "download",
                        format!("Descargando {}...", spec.server_title),
                        received_bytes,
                        total_bytes,
                        speed_bps,
                        retry_badge.clone(),
                        pct,
                        eta_seconds,
                        None,
                        None,
                    );
                }
                last_emit = now;
            }
        }

        drop(chunk_tx);
        let (written_total, returned_hasher) = writer_handle
            .await
            .map_err(|e| Dr3shError::unknown(format!("Fallback writer task join error: {e}")))??;
        received_bytes = written_total;
        hasher = returned_hasher;

        if let Some(cause) = attempt_retry_cause {
            if retry_count >= MAX_RETRIES {
                return Err(Dr3shError::Network {
                    last_cause: cause,
                    retries: retry_count,
                    offset: received_bytes,
                });
            }
            retry_count += 1;
            total_retries_used += 1;
            let wait = backoff_with_jitter(retry_count);
            log_event(
                Some(spec.op_id),
                "DOWNLOAD",
                "WARN",
                &format!("retry {retry_count}/{MAX_RETRIES}"),
                &[
                    ("cause", cause),
                    ("at_byte", received_bytes.to_string()),
                    ("total", total_bytes.to_string()),
                    ("host", final_host),
                    ("wait_ms", wait.as_millis().to_string()),
                ],
            );
            if let Some(app_handle) = spec.app {
                emit_progress_detailed(
                    app_handle,
                    spec.op_id,
                    spec.server_id,
                    "download",
                    format!("Reconectando ({retry_count}/{MAX_RETRIES})..."),
                    received_bytes,
                    total_bytes,
                    0,
                    Some(RetryInfo {
                        attempt: retry_count,
                        max: MAX_RETRIES,
                    }),
                    None,
                    None,
                    None,
                    None,
                );
            }
            sleep_or_cancel(wait, spec.cancel_flag).await?;
            continue;
        }

        if !magic_checked && received_bytes > 0 {
            let _ = fs::remove_file(spec.part_path);
            let snippet = sanitize_snippet(&header_buf);
            return Err(Dr3shError::HtmlInsteadOfZip {
                content_type,
                snippet,
                host: final_host,
            });
        }

        if let Some(app_handle) = spec.app
            && total_bytes > 0
        {
            emit_progress_detailed(
                app_handle,
                spec.op_id,
                spec.server_id,
                "download",
                format!("Descargando {}...", spec.server_title),
                received_bytes,
                total_bytes,
                smoothed_speed.max(0.0) as u64,
                None,
                Some(68),
                None,
                None,
                None,
            );
        }

        let digest = hasher.finalize();
        let actual_hash: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        return Ok((
            received_bytes,
            actual_hash,
            smoothed_speed.max(0.0) as u64,
            total_retries_used,
        ));
    }
}

async fn wait_until_cancelled(flag: &Arc<AtomicBool>) {
    while !flag.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(80)).await;
    }
}

pub fn map_io_error(path: &Path, err: std::io::Error) -> Dr3shError {
    let raw_os = err.raw_os_error();
    let os_code = raw_os
        .map(|c| c.to_string())
        .unwrap_or_else(|| err.kind().to_string());
    let path_str = path.to_string_lossy().to_string();

    if raw_os == Some(32) || raw_os == Some(33) {
        return Dr3shError::FsLocked {
            path: path_str,
            os_code,
        };
    }
    if err.kind() == std::io::ErrorKind::PermissionDenied || raw_os == Some(5) {
        return Dr3shError::FsPermission {
            path: path_str,
            os_code,
        };
    }
    if raw_os == Some(112) {
        let drive = drive_root_of(path).to_string_lossy().to_string();
        return Dr3shError::PreflightNoSpace {
            drive,
            required: "espacio adicional".to_string(),
            free: "0 MB".to_string(),
            path: path_str,
        };
    }
    Dr3shError::FsPermission {
        path: path_str,
        os_code: format!("{os_code}: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn build_valid_zip_bytes() -> (Vec<u8>, String) {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            zip.start_file("mods/test_mod.jar", options).unwrap();
            zip.write_all(&vec![0xABu8; 64 * 1024]).unwrap();
            zip.finish().unwrap();
        }
        let bytes = buf.into_inner();
        let sha: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        (bytes, sha)
    }

    #[tokio::test]
    async fn test_sim_http_range_resume_and_200_fallback() {
        let (zip_bytes, zip_sha) = build_valid_zip_bytes();
        let total_len = zip_bytes.len();
        let split_point = total_len / 2;

        // Server 1: Drops connection at split_point on request 1, serves 206 Partial Content on request 2
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let req_count = Arc::new(AtomicUsize::new(0));
        let req_count_srv = req_count.clone();
        let zip_srv = zip_bytes.clone();

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let num = req_count_srv.fetch_add(1, Ordering::SeqCst);
                let data = zip_srv.clone();
                tokio::spawn(async move {
                    let mut req_buf = [0u8; 2048];
                    let n = socket.read(&mut req_buf).await.unwrap_or(0);
                    let req_str = String::from_utf8_lossy(&req_buf[..n]);
                    if num == 0 {
                        // Send 200 OK header claiming full Content-Length, but drop after split_point
                        let hdr = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/zip\r\nContent-Length: {}\r\n\r\n",
                            data.len()
                        );
                        let _ = socket.write_all(hdr.as_bytes()).await;
                        let _ = socket.write_all(&data[..split_point]).await;
                        let _ = socket.shutdown().await;
                    } else {
                        assert!(req_str.to_ascii_lowercase().contains(&format!("range: bytes={split_point}-")));
                        let rem = &data[split_point..];
                        let hdr = format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Type: application/zip\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\n\r\n",
                            split_point,
                            data.len() - 1,
                            data.len(),
                            rem.len()
                        );
                        let _ = socket.write_all(hdr.as_bytes()).await;
                        let _ = socket.write_all(rem).await;
                        let _ = socket.shutdown().await;
                    }
                });
            }
        });

        let temp = tempfile::tempdir().unwrap();
        let part_path = temp.path().join("test_resume.part");
        let cancel = Arc::new(AtomicBool::new(false));
        let url = format!("http://{addr}/pack.zip");

        let outcome = download_zip_resumable(DownloadSpec {
            app: None,
            op_id: "sim_d1",
            server_id: "cobbleverse",
            server_title: "Desgraciamon",
            url: &url,
            part_path: &part_path,
            expected_size: Some(total_len as u64),
            expected_sha256: Some(&zip_sha),
            cancel_flag: &cancel,
        })
        .await
        .expect("resumable download should succeed");

        assert_eq!(outcome.total_bytes, total_len as u64);
        assert_eq!(outcome.sha256, zip_sha);
        assert_eq!(outcome.retries_used, 1);
    }

    #[tokio::test]
    async fn test_sim_html_and_fake_magic_bytes_rejected() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mode = Arc::new(AtomicUsize::new(0));
        let mode_srv = mode.clone();

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let m = mode_srv.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut req_buf = [0u8; 1024];
                    let _ = socket.read(&mut req_buf).await;
                    if m == 0 {
                        let body = b"<html><body>Error Gate</body></html>";
                        let hdr = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\r\n",
                            body.len()
                        );
                        let _ = socket.write_all(hdr.as_bytes()).await;
                        let _ = socket.write_all(body).await;
                    } else {
                        let body = b"<!DOCTYPE html>fake binary content";
                        let hdr = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n\r\n",
                            body.len()
                        );
                        let _ = socket.write_all(hdr.as_bytes()).await;
                        let _ = socket.write_all(body).await;
                    }
                    let _ = socket.shutdown().await;
                });
            }
        });

        let temp = tempfile::tempdir().unwrap();
        let part_path = temp.path().join("bad.part");
        let cancel = Arc::new(AtomicBool::new(false));
        let url = format!("http://{addr}/pack.zip");

        let err1 = download_zip_resumable(DownloadSpec {
            app: None,
            op_id: "sim_html1",
            server_id: "srv",
            server_title: "Srv",
            url: &url,
            part_path: &part_path,
            expected_size: None,
            expected_sha256: None,
            cancel_flag: &cancel,
        })
        .await
        .unwrap_err();
        assert_eq!(err1.code(), "E_HTML_INSTEAD_OF_ZIP");
        assert!(!part_path.exists());

        let err2 = download_zip_resumable(DownloadSpec {
            app: None,
            op_id: "sim_html2",
            server_id: "srv",
            server_title: "Srv",
            url: &url,
            part_path: &part_path,
            expected_size: None,
            expected_sha256: None,
            cancel_flag: &cancel,
        })
        .await
        .unwrap_err();
        assert_eq!(err2.code(), "E_HTML_INSTEAD_OF_ZIP");
        assert!(!part_path.exists());
    }

    #[tokio::test]
    async fn test_sim_corrupt_part_auto_redownload_and_404_no_retry() {
        let (zip_bytes, zip_sha) = build_valid_zip_bytes();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let zip_srv = zip_bytes.clone();

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let data = zip_srv.clone();
                tokio::spawn(async move {
                    let mut req_buf = [0u8; 1024];
                    let n = socket.read(&mut req_buf).await.unwrap_or(0);
                    let req_str = String::from_utf8_lossy(&req_buf[..n]);
                    if req_str.contains("GET /missing.zip") {
                        let hdr = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";
                        let _ = socket.write_all(hdr.as_bytes()).await;
                    } else {
                        let hdr = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/zip\r\nContent-Length: {}\r\n\r\n",
                            data.len()
                        );
                        let _ = socket.write_all(hdr.as_bytes()).await;
                        let _ = socket.write_all(&data).await;
                    }
                    let _ = socket.shutdown().await;
                });
            }
        });

        let temp = tempfile::tempdir().unwrap();
        let part_path = temp.path().join("corrupt.part");
        // Write a same-length file with valid PK\x03\x04 magic but corrupted payload
        let mut corrupted = zip_bytes.clone();
        let last_idx = corrupted.len() - 10;
        corrupted[last_idx] ^= 0xFF;
        fs::write(&part_path, &corrupted).unwrap();

        let cancel = Arc::new(AtomicBool::new(false));
        let url_ok = format!("http://{addr}/valid.zip");
        let outcome = download_zip_resumable(DownloadSpec {
            app: None,
            op_id: "sim_corrupt_recover",
            server_id: "srv",
            server_title: "Srv",
            url: &url_ok,
            part_path: &part_path,
            expected_size: Some(zip_bytes.len() as u64),
            expected_sha256: Some(&zip_sha),
            cancel_flag: &cancel,
        })
        .await
        .expect("should delete corrupt .part and redownload cleanly from 0");
        assert_eq!(outcome.sha256, zip_sha);

        // Now test 404 fails immediately without retry
        let url_404 = format!("http://{addr}/missing.zip");
        let part_404 = temp.path().join("404.part");
        let err_404 = download_zip_resumable(DownloadSpec {
            app: None,
            op_id: "sim_404",
            server_id: "srv",
            server_title: "Srv",
            url: &url_404,
            part_path: &part_404,
            expected_size: None,
            expected_sha256: None,
            cancel_flag: &cancel,
        })
        .await
        .unwrap_err();
        assert_eq!(err_404.code(), "E_HTTP_NOT_FOUND");
    }

    #[tokio::test]
    async fn test_sim_multipart_dynamic_sliding_window_and_sha256() {
        let mut raw_payload = vec![0u8; 96 * 1024];
        for (idx, b) in raw_payload.iter_mut().enumerate() {
            *b = ((idx.wrapping_mul(131).wrapping_add(17)) % 251) as u8;
        }
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file("mods/big_mod.jar", options).unwrap();
            zip.write_all(&raw_payload).unwrap();
            zip.finish().unwrap();
        }
        let zip_bytes = buf.into_inner();
        let zip_sha: String = Sha256::digest(&zip_bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let zip_srv = Arc::new(zip_bytes.clone());

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let data = Arc::clone(&zip_srv);
                tokio::spawn(async move {
                    let mut req_buf = [0u8; 2048];
                    let n = socket.read(&mut req_buf).await.unwrap_or(0);
                    let req_str = String::from_utf8_lossy(&req_buf[..n]);
                    let total = data.len();
                    if let Some(range_line) = req_str
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("range:"))
                        && let Some((_, spec)) = range_line.split_once('=')
                        && let Some((s_str, e_str)) = spec.trim().split_once('-')
                    {
                        let start = s_str.trim().parse::<usize>().unwrap_or(0);
                        let end = e_str
                            .trim()
                            .parse::<usize>()
                            .unwrap_or(total - 1)
                            .min(total - 1);
                        if start < 8192 && end > start {
                            tokio::time::sleep(Duration::from_millis(45)).await;
                        }
                        let slice = &data[start..=end];
                        let hdr = format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Type: application/zip\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\n\r\n",
                            start,
                            end,
                            total,
                            slice.len()
                        );
                        let _ = socket.write_all(hdr.as_bytes()).await;
                        let _ = socket.write_all(slice).await;
                    } else {
                        let hdr = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/zip\r\nContent-Length: {}\r\n\r\n",
                            total
                        );
                        let _ = socket.write_all(hdr.as_bytes()).await;
                        let _ = socket.write_all(&data).await;
                    }
                    let _ = socket.shutdown().await;
                });
            }
        });

        let temp = tempfile::tempdir().unwrap();
        let part_path = temp.path().join("multipart_dyn.part");
        let cancel = Arc::new(AtomicBool::new(false));
        let url = format!("http://{addr}/pack.zip");
        let cfg = MultipartConfig {
            threshold: 16 * 1024,
            chunk_size: 4 * 1024,
            concurrency: 4,
            max_reorder_ahead: 8,
        };

        let spec = DownloadSpec {
            app: None,
            op_id: "sim_dyn_mp",
            server_id: "srv",
            server_title: "Srv",
            url: &url,
            part_path: &part_path,
            expected_size: Some(zip_bytes.len() as u64),
            expected_sha256: Some(&zip_sha),
            cancel_flag: &cancel,
        };

        let (sz, sha, _, _) = try_multipart_download_with_config(&spec, cfg)
            .await
            .expect("dynamic multipart should succeed")
            .expect("should return Some outcome");
        assert_eq!(sz, zip_bytes.len() as u64);
        assert_eq!(sha, zip_sha);
        assert_eq!(fs::read(&part_path).unwrap(), zip_bytes);

        // Also test seamless sequential prefix resume via dynamic multipart
        let prefix_len = 40 * 1024;
        fs::write(&part_path, &zip_bytes[..prefix_len]).unwrap();
        let (sz2, sha2, _, _) = try_multipart_download_with_config(&spec, cfg)
            .await
            .expect("resumed dynamic multipart should succeed")
            .expect("should return Some outcome on resume");
        assert_eq!(sz2, zip_bytes.len() as u64);
        assert_eq!(sha2, zip_sha);
        assert_eq!(fs::read(&part_path).unwrap(), zip_bytes);
    }

    #[tokio::test]
    async fn test_sim_multipart_transient_drop_recovers_without_deleting_part() {
        let mut raw_payload = vec![0u8; 128 * 1024];
        for (idx, b) in raw_payload.iter_mut().enumerate() {
            *b = ((idx.wrapping_mul(97).wrapping_add(31)) % 251) as u8;
        }
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file("mods/flaky_mod.jar", options).unwrap();
            zip.write_all(&raw_payload).unwrap();
            zip.finish().unwrap();
        }
        let zip_bytes = buf.into_inner();
        let zip_sha: String = Sha256::digest(&zip_bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let zip_srv = Arc::new(zip_bytes.clone());
        let dropped_chunk_once = Arc::new(AtomicBool::new(false));

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let data = Arc::clone(&zip_srv);
                let drop_flag = Arc::clone(&dropped_chunk_once);
                tokio::spawn(async move {
                    let mut req_buf = [0u8; 2048];
                    let n = socket.read(&mut req_buf).await.unwrap_or(0);
                    let req_str = String::from_utf8_lossy(&req_buf[..n]);
                    let total = data.len();
                    if let Some(range_line) = req_str
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("range:"))
                        && let Some((_, spec)) = range_line.split_once('=')
                        && let Some((s_str, e_str)) = spec.trim().split_once('-')
                    {
                        let start = s_str.trim().parse::<usize>().unwrap_or(0);
                        let end = e_str
                            .trim()
                            .parse::<usize>()
                            .unwrap_or(total - 1)
                            .min(total - 1);
                        let slice = &data[start..=end];
                        let hdr = format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Type: application/zip\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\n\r\n",
                            start,
                            end,
                            total,
                            slice.len()
                        );
                        let _ = socket.write_all(hdr.as_bytes()).await;
                        if start == 32768 && end > start && !drop_flag.swap(true, Ordering::SeqCst) {
                            let _ = socket.write_all(&slice[..slice.len() / 2]).await;
                            let _ = socket.shutdown().await;
                            return;
                        }
                        let _ = socket.write_all(slice).await;
                    } else {
                        let hdr = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/zip\r\nContent-Length: {}\r\n\r\n",
                            total
                        );
                        let _ = socket.write_all(hdr.as_bytes()).await;
                        let _ = socket.write_all(&data).await;
                    }
                    let _ = socket.shutdown().await;
                });
            }
        });

        let temp = tempfile::tempdir().unwrap();
        let part_path = temp.path().join("multipart_flaky.part");
        let cancel = Arc::new(AtomicBool::new(false));
        let url = format!("http://{addr}/pack.zip");
        let cfg = MultipartConfig {
            threshold: 16 * 1024,
            chunk_size: 32 * 1024,
            concurrency: 3,
            max_reorder_ahead: 4,
        };
        let spec = DownloadSpec {
            app: None,
            op_id: "sim_mp_drop",
            server_id: "srv",
            server_title: "Srv",
            url: &url,
            part_path: &part_path,
            expected_size: Some(zip_bytes.len() as u64),
            expected_sha256: Some(&zip_sha),
            cancel_flag: &cancel,
        };

        let (sz, sha, _, retries) = try_multipart_download_with_config(&spec, cfg)
            .await
            .expect("flaky multipart should succeed on retry")
            .expect("should return Some outcome");
        assert_eq!(sz, zip_bytes.len() as u64);
        assert_eq!(sha, zip_sha);
        assert!(retries >= 1, "expected at least 1 retry, got {retries}");
        assert_eq!(fs::read(&part_path).unwrap(), zip_bytes);
    }
}
