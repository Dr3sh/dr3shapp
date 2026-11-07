use crate::dr3sh::errors::Dr3shError;
use crate::dr3sh::logger::{log_event, sanitize_url};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tauri_plugin_http::reqwest;

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
        "verify" | "extract" | "apply" => "extracting",
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
        retry,
        percent,
        phase: legacy_phase.to_string(),
        phase_text: text,
        downloaded_formatted: dl_fmt,
        total_formatted: tot_fmt,
        speed_formatted: spd_fmt,
    };
    let _ = app.emit("dr3sh://progress", &payload);
    let _ = app.emit("dr3sh-download-progress", &payload);
}

static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    let ua = format!(
        "Dr3shLauncher/{} (+https://github.com/Dr3sh/dr3shapp)",
        env!("CARGO_PKG_VERSION")
    );
    reqwest::Client::builder()
        .user_agent(ua)
        .tcp_nodelay(true)
        .connect_timeout(Duration::from_secs(20))
        .tcp_keepalive(Some(Duration::from_secs(10)))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .expect("failed to build shared Dr3sh HTTP client")
});

pub fn shared_http_client() -> &'static reqwest::Client {
    &HTTP_CLIENT
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

fn hash_existing_part_file(path: &Path) -> Result<(Sha256, u64, Option<[u8; 4]>), std::io::Error> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut reader = BufReader::with_capacity(512 * 1024, file);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut first_four: Option<[u8; 4]> = None;
    let mut total_read: u64 = 0;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        if first_four.is_none() && total_read == 0 && n >= 4 {
            first_four = Some([buf[0], buf[1], buf[2], buf[3]]);
        }
        hasher.update(&buf[..n]);
        total_read += n as u64;
    }
    let _ = len;
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

        let (mut hasher, mut received_bytes, mut magic_checked) = if spec.part_path.exists() {
            match hash_existing_part_file(spec.part_path) {
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
                tokio::time::sleep(wait).await;
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
                tokio::time::sleep(wait).await;
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
            tokio::time::sleep(wait).await;
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

        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .append(is_partial && received_bytes > 0)
            .truncate(!is_partial || received_bytes == 0)
            .open(spec.part_path)
            .map_err(|e| map_io_error(spec.part_path, e))?;
        let mut writer = BufWriter::with_capacity(1024 * 1024, file);

        let attempt_start = Instant::now();
        let bytes_at_attempt_start = received_bytes;
        let mut bytes_since_retry_reset: u64 = 0;
        let mut last_emit = Instant::now();
        let mut last_emit_bytes = received_bytes;
        let mut window_30s: VecDeque<(Instant, u64)> = VecDeque::new();
        window_30s.push_back((attempt_start, received_bytes));
        let mut header_buf: Vec<u8> = Vec::new();
        let mut attempt_retry_cause: Option<String> = None;

        loop {
            if spec.cancel_flag.load(Ordering::Relaxed) {
                let _ = writer.flush();
                return Err(Dr3shError::Cancelled {
                    stage: "DOWNLOAD".to_string(),
                });
            }

            let chunk_fut = tokio::time::timeout(Duration::from_secs(20), resp.chunk());
            let chunk_res = tokio::select! {
                r = chunk_fut => r,
                _ = wait_until_cancelled(spec.cancel_flag) => {
                    let _ = writer.flush();
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
                        drop(writer);
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

            if let Err(e) = writer.write_all(&chunk) {
                drop(writer);
                return Err(map_io_error(spec.part_path, e));
            }
            hasher.update(&chunk);
            let chunk_len = chunk.len() as u64;
            received_bytes += chunk_len;
            bytes_since_retry_reset += chunk_len;

            if retry_count > 0 && bytes_since_retry_reset >= 5 * 1024 * 1024 {
                retry_count = 0;
                bytes_since_retry_reset = 0;
            }

            let now = Instant::now();
            window_30s.push_back((now, received_bytes));
            while window_30s
                .front()
                .is_some_and(|(t, _)| now.duration_since(*t) > Duration::from_secs(30))
            {
                window_30s.pop_front();
            }

            let attempt_elapsed = now.duration_since(attempt_start).as_secs_f64();
            if attempt_elapsed >= 30.0
                && let Some(&(oldest_t, oldest_b)) = window_30s.front()
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
            if elapsed_emit >= 0.10 || (total_bytes > 0 && received_bytes >= total_bytes) {
                let delta = received_bytes.saturating_sub(last_emit_bytes);
                let inst_speed = (delta as f64) / elapsed_emit.max(0.05);
                smoothed_speed = if smoothed_speed <= 0.0 {
                    inst_speed
                } else {
                    smoothed_speed * 0.70 + inst_speed * 0.30
                };
                let speed_bps = smoothed_speed.max(0.0) as u64;
                let pct = if total_bytes > 0 {
                    let ratio = (received_bytes as f64 / total_bytes as f64).clamp(0.0, 1.0);
                    Some((10.0 + ratio * 70.0).round() as u32)
                } else {
                    None
                };

                if let Some(app_handle) = spec.app {
                    emit_progress(
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
                    );
                }
                last_emit = now;
                last_emit_bytes = received_bytes;
            }
        }

        if let Err(e) = writer.flush() {
            return Err(map_io_error(spec.part_path, e));
        }
        drop(writer);

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
                emit_progress(
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
                );
            }
            tokio::time::sleep(wait).await;
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
}
