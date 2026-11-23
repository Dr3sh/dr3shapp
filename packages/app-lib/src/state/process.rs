use crate::ErrorKind;
use crate::State;
use crate::event::emit::{emit_instance, emit_process};
use crate::event::{InstancePayloadType, ProcessPayloadType};
use crate::util::io::IOError;
use crate::util::rpc::RpcServer;
use chrono::{DateTime, TimeZone, Utc};
use dashmap::DashMap;
use serde::Deserialize;
use serde::Serialize;
use sqlx::SqlitePool;
use std::collections::VecDeque;
use std::fmt::Debug;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{Arc, LazyLock};
use std::time::Instant;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncSeekExt, BufReader};
use tokio::process::{Child, Command};
use uuid::Uuid;

const LAUNCHER_LOG_PATH: &str = "launcher_log.txt";
const LOG_BUFFER_CAPACITY: usize = 50_000;

struct LogRingBuffer {
    lines: VecDeque<String>,
}

impl LogRingBuffer {
    fn new() -> Self {
        Self {
            lines: VecDeque::new(),
        }
    }

    fn push(&mut self, line: String) {
        if self.lines.len() >= LOG_BUFFER_CAPACITY {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    fn get_all(&self) -> Vec<String> {
        self.lines.iter().cloned().collect()
    }

    fn clear(&mut self) {
        self.lines.clear();
    }
}

static LOG_BUFFERS: LazyLock<DashMap<String, LogRingBuffer>> =
    LazyLock::new(DashMap::new);

pub fn push_log_line(instance_id: &str, line: String) {
    LOG_BUFFERS
        .entry(instance_id.to_string())
        .or_insert_with(LogRingBuffer::new)
        .push(line);
}

pub fn get_log_buffer(instance_id: &str) -> Vec<String> {
    LOG_BUFFERS
        .get(instance_id)
        .map(|buf| buf.get_all())
        .unwrap_or_default()
}

pub fn clear_log_buffer(instance_id: &str) {
    if let Some(mut buf) = LOG_BUFFERS.get_mut(instance_id) {
        buf.clear();
    }
}

pub fn remove_log_buffer(instance_id: &str) {
    LOG_BUFFERS.remove(instance_id);
}

async fn clear_persisted_process(
    pool: &SqlitePool,
    process: Option<(i64, i64)>,
) {
    let Some((pid, start_time)) = process else {
        return;
    };
    if let Err(error) = sqlx::query!(
        "DELETE FROM processes WHERE pid = ? AND start_time = ?",
        pid,
        start_time,
    )
    .execute(pool)
    .await
    {
        tracing::warn!("Failed to clear persisted process {pid}: {error}");
    }
}

pub(crate) async fn clear_persisted_process_row(
    pool: &SqlitePool,
    process: Option<(i64, i64)>,
) {
    clear_persisted_process(pool, process).await;
}

pub(crate) async fn kill_pid_tree(pid: u32) {
    #[cfg(target_os = "windows")]
    {
        let pid_str = pid.to_string();
        let _ = tokio::task::spawn_blocking(move || {
            let mut cmd = std::process::Command::new("taskkill");
            cmd.args(["/PID", &pid_str, "/T", "/F"]);
            std::os::windows::process::CommandExt::creation_flags(
                &mut cmd,
                0x0800_0000,
            );
            let _ = cmd.output();
        })
        .await;
    }

    let _ = tokio::task::spawn_blocking(move || {
        let sys_pid = sysinfo::Pid::from_u32(pid);
        let mut system = sysinfo::System::new();
        system.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[sys_pid]),
            false,
            sysinfo::ProcessRefreshKind::nothing(),
        );
        if let Some(proc_info) = system.process(sys_pid) {
            let _ = proc_info.kill();
        }
    })
    .await;
}

