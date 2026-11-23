//! Theseus process management interface

use crate::ErrorKind;
use crate::state::ProcessMetadata;
pub use crate::{
    State,
    state::{Hooks, MemorySettings, Settings, WindowSize},
};
use uuid::Uuid;

// Gets each running stored process in the state
#[tracing::instrument]
pub async fn get_all() -> crate::Result<Vec<ProcessMetadata>> {
    let state = State::get().await?;
    let mut processes = state.process_manager.get_all();
    let active_instances: std::collections::HashSet<String> = processes
        .iter()
        .map(|p| p.instance_id.clone())
        .collect();
    if let Ok(persisted) =
        crate::state::get_persisted_running_processes(None, &state).await
    {
        for (_, _, meta) in persisted {
            if !active_instances.contains(&meta.instance_id) {
                processes.push(meta);
            }
        }
    }
    Ok(processes)
}

pub async fn resolve_instance_id(instance: &str) -> crate::Result<String> {
    let state = State::get().await?;
    resolve_instance_id_with_state(instance, &state)
        .await?
        .ok_or(
            ErrorKind::InputError(format!(
                "unknown instance id or path {instance}"
            ))
            .into(),
        )
}

async fn resolve_instance_id_with_state(
    instance: &str,
    state: &State,
) -> crate::Result<Option<String>> {
    sqlx::query_scalar!(
        "
		SELECT id
		FROM instances
		WHERE id = ? OR path = ?
		ORDER BY CASE WHEN id = ? THEN 0 ELSE 1 END
		LIMIT 1
		",
        instance,
        instance,
        instance,
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(Into::into)
}

// Gets the UUID of each stored process in the state by instance id
#[tracing::instrument]
pub async fn get_by_instance_id(
    instance_id: &str,
) -> crate::Result<Vec<ProcessMetadata>> {
    let state = State::get().await?;
    let mut processes: Vec<ProcessMetadata> = state
        .process_manager
        .get_all()
        .into_iter()
        .filter(|x| x.instance_id == instance_id)
        .collect();
    if processes.is_empty()
        && let Ok(persisted) = crate::state::get_persisted_running_processes(
            Some(instance_id),
            &state,
        )
        .await
    {
        for (_, _, meta) in persisted {
            processes.push(meta);
        }
    }
    Ok(processes)
}

// Kill a child process stored in the state by UUID, as a string
#[tracing::instrument]
pub async fn kill(uuid: Uuid) -> crate::Result<()> {
    let state = State::get().await?;
    state.process_manager.kill(uuid).await?;

    if state.process_manager.get(uuid).is_none()
        && let Ok(persisted) =
            crate::state::get_persisted_running_processes(None, &state).await
    {
        for (pid, start_time, meta) in persisted {
            if meta.uuid == uuid {
                if let Ok(pid_u32) = u32::try_from(pid) {
                    crate::state::kill_pid_tree(pid_u32).await;
                }
                crate::state::clear_persisted_process_row(
                    &state.pool,
                    Some((pid, start_time)),
                )
                .await;
            }
        }
    }

    Ok(())
}

// Wait for a child process stored in the state by UUID
#[tracing::instrument]
pub async fn wait_for(uuid: Uuid) -> crate::Result<()> {
    let state = State::get().await?;
    state.process_manager.wait_for(uuid).await?;

    Ok(())
}
