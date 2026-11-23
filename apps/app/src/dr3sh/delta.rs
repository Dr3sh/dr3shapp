use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use flate2::read::DeflateDecoder;
use sha2::{Digest, Sha256};
use tauri::AppHandle;
use tauri_plugin_http::reqwest;

use crate::dr3sh::download::{
    emit_progress_detailed, multipart_http_client, shared_http_client,
};
use crate::dr3sh::errors::Dr3shError;
use crate::dr3sh::logger::{log_event, sanitize_url};
use crate::dr3sh::manifest::{validate_relative_path, PlanActionKind, ReconciliationPlan};

const EOCD_TAIL_WINDOW: u64 = 65_558;
const MAX_CD_BYTES: u64 = 64 * 1024 * 1024;
const BATCH_COALESCE_GAP: u64 = 65_536;
const MAX_BATCH_BYTES: u64 = 8 * 1024 * 1024;
const DELTA_WORKERS: usize = 6;
const LFH_OVERHEAD_PADDING: u64 = 1024;

#[derive(Debug, Clone)]
pub struct PartialZipOutcome {
    pub downloaded_bytes: u64,
    pub files_extracted: u64,
    pub avg_speed_bps: u64,
}

#[derive(Debug)]
pub enum PartialZipAttempt {
    Completed(PartialZipOutcome),
    FallbackToFull(String),
}

pub fn should_use_partial_zip(
    zip_size: u64,
    plan: &ReconciliationPlan,
    min_zip_size: u64,
) -> bool {
    plan.needs_zip()
        && zip_size >= min_zip_size
        && plan.write_bytes_total < (zip_size.saturating_mul(65) / 100)
}

#[derive(Debug, Clone)]
struct CdEntry {
    raw_name: String,
    compression_method: u16,
    compressed_size: u64,
    uncompressed_size: u64,
    name_len: u64,
    extra_len: u64,
    local_header_offset: u64,
}

#[derive(Debug, Clone)]
struct MatchedDeltaEntry {
    canonical_rel_path: String,
    expected_sha256: Option<String>,
    compression_method: u16,
    compressed_size: u64,
    uncompressed_size: u64,
    local_header_offset: u64,
    est_end: u64,
}

#[derive(Debug, Clone)]
struct RangeBatch {
    range_start: u64,
    range_end: u64,
    entries: Vec<MatchedDeltaEntry>,
}

enum ExtractOneResult {
    Ok(u64),
    Fallback(String),
    Err(Dr3shError),
}

fn read_u16_le(buf: &[u8], off: usize) -> Option<u16> {
    let slice: [u8; 2] = buf.get(off..off + 2)?.try_into().ok()?;
    Some(u16::from_le_bytes(slice))
}

fn read_u32_le(buf: &[u8], off: usize) -> Option<u32> {
    let slice: [u8; 4] = buf.get(off..off + 4)?.try_into().ok()?;
    Some(u32::from_le_bytes(slice))
}

fn read_u64_le(buf: &[u8], off: usize) -> Option<u64> {
    let slice: [u8; 8] = buf.get(off..off + 8)?.try_into().ok()?;
    Some(u64::from_le_bytes(slice))
}

fn parse_content_range_bounds(header_val: &str) -> Option<(u64, u64, Option<u64>)> {
    let trimmed = header_val.trim();
    let rest = trimmed
        .strip_prefix("bytes ")
        .or_else(|| trimmed.strip_prefix("BYTES "))?;
    let (range_part, total_part) = rest.split_once('/')?;
    let (start_s, end_s) = range_part.split_once('-')?;
    let start = start_s.trim().parse::<u64>().ok()?;
    let end = end_s.trim().parse::<u64>().ok()?;
    let total = if total_part.trim() == "*" {
        None
    } else {
        total_part.trim().parse::<u64>().ok()
    };
    Some((start, end, total))
}

fn looks_like_html_payload(bytes: &[u8]) -> bool {
    let probe_len = bytes.len().min(256);
    let head = String::from_utf8_lossy(&bytes[..probe_len])
        .trim_start()
        .to_ascii_lowercase();
    head.starts_with("<!doctype") || head.starts_with("<html")
}

async fn resolve_fresh_cdn_url(original_url: &str) -> Option<String> {
    let client = shared_http_client();
    let resp = client
        .get(original_url)
        .header(reqwest::header::RANGE, "bytes=0-0")
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .ok()?;
    let final_url = resp.url().to_string();
    drop(resp);
    Some(final_url)
}