pub(crate) async fn get_persisted_running_processes(
    instance_id: Option<&str>,
    state: &crate::State,
) -> crate::Result<Vec<(i64, i64, ProcessMetadata)>> {
    let rows: Vec<(i64, i64, String, String, String)> =
        if let Some(inst_id) = instance_id {
            sqlx::query_as(
                "SELECT p.pid, p.start_time, p.name, p.instance_id, COALESCE(i.path, p.instance_id)
                 FROM processes p
                 LEFT JOIN instances i ON i.id = p.instance_id
                 WHERE p.instance_id = ?",
            )
            .bind(inst_id)
            .fetch_all(&state.pool)
            .await?
        } else {
            sqlx::query_as(
                "SELECT p.pid, p.start_time, p.name, p.instance_id, COALESCE(i.path, p.instance_id)
                 FROM processes p
                 LEFT JOIN instances i ON i.id = p.instance_id",
            )
            .fetch_all(&state.pool)
            .await?
        };

    if rows.is_empty() {
        return Ok(Vec::new());
    }

    let start_times = running_process_start_times(
        rows.iter().map(|(pid, _, _, _, _)| *pid).collect(),
    )
    .await?;

    let mut live = Vec::new();
    for (pid, start_time, name, inst_id, inst_path) in rows {
        let is_live = start_times
            .get(&pid)
            .is_some_and(|started_at| started_at.abs_diff(start_time) <= 2);
        if is_live {
            let start_dt = Utc
                .timestamp_opt(start_time, 0)
                .single()
                .unwrap_or_else(Utc::now);
            let uuid = Uuid::from_u128(
                u32::try_from(pid).map(u128::from).unwrap_or(0),
            );
            live.push((
                pid,
                start_time,
                ProcessMetadata {
                    uuid,
                    instance_id: inst_id,
                    instance_path: inst_path,
                    instance_name: name,
                    start_time: start_dt,
                },
            ));
        } else {
            clear_persisted_process(&state.pool, Some((pid, start_time))).await;
        }
    }

    Ok(live)
}

/// Looks up the start time, in Unix seconds, of each listed PID that is still running.
///
/// Only the listed processes are refreshed, on a blocking thread, so callers holding
/// content locks are not stalled by a full system scan.
pub(crate) async fn running_process_start_times(
    pids: Vec<i64>,
) -> crate::Result<std::collections::HashMap<i64, i64>> {
    Ok(tokio::task::spawn_blocking(move || {
        let pids = pids
            .into_iter()
            .filter_map(|pid| u32::try_from(pid).ok())
            .map(sysinfo::Pid::from_u32)
            .collect::<Vec<_>>();
        let mut system = sysinfo::System::new();
        system.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&pids),
            false,
            sysinfo::ProcessRefreshKind::nothing(),
        );
        pids.iter()
            .filter_map(|pid| {
                system.process(*pid).map(|process| {
                    (i64::from(pid.as_u32()), process.start_time() as i64)
                })
            })
            .collect()
    })
    .await?)
}

pub(crate) async fn instance_has_running_process(
    instance_id: &str,
    state: &crate::State,
) -> crate::Result<bool> {
    if state
        .process_manager
        .get_all()
        .iter()
        .any(|process| process.instance_id == instance_id)
    {
        return Ok(true);
    }

    let processes = sqlx::query!(
        "
		SELECT pid, start_time
		FROM processes
		WHERE instance_id = ?
		",
        instance_id,
    )
    .fetch_all(&state.pool)
    .await?;
    if processes.is_empty() {
        return Ok(false);
    }
    let start_times = running_process_start_times(
        processes.iter().map(|process| process.pid).collect(),
    )
    .await?;
    let mut running = false;
    for process in processes {
        let process_is_running =
            start_times.get(&process.pid).is_some_and(|started_at| {
                started_at.abs_diff(process.start_time) <= 2
            });
        if process_is_running {
            running = true;
        } else {
            clear_persisted_process(
                &state.pool,
                Some((process.pid, process.start_time)),
            )
            .await;
        }
    }
    Ok(running)
}

pub struct ProcessManager {
    processes: DashMap<Uuid, Process>,
}

