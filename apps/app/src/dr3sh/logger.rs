use chrono::{Local, NaiveDate};
use dashmap::DashMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpDiagnosticMeta {
    pub op_id: String,
    pub server_id: String,
    pub from_version: Option<String>,
    pub to_version: String,
    pub stage: String,
    pub error_code: Option<String>,
    pub launcher_version: String,
    pub os_version: String,
}

static OP_DIAGNOSTICS: LazyLock<DashMap<String, OpDiagnosticMeta>> = LazyLock::new(DashMap::new);
static LAST_OP_ID: LazyLock<Mutex<Option<String>>> = LazyLock::new(|| Mutex::new(None));
static LOG_WRITE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

static URL_QUERY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(https?://[^\s\?'"<>]+)\?[^\s'"<>]*"#).unwrap());
static BEARER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(bearer|token)\s+[A-Za-z0-9._~+/=-]{8,}").unwrap());
static GH_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:gh[pousr]_[A-Za-z0-9_]{10,}|github_pat_[A-Za-z0-9_]{10,})\b").unwrap()
});
static JWT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b").unwrap()
});
static MS_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?:\bM\.[A-Za-z0-9._!-]{12,}|(?i)\b(?:access_token|refresh_token|xsts_token|xbl_token)=[^\s&'"]+)"#,
    )
    .unwrap()
});

pub fn logs_base_dir() -> PathBuf {
    if let Ok(appdata) = std::env::var("APPDATA") {
        PathBuf::from(appdata)
            .join("com.dr3sh.launcher")
            .join("logs")
    } else {
        std::env::temp_dir().join("com.dr3sh.launcher").join("logs")
    }
}

pub fn logs_root_dir() -> PathBuf {
    logs_base_dir()
}

pub fn new_op_id() -> String {
    let raw = uuid::Uuid::new_v4().simple().to_string();
    raw[..8].to_string()
}

pub fn launcher_data_dir() -> PathBuf {
    if let Ok(appdata) = std::env::var("APPDATA") {
        PathBuf::from(appdata).join("com.dr3sh.launcher")
    } else {
        std::env::temp_dir().join("com.dr3sh.launcher")
    }
}

pub fn sanitize_url(url: &str) -> String {
    let trimmed = url.trim();
    let without_query = match trimmed.split_once('?') {
        Some((base, _)) => base,
        None => trimmed,
    };
    let without_frag = match without_query.split_once('#') {
        Some((base, _)) => base,
        None => without_query,
    };
    redact_sensitive(without_frag)
}

pub fn redact_sensitive(input: &str) -> String {
    let s = URL_QUERY_RE.replace_all(input, "$1");
    let s = GH_TOKEN_RE.replace_all(&s, "[REDACTED_GH_TOKEN]");
    let s = JWT_RE.replace_all(&s, "[REDACTED_JWT]");
    let s = MS_TOKEN_RE.replace_all(&s, "[REDACTED_MS_TOKEN]");
    let s = BEARER_RE.replace_all(&s, "$1 [REDACTED]");
    s.into_owned()
}

fn format_kv_value(val: &str) -> String {
    let redacted = redact_sensitive(val);
    if redacted.is_empty()
        || redacted.contains(|c: char| c.is_whitespace() || c == '"' || c == '=')
    {
        let escaped = redacted.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    } else {
        redacted
    }
}

pub fn format_log_line(
    op_id: Option<&str>,
    stage: &str,
    level: &str,
    message: &str,
    kvs: &[(&str, String)],
) -> String {
    let ts = Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z");
    let op_str = op_id.unwrap_or("sys");
    let clean_msg = redact_sensitive(message)
        .replace('\r', " ")
        .replace('\n', " ");
    let mut line = format!("{ts} [op={op_str}] [{stage}] {level} {clean_msg}");
    for (k, v) in kvs {
        line.push(' ');
        line.push_str(k);
        line.push('=');
        line.push_str(&format_kv_value(v));
    }
    line.push('\n');
    line
}

fn write_and_flush(path: &Path, line: &str, sync_disk: bool) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(line.as_bytes());
        let _ = file.flush();
        if sync_disk {
            let _ = file.sync_data();
        }
    }
}

pub fn log_event(
    op_id: Option<&str>,
    stage: &str,
    level: &str,
    message: &str,
    kvs: &[(&str, String)],
) {
    let line = format_log_line(op_id, stage, level, message, kvs);
    let base = logs_base_dir();
    let today = Local::now().format("%Y-%m-%d").to_string();
    let daily_path = base.join(format!("launcher-{today}.log"));
    let sync_disk = matches!(level, "ERROR" | "WARN") || stage == "DONE";

    let _guard = LOG_WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if level != "DEBUG" {
        write_and_flush(&daily_path, &line, sync_disk);
    }
    if let Some(op) = op_id {
        let op_path = base.join("ops").join(format!("op-{op}.log"));
        write_and_flush(&op_path, &line, sync_disk);
    }
}