async fn fetch_range_with_retries(
    original_url: &str,
    resolved_url: &Arc<tokio::sync::RwLock<String>>,
    start: u64,
    end: u64,
    cancel_token: &Arc<AtomicBool>,
) -> Result<Result<(Bytes, u64, Option<u64>), String>, Dr3shError> {
    const MAX_RETRIES: u32 = 4;
    let client = multipart_http_client();
    let range_header = format!("bytes={start}-{end}");

    for attempt in 0..=MAX_RETRIES {
        if cancel_token.load(Ordering::Relaxed) {
            return Err(Dr3shError::Cancelled {
                stage: "download".to_string(),
            });
        }

        let target_url = resolved_url.read().await.clone();
        let req_res = client
            .get(&target_url)
            .header(reqwest::header::RANGE, &range_header)
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .timeout(Duration::from_secs(30))
            .send()
            .await;

        match req_res {
            Ok(resp) => {
                let status = resp.status();
                if status == reqwest::StatusCode::PARTIAL_CONTENT {
                    let final_url = resp.url().to_string();
                    if final_url != target_url {
                        *resolved_url.write().await = final_url;
                    }
                    let content_range = resp
                        .headers()
                        .get(reqwest::header::CONTENT_RANGE)
                        .and_then(|v| v.to_str().ok())
                        .and_then(parse_content_range_bounds);
                    let body = match resp.bytes().await {
                        Ok(b) => b,
                        Err(err) => {
                            if attempt < MAX_RETRIES {
                                tokio::time::sleep(Duration::from_millis(500 * (attempt as u64 + 1))).await;
                                continue;
                            }
                            return Ok(Err(format!("Error leyendo rango HTTP ({err})")));
                        }
                    };
                    if looks_like_html_payload(&body) {
                        return Ok(Err("Respuesta HTML inesperada en petición parcial".to_string()));
                    }
                    let cr_start = content_range.map(|(s, _, _)| s).unwrap_or(start);
                    let cr_total = content_range.and_then(|(_, _, t)| t);
                    return Ok(Ok((body, cr_start, cr_total)));
                } else if status == reqwest::StatusCode::OK {
                    return Ok(Err(
                        "El servidor devolvió 200 OK ignorando la cabecera Range".to_string(),
                    ));
                } else if (status == reqwest::StatusCode::FORBIDDEN
                    || status == reqwest::StatusCode::UNAUTHORIZED)
                    && attempt < MAX_RETRIES
                {
                    if let Some(fresh) = resolve_fresh_cdn_url(original_url).await {
                        *resolved_url.write().await = fresh;
                    }
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    continue;
                } else if (status == reqwest::StatusCode::TOO_MANY_REQUESTS
                    || status.is_server_error())
                    && attempt < MAX_RETRIES
                {
                    tokio::time::sleep(Duration::from_millis(700 * (attempt as u64 + 1))).await;
                    continue;
                } else {
                    return Ok(Err(format!(
                        "HTTP {} al solicitar rango {} ({})",
                        status.as_u16(),
                        range_header,
                        sanitize_url(original_url)
                    )));
                }
            }
            Err(err) => {
                if attempt < MAX_RETRIES {
                    tokio::time::sleep(Duration::from_millis(600 * (attempt as u64 + 1))).await;
                    continue;
                }
                return Ok(Err(format!("Fallo de red solicitando rango HTTP: {err}")));
            }
        }
    }

    Ok(Err("Reintentos agotados en petición HTTP Range".to_string()))
}

fn find_eocd(tail: &[u8]) -> Option<usize> {
    if tail.len() < 22 {
        return None;
    }
    let min_end = tail.len() - 22;
    let search_start = tail.len().saturating_sub(22 + 65_535);
    let mut pos = min_end;
    loop {
        if tail[pos..pos + 4] == [0x50, 0x4b, 0x05, 0x06] {
            let comment_len = read_u16_le(tail, pos + 20)? as usize;
            if pos + 22 + comment_len == tail.len() {
                return Some(pos);
            }
        }
        if pos == search_start {
            break;
        }
        pos -= 1;
    }
    None
}

fn parse_cd_entries(cd: &[u8]) -> Result<Vec<CdEntry>, String> {
    let mut entries = Vec::new();
    let mut cursor = 0usize;

    while cursor < cd.len() {
        if cursor + 46 > cd.len() {
            return Err("Entrada del Central Directory truncada".to_string());
        }
        if cd[cursor..cursor + 4] != [0x50, 0x4b, 0x01, 0x02] {
            return Err("Firma inválida en Central Directory".to_string());
        }
        let compression_method =
            read_u16_le(cd, cursor + 10).ok_or("No se pudo leer método de compresión")?;
        let comp_size_32 = read_u32_le(cd, cursor + 20).ok_or("comp_size")? as u64;
        let uncomp_size_32 = read_u32_le(cd, cursor + 24).ok_or("uncomp_size")? as u64;
        let name_len = read_u16_le(cd, cursor + 28).ok_or("name_len")? as usize;
        let extra_len = read_u16_le(cd, cursor + 30).ok_or("extra_len")? as usize;
        let comment_len = read_u16_le(cd, cursor + 32).ok_or("comment_len")? as usize;
        let local_header_off_32 = read_u32_le(cd, cursor + 42).ok_or("lfh_off")? as u64;

        let name_start = cursor + 46;
        let name_end = name_start + name_len;
        let extra_end = name_end + extra_len;
        let next_cursor = extra_end + comment_len;
        if next_cursor > cd.len() {
            return Err("Extensión de entrada del Central Directory fuera de rango".to_string());
        }

        let mut uncompressed_size = uncomp_size_32;
        let mut compressed_size = comp_size_32;
        let mut local_header_offset = local_header_off_32;

        if uncomp_size_32 == 0xFFFF_FFFF
            || comp_size_32 == 0xFFFF_FFFF
            || local_header_off_32 == 0xFFFF_FFFF
        {
            let extra = &cd[name_end..extra_end];
            let mut ex_pos = 0usize;
            while ex_pos + 4 <= extra.len() {
                let header_id = read_u16_le(extra, ex_pos).unwrap_or(0);
                let data_size = read_u16_le(extra, ex_pos + 2).unwrap_or(0) as usize;
                let data_start = ex_pos + 4;
                let data_end = data_start + data_size;
                if data_end > extra.len() {
                    break;
                }
                if header_id == 0x0001 {
                    let z64 = &extra[data_start..data_end];
                    let mut z_off = 0usize;
                    if uncomp_size_32 == 0xFFFF_FFFF && z_off + 8 <= z64.len() {
                        uncompressed_size = read_u64_le(z64, z_off).unwrap_or(uncompressed_size);
                        z_off += 8;
                    }
                    if comp_size_32 == 0xFFFF_FFFF && z_off + 8 <= z64.len() {
                        compressed_size = read_u64_le(z64, z_off).unwrap_or(compressed_size);
                        z_off += 8;
                    }
                    if local_header_off_32 == 0xFFFF_FFFF && z_off + 8 <= z64.len() {
                        local_header_offset =
                            read_u64_le(z64, z_off).unwrap_or(local_header_offset);
                    }
                    break;
                }
                ex_pos = data_end;
            }
        }

        let raw_name = String::from_utf8_lossy(&cd[name_start..name_end]).to_string();
        entries.push(CdEntry {
            raw_name,
            compression_method,
            compressed_size,
            uncompressed_size,
            name_len: name_len as u64,
            extra_len: extra_len as u64,
            local_header_offset,
        });

        cursor = next_cursor;
    }

    Ok(entries)
}

