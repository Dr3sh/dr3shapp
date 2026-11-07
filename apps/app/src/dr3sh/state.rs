use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use chrono::Utc;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tauri::Emitter;

use crate::dr3sh::errors::Dr3shErrorPayload;
use crate::dr3sh::logger::{launcher_data_dir, log_event};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerInstallState {
    NotInstalled,
    Preparing,
    Downloading,
    Verifying,
    Extracting,
    Applying,
    Installed,
    Recovering,
    Error,
}

impl Default for ServerInstallState {
    fn default() -> Self {
        Self::NotInstalled
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastErrorRecord {
    pub code: String,
    pub op_id: String,
    pub message: String,
    pub details: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerStateEntry {
    pub server_id: String,
    pub instance_id: Option<String>,
    pub instance_path: Option<String>,
    pub installed_version: Option<String>,
    #[serde(default)]
    pub state: ServerInstallState,
    pub last_error: Option<LastErrorRecord>,
    #[serde(default)]
    pub migration_notice_pending: bool,
    #[serde(default)]
    pub locked: bool,
    pub active_op_id: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CachedServerGate {
    pub status: Option<String>,
    pub maintenance: bool,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Dr3shPersistentState {
    #[serde(default)]
    pub servers: HashMap<String, ServerStateEntry>,
    #[serde(default)]
    pub remote_gates: HashMap<String, CachedServerGate>,
    #[serde(default)]
    pub local_storage_migrated: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LegacyLocalStateEntry {
    pub installed_version: Option<String>,
    pub installed_folder: Option<String>,
    pub installed_instance_id: Option<String>,
}

static PERSISTENT_STATE: OnceLock<Mutex<Dr3shPersistentState>> = OnceLock::new();
static INSTANCE_LOCKS: OnceLock<DashMap<String, Arc<tokio::sync::Mutex<()>>>> = OnceLock::new();
static ACTIVE_OPS: OnceLock<DashMap<String, Arc<AtomicBool>>> = OnceLock::new();

fn state_file_path() -> PathBuf {
    launcher_data_dir().join("dr3sh_state.json")
}

fn load_state_from_disk() -> Dr3shPersistentState {
    let path = state_file_path();
    if let Ok(raw) = fs::read_to_string(&path) {
        if let Ok(mut parsed) = serde_json::from_str::<Dr3shPersistentState>(&raw) {
            for entry in parsed.servers.values_mut() {
                entry.locked = false;
                entry.active_op_id = None;
                if matches!(
                    entry.state,
                    ServerInstallState::Preparing
                        | ServerInstallState::Downloading
                        | ServerInstallState::Verifying
                        | ServerInstallState::Extracting
                        | ServerInstallState::Applying
                        | ServerInstallState::Recovering
                ) {
                    if entry.installed_version.is_some() {
                        entry.state = ServerInstallState::Installed;
                    } else {
                        entry.state = ServerInstallState::NotInstalled;
                    }
                }
            }
            return parsed;
        }
    }
    Dr3shPersistentState::default()
}

fn save_state_to_disk(state: &Dr3shPersistentState) {
    let dir = launcher_data_dir();
    let _ = fs::create_dir_all(&dir);
    let path = state_file_path();
    let tmp = dir.join("dr3sh_state.json.tmp");
    if let Ok(bytes) = serde_json::to_vec_pretty(state) {
        if let Ok(mut f) = File::create(&tmp) {
            if f.write_all(&bytes).is_ok() {
                let _ = f.sync_all();
                let _ = fs::rename(&tmp, &path);
            }
        }
    }
}

fn state_mutex() -> &'static Mutex<Dr3shPersistentState> {
    PERSISTENT_STATE.get_or_init(|| Mutex::new(load_state_from_disk()))
}

fn instance_locks() -> &'static DashMap<String, Arc<tokio::sync::Mutex<()>>> {
    INSTANCE_LOCKS.get_or_init(DashMap::new)
}

fn active_ops() -> &'static DashMap<String, Arc<AtomicBool>> {
    ACTIVE_OPS.get_or_init(DashMap::new)
}

pub fn get_instance_lock(server_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    instance_locks()
        .entry(server_id.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .value()
        .clone()
}

pub fn is_instance_locked(server_id: &str) -> bool {
    if let Some(lock) = instance_locks().get(server_id) {
        lock.try_lock().is_err()
    } else {
        false
    }
}

pub fn register_operation(op_id: &str) -> Arc<AtomicBool> {
    let token = Arc::new(AtomicBool::new(false));
    active_ops().insert(op_id.to_string(), token.clone());
    token
}

pub fn cancel_operation(op_id: &str) -> bool {
    if op_id == "*" || op_id.is_empty() {
        let mut any = false;
        for entry in active_ops().iter() {
            entry.value().store(true, Ordering::SeqCst);
            any = true;
        }
        return any;
    }
    if let Some(token) = active_ops().get(op_id) {
        token.store(true, Ordering::SeqCst);
        true
    } else {
        false
    }
}

pub fn complete_operation(op_id: &str) {
    active_ops().remove(op_id);
}

pub fn has_active_operations() -> bool {
    !active_ops().is_empty()
}

pub fn get_all_server_states() -> HashMap<String, ServerStateEntry> {
    let guard = state_mutex().lock().unwrap_or_else(|e| e.into_inner());
    let mut map = guard.servers.clone();
    for (sid, entry) in map.iter_mut() {
        entry.locked = is_instance_locked(sid);
    }
    map
}

pub fn get_server_state(server_id: &str) -> ServerStateEntry {
    let guard = state_mutex().lock().unwrap_or_else(|e| e.into_inner());
    let mut entry = guard
        .servers
        .get(server_id)
        .cloned()
        .unwrap_or_else(|| ServerStateEntry {
            server_id: server_id.to_string(),
            state: ServerInstallState::NotInstalled,
            updated_at: Utc::now().to_rfc3339(),
            ..Default::default()
        });
    entry.locked = is_instance_locked(server_id);
    entry
}

pub fn update_server_state<F>(
    app: Option<&tauri::AppHandle>,
    server_id: &str,
    mutator: F,
) -> ServerStateEntry
where
    F: FnOnce(&mut ServerStateEntry),
{
    let updated = {
        let mut guard = state_mutex().lock().unwrap_or_else(|e| e.into_inner());
        let entry = guard
            .servers
            .entry(server_id.to_string())
            .or_insert_with(|| ServerStateEntry {
                server_id: server_id.to_string(),
                state: ServerInstallState::NotInstalled,
                updated_at: Utc::now().to_rfc3339(),
                ..Default::default()
            });
        mutator(entry);
        entry.updated_at = Utc::now().to_rfc3339();
        let snapshot = entry.clone();
        save_state_to_disk(&guard);
        snapshot
    };

    let mut with_lock = updated;
    with_lock.locked = is_instance_locked(server_id);
    if let Some(app_handle) = app {
        let _ = app_handle.emit("dr3sh://state", &with_lock);
    }
    with_lock
}

pub fn set_server_error(
    app: Option<&tauri::AppHandle>,
    server_id: &str,
    payload: &Dr3shErrorPayload,
) -> ServerStateEntry {
    update_server_state(app, server_id, |entry| {
        entry.state = ServerInstallState::Error;
        entry.active_op_id = None;
        entry.last_error = Some(LastErrorRecord {
            code: payload.code.clone(),
            op_id: payload.op_id.clone(),
            message: payload.message.clone(),
            details: payload.details.clone(),
        });
    })
}

pub fn update_cached_remote_gate(server_id: &str, status: Option<String>, maintenance: bool) {
    let mut guard = state_mutex().lock().unwrap_or_else(|e| e.into_inner());
    guard.remote_gates.insert(
        server_id.to_string(),
        CachedServerGate {
            status,
            maintenance,
            updated_at: Utc::now().to_rfc3339(),
        },
    );
    save_state_to_disk(&guard);
}

pub fn get_cached_remote_gate(server_id: &str) -> Option<CachedServerGate> {
    let guard = state_mutex().lock().unwrap_or_else(|e| e.into_inner());
    guard.remote_gates.get(server_id).cloned()
}

pub fn migrate_local_storage_state(
    app: Option<&tauri::AppHandle>,
    entries: HashMap<String, LegacyLocalStateEntry>,
) -> HashMap<String, ServerStateEntry> {
    {
        let mut guard = state_mutex().lock().unwrap_or_else(|e| e.into_inner());
        for (server_id, legacy) in entries {
            let entry = guard
                .servers
                .entry(server_id.clone())
                .or_insert_with(|| ServerStateEntry {
                    server_id: server_id.clone(),
                    state: ServerInstallState::NotInstalled,
                    updated_at: Utc::now().to_rfc3339(),
                    ..Default::default()
                });

            if entry.installed_version.is_none() && legacy.installed_version.is_some() {
                entry.installed_version = legacy.installed_version;
            }
            if entry.instance_id.is_none() && legacy.installed_instance_id.is_some() {
                entry.instance_id = legacy.installed_instance_id;
            }
            if entry.instance_path.is_none() && legacy.installed_folder.is_some() {
                entry.instance_path = legacy.installed_folder;
            }
            if entry.installed_version.is_some() && entry.state == ServerInstallState::NotInstalled {
                entry.state = ServerInstallState::Installed;
            }
            entry.updated_at = Utc::now().to_rfc3339();
        }
        guard.local_storage_migrated = true;
        save_state_to_disk(&guard);
    }

    log_event(
        None,
        "STATE",
        "INFO",
        "migrated legacy localStorage install state into dr3sh_state.json",
        &[],
    );

    let all = get_all_server_states();
    if let Some(app_handle) = app {
        for val in all.values() {
            let _ = app_handle.emit("dr3sh://state", val);
        }
    }
    all
}