impl Default for ProcessManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            processes: DashMap::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_new_process(
        &self,
        instance_id: &str,
        instance_path: &str,
        instance_name: &str,
        mut mc_command: Command,
        post_exit_command: Option<String>,
        post_exit_env_vars: Vec<(String, String)>,
        logs_folder: PathBuf,
        _xml_logging: bool,
        main_class_keep_alive: Option<TempDir>,
        rpc_server: Option<RpcServer>,
        state: &Arc<State>,
        post_process_init: impl AsyncFnOnce(
            &ProcessMetadata,
            Option<&RpcServer>,
        ) -> crate::Result<()>,
    ) -> crate::Result<ProcessMetadata> {
        let executable = mc_command
            .as_std()
            .get_program()
            .to_string_lossy()
            .into_owned();

        if !logs_folder.exists() {
            tokio::fs::create_dir_all(&logs_folder)
                .await
                .map_err(|e| IOError::with_path(e, &logs_folder))?;
        }

        let log_path = logs_folder.join(LAUNCHER_LOG_PATH);

        clear_log_buffer(instance_id);

        {
            let mut log_file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&log_path)
                .map_err(|e| IOError::with_path(e, &log_path))?;

            let now = chrono::Local::now();
            writeln!(
                log_file,
                "# Minecraft launcher log started at {}",
                now.format("%Y-%m-%d %H:%M:%S")
            )
            .map_err(|e| IOError::with_path(e, &log_path))?;
            writeln!(log_file, "# Instance: {instance_path} \n")
                .map_err(|e| IOError::with_path(e, &log_path))?;
            writeln!(log_file).map_err(|e| IOError::with_path(e, &log_path))?;
        }

        let initial_log_offset = std::fs::metadata(&log_path)
            .map(|m| m.len())
            .unwrap_or(0);

        // Redirect stdio directly to disk with detached flags so closing the launcher
        // never breaks pipes or terminates the Java process.
        let stdout_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| IOError::with_path(e, &log_path))?;
        let stderr_file = stdout_file
            .try_clone()
            .map_err(|e| IOError::with_path(e, &log_path))?;

        mc_command.stdin(std::process::Stdio::null());
        mc_command.stdout(std::process::Stdio::from(stdout_file));
        mc_command.stderr(std::process::Stdio::from(stderr_file));
        mc_command.kill_on_drop(false);

        #[cfg(target_os = "windows")]
        let mc_proc = {
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

            mc_command.creation_flags(
                DETACHED_PROCESS
                    | CREATE_NEW_PROCESS_GROUP
                    | CREATE_BREAKAWAY_FROM_JOB,
            );
            match mc_command.spawn() {
                Ok(child) => child,
                Err(err)
                    if err.kind() == std::io::ErrorKind::PermissionDenied
                        || err.raw_os_error() == Some(5) =>
                {
                    tracing::warn!(
                        "Retrying Java spawn without CREATE_BREAKAWAY_FROM_JOB: {err}"
                    );
                    mc_command.creation_flags(
                        DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP,
                    );
                    mc_command.spawn().map_err(IOError::from)?
                }
                Err(err) => return Err(IOError::from(err).into()),
            }
        };

        #[cfg(not(target_os = "windows"))]
        let mc_proc = mc_command.spawn().map_err(IOError::from)?;

        let child_pid = mc_proc.id();

        let process = Process {
            metadata: ProcessMetadata {
                uuid: Uuid::new_v4(),
                start_time: Utc::now(),
                instance_id: instance_id.to_string(),
                instance_path: instance_path.to_string(),
                instance_name: instance_name.to_string(),
            },
            child: mc_proc,
            rpc_server,
            _main_class_keep_alive: main_class_keep_alive,
        };

        let persisted_process = child_pid.map(|pid| {
            (i64::from(pid), process.metadata.start_time.timestamp())
        });
        if let Some((pid, start_time)) = persisted_process {
            let post_exit_command = post_exit_command.as_deref();
            if let Err(error) = sqlx::query!(
                "
				INSERT INTO processes
					(pid, start_time, name, executable, instance_id,
					 post_exit_command)
				VALUES (?, ?, ?, ?, ?, ?)
				ON CONFLICT(pid) DO UPDATE SET
					start_time = excluded.start_time,
					name = excluded.name,
					executable = excluded.executable,
					instance_id = excluded.instance_id,
					post_exit_command = excluded.post_exit_command
				",
                pid,
                start_time,
                instance_name,
                executable,
                instance_id,
                post_exit_command,
            )
            .execute(&state.pool)
            .await
            {
                if let Some(pid_u32) = child_pid {
                    kill_pid_tree(pid_u32).await;
                }
                return Err(error.into());
            }
        }

        if let Err(e) = post_process_init(
            &process.metadata,
            process.rpc_server.as_ref(),
        )
        .await
        {
            tracing::error!("Failed to run post-process init: {e}");
            clear_persisted_process(&state.pool, persisted_process).await;
            if let Some(pid_u32) = child_pid {
                kill_pid_tree(pid_u32).await;
            }
            return Err(e);
        }

        let metadata = process.metadata.clone();

        self.processes.insert(metadata.uuid, process);

        {
            let log_path_clone = log_path.clone();
            let instance_id_c = metadata.instance_id.clone();
            let uuid = metadata.uuid;
            let state_c = Arc::clone(state);
            tokio::spawn(async move {
                Process::tail_log_file(
                    &instance_id_c,
                    log_path_clone,
                    initial_log_offset,
                    uuid,
                    state_c,
                )
                .await;
            });
        }

        tokio::spawn(Process::sequential_process_manager(
            instance_id.to_string(),
            instance_path.to_string(),
            post_exit_command,
            post_exit_env_vars,
            metadata.uuid,
            persisted_process,
            Arc::clone(state),
        ));

        emit_process(
            instance_id,
            metadata.uuid,
            ProcessPayloadType::Launched,
            "Launched Minecraft",
        )
        .await?;

        Ok(metadata)
    }

    pub fn get(&self, id: Uuid) -> Option<ProcessMetadata> {
        self.processes.get(&id).map(|x| x.metadata.clone())
    }

    pub fn get_rpc(&self, id: Uuid) -> Option<RpcServer> {
        self.processes
            .get(&id)
            .and_then(|x| x.rpc_server.clone())
    }

    pub fn get_all(&self) -> Vec<ProcessMetadata> {
        self.processes
            .iter()
            .map(|x| x.value().metadata.clone())
            .collect()
    }

    pub fn try_wait(
        &self,
        id: Uuid,
    ) -> crate::Result<Option<Option<ExitStatus>>> {
        if let Some(mut process) = self.processes.get_mut(&id) {
            Ok(Some(process.child.try_wait()?))
        } else {
            Ok(None)
        }
    }

    pub async fn wait_for(&self, id: Uuid) -> crate::Result<()> {
        if let Some(mut process) = self.processes.get_mut(&id) {
            process.child.wait().await?;
        }
        Ok(())
    }

    pub async fn kill(&self, id: Uuid) -> crate::Result<()> {
        let pid = self
            .processes
            .get(&id)
            .and_then(|process| process.child.id());
        if let Some(pid) = pid {
            kill_pid_tree(pid).await;
        }
        if let Some(mut process) = self.processes.get_mut(&id) {
            let _ = process.child.kill().await;
        }

        Ok(())
    }

    fn remove(&self, id: Uuid) {
        self.processes.remove(&id);
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ProcessMetadata {
    pub uuid: Uuid,
    pub instance_id: String,
    pub instance_path: String,
    pub instance_name: String,
    pub start_time: DateTime<Utc>,
}

#[derive(Debug)]
struct Process {
    metadata: ProcessMetadata,
    child: Child,
    _main_class_keep_alive: Option<TempDir>,
    rpc_server: Option<RpcServer>,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
#[cfg_attr(
    feature = "export-ts",
    derive(ts_rs::TS, postcard_bindgen::PostcardBindings)
)]
pub struct Log4jEvent {
    pub timestamp_millis: Option<i64>,
    pub logger_name: Option<String>,
    pub level: Option<String>,
    pub thread_name: Option<String>,
    pub message: Option<String>,
    pub throwable: Option<String>,
}