fn decompress_and_write_entry(
    compressed: Bytes,
    compression_method: u16,
    out_path: &Path,
    expected_sha256: Option<&str>,
    expected_uncompressed_size: u64,
    canonical_rel_path: &str,
    cancel_token: &AtomicBool,
) -> ExtractOneResult {
    if cancel_token.load(Ordering::Relaxed) {
        return ExtractOneResult::Err(Dr3shError::Cancelled {
            stage: "download".to_string(),
        });
    }

    let out_file = match File::create(out_path) {
        Ok(f) => f,
        Err(e) => {
            return ExtractOneResult::Err(Dr3shError::FsPermission {
                path: out_path.to_string_lossy().to_string(),
                os_code: e.raw_os_error().unwrap_or(-1).to_string(),
            });
        }
    };

    let mut writer = BufWriter::with_capacity(256 * 1024, out_file);
    let mut hasher = Sha256::new();
    let mut written_len = 0u64;
    let mut buf = vec![0u8; 256 * 1024];

    match compression_method {
        0 => {
            let mut offset = 0usize;
            while offset < compressed.len() {
                if cancel_token.load(Ordering::Relaxed) {
                    drop(writer);
                    let _ = fs::remove_file(out_path);
                    return ExtractOneResult::Err(Dr3shError::Cancelled {
                        stage: "download".to_string(),
                    });
                }
                let end = (offset + buf.len()).min(compressed.len());
                let slice = &compressed[offset..end];
                if let Err(e) = writer.write_all(slice) {
                    return ExtractOneResult::Err(Dr3shError::FsPermission {
                        path: out_path.to_string_lossy().to_string(),
                        os_code: e.raw_os_error().unwrap_or(-1).to_string(),
                    });
                }
                hasher.update(slice);
                written_len += slice.len() as u64;
                offset = end;
            }
        }
        8 => {
            let mut decoder = DeflateDecoder::new(&compressed[..]);
            loop {
                if cancel_token.load(Ordering::Relaxed) {
                    drop(writer);
                    let _ = fs::remove_file(out_path);
                    return ExtractOneResult::Err(Dr3shError::Cancelled {
                        stage: "download".to_string(),
                    });
                }
                let n = match decoder.read(&mut buf) {
                    Ok(n) => n,
                    Err(e) => {
                        return ExtractOneResult::Fallback(format!(
                            "Error descomprimiendo entrada parcial {canonical_rel_path}: {e}"
                        ));
                    }
                };
                if n == 0 {
                    break;
                }
                if let Err(e) = writer.write_all(&buf[..n]) {
                    return ExtractOneResult::Err(Dr3shError::FsPermission {
                        path: out_path.to_string_lossy().to_string(),
                        os_code: e.raw_os_error().unwrap_or(-1).to_string(),
                    });
                }
                hasher.update(&buf[..n]);
                written_len += n as u64;
            }
        }
        other => {
            return ExtractOneResult::Fallback(format!(
                "Método de compresión ZIP no soportado en modo delta ({other}) para {canonical_rel_path}"
            ));
        }
    }

    if let Err(e) = writer.flush() {
        return ExtractOneResult::Err(Dr3shError::FsPermission {
            path: out_path.to_string_lossy().to_string(),
            os_code: e.raw_os_error().unwrap_or(-1).to_string(),
        });
    }
    drop(writer);

    if written_len != expected_uncompressed_size {
        return ExtractOneResult::Fallback(format!(
            "Tamaño inesperado al extraer {canonical_rel_path}: esperado {expected_uncompressed_size}, obtenido {written_len}"
        ));
    }

    if let Some(exp_sha) = expected_sha256.map(str::trim).filter(|s| !s.is_empty()) {
        let actual_sha: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if !actual_sha.eq_ignore_ascii_case(exp_sha) {
            return ExtractOneResult::Fallback(format!(
                "SHA-256 distinto en descarga parcial de {canonical_rel_path}"
            ));
        }
    }

    ExtractOneResult::Ok(written_len)
}