pub fn log_op_debug_batch(op_id: Option<&str>, content: &str) {
    let Some(op) = op_id else {
        return;
    };
    if content.is_empty() {
        return;
    }
    let base = logs_base_dir();
    let op_path = base.join("ops").join(format!("op-{op}.log"));
    let _guard = LOG_WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    write_and_flush(&op_path, content, false);
}

pub fn register_op_meta(meta: OpDiagnosticMeta) {
    let op_id = meta.op_id.clone();
    if let Ok(mut last) = LAST_OP_ID.lock() {
        *last = Some(op_id.clone());
    }
    OP_DIAGNOSTICS.insert(op_id, meta);
}

pub fn update_op_stage(op_id: &str, stage: &str, error_code: Option<&str>) {
    if let Some(mut entry) = OP_DIAGNOSTICS.get_mut(op_id) {
        entry.stage = stage.to_string();
        if let Some(code) = error_code {
            entry.error_code = Some(code.to_string());
        }
    }
}

pub fn prune_logs() {
    let base = logs_base_dir();
    if !base.exists() {
        return;
    }

    let today = Local::now().date_naive();
    if let Ok(entries) = fs::read_dir(&base) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if let Some(name) = path.file_name().and_then(|n| n.to_str())
                && let Some(date_str) = name
                    .strip_prefix("launcher-")
                    .and_then(|s| s.strip_suffix(".log"))
                && let Ok(file_date) = NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
                && (today - file_date).num_days() > 14
            {
                let _ = fs::remove_file(&path);
            }
        }
    }

    let ops_dir = base.join("ops");
    if ops_dir.exists()
        && let Ok(entries) = fs::read_dir(&ops_dir)
    {
        let mut op_files: Vec<(PathBuf, SystemTime, u64)> = Vec::new();
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file()
                && let Ok(meta) = entry.metadata()
            {
                let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                op_files.push((p, mtime, meta.len()));
            }
        }
        op_files.sort_by_key(|(_, mtime, _)| *mtime);
        while op_files.len() > 50 {
            if let Some((oldest, _, _)) = op_files.first() {
                let _ = fs::remove_file(oldest);
            }
            op_files.remove(0);
        }
    }

    let mut all_logs: Vec<(PathBuf, SystemTime, u64)> = Vec::new();
    for dir in [&base, &ops_dir] {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_file()
                    && let Ok(meta) = entry.metadata()
                {
                    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    all_logs.push((p, mtime, meta.len()));
                }
            }
        }
    }
    all_logs.sort_by_key(|(_, mtime, _)| *mtime);
    let mut total_size: u64 = all_logs.iter().map(|(_, _, sz)| *sz).sum();
    const MAX_TOTAL_BYTES: u64 = 50 * 1024 * 1024;
    for (path, _, sz) in all_logs {
        if total_size <= MAX_TOTAL_BYTES {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total_size = total_size.saturating_sub(sz);
        }
    }
}

pub fn install_panic_hook() {
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let payload = if let Some(s) = panic_info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = panic_info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown panic payload".to_string()
        };
        let location = panic_info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown".to_string());
        let bt = std::backtrace::Backtrace::force_capture();
        let active_op = LAST_OP_ID
            .lock()
            .ok()
            .and_then(|g| g.clone());
        log_event(
            active_op.as_deref(),
            "PANIC",
            "ERROR",
            "Unhandled Rust panic",
            &[
                ("location", location),
                ("message", payload),
                ("backtrace", format!("{bt}")),
            ],
        );
        prev_hook(panic_info);
    }));
}

fn read_last_n_lines(path: &Path, n: usize) -> Vec<String> {
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    let reader = BufReader::new(file);
    let mut lines: Vec<String> = Vec::new();
    for l in reader.lines().map_while(Result::ok) {
        lines.push(redact_sensitive(&l));
    }
    if lines.len() > n {
        lines.split_off(lines.len() - n)
    } else {
        lines
    }
}