impl Process {
    async fn tail_log_file(
        instance_id: &str,
        log_path: PathBuf,
        initial_offset: u64,
        uuid: Uuid,
        state: Arc<State>,
    ) {
        let Ok(mut file) = tokio::fs::File::open(&log_path).await else {
            return;
        };
        let _ = file.seek(std::io::SeekFrom::Start(initial_offset)).await;
        let mut reader = BufReader::new(file);
        let mut line = String::new();

        loop {
            let mut read_any = false;
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        read_any = true;
                        let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
                        if !trimmed.is_empty() {
                            Self::emit_legacy_log(instance_id, trimmed);
                            if let Some(pos) = trimmed.find("Connecting to ") {
                                let clean_msg = trimmed[pos..]
                                    .trim_end_matches("</log4j:Message>")
                                    .trim_end_matches("]]>")
                                    .trim();
                                let _ = Self::parse_and_insert_server_join(
                                    instance_id,
                                    clean_msg,
                                    Utc::now(),
                                    &state.pool,
                                )
                                .await;
                            }
                        }
                    }
                }
            }

            if state.process_manager.get(uuid).is_none() {
                if !read_any {
                    break;
                }
            } else if !read_any {
                tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
            }
        }
    }

    fn emit_legacy_log(instance_id: &str, message: &str) {
        push_log_line(instance_id, message.to_string());
    }

    fn append_to_log_file(
        path: impl AsRef<Path>,
        line: &str,
    ) -> std::io::Result<()> {
        let mut file =
            OpenOptions::new().append(true).create(true).open(path)?;

        file.write_all(line.as_bytes())?;
        Ok(())
    }

    async fn parse_and_insert_server_join(
        instance_id: &str,
        message: &str,
        timestamp: DateTime<Utc>,
        pool: &SqlitePool,
    ) -> crate::Result<()> {
        let Some(host_port_string) = message.strip_prefix("Connecting to ")
        else {
            return Ok(());
        };
        let Some((host, port_string)) = host_port_string.rsplit_once(", ")
        else {
            return Ok(());
        };
        let Some(port) = port_string.parse::<u16>().ok() else {
            return Ok(());
        };

        crate::state::server_join_log::JoinLogEntry {
            instance_id: instance_id.to_owned(),
            host: host.to_string(),
            port,
            join_time: timestamp,
        }
        .upsert(pool)
        .await?;
        {
            let instance_id = instance_id.to_owned();
            let host = host.to_owned();
            tokio::spawn(async move {
                let _ = emit_instance(
                    &instance_id,
                    InstancePayloadType::ServerJoined {
                        host,
                        port,
                        timestamp: timestamp.to_rfc3339(),
                    },
                )
                .await;
            });
        }

        Ok(())
    }

    // Spawns a new child process and inserts it into the hashmap
    // Also, as the process ends, it spawns the follow-up process if it exists
    // By convention, ExitStatus is last command's exit status, and we exit on the first non-zero exit status
    async fn sequential_process_manager(
        instance_id: String,
        instance_path: String,
        post_exit_command: Option<String>,
        post_exit_env_vars: Vec<(String, String)>,
        uuid: Uuid,
        persisted_process: Option<(i64, i64)>,
        state: Arc<State>,
    ) -> crate::Result<()> {
        async fn update_playtime(
            last_updated_playtime: &mut Instant,
            instance_id: &str,
            force_update: bool,
            pool: &SqlitePool,
        ) {
            let elapsed = last_updated_playtime.elapsed().as_secs();
            if elapsed == 0 || (!force_update && elapsed < 60) {
                return;
            }

            if let Err(e) =
                crate::state::instances::commands::add_instance_recent_playtime(
                    instance_id,
                    elapsed,
                    pool,
                )
                .await
            {
                tracing::warn!(
                    "Failed to update playtime for instance {}: {}",
                    instance_id,
                    e
                );
            }
            *last_updated_playtime = Instant::now();
        }

        // Wait on current Minecraft Child
        let mc_exit_status;
        let mut last_updated_playtime = Instant::now();

        loop {
            if let Some(process) = state.process_manager.try_wait(uuid)? {
                if let Some(t) = process {
                    mc_exit_status = t;
                    break;
                }
            } else {
                mc_exit_status = ExitStatus::default();
                break;
            }

            // sleep for 10ms
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

            // Auto-update playtime every minute
            update_playtime(
                &mut last_updated_playtime,
                &instance_id,
                false,
                &state.pool,
            )
            .await;
        }

        state.process_manager.remove(uuid);
        clear_persisted_process(&state.pool, persisted_process).await;
        let sync_instance_id = instance_id.clone();
        tokio::spawn(async move {
            if let Err(error) =
                crate::api::instance::reconcile_instance_synced_options(
                    &sync_instance_id,
                )
                .await
            {
                tracing::warn!(
                    "Failed to reconcile synced options after closing {sync_instance_id}: {error}"
                );
            }
        });
        emit_process(
            &instance_id,
            uuid,
            ProcessPayloadType::Finished,
            "Exited process",
        )
        .await?;

        // Now fully complete- update playtime one last time
        update_playtime(
            &mut last_updated_playtime,
            &instance_id,
            true,
            &state.pool,
        )
        .await;

        // Publish play time update
        // Allow failure, it will be stored locally and sent next time
        // Sent in another thread as first call may take a couple seconds and hold up process ending
        let playtime_instance_id = instance_id.clone();
        tokio::spawn(async move {
            if let Err(e) =
                crate::api::instance::try_update_playtime_by_instance_id(
                    &playtime_instance_id,
                )
                .await
            {
                tracing::warn!(
                    "Failed to update playtime for instance {}: {}",
                    playtime_instance_id,
                    e
                );
            }
        });

        let logs_folder = state.directories.instance_logs_dir(&instance_path);
        let log_path = logs_folder.join(LAUNCHER_LOG_PATH);

        if log_path.exists()
            && let Err(e) = Process::append_to_log_file(
                &log_path,
                &format!("\n# Process exited with status: {mc_exit_status}\n"),
            )
        {
            tracing::warn!("Failed to write exit status to log file: {}", e);
        }

        let _ = state
            .presence
            .discord_rpc
            .clear_to_default(true, &state.pool, &state.process_manager)
            .await;

        let _ = state.presence.friends_socket.update_status(None).await;

        #[cfg(feature = "tauri")]
        {
            let settings = crate::state::Settings::get(&state.pool).await?;
            if settings.refocus_on_game_close
                && let Some(window) =
                    crate::EventState::get_main_window().await?
            {
                window.unminimize()?;
                window.set_focus()?;
            }
        }

        if mc_exit_status.success() {
            // We do not wait on the post exist command to finish running! We let it spawn + run on its own.
            // This behaviour may be changed in the future
            if let Some(hook) = post_exit_command {
                let mut cmd = shlex::split(&hook)
                    .ok_or(ErrorKind::LauncherError(format!(
                        "invalid post-exit command {hook}",
                    )))?
                    .into_iter();

                if let Some(command) = cmd.next() {
                    let mut command = Command::new(command);
                    command.args(cmd).envs(post_exit_env_vars).current_dir(
                        state.directories.instances_dir().join(&instance_path),
                    );
                    command.spawn().map_err(IOError::from)?;
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::running_process_start_times;

    #[tokio::test]
    async fn reads_start_time_of_running_process() {
        let pid = i64::from(std::process::id());
        let start_times = running_process_start_times(vec![pid, i64::MAX])
            .await
            .unwrap();

        let started_at = start_times[&pid];
        let now = chrono::Utc::now().timestamp();
        assert!(started_at > 0 && started_at <= now);
        assert_eq!(start_times.len(), 1);
    }
}