#[allow(clippy::too_many_arguments)]
pub async fn download_and_extract_partial_zip(
    app: Option<&AppHandle>,
    op_id: &str,
    server_id: &str,
    zip_url: &str,
    expected_zip_size: u64,
    instance_dir: &Path,
    txn_id: &str,
    plan: &ReconciliationPlan,
    cancel_token: &Arc<AtomicBool>,
) -> Result<PartialZipAttempt, Dr3shError> {
    if cancel_token.load(Ordering::Relaxed) {
        return Err(Dr3shError::Cancelled {
            stage: "download".to_string(),
        });
    }
    if expected_zip_size < 22 {
        return Ok(PartialZipAttempt::FallbackToFull(
            "Tamaño de ZIP insuficiente para descarga parcial".to_string(),
        ));
    }

    let op_start = Instant::now();
    let total_downloaded_network = Arc::new(AtomicU64::new(0));
    let resolved_url = Arc::new(tokio::sync::RwLock::new(zip_url.to_string()));

    let tail_start = expected_zip_size.saturating_sub(EOCD_TAIL_WINDOW);
    let tail_end = expected_zip_size - 1;
    let (tail_bytes, actual_tail_start, cr_total) = match fetch_range_with_retries(
        zip_url,
        &resolved_url,
        tail_start,
        tail_end,
        cancel_token,
    )
    .await?
    {
        Ok(res) => res,
        Err(reason) => return Ok(PartialZipAttempt::FallbackToFull(reason)),
    };
    total_downloaded_network.fetch_add(tail_bytes.len() as u64, Ordering::Relaxed);
    let remote_zip_size = cr_total.unwrap_or(expected_zip_size);
    if remote_zip_size < 22 {
        return Ok(PartialZipAttempt::FallbackToFull(
            "Tamaño remoto de ZIP inválido".to_string(),
        ));
    }

    let Some(eocd_pos) = find_eocd(&tail_bytes) else {
        return Ok(PartialZipAttempt::FallbackToFull(
            "No se encontró el marcador EOCD al final del ZIP".to_string(),
        ));
    };

    let disk_num = read_u16_le(&tail_bytes, eocd_pos + 4).unwrap_or(0);
    let cd_disk = read_u16_le(&tail_bytes, eocd_pos + 6).unwrap_or(0);
    if disk_num != 0 || cd_disk != 0 {
        return Ok(PartialZipAttempt::FallbackToFull(
            "Archivos ZIP multi-disco no soportados en modo delta".to_string(),
        ));
    }

    let mut cd_size = read_u32_le(&tail_bytes, eocd_pos + 12).unwrap_or(0) as u64;
    let mut cd_offset = read_u32_le(&tail_bytes, eocd_pos + 16).unwrap_or(0) as u64;

    if cd_size == 0xFFFF_FFFF || cd_offset == 0xFFFF_FFFF {
        if eocd_pos < 20 || tail_bytes[eocd_pos - 20..eocd_pos - 16] != [0x50, 0x4b, 0x06, 0x07] {
            return Ok(PartialZipAttempt::FallbackToFull(
                "No se encontró el localizador ZIP64 EOCD".to_string(),
            ));
        }
        let Some(z64_offset) = read_u64_le(&tail_bytes, eocd_pos - 12) else {
            return Ok(PartialZipAttempt::FallbackToFull(
                "Localizador ZIP64 inválido".to_string(),
            ));
        };
        let tail_end_abs = actual_tail_start + tail_bytes.len() as u64;
        let z64_buf = if z64_offset >= actual_tail_start && z64_offset + 56 <= tail_end_abs {
            let rel = (z64_offset - actual_tail_start) as usize;
            tail_bytes.slice(rel..rel + 56)
        } else {
            match fetch_range_with_retries(
                zip_url,
                &resolved_url,
                z64_offset,
                z64_offset + 55,
                cancel_token,
            )
            .await?
            {
                Ok((b, _, _)) => {
                    total_downloaded_network.fetch_add(b.len() as u64, Ordering::Relaxed);
                    b
                }
                Err(reason) => return Ok(PartialZipAttempt::FallbackToFull(reason)),
            }
        };
        if z64_buf.len() < 56 || z64_buf[0..4] != [0x50, 0x4b, 0x06, 0x06] {
            return Ok(PartialZipAttempt::FallbackToFull(
                "Cabecera ZIP64 EOCD inválida".to_string(),
            ));
        }
        cd_size = read_u64_le(&z64_buf, 40).unwrap_or(0);
        cd_offset = read_u64_le(&z64_buf, 48).unwrap_or(0);
    }

    if cd_size == 0
        || cd_size > MAX_CD_BYTES
        || cd_offset.saturating_add(cd_size) > remote_zip_size
    {
        return Ok(PartialZipAttempt::FallbackToFull(
            "Dimensiones del Central Directory fuera de rango".to_string(),
        ));
    }

    let tail_end_abs = actual_tail_start + tail_bytes.len() as u64;
    let cd_bytes = if cd_offset >= actual_tail_start && cd_offset + cd_size <= tail_end_abs {
        let rel = (cd_offset - actual_tail_start) as usize;
        tail_bytes.slice(rel..rel + cd_size as usize)
    } else {
        match fetch_range_with_retries(
            zip_url,
            &resolved_url,
            cd_offset,
            cd_offset + cd_size - 1,
            cancel_token,
        )
        .await?
        {
            Ok((b, _, _)) => {
                total_downloaded_network.fetch_add(b.len() as u64, Ordering::Relaxed);
                b
            }
            Err(reason) => return Ok(PartialZipAttempt::FallbackToFull(reason)),
        }
    };

    if cd_bytes.len() as u64 != cd_size {
        return Ok(PartialZipAttempt::FallbackToFull(
            "Central Directory incompleto recibido por HTTP Range".to_string(),
        ));
    }

    let cd_entries = match parse_cd_entries(&cd_bytes) {
        Ok(list) => list,
        Err(reason) => return Ok(PartialZipAttempt::FallbackToFull(reason)),
    };

    let mut common_prefix: Option<String> = None;
    let mut normalized_entries: Vec<(CdEntry, String)> = Vec::with_capacity(cd_entries.len());
    for entry in cd_entries {
        if entry.raw_name.ends_with('/') || entry.raw_name.ends_with('\\') {
            continue;
        }
        let norm = entry.raw_name.replace('\\', "/");
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
        normalized_entries.push((entry, norm));
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

    let mut needed: HashMap<String, (String, Option<String>, u64)> = HashMap::new();
    let mut needed_by_archive_name: HashMap<String, (String, Option<String>, u64)> =
        HashMap::new();
    for item in &plan.items {
        if matches!(
            item.action,
            PlanActionKind::Write | PlanActionKind::BackupThenWrite
        ) {
            needed.insert(
                item.path.to_ascii_lowercase(),
                (
                    item.path.clone(),
                    item.expected_sha256.clone(),
                    item.expected_size,
                ),
            );
            if let Some(ref arch_name) = item.archive_entry_name {
                needed_by_archive_name.insert(
                    arch_name.replace('\\', "/").to_ascii_lowercase(),
                    (
                        item.path.clone(),
                        item.expected_sha256.clone(),
                        item.expected_size,
                    ),
                );
            }
        }
    }

    let total_files = needed.len() as u64;
    let mut matched_entries: Vec<MatchedDeltaEntry> = Vec::with_capacity(needed.len());
    for (cd_entry, norm) in normalized_entries {
        if cancel_token.load(Ordering::Relaxed) {
            return Err(Dr3shError::Cancelled {
                stage: "download".to_string(),
            });
        }
        let key = norm.to_ascii_lowercase();
        let matched = if needed_by_archive_name.contains_key(&key) {
            needed_by_archive_name.remove(&key).and_then(|v| {
                needed.remove(&v.0.to_ascii_lowercase()).or(Some(v))
            })
        } else {
            needed.remove(&key).or_else(|| {
                if let Some(ref prefix) = common_prefix
                    && norm.len() > prefix.len()
                {
                    let stripped = norm[prefix.len()..]
                        .trim_start_matches('/')
                        .to_ascii_lowercase();
                    needed.remove(&stripped)
                } else {
                    None
                }
            })
        };

        if let Some((canonical_rel_path, expected_sha256, _expected_size)) = matched {
            if cd_entry.compression_method != 0 && cd_entry.compression_method != 8 {
                return Ok(PartialZipAttempt::FallbackToFull(format!(
                    "Método de compresión no soportado en delta: {}",
                    cd_entry.compression_method
                )));
            }
            let est_end = cd_entry
                .local_header_offset
                .saturating_add(30)
                .saturating_add(cd_entry.name_len)
                .saturating_add(cd_entry.extra_len)
                .saturating_add(cd_entry.compressed_size)
                .saturating_add(LFH_OVERHEAD_PADDING)
                .min(remote_zip_size.saturating_sub(1));

            matched_entries.push(MatchedDeltaEntry {
                canonical_rel_path,
                expected_sha256,
                compression_method: cd_entry.compression_method,
                compressed_size: cd_entry.compressed_size,
                uncompressed_size: cd_entry.uncompressed_size,
                local_header_offset: cd_entry.local_header_offset,
                est_end,
            });
        }
    }

    if !needed.is_empty() {
        return Ok(PartialZipAttempt::FallbackToFull(format!(
            "Faltaron {} entrada(s) en el índice ZIP remoto",
            needed.len()
        )));
    }

    let total_compressed_needed: u64 = matched_entries
        .iter()
        .map(|e| e.compressed_size)
        .sum::<u64>();
    if total_compressed_needed >= (remote_zip_size.saturating_mul(70) / 100) {
        return Ok(PartialZipAttempt::FallbackToFull(
            "El tamaño comprimido de cambios supera el 70% del ZIP completo".to_string(),
        ));
    }

    matched_entries.sort_by_key(|e| e.local_header_offset);
    let mut batches: VecDeque<RangeBatch> = VecDeque::new();
    for entry in matched_entries {
        let should_merge = batches.back().is_some_and(|cur| {
            entry.local_header_offset <= cur.range_end.saturating_add(BATCH_COALESCE_GAP)
                && entry.est_end.max(cur.range_end).saturating_sub(cur.range_start) + 1
                    <= MAX_BATCH_BYTES
        });
        if should_merge && let Some(cur) = batches.back_mut() {
            cur.range_end = cur.range_end.max(entry.est_end);
            cur.entries.push(entry);
        } else {
            batches.push_back(RangeBatch {
                range_start: entry.local_header_offset,
                range_end: entry.est_end,
                entries: vec![entry],
            });
        }
    }

    let estimated_total_download: u64 = total_downloaded_network.load(Ordering::Relaxed)
        + batches
            .iter()
            .map(|b| b.range_end.saturating_sub(b.range_start) + 1)
            .sum::<u64>();
    let estimated_total_download = estimated_total_download.max(1);

    let staging_dir = instance_dir.join(".dr3sh").join("staging").join(txn_id);
    if staging_dir.exists() {
        let _ = fs::remove_dir_all(&staging_dir);
    }
    fs::create_dir_all(&staging_dir).map_err(|e| Dr3shError::FsPermission {
        path: staging_dir.to_string_lossy().to_string(),
        os_code: e.raw_os_error().unwrap_or(-1).to_string(),
    })?;

    let mut created_parents: HashSet<PathBuf> = HashSet::new();
    for batch in &batches {
        for entry in &batch.entries {
            if let Some(parent) = staging_dir.join(&entry.canonical_rel_path).parent()
                && created_parents.insert(parent.to_path_buf())
            {
                fs::create_dir_all(parent).map_err(|e| Dr3shError::FsPermission {
                    path: parent.to_string_lossy().to_string(),
                    os_code: e.raw_os_error().unwrap_or(-1).to_string(),
                })?;
            }
        }
    }

    log_event(
        Some(op_id),
        "DOWNLOAD",
        "INFO",
        "starting incremental HTTP Range delta update",
        &[
            ("files_needed", total_files.to_string()),
            ("batches", batches.len().to_string()),
            ("est_delta_bytes", estimated_total_download.to_string()),
            ("full_zip_bytes", remote_zip_size.to_string()),
        ],
    );

    if let Some(app_handle) = app {
        emit_progress_detailed(
            app_handle,
            op_id,
            server_id,
            "download",
            format!("Descargando actualización ({total_files} archivos)..."),
            total_downloaded_network
                .load(Ordering::Relaxed)
                .min(estimated_total_download),
            estimated_total_download,
            0,
            None,
            Some(10),
            None,
            Some(0),
            Some(total_files),
        );
    }

    let queue = Arc::new(tokio::sync::Mutex::new(batches));
    let files_done = Arc::new(AtomicU64::new(0));
    let fallback_reason: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let fatal_error: Arc<Mutex<Option<Dr3shError>>> = Arc::new(Mutex::new(None));
    let abort_workers = Arc::new(AtomicBool::new(false));
    let last_emit = Arc::new(Mutex::new(Instant::now()));

    let worker_count = DELTA_WORKERS.min(total_files.max(1) as usize);
    let mut handles = Vec::with_capacity(worker_count);

    for _ in 0..worker_count {
        let queue_c = Arc::clone(&queue);
        let url_orig = zip_url.to_string();
        let resolved_c = Arc::clone(&resolved_url);
        let cancel_c = Arc::clone(cancel_token);
        let abort_c = Arc::clone(&abort_workers);
        let fallback_c = Arc::clone(&fallback_reason);
        let fatal_c = Arc::clone(&fatal_error);
        let dl_bytes_c = Arc::clone(&total_downloaded_network);
        let files_done_c = Arc::clone(&files_done);
        let staging_c = staging_dir.clone();
        let app_c = app.cloned();
        let op_c = op_id.to_string();
        let srv_c = server_id.to_string();
        let last_emit_c = Arc::clone(&last_emit);

        handles.push(tokio::spawn(async move {
            loop {
                if cancel_c.load(Ordering::Relaxed) {
                    abort_c.store(true, Ordering::Release);
                    if let Ok(mut g) = fatal_c.lock()
                        && g.is_none()
                    {
                        *g = Some(Dr3shError::Cancelled {
                            stage: "download".to_string(),
                        });
                    }
                    break;
                }
                if abort_c.load(Ordering::Acquire) {
                    break;
                }

                let batch = {
                    let mut q = queue_c.lock().await;
                    q.pop_front()
                };
                let Some(batch) = batch else {
                    break;
                };

                let batch_res = fetch_range_with_retries(
                    &url_orig,
                    &resolved_c,
                    batch.range_start,
                    batch.range_end,
                    &cancel_c,
                )
                .await;

                let (batch_bytes, actual_start) = match batch_res {
                    Ok(Ok((b, cr_start, _))) => {
                        dl_bytes_c.fetch_add(b.len() as u64, Ordering::Relaxed);
                        (b, cr_start)
                    }
                    Ok(Err(reason)) => {
                        abort_c.store(true, Ordering::Release);
                        if let Ok(mut g) = fallback_c.lock()
                            && g.is_none()
                        {
                            *g = Some(reason);
                        }
                        break;
                    }
                    Err(err) => {
                        abort_c.store(true, Ordering::Release);
                        if let Ok(mut g) = fatal_c.lock()
                            && g.is_none()
                        {
                            *g = Some(err);
                        }
                        break;
                    }
                };

                for entry in batch.entries {
                    if cancel_c.load(Ordering::Relaxed) {
                        abort_c.store(true, Ordering::Release);
                        if let Ok(mut g) = fatal_c.lock()
                            && g.is_none()
                        {
                            *g = Some(Dr3shError::Cancelled {
                                stage: "download".to_string(),
                            });
                        }
                        break;
                    }
                    if abort_c.load(Ordering::Acquire) {
                        break;
                    }

                    if entry.local_header_offset < actual_start {
                        abort_c.store(true, Ordering::Release);
                        if let Ok(mut g) = fallback_c.lock()
                            && g.is_none()
                        {
                            *g = Some("Desalineación de rango recibido del servidor".to_string());
                        }
                        break;
                    }

                    let rel_lfh = (entry.local_header_offset - actual_start) as usize;
                    if rel_lfh + 30 > batch_bytes.len()
                        || batch_bytes[rel_lfh..rel_lfh + 4] != [0x50, 0x4b, 0x03, 0x04]
                    {
                        abort_c.store(true, Ordering::Release);
                        if let Ok(mut g) = fallback_c.lock()
                            && g.is_none()
                        {
                            *g = Some(format!(
                                "Local File Header inválido para {}",
                                entry.canonical_rel_path
                            ));
                        }
                        break;
                    }

                    let lfh_name_len =
                        read_u16_le(&batch_bytes, rel_lfh + 26).unwrap_or(0) as usize;
                    let lfh_extra_len =
                        read_u16_le(&batch_bytes, rel_lfh + 28).unwrap_or(0) as usize;
                    let comp_start_rel = rel_lfh + 30 + lfh_name_len + lfh_extra_len;
                    let comp_end_rel = comp_start_rel + entry.compressed_size as usize;

                    let compressed_slice = if comp_end_rel <= batch_bytes.len() {
                        batch_bytes.slice(comp_start_rel..comp_end_rel)
                    } else if entry.compressed_size == 0 {
                        Bytes::new()
                    } else {
                        let abs_comp_start =
                            entry.local_header_offset + 30 + lfh_name_len as u64 + lfh_extra_len as u64;
                        let abs_comp_end = abs_comp_start + entry.compressed_size - 1;
                        match fetch_range_with_retries(
                            &url_orig,
                            &resolved_c,
                            abs_comp_start,
                            abs_comp_end,
                            &cancel_c,
                        )
                        .await
                        {
                            Ok(Ok((exact_bytes, _, _)))
                                if exact_bytes.len() as u64 >= entry.compressed_size =>
                            {
                                dl_bytes_c.fetch_add(exact_bytes.len() as u64, Ordering::Relaxed);
                                exact_bytes.slice(..entry.compressed_size as usize)
                            }
                            Ok(Ok(_)) => {
                                abort_c.store(true, Ordering::Release);
                                if let Ok(mut g) = fallback_c.lock()
                                    && g.is_none()
                                {
                                    *g = Some(
                                        "Lectura parcial incompleta tras LFH extendido".to_string(),
                                    );
                                }
                                break;
                            }
                            Ok(Err(reason)) => {
                                abort_c.store(true, Ordering::Release);
                                if let Ok(mut g) = fallback_c.lock()
                                    && g.is_none()
                                {
                                    *g = Some(reason);
                                }
                                break;
                            }
                            Err(err) => {
                                abort_c.store(true, Ordering::Release);
                                if let Ok(mut g) = fatal_c.lock()
                                    && g.is_none()
                                {
                                    *g = Some(err);
                                }
                                break;
                            }
                        }
                    };

                    let out_path = staging_c.join(&entry.canonical_rel_path);
                    let comp_method = entry.compression_method;
                    let exp_sha = entry.expected_sha256.clone();
                    let exp_size = entry.uncompressed_size;
                    let rel_name = entry.canonical_rel_path.clone();
                    let cancel_block = Arc::clone(&cancel_c);

                    let extract_res = tokio::task::spawn_blocking(move || {
                        decompress_and_write_entry(
                            compressed_slice,
                            comp_method,
                            &out_path,
                            exp_sha.as_deref(),
                            exp_size,
                            &rel_name,
                            &cancel_block,
                        )
                    })
                    .await;

                    match extract_res {
                        Ok(ExtractOneResult::Ok(_)) => {
                            let done_files = files_done_c.fetch_add(1, Ordering::Relaxed) + 1;
                            if let Some(ref app_handle) = app_c {
                                let should_emit = done_files == total_files
                                    || last_emit_c
                                        .try_lock()
                                        .is_ok_and(|mut t| {
                                            if t.elapsed() >= Duration::from_millis(120) {
                                                *t = Instant::now();
                                                true
                                            } else {
                                                false
                                            }
                                        });
                                if should_emit {
                                    let done_b = dl_bytes_c
                                        .load(Ordering::Relaxed)
                                        .min(estimated_total_download);
                                    let ratio =
                                        (done_b as f64 / estimated_total_download as f64).clamp(0.0, 1.0);
                                    let pct = (10.0 + ratio * 80.0).round() as u32;
                                    let elapsed = op_start.elapsed().as_secs_f64().max(0.2);
                                    let spd = (done_b as f64 / elapsed) as u64;
                                    let eta = if estimated_total_download > done_b && spd > 1024 {
                                        Some(((estimated_total_download - done_b) / spd).max(1))
                                    } else {
                                        None
                                    };
                                    emit_progress_detailed(
                                        app_handle,
                                        &op_c,
                                        &srv_c,
                                        "download",
                                        format!(
                                            "Descargando actualización ({done_files}/{total_files})..."
                                        ),
                                        done_b,
                                        estimated_total_download,
                                        spd,
                                        None,
                                        Some(pct),
                                        eta,
                                        Some(done_files),
                                        Some(total_files),
                                    );
                                }
                            }
                        }
                        Ok(ExtractOneResult::Fallback(reason)) => {
                            abort_c.store(true, Ordering::Release);
                            if let Ok(mut g) = fallback_c.lock()
                                && g.is_none()
                            {
                                *g = Some(reason);
                            }
                            break;
                        }
                        Ok(ExtractOneResult::Err(err)) => {
                            abort_c.store(true, Ordering::Release);
                            if let Ok(mut g) = fatal_c.lock()
                                && g.is_none()
                            {
                                *g = Some(err);
                            }
                            break;
                        }
                        Err(join_err) => {
                            abort_c.store(true, Ordering::Release);
                            if let Ok(mut g) = fatal_c.lock()
                                && g.is_none()
                            {
                                *g = Some(Dr3shError::unknown(format!(
                                    "Delta worker join error: {join_err}"
                                )));
                            }
                            break;
                        }
                    }
                }
            }
        }));
    }

    for h in handles {
        let _ = h.await;
    }

    if let Some(err) = fatal_error.lock().ok().and_then(|mut g| g.take()) {
        let _ = fs::remove_dir_all(&staging_dir);
        return Err(err);
    }

    if let Some(reason) = fallback_reason.lock().ok().and_then(|mut g| g.take()) {
        let _ = fs::remove_dir_all(&staging_dir);
        return Ok(PartialZipAttempt::FallbackToFull(reason));
    }

    let final_dl_bytes = total_downloaded_network.load(Ordering::Relaxed);
    let elapsed_secs = op_start.elapsed().as_secs_f64().max(0.1);
    let avg_speed_bps = (final_dl_bytes as f64 / elapsed_secs) as u64;
    let extracted_count = files_done.load(Ordering::Relaxed);

    if let Some(app_handle) = app {
        emit_progress_detailed(
            app_handle,
            op_id,
            server_id,
            "download",
            format!("Actualización descargada ({extracted_count} archivos)."),
            final_dl_bytes,
            final_dl_bytes.max(1),
            avg_speed_bps,
            None,
            Some(92),
            None,
            Some(extracted_count),
            Some(total_files),
        );
    }

    Ok(PartialZipAttempt::Completed(PartialZipOutcome {
        downloaded_bytes: final_dl_bytes,
        files_extracted: extracted_count,
        avg_speed_bps,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dr3sh::manifest::{FilePolicy, FilePlanItem};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use zip::write::SimpleFileOptions;

    fn sha256_hex(data: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(data);
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    fn build_test_zip_wrapped() -> (Vec<u8>, String, Vec<u8>, String, Vec<u8>) {
        let mut buf = std::io::Cursor::new(Vec::new());
        let big_unchanged = vec![0x41u8; 512 * 1024];
        let changed_deflate = b"new-mod-jar-binary-payload-hello-world-v2".repeat(128);
        let changed_stored = b"small-config-delta-update".to_vec();

        {
            let mut zw = zip::ZipWriter::new(&mut buf);
            let stored_opts = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            let deflate_opts = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);

            zw.start_file("MyPack/mods/big_unchanged.jar", stored_opts)
                .unwrap();
            zw.write_all(&big_unchanged).unwrap();

            zw.start_file("MyPack/mods/updated_mod.jar", deflate_opts)
                .unwrap();
            zw.write_all(&changed_deflate).unwrap();

            zw.start_file("MyPack/config/updated_cfg.toml", stored_opts)
                .unwrap();
            zw.write_all(&changed_stored).unwrap();

            zw.finish().unwrap();
        }

        let zip_bytes = buf.into_inner();
        let sha_deflate = sha256_hex(&changed_deflate);
        let sha_stored = sha256_hex(&changed_stored);
        (zip_bytes, sha_deflate, changed_deflate, sha_stored, changed_stored)
    }

    async fn spawn_range_server(
        zip_data: Vec<u8>,
        support_range: bool,
    ) -> (std::net::SocketAddr, Arc<AtomicU64>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let bytes_served = Arc::new(AtomicU64::new(0));
        let served_c = Arc::clone(&bytes_served);
        let data = Arc::new(zip_data);

        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let data_c = Arc::clone(&data);
                let served_inner = Arc::clone(&served_c);
                tokio::spawn(async move {
                    loop {
                        let mut req_buf = vec![0u8; 8192];
                        let n = match stream.read(&mut req_buf).await {
                            Ok(n) if n > 0 => n,
                            _ => break,
                        };
                        let req_str = String::from_utf8_lossy(&req_buf[..n]);
                        let range_line = req_str
                            .lines()
                            .find(|l| l.to_ascii_lowercase().starts_with("range:"))
                            .map(|l| l.split_once(':').map(|x| x.1.trim()).unwrap_or(""));

                        if support_range
                            && let Some(r_val) = range_line
                            && let Some(bounds) = r_val.strip_prefix("bytes=")
                            && let Some((s_str, e_str)) = bounds.split_once('-')
                        {
                            let total = data_c.len();
                            let start = s_str.trim().parse::<usize>().unwrap_or(0);
                            let end = e_str
                                .trim()
                                .parse::<usize>()
                                .unwrap_or(total.saturating_sub(1))
                                .min(total.saturating_sub(1));
                            let slice = if start <= end && start < total {
                                &data_c[start..=end]
                            } else {
                                &[][..]
                            };
                            served_inner.fetch_add(slice.len() as u64, Ordering::Relaxed);
                            let header = format!(
                                "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",
                                slice.len(),
                                start,
                                end,
                                total
                            );
                            let _ = stream.write_all(header.as_bytes()).await;
                            let _ = stream.write_all(slice).await;
                            break;
                        } else {
                            served_inner.fetch_add(data_c.len() as u64, Ordering::Relaxed);
                            let header = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                data_c.len()
                            );
                            let _ = stream.write_all(header.as_bytes()).await;
                            let _ = stream.write_all(&data_c).await;
                            break;
                        }
                    }
                });
            }
        });

        (addr, bytes_served)
    }

    #[tokio::test]
    async fn test_partial_zip_downloads_only_changed_files() {
        let (zip_bytes, sha_deflate, raw_deflate, sha_stored, raw_stored) =
            build_test_zip_wrapped();
        let full_len = zip_bytes.len() as u64;
        let (addr, served) = spawn_range_server(zip_bytes, true).await;

        let base = std::env::temp_dir().join(format!("dr3sh_delta_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&base).unwrap();

        let plan = ReconciliationPlan {
            migration_mode: false,
            items: vec![
                FilePlanItem {
                    path: "mods/big_unchanged.jar".to_string(),
                    policy: FilePolicy::Managed,
                    action: PlanActionKind::Skip,
                    tag: None,
                    expected_sha256: None,
                    expected_size: 512 * 1024,
                    archive_entry_name: None,
                },
                FilePlanItem {
                    path: "mods/updated_mod.jar".to_string(),
                    policy: FilePolicy::Managed,
                    action: PlanActionKind::Write,
                    tag: None,
                    expected_sha256: Some(sha_deflate.clone()),
                    expected_size: raw_deflate.len() as u64,
                    archive_entry_name: None,
                },
                FilePlanItem {
                    path: "config/updated_cfg.toml".to_string(),
                    policy: FilePolicy::UpdateOnChange,
                    action: PlanActionKind::BackupThenWrite,
                    tag: None,
                    expected_sha256: Some(sha_stored.clone()),
                    expected_size: raw_stored.len() as u64,
                    archive_entry_name: None,
                },
            ],
            counts_by_action: HashMap::new(),
            counts_by_tag: HashMap::new(),
            write_bytes_total: (raw_deflate.len() + raw_stored.len()) as u64,
        };

        assert!(should_use_partial_zip(full_len, &plan, 1024));

        let cancel = Arc::new(AtomicBool::new(false));
        let url = format!("http://{addr}/pack.zip");
        let attempt = download_and_extract_partial_zip(
            None,
            "delta_test",
            "desgraciamon3",
            &url,
            full_len,
            &base,
            "txn_delta_1",
            &plan,
            &cancel,
        )
        .await
        .unwrap();

        match attempt {
            PartialZipAttempt::Completed(outcome) => {
                assert_eq!(outcome.files_extracted, 2);
                assert!(
                    outcome.downloaded_bytes < 100 * 1024,
                    "expected small delta download, downloaded={} vs full={}",
                    outcome.downloaded_bytes,
                    full_len
                );
                assert!(served.load(Ordering::Relaxed) < 100 * 1024);
                let staging = base.join(".dr3sh").join("staging").join("txn_delta_1");
                assert_eq!(
                    fs::read(staging.join("mods/updated_mod.jar")).unwrap(),
                    raw_deflate
                );
                assert_eq!(
                    fs::read(staging.join("config/updated_cfg.toml")).unwrap(),
                    raw_stored
                );
                assert!(!staging.join("mods/big_unchanged.jar").exists());
            }
            PartialZipAttempt::FallbackToFull(reason) => {
                panic!("unexpected fallback: {reason}");
            }
        }

        let _ = fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn test_partial_zip_falls_back_when_no_range_support() {
        let (zip_bytes, sha_deflate, raw_deflate, _, _) = build_test_zip_wrapped();
        let full_len = zip_bytes.len() as u64;
        let (addr, _) = spawn_range_server(zip_bytes, false).await;

        let base = std::env::temp_dir().join(format!("dr3sh_delta_fb_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&base).unwrap();

        let plan = ReconciliationPlan {
            migration_mode: false,
            items: vec![FilePlanItem {
                path: "mods/updated_mod.jar".to_string(),
                policy: FilePolicy::Managed,
                action: PlanActionKind::Write,
                tag: None,
                expected_sha256: Some(sha_deflate),
                expected_size: raw_deflate.len() as u64,
                archive_entry_name: None,
            }],
            counts_by_action: HashMap::new(),
            counts_by_tag: HashMap::new(),
            write_bytes_total: raw_deflate.len() as u64,
        };

        let cancel = Arc::new(AtomicBool::new(false));
        let url = format!("http://{addr}/pack.zip");
        let attempt = download_and_extract_partial_zip(
            None,
            "delta_fb_test",
            "desgraciamon3",
            &url,
            full_len,
            &base,
            "txn_delta_fb",
            &plan,
            &cancel,
        )
        .await
        .unwrap();

        assert!(matches!(attempt, PartialZipAttempt::FallbackToFull(_)));
        let _ = fs::remove_dir_all(&base);
    }
}