pub fn build_diagnostics(op_id: Option<&str>) -> String {
    let resolved_op = op_id
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| LAST_OP_ID.lock().ok().and_then(|g| g.clone()));

    let meta = resolved_op
        .as_ref()
        .and_then(|id| OP_DIAGNOSTICS.get(id).map(|m| m.clone()));

    let base = logs_base_dir();
    let (log_lines, resolved_id_str) = if let Some(ref id) = resolved_op {
        let op_file = base.join("ops").join(format!("op-{id}.log"));
        let lines = read_last_n_lines(&op_file, 50);
        (lines, id.clone())
    } else {
        let ops_dir = base.join("ops");
        let mut newest: Option<(PathBuf, SystemTime)> = None;
        if let Ok(entries) = fs::read_dir(&ops_dir) {
            for e in entries.flatten() {
                if let Ok(m) = e.metadata()
                    && m.is_file()
                {
                    let mtime = m.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    if newest.as_ref().is_none_or(|(_, t)| mtime > *t) {
                        newest = Some((e.path(), mtime));
                    }
                }
            }
        }
        if let Some((p, _)) = newest {
            let name = p
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("op-unknown")
                .strip_prefix("op-")
                .unwrap_or("unknown")
                .to_string();
            (read_last_n_lines(&p, 50), name)
        } else {
            let today = Local::now().format("%Y-%m-%d").to_string();
            let daily = base.join(format!("launcher-{today}.log"));
            (read_last_n_lines(&daily, 50), "none".to_string())
        }
    };

    let error_code = meta
        .as_ref()
        .and_then(|m| m.error_code.clone())
        .unwrap_or_else(|| "NONE".to_string());
    let stage = meta
        .as_ref()
        .map(|m| m.stage.clone())
        .unwrap_or_else(|| "UNKNOWN".to_string());
    let launcher_ver = meta
        .as_ref()
        .map(|m| m.launcher_version.clone())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    let os_ver = meta
        .as_ref()
        .map(|m| m.os_version.clone())
        .unwrap_or_else(os_version_string);
    let server_id = meta
        .as_ref()
        .map(|m| m.server_id.clone())
        .unwrap_or_else(|| "unknown".to_string());
    let from_ver = meta
        .as_ref()
        .and_then(|m| m.from_version.clone())
        .unwrap_or_else(|| "none".to_string());
    let to_ver = meta
        .as_ref()
        .map(|m| m.to_version.clone())
        .unwrap_or_else(|| "unknown".to_string());

    let mut out = String::new();
    out.push_str("=== DR3SH LAUNCHER DIAGNOSTICS ===\n");
    out.push_str(&format!("op_id: {resolved_id_str}\n"));
    out.push_str(&format!("error_code: {error_code}\n"));
    out.push_str(&format!("stage: {stage}\n"));
    out.push_str(&format!("launcher_version: {launcher_ver}\n"));
    out.push_str(&format!("os: {os_ver}\n"));
    out.push_str(&format!("server_id: {server_id}\n"));
    out.push_str(&format!("versions: {from_ver} -> {to_ver}\n"));
    out.push_str("--- LAST 50 LOG LINES ---\n");
    if log_lines.is_empty() {
        out.push_str("(sin líneas de registro disponibles)\n");
    } else {
        for line in log_lines {
            out.push_str(&line);
            out.push('\n');
        }
    }
    let _ = Duration::from_secs(0);
    out
}

pub fn os_version_string() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_url_strips_query_and_fragment() {
        let url = "https://objects.githubusercontent.com/github-production-release-asset/123/pack.zip?X-Amz-Algorithm=AWS4&X-Amz-Signature=secret123#frag";
        assert_eq!(
            sanitize_url(url),
            "https://objects.githubusercontent.com/github-production-release-asset/123/pack.zip"
        );
    }

    #[test]
    fn test_redact_sensitive_tokens() {
        let fake_gho = format!("{}_{}", "gho", "000000000000000000000000000000000000");
        let fake_pat = format!("{}_{}", "github_pat", "11AABBCCDD_1234567890");
        let raw = format!(
            "Auth Bearer secret_jwt_token_12345678 and {} and {} and eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U and https://cdn.example.com/a.zip?token=abc",
            fake_gho, fake_pat
        );
        let redacted = redact_sensitive(&raw);
        assert!(!redacted.contains("secret_jwt_token_12345678"));
        assert!(!redacted.contains(&fake_gho));
        assert!(!redacted.contains(&fake_pat));
        assert!(!redacted.contains("eyJhbGciOiJIUzI1NiJ9"));
        assert!(!redacted.contains("?token=abc"));
        assert!(redacted.contains("https://cdn.example.com/a.zip"));
        assert!(redacted.contains("[REDACTED_GH_TOKEN]"));
        assert!(redacted.contains("[REDACTED_JWT]"));
    }
}
