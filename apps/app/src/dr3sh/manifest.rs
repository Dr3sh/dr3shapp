use crate::dr3sh::errors::Dr3shError;
use crate::dr3sh::logger::log_event;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::Path;
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilePolicy {
    Managed,
    UpdateOnChange,
    Seed,
    Ignore,
}

impl FilePolicy {
    pub fn from_str_strict(s: &str) -> Option<Self> {
        match s {
            "managed" => Some(Self::Managed),
            "update_on_change" => Some(Self::UpdateOnChange),
            "seed" => Some(Self::Seed),
            "ignore" => Some(Self::Ignore),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Managed => "managed",
            Self::UpdateOnChange => "update_on_change",
            Self::Seed => "seed",
            Self::Ignore => "ignore",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestZip {
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestRule {
    pub pattern: String,
    pub policy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestFileEntry {
    pub path: String,
    pub sha256: String,
    pub size: u64,
    pub policy: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub pack_id: String,
    pub version: String,
    pub minecraft_version: String,
    pub loader: String,
    #[serde(default)]
    pub loader_version: String,
    pub zip: ManifestZip,
    pub rules: Vec<ManifestRule>,
    pub files: Vec<ManifestFileEntry>,
}

pub fn default_rules() -> Vec<ManifestRule> {
    vec![
        ManifestRule {
            pattern: "mods/**".to_string(),
            policy: "managed".to_string(),
        },
        ManifestRule {
            pattern: "resourcepacks/**".to_string(),
            policy: "managed".to_string(),
        },
        ManifestRule {
            pattern: "shaderpacks/**".to_string(),
            policy: "managed".to_string(),
        },
        ManifestRule {
            pattern: "config/**".to_string(),
            policy: "update_on_change".to_string(),
        },
        ManifestRule {
            pattern: "defaultconfigs/**".to_string(),
            policy: "update_on_change".to_string(),
        },
        ManifestRule {
            pattern: "kubejs/**".to_string(),
            policy: "update_on_change".to_string(),
        },
        ManifestRule {
            pattern: "options.txt".to_string(),
            policy: "seed".to_string(),
        },
        ManifestRule {
            pattern: "optionsof.txt".to_string(),
            policy: "seed".to_string(),
        },
        ManifestRule {
            pattern: "optionsshaders.txt".to_string(),
            policy: "seed".to_string(),
        },
        ManifestRule {
            pattern: "servers.dat".to_string(),
            policy: "seed".to_string(),
        },
        ManifestRule {
            pattern: "saves/**".to_string(),
            policy: "ignore".to_string(),
        },
        ManifestRule {
            pattern: "screenshots/**".to_string(),
            policy: "ignore".to_string(),
        },
        ManifestRule {
            pattern: "logs/**".to_string(),
            policy: "ignore".to_string(),
        },
        ManifestRule {
            pattern: "crash-reports/**".to_string(),
            policy: "ignore".to_string(),
        },
        ManifestRule {
            pattern: "backups/**".to_string(),
            policy: "ignore".to_string(),
        },
        ManifestRule {
            pattern: "xaerowaypoints/**".to_string(),
            policy: "ignore".to_string(),
        },
        ManifestRule {
            pattern: "xaeroworldmap/**".to_string(),
            policy: "ignore".to_string(),
        },
        ManifestRule {
            pattern: "schematics/**".to_string(),
            policy: "ignore".to_string(),
        },
        ManifestRule {
            pattern: "replay_recordings/**".to_string(),
            policy: "ignore".to_string(),
        },
        ManifestRule {
            pattern: "replay_videos/**".to_string(),
            policy: "ignore".to_string(),
        },
        ManifestRule {
            pattern: ".dr3sh/**".to_string(),
            policy: "ignore".to_string(),
        },
        ManifestRule {
            pattern: "**".to_string(),
            policy: "update_on_change".to_string(),
        },
    ]
}

pub fn is_valid_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn is_safe_relative_path(path: &str) -> bool {
    if path.is_empty()
        || path.contains('\\')
        || path.starts_with('/')
        || path.ends_with('/')
        || path.contains(':')
        || path.contains('\0')
    {
        return false;
    }
    for seg in path.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return false;
        }
    }
    true
}

fn match_segment_glob(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p_idx, mut t_idx) = (0usize, 0usize);
    let mut star_idx: Option<usize> = None;
    let mut match_idx = 0usize;

    while t_idx < text.len() {
        if p_idx < pattern.len() && pattern[p_idx] == b'*' {
            star_idx = Some(p_idx);
            match_idx = t_idx;
            p_idx += 1;
        } else if p_idx < pattern.len()
            && pattern[p_idx].to_ascii_lowercase() == text[t_idx].to_ascii_lowercase()
        {
            p_idx += 1;
            t_idx += 1;
        } else if let Some(s_idx) = star_idx {
            p_idx = s_idx + 1;
            match_idx += 1;
            t_idx = match_idx;
        } else {
            return false;
        }
    }

    while p_idx < pattern.len() && pattern[p_idx] == b'*' {
        p_idx += 1;
    }
    p_idx == pattern.len()
}

pub fn glob_matches(pattern: &str, path: &str) -> bool {
    let pat = pattern.trim_matches('/');
    let txt = path.trim_matches('/');
    if pat == "**" {
        return true;
    }
    let pat_segs: Vec<&str> = if pat.is_empty() {
        Vec::new()
    } else {
        pat.split('/').collect()
    };
    let txt_segs: Vec<&str> = if txt.is_empty() {
        Vec::new()
    } else {
        txt.split('/').collect()
    };
    match_segments(&pat_segs, &txt_segs)
}

fn match_segments(pat: &[&str], txt: &[&str]) -> bool {
    if pat.is_empty() {
        return txt.is_empty();
    }
    if pat[0] == "**" {
        if match_segments(&pat[1..], txt) {
            return true;
        }
        if !txt.is_empty() && match_segments(pat, &txt[1..]) {
            return true;
        }
        return false;
    }
    if txt.is_empty() {
        return false;
    }
    if match_segment_glob(pat[0].as_bytes(), txt[0].as_bytes()) {
        return match_segments(&pat[1..], &txt[1..]);
    }
    false
}

pub fn resolve_policy_from_rules(rules: &[ManifestRule], path: &str) -> FilePolicy {
    for rule in rules {
        if glob_matches(&rule.pattern, path)
            && let Some(p) = FilePolicy::from_str_strict(&rule.policy)
        {
            return p;
        }
    }
    FilePolicy::UpdateOnChange
}

impl Manifest {
    pub fn validate(&mut self, op_id: Option<&str>) -> Result<(), Dr3shError> {
        if self.schema != 1 {
            return Err(Dr3shError::ManifestInvalid {
                reason: format!("unsupported schema {}, expected 1", self.schema),
            });
        }
        if !self.zip.url.starts_with("https://") {
            return Err(Dr3shError::ManifestInvalid {
                reason: format!("zip.url must start with https:// (got {})", self.zip.url),
            });
        }
        if !is_valid_sha256_hex(&self.zip.sha256) {
            return Err(Dr3shError::ManifestInvalid {
                reason: "zip.sha256 is not 64 hex characters".to_string(),
            });
        }
        if self.rules.is_empty() {
            return Err(Dr3shError::ManifestInvalid {
                reason: "rules cannot be empty".to_string(),
            });
        }
        if self.rules.last().map(|r| r.pattern.as_str()) != Some("**") {
            return Err(Dr3shError::ManifestInvalid {
                reason: "last rule pattern must be '**'".to_string(),
            });
        }
        for rule in &self.rules {
            if FilePolicy::from_str_strict(&rule.policy).is_none() {
                return Err(Dr3shError::ManifestInvalid {
                    reason: format!("invalid policy '{}' in rules", rule.policy),
                });
            }
        }

        let mut seen_paths: HashSet<String> = HashSet::with_capacity(self.files.len());
        let rules_snapshot = self.rules.clone();

        for file in &mut self.files {
            if !is_safe_relative_path(&file.path) {
                return Err(Dr3shError::ManifestInvalid {
                    reason: format!("unsafe path in files: '{}'", file.path),
                });
            }
            let lower = file.path.to_ascii_lowercase();
            if !seen_paths.insert(lower) {
                return Err(Dr3shError::ManifestInvalid {
                    reason: format!("duplicate path in files: '{}'", file.path),
                });
            }
            if !is_valid_sha256_hex(&file.sha256) {
                return Err(Dr3shError::ManifestInvalid {
                    reason: format!("invalid sha256 for file '{}'", file.path),
                });
            }
            let declared_policy = FilePolicy::from_str_strict(&file.policy).ok_or_else(|| {
                Dr3shError::ManifestInvalid {
                    reason: format!(
                        "invalid policy '{}' on file '{}'",
                        file.policy, file.path
                    ),
                }
            })?;
            if declared_policy == FilePolicy::Ignore {
                return Err(Dr3shError::ManifestInvalid {
                    reason: format!("file '{}' has forbidden 'ignore' policy", file.path),
                });
            }
            let recomputed = resolve_policy_from_rules(&rules_snapshot, &file.path);
            if recomputed == FilePolicy::Ignore {
                return Err(Dr3shError::ManifestInvalid {
                    reason: format!(
                        "file '{}' resolves to 'ignore' policy via rules",
                        file.path
                    ),
                });
            }
            if recomputed != declared_policy {
                log_event(
                    op_id,
                    "VERIFY",
                    "WARN",
                    "Manifest file policy differed from rules; using rule value",
                    &[
                        ("path", file.path.clone()),
                        ("declared", declared_policy.as_str().to_string()),
                        ("recomputed", recomputed.as_str().to_string()),
                    ],
                );
                file.policy = recomputed.as_str().to_string();
            }
        }

        Ok(())
    }

    pub fn from_json_or_legacy(
        raw_bytes: &[u8],
        fallback_pack_id: &str,
        fallback_mc_version: &str,
        fallback_loader: &str,
        fallback_loader_version: &str,
        fallback_zip_url: &str,
        op_id: Option<&str>,
    ) -> Result<Self, Dr3shError> {
        let val: serde_json::Value =
            serde_json::from_slice(raw_bytes).map_err(|e| Dr3shError::ManifestInvalid {
                reason: format!("JSON syntax error: {e}"),
            })?;

        if val.get("schema").is_none() && val.get("format").and_then(|v| v.as_u64()) == Some(2) {
            let version = val
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("1.0.0")
                .to_string();
            let archive = val.get("archive");
            let zip_size = archive
                .and_then(|a| a.get("size"))
                .and_then(|s| s.as_u64())
                .unwrap_or(0);
            let zip_sha = archive
                .and_then(|a| a.get("sha256"))
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let rules = default_rules();
            let mut files = Vec::new();
            let mut seen = HashSet::new();

            if let Some(arr) = val.get("files").and_then(|f| f.as_array()) {
                for item in arr {
                    let Some(raw_p) = item.get("path").and_then(|p| p.as_str()) else {
                        continue;
                    };
                    let norm_p = raw_p.replace('\\', "/").trim_start_matches('/').to_string();
                    if !is_safe_relative_path(&norm_p) {
                        continue;
                    }
                    let pol = resolve_policy_from_rules(&rules, &norm_p);
                    if pol == FilePolicy::Ignore {
                        continue;
                    }
                    let lower = norm_p.to_ascii_lowercase();
                    if !seen.insert(lower) {
                        continue;
                    }
                    let sha = item
                        .get("sha256")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    let sz = item.get("size").and_then(|s| s.as_u64()).unwrap_or(0);
                    let arch_p = item
                        .get("archive_path")
                        .and_then(|s| s.as_str())
                        .map(|s| s.to_string());
                    if is_valid_sha256_hex(&sha) {
                        files.push(ManifestFileEntry {
                            path: norm_p,
                            sha256: sha,
                            size: sz,
                            policy: pol.as_str().to_string(),
                            archive_path: arch_p,
                            mtime: None,
                        });
                    }
                }
            }

            log_event(
                op_id,
                "VERIFY",
                "WARN",
                "Normalized legacy format=2 manifest into schema=1",
                &[
                    ("pack_id", fallback_pack_id.to_string()),
                    ("version", version.clone()),
                    ("files", files.len().to_string()),
                ],
            );

            let mut m = Manifest {
                schema: 1,
                pack_id: fallback_pack_id.to_string(),
                version,
                minecraft_version: fallback_mc_version.to_string(),
                loader: fallback_loader.to_string(),
                loader_version: fallback_loader_version.to_string(),
                zip: ManifestZip {
                    url: fallback_zip_url.to_string(),
                    size: zip_size,
                    sha256: zip_sha,
                },
                rules,
                files,
            };
            m.validate(op_id)?;
            return Ok(m);
        }

        let mut manifest: Manifest =
            serde_json::from_value(val).map_err(|e| Dr3shError::ManifestInvalid {
                reason: format!("invalid manifest structure: {e}"),
            })?;
        manifest.validate(op_id)?;
        Ok(manifest)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PlanActionKind {
    Skip,
    Keep,
    Write,
    BackupThenWrite,
    Remove,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilePlanItem {
    pub path: String,
    pub policy: FilePolicy,
    pub action: PlanActionKind,
    pub tag: Option<String>,
    pub expected_sha256: Option<String>,
    pub expected_size: u64,
    pub archive_entry_name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReconciliationPlan {
    pub migration_mode: bool,
    pub items: Vec<FilePlanItem>,
    pub counts_by_action: HashMap<String, usize>,
    pub counts_by_tag: HashMap<String, usize>,
    pub write_bytes_total: u64,
}

impl ReconciliationPlan {
    pub fn needs_zip_download(&self) -> bool {
        self.items.iter().any(|item| {
            matches!(
                item.action,
                PlanActionKind::Write | PlanActionKind::BackupThenWrite
            )
        })
    }

    pub fn needs_zip(&self) -> bool {
        self.needs_zip_download()
    }

    pub fn has_any_changes(&self) -> bool {
        self.items.iter().any(|item| {
            matches!(
                item.action,
                PlanActionKind::Write
                    | PlanActionKind::BackupThenWrite
                    | PlanActionKind::Remove
            )
        })
    }

    pub fn write_count(&self) -> usize {
        self.items
            .iter()
            .filter(|i| i.action == PlanActionKind::Write)
            .count()
    }

    pub fn backup_then_write_count(&self) -> usize {
        self.items
            .iter()
            .filter(|i| i.action == PlanActionKind::BackupThenWrite)
            .count()
    }

    pub fn remove_count(&self) -> usize {
        self.items
            .iter()
            .filter(|i| i.action == PlanActionKind::Remove)
            .count()
    }

    pub fn noop_count(&self) -> usize {
        self.items
            .iter()
            .filter(|i| matches!(i.action, PlanActionKind::Skip | PlanActionKind::Keep))
            .count()
    }

    pub fn unknown_managed_count(&self) -> usize {
        self.items
            .iter()
            .filter(|i| i.tag.as_deref() == Some("UNKNOWN_MANAGED_REMOVED"))
            .count()
    }

    pub fn has_migration_backups(&self) -> bool {
        self.migration_mode
            && self.items.iter().any(|i| {
                matches!(
                    i.tag.as_deref(),
                    Some("OVERWROTE_NO_BASELINE") | Some("UNKNOWN_MANAGED_REMOVED")
                )
            })
    }
}

pub fn validate_relative_path(path: &str) -> Result<(), Dr3shError> {
    if is_safe_relative_path(path) {
        Ok(())
    } else {
        Err(Dr3shError::ZipUnsafePath {
            entry: path.to_string(),
        })
    }
}

pub fn file_size_and_mtime(path: &Path) -> Option<(u64, u64)> {
    let meta = fs::metadata(path).ok()?;
    let mtime = file_mtime_secs(&meta).unwrap_or(0);
    Some((meta.len(), mtime))
}

pub fn load_baseline(instance_root: &Path) -> Option<Manifest> {
    let path = instance_root.join(".dr3sh").join("installed_manifest.json");
    let raw = fs::read_to_string(&path).ok()?;
    serde_json::from_str::<Manifest>(&raw).ok()
}

pub fn save_baseline_atomic(instance_root: &Path, manifest: &Manifest) -> Result<(), Dr3shError> {
    let dr3sh_dir = instance_root.join(".dr3sh");
    fs::create_dir_all(&dr3sh_dir).map_err(|e| Dr3shError::FsPermission {
        path: dr3sh_dir.to_string_lossy().to_string(),
        os_code: e.raw_os_error().unwrap_or(-1).to_string(),
    })?;
    let target = dr3sh_dir.join("installed_manifest.json");
    let tmp = dr3sh_dir.join("installed_manifest.json.tmp");
    let bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|e| Dr3shError::unknown(format!("Failed to serialize baseline: {e}")))?;
    {
        use std::io::Write;
        let mut f = File::create(&tmp).map_err(|e| Dr3shError::FsPermission {
            path: tmp.to_string_lossy().to_string(),
            os_code: e.raw_os_error().unwrap_or(-1).to_string(),
        })?;
        f.write_all(&bytes).map_err(|e| Dr3shError::FsPermission {
            path: tmp.to_string_lossy().to_string(),
            os_code: e.raw_os_error().unwrap_or(-1).to_string(),
        })?;
        let _ = f.sync_all();
    }
    fs::rename(&tmp, &target).map_err(|e| Dr3shError::FsPermission {
        path: target.to_string_lossy().to_string(),
        os_code: e.raw_os_error().unwrap_or(-1).to_string(),
    })?;
    Ok(())
}

pub fn decide_file_action(
    policy: FilePolicy,
    b_hash: Option<&str>,
    n_hash: Option<&str>,
    l_hash: Option<&str>,
) -> (PlanActionKind, Option<&'static str>) {
    let b = b_hash.map(|s| s.to_ascii_lowercase());
    let n = n_hash.map(|s| s.to_ascii_lowercase());
    let l = l_hash.map(|s| s.to_ascii_lowercase());

    match policy {
        FilePolicy::Ignore => (PlanActionKind::Skip, None),
        FilePolicy::Seed => {
            if n.is_some() && l.is_none() {
                (PlanActionKind::Write, None)
            } else {
                (PlanActionKind::Skip, None)
            }
        }
        FilePolicy::Managed => match (&n, &l) {
            (Some(n_val), Some(l_val)) if l_val == n_val => (PlanActionKind::Skip, None),
            (Some(_), _) => (PlanActionKind::Write, None),
            (None, Some(_)) => (PlanActionKind::Remove, None),
            (None, None) => (PlanActionKind::Skip, None),
        },
        FilePolicy::UpdateOnChange => {
            let Some(n_val) = n else {
                return (PlanActionKind::Skip, Some("KEPT_REMOVED_FROM_PACK"));
            };
            if l.as_deref() == Some(n_val.as_str()) {
                return (PlanActionKind::Skip, None);
            }
            let Some(l_val) = l else {
                return (PlanActionKind::Write, None);
            };
            match b {
                Some(ref b_val) if &n_val == b_val => {
                    (PlanActionKind::Keep, Some("KEPT_PACK_UNCHANGED"))
                }
                Some(ref b_val) if &l_val == b_val => {
                    (PlanActionKind::Write, Some("UPDATED_UNMODIFIED"))
                }
                Some(_) => (
                    PlanActionKind::BackupThenWrite,
                    Some("OVERWROTE_PLAYER_CHANGE"),
                ),
                None => (
                    PlanActionKind::BackupThenWrite,
                    Some("OVERWROTE_NO_BASELINE"),
                ),
            }
        }
    }
}

pub fn file_mtime_secs(meta: &fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

pub fn hash_local_file(path: &Path) -> Result<String, std::io::Error> {
    let f = File::open(path)?;
    let mut reader = BufReader::with_capacity(256 * 1024, f);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

fn collect_local_files_under_managed(
    instance_root: &Path,
    rules: &[ManifestRule],
) -> Vec<String> {
    let mut out = Vec::new();
    walk_instance_for_managed(instance_root, instance_root, rules, &mut out);
    out
}

fn walk_instance_for_managed(
    root: &Path,
    current: &Path,
    rules: &[ManifestRule],
    out: &mut Vec<String>,
) {
    let Ok(entries) = fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if rel_str.is_empty() {
            continue;
        }
        let top_seg = rel_str.split('/').next().unwrap_or("");
        if top_seg.eq_ignore_ascii_case(".dr3sh")
            || resolve_policy_from_rules(rules, &format!("{rel_str}/placeholder"))
                == FilePolicy::Ignore
            || resolve_policy_from_rules(rules, &rel_str) == FilePolicy::Ignore
        {
            continue;
        }
        if let Ok(ft) = entry.file_type() {
            if ft.is_dir() {
                walk_instance_for_managed(root, &path, rules, out);
            } else if ft.is_file()
                && resolve_policy_from_rules(rules, &rel_str) == FilePolicy::Managed
            {
                out.push(rel_str);
            }
        }
    }
}

pub fn build_reconciliation_plan(
    instance_root: &Path,
    baseline: Option<&Manifest>,
    new_manifest: &Manifest,
    ignore_mtime_cache: bool,
) -> ReconciliationPlan {
    let migration_mode = baseline.is_none();

    let mut b_map: HashMap<String, &ManifestFileEntry> = HashMap::new();
    if let Some(b) = baseline {
        for f in &b.files {
            b_map.insert(f.path.to_ascii_lowercase(), f);
        }
    }

    let mut n_map: HashMap<String, &ManifestFileEntry> = HashMap::new();
    for f in &new_manifest.files {
        n_map.insert(f.path.to_ascii_lowercase(), f);
    }

    let mut all_keys: Vec<String> = b_map
        .keys()
        .chain(n_map.keys())
        .cloned()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    all_keys.sort();

    let mut items = Vec::with_capacity(all_keys.len());
    let mut counts_by_action: HashMap<String, usize> = HashMap::new();
    let mut counts_by_tag: HashMap<String, usize> = HashMap::new();
    let mut write_bytes_total: u64 = 0;

    for key in &all_keys {
        let b_entry = b_map.get(key).copied();
        let n_entry = n_map.get(key).copied();
        let canonical_path = n_entry
            .map(|e| e.path.clone())
            .or_else(|| b_entry.map(|e| e.path.clone()))
            .unwrap_or_else(|| key.clone());

        let policy = resolve_policy_from_rules(&new_manifest.rules, &canonical_path);
        if policy == FilePolicy::Ignore {
            continue;
        }

        let local_path = instance_root.join(&canonical_path);
        let l_hash = if local_path.is_file() {
            if !ignore_mtime_cache
                && let Some(b_ent) = b_entry
                && let Some(stored_mtime) = b_ent.mtime
                && let Ok(meta) = fs::metadata(&local_path)
                && meta.len() == b_ent.size
                && file_mtime_secs(&meta) == Some(stored_mtime)
            {
                Some(b_ent.sha256.clone())
            } else {
                hash_local_file(&local_path).ok()
            }
        } else {
            None
        };

        let (mut action, mut tag) = decide_file_action(
            policy,
            b_entry.map(|e| e.sha256.as_str()),
            n_entry.map(|e| e.sha256.as_str()),
            l_hash.as_deref(),
        );

        if ignore_mtime_cache
            && policy == FilePolicy::Managed
            && let Some(n_ent) = n_entry
            && l_hash.as_deref().map(|s| s.to_ascii_lowercase())
                != Some(n_ent.sha256.to_ascii_lowercase())
        {
            action = PlanActionKind::Write;
            tag = None;
        }

        let exp_size = n_entry.map(|e| e.size).unwrap_or(0);
        if matches!(
            action,
            PlanActionKind::Write | PlanActionKind::BackupThenWrite
        ) {
            write_bytes_total += exp_size;
        }

        let action_name = format!("{action:?}").to_uppercase();
        *counts_by_action.entry(action_name).or_insert(0) += 1;
        if let Some(t) = tag {
            *counts_by_tag.entry(t.to_string()).or_insert(0) += 1;
        }

        items.push(FilePlanItem {
            path: canonical_path,
            policy,
            action,
            tag: tag.map(|s| s.to_string()),
            expected_sha256: n_entry.map(|e| e.sha256.clone()),
            expected_size: exp_size,
            archive_entry_name: n_entry.and_then(|e| e.archive_path.clone()),
        });
    }

    let known_keys: HashSet<String> = all_keys.into_iter().collect();
    for local_rel in collect_local_files_under_managed(instance_root, &new_manifest.rules) {
        let lower = local_rel.to_ascii_lowercase();
        if !known_keys.contains(&lower) {
            *counts_by_action.entry("REMOVE".to_string()).or_insert(0) += 1;
            *counts_by_tag
                .entry("UNKNOWN_MANAGED_REMOVED".to_string())
                .or_insert(0) += 1;
            items.push(FilePlanItem {
                path: local_rel,
                policy: FilePolicy::Managed,
                action: PlanActionKind::Remove,
                tag: Some("UNKNOWN_MANAGED_REMOVED".to_string()),
                expected_sha256: None,
                expected_size: 0,
                archive_entry_name: None,
            });
        }
    }

    ReconciliationPlan {
        migration_mode,
        items,
        counts_by_action,
        counts_by_tag,
        write_bytes_total,
    }
}

pub fn verify_zip_against_manifest(
    zip_path: &Path,
    manifest: &Manifest,
) -> Result<HashMap<String, String>, Dr3shError> {
    let file = File::open(zip_path).map_err(|e| Dr3shError::ZipCorrupt {
        entry: "<archive>".to_string(),
        error: e.to_string(),
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| Dr3shError::ZipCorrupt {
        entry: "<archive>".to_string(),
        error: e.to_string(),
    })?;

    let mut zip_entries_by_lower: HashMap<String, String> = HashMap::new();
    let mut raw_norm_pairs: Vec<(String, String)> = Vec::new();
    let mut common_prefix: Option<String> = None;

    for i in 0..archive.len() {
        let entry = archive.by_index_raw(i).map_err(|e| Dr3shError::ZipCorrupt {
            entry: format!("index {i}"),
            error: e.to_string(),
        })?;
        if entry.is_dir() {
            continue;
        }
        let raw_name = entry
            .name()
            .map_err(|e| Dr3shError::ZipCorrupt {
                entry: format!("index {i}"),
                error: e.to_string(),
            })?
            .to_string();
        let norm = raw_name.replace('\\', "/");
        if !is_safe_relative_path(&norm) {
            return Err(Dr3shError::ZipUnsafePath { entry: raw_name });
        }
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
        zip_entries_by_lower.insert(norm.to_ascii_lowercase(), raw_name.clone());
        raw_norm_pairs.push((norm, raw_name));
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
        if !standard_dirs.contains(&folder.as_str()) {
            let prefix_len = prefix.len();
            for (norm, raw_name) in raw_norm_pairs {
                if norm.len() > prefix_len {
                    let stripped = norm[prefix_len..].trim_start_matches('/').to_ascii_lowercase();
                    zip_entries_by_lower.entry(stripped).or_insert(raw_name);
                }
            }
        }
    }

    let mut missing = Vec::new();
    let mut resolved_map: HashMap<String, String> = HashMap::new();
    for mf in &manifest.files {
        let lookup = mf
            .archive_path
            .as_deref()
            .unwrap_or(&mf.path)
            .replace('\\', "/")
            .to_ascii_lowercase();
        let fallback_lookup = mf.path.to_ascii_lowercase();
        if let Some(actual_name) = zip_entries_by_lower
            .get(&lookup)
            .or_else(|| zip_entries_by_lower.get(&fallback_lookup))
        {
            resolved_map.insert(mf.path.to_ascii_lowercase(), actual_name.clone());
        } else {
            missing.push(mf.path.clone());
        }
    }

    if !missing.is_empty() {
        return Err(Dr3shError::ZipContentMismatch {
            missing_entries: missing,
        });
    }

    Ok(resolved_map)
}

pub fn synthesize_manifest_from_zip(
    zip_path: &Path,
    pack_id: &str,
    version: &str,
    mc_version: &str,
    loader: &str,
    loader_version: &str,
    zip_url: &str,
) -> Result<Manifest, Dr3shError> {
    let zip_size = fs::metadata(zip_path)
        .map(|m| m.len())
        .map_err(|e| Dr3shError::ZipCorrupt {
            entry: "<archive>".to_string(),
            error: e.to_string(),
        })?;
    let zip_sha256 = hash_local_file(zip_path).map_err(|e| Dr3shError::ZipCorrupt {
        entry: "<archive>".to_string(),
        error: e.to_string(),
    })?;

    let file = File::open(zip_path).map_err(|e| Dr3shError::ZipCorrupt {
        entry: "<archive>".to_string(),
        error: e.to_string(),
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| Dr3shError::ZipCorrupt {
        entry: "<archive>".to_string(),
        error: e.to_string(),
    })?;

    let total_entries = archive.len();
    let mut common_prefix: Option<String> = None;
    for i in 0..total_entries {
        if let Ok(entry) = archive.by_index_raw(i)
            && !entry.is_dir()
            && let Ok(raw_n) = entry.name()
        {
            let name = raw_n.replace('\\', "/");
            let trimmed = name.trim_start_matches('/');
            if trimmed.is_empty() {
                continue;
            }
            if let Some((first_seg, _)) = trimmed.split_once('/') {
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

    let rules = default_rules();
    let mut files = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for i in 0..total_entries {
        let mut entry = archive.by_index(i).map_err(|e| Dr3shError::ZipCorrupt {
            entry: format!("index {i}"),
            error: e.to_string(),
        })?;
        if entry.is_dir() {
            continue;
        }
        let raw_name = entry
            .name()
            .map_err(|e| Dr3shError::ZipCorrupt {
                entry: format!("index {i}"),
                error: e.to_string(),
            })?
            .to_string();
        let norm_raw = raw_name.replace('\\', "/");
        if !is_safe_relative_path(&norm_raw) {
            return Err(Dr3shError::ZipUnsafePath { entry: raw_name });
        }
        let mut rel = norm_raw.as_str();
        if let Some(ref prefix) = common_prefix
            && let Some(stripped) = rel.strip_prefix(prefix.as_str())
        {
            rel = stripped.trim_start_matches('/');
        }
        if !is_safe_relative_path(rel) {
            continue;
        }
        let policy = resolve_policy_from_rules(&rules, rel);
        if policy == FilePolicy::Ignore {
            continue;
        }
        let lower = rel.to_ascii_lowercase();
        if !seen.insert(lower) {
            continue;
        }

        let mut hasher = Sha256::new();
        let mut buf = [0u8; 64 * 1024];
        let mut size: u64 = 0;
        loop {
            let n = entry.read(&mut buf).map_err(|e| Dr3shError::ZipCorrupt {
                entry: raw_name.clone(),
                error: e.to_string(),
            })?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            size += n as u64;
        }
        let digest = hasher.finalize();
        let sha_hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();

        files.push(ManifestFileEntry {
            path: rel.to_string(),
            sha256: sha_hex,
            size,
            policy: policy.as_str().to_string(),
            archive_path: if rel != raw_name {
                Some(raw_name)
            } else {
                None
            },
            mtime: None,
        });
    }

    let safe_url = if zip_url.starts_with("https://") {
        zip_url.to_string()
    } else {
        format!(
            "https://github.com/Dr3sh/modpack-versions/releases/download/{pack_id}-v{version}/{pack_id}-{version}.zip"
        )
    };

    let mut manifest = Manifest {
        schema: 1,
        pack_id: pack_id.to_string(),
        version: version.to_string(),
        minecraft_version: mc_version.to_string(),
        loader: loader.to_string(),
        loader_version: loader_version.to_string(),
        zip: ManifestZip {
            url: safe_url,
            size: zip_size,
            sha256: zip_sha256,
        },
        rules,
        files,
    };
    manifest.validate(None)?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_hash(ch: char) -> String {
        std::iter::repeat_n(ch, 64).collect()
    }

    #[test]
    fn test_glob_matching_and_first_match_wins() {
        assert!(glob_matches("mods/**", "mods/sodium.jar"));
        assert!(glob_matches("mods/**", "MODS/Sub/Sodium.JAR"));
        assert!(glob_matches("mods/*.jar", "mods/sodium.jar"));
        assert!(!glob_matches("mods/*.jar", "mods/nested/sodium.jar"));
        assert!(glob_matches("options.txt", "OPTIONS.TXT"));
        assert!(glob_matches("**", "anything/at/all.json"));

        let rules = vec![
            ManifestRule {
                pattern: "config/special.toml".to_string(),
                policy: "seed".to_string(),
            },
            ManifestRule {
                pattern: "config/**".to_string(),
                policy: "update_on_change".to_string(),
            },
            ManifestRule {
                pattern: "**".to_string(),
                policy: "managed".to_string(),
            },
        ];
        assert_eq!(
            resolve_policy_from_rules(&rules, "config/special.toml"),
            FilePolicy::Seed
        );
        assert_eq!(
            resolve_policy_from_rules(&rules, "config/other.toml"),
            FilePolicy::UpdateOnChange
        );
    }

    #[test]
    fn test_decision_table_all_policies_and_rows() {
        let h_a = sample_hash('a');
        let h_b = sample_hash('b');
        let h_c = sample_hash('c');

        assert_eq!(
            decide_file_action(FilePolicy::Ignore, Some(&h_a), Some(&h_b), Some(&h_c)),
            (PlanActionKind::Skip, None)
        );

        assert_eq!(
            decide_file_action(FilePolicy::Seed, None, Some(&h_a), None),
            (PlanActionKind::Write, None)
        );
        assert_eq!(
            decide_file_action(FilePolicy::Seed, Some(&h_a), Some(&h_b), Some(&h_a)),
            (PlanActionKind::Skip, None)
        );

        assert_eq!(
            decide_file_action(FilePolicy::Managed, Some(&h_a), Some(&h_a), Some(&h_a)),
            (PlanActionKind::Skip, None)
        );
        assert_eq!(
            decide_file_action(FilePolicy::Managed, Some(&h_a), Some(&h_b), Some(&h_a)),
            (PlanActionKind::Write, None)
        );
        assert_eq!(
            decide_file_action(FilePolicy::Managed, Some(&h_a), Some(&h_b), None),
            (PlanActionKind::Write, None)
        );
        assert_eq!(
            decide_file_action(FilePolicy::Managed, Some(&h_a), None, Some(&h_a)),
            (PlanActionKind::Remove, None)
        );
        assert_eq!(
            decide_file_action(FilePolicy::Managed, Some(&h_a), None, None),
            (PlanActionKind::Skip, None)
        );

        // Policy update_on_change: all 7 rows
        // Row 1: path not in N -> SKIP (KEPT_REMOVED_FROM_PACK)
        assert_eq!(
            decide_file_action(FilePolicy::UpdateOnChange, Some(&h_a), None, Some(&h_a)),
            (PlanActionKind::Skip, Some("KEPT_REMOVED_FROM_PACK"))
        );
        // Row 2: L == N -> SKIP
        assert_eq!(
            decide_file_action(
                FilePolicy::UpdateOnChange,
                Some(&h_a),
                Some(&h_b),
                Some(&h_b)
            ),
            (PlanActionKind::Skip, None)
        );
        // Row 3: L absent -> WRITE
        assert_eq!(
            decide_file_action(FilePolicy::UpdateOnChange, Some(&h_a), Some(&h_b), None),
            (PlanActionKind::Write, None)
        );
        // Row 4: B exists and N == B -> KEEP (KEPT_PACK_UNCHANGED)
        assert_eq!(
            decide_file_action(
                FilePolicy::UpdateOnChange,
                Some(&h_a),
                Some(&h_a),
                Some(&h_c)
            ),
            (PlanActionKind::Keep, Some("KEPT_PACK_UNCHANGED"))
        );
        // Row 5: B exists, N != B, L == B -> WRITE (UPDATED_UNMODIFIED)
        assert_eq!(
            decide_file_action(
                FilePolicy::UpdateOnChange,
                Some(&h_a),
                Some(&h_b),
                Some(&h_a)
            ),
            (PlanActionKind::Write, Some("UPDATED_UNMODIFIED"))
        );
        // Row 6: B exists, N != B, L != B -> BACKUP_THEN_WRITE (OVERWROTE_PLAYER_CHANGE)
        assert_eq!(
            decide_file_action(
                FilePolicy::UpdateOnChange,
                Some(&h_a),
                Some(&h_b),
                Some(&h_c)
            ),
            (
                PlanActionKind::BackupThenWrite,
                Some("OVERWROTE_PLAYER_CHANGE")
            )
        );
        // Row 7: B none, L != N -> BACKUP_THEN_WRITE (OVERWROTE_NO_BASELINE)
        assert_eq!(
            decide_file_action(FilePolicy::UpdateOnChange, None, Some(&h_b), Some(&h_a)),
            (
                PlanActionKind::BackupThenWrite,
                Some("OVERWROTE_NO_BASELINE")
            )
        );
    }

    #[test]
    fn test_unsafe_paths_and_manifest_validation() {
        assert!(!is_safe_relative_path("../evil.jar"));
        assert!(!is_safe_relative_path("mods/../evil.jar"));
        assert!(!is_safe_relative_path("/mods/a.jar"));
        assert!(!is_safe_relative_path("C:/mods/a.jar"));
        assert!(!is_safe_relative_path("mods\\a.jar"));
        assert!(is_safe_relative_path("mods/sodium-0.6.0.jar"));

        let valid_hash = sample_hash('1');
        let mut m = Manifest {
            schema: 1,
            pack_id: "test".to_string(),
            version: "1.0.0".to_string(),
            minecraft_version: "1.21.1".to_string(),
            loader: "fabric".to_string(),
            loader_version: "0.16.9".to_string(),
            zip: ManifestZip {
                url: "https://github.com/Dr3sh/modpack-versions/releases/download/v1/test.zip"
                    .to_string(),
                size: 100,
                sha256: valid_hash.clone(),
            },
            rules: default_rules(),
            files: vec![ManifestFileEntry {
                path: "mods/a.jar".to_string(),
                sha256: valid_hash.clone(),
                size: 50,
                policy: "managed".to_string(),
                archive_path: None,
                mtime: None,
            }],
        };
        assert!(m.validate(None).is_ok());

        // Invalid schema
        let mut bad_schema = m.clone();
        bad_schema.schema = 2;
        assert!(bad_schema.validate(None).is_err());

        // Duplicate case-insensitive paths
        let mut dup = m.clone();
        dup.files.push(ManifestFileEntry {
            path: "MODS/A.JAR".to_string(),
            sha256: valid_hash.clone(),
            size: 50,
            policy: "managed".to_string(),
            archive_path: None,
            mtime: None,
        });
        assert!(dup.validate(None).is_err());

        // Forbidden ignore file
        let mut ign = m.clone();
        ign.files.push(ManifestFileEntry {
            path: "saves/world/level.dat".to_string(),
            sha256: valid_hash,
            size: 50,
            policy: "managed".to_string(),
            archive_path: None,
            mtime: None,
        });
        assert!(ign.validate(None).is_err());
    }

    #[test]
    fn test_migration_and_unknown_managed_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let inst = temp.path();
        fs::create_dir_all(inst.join("mods")).unwrap();
        fs::create_dir_all(inst.join("config")).unwrap();
        fs::write(inst.join("mods/unknown_extra.jar"), b"extra").unwrap();
        fs::write(inst.join("config/player_custom.toml"), b"custom").unwrap();

        let h_mod = sample_hash('a');
        let manifest = Manifest {
            schema: 1,
            pack_id: "test".to_string(),
            version: "1.0.0".to_string(),
            minecraft_version: "1.21.1".to_string(),
            loader: "fabric".to_string(),
            loader_version: "0.16.9".to_string(),
            zip: ManifestZip {
                url: "https://github.com/Dr3sh/test.zip".to_string(),
                size: 100,
                sha256: sample_hash('f'),
            },
            rules: default_rules(),
            files: vec![ManifestFileEntry {
                path: "mods/pack_mod.jar".to_string(),
                sha256: h_mod,
                size: 10,
                policy: "managed".to_string(),
                archive_path: None,
                mtime: None,
            }],
        };

        let plan = build_reconciliation_plan(inst, None, &manifest, false);
        assert!(plan.migration_mode);
        let unknown_mod = plan
            .items
            .iter()
            .find(|i| i.path == "mods/unknown_extra.jar")
            .expect("should detect unknown mod in mods/");
        assert_eq!(unknown_mod.action, PlanActionKind::Remove);
        assert_eq!(
            unknown_mod.tag.as_deref(),
            Some("UNKNOWN_MANAGED_REMOVED")
        );

        // Unknown file in config/ must not be touched
        assert!(!plan.items.iter().any(|i| i.path.contains("player_custom")));
    }
}
