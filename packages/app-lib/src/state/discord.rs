use sqlx::SqlitePool;

use crate::state::ProcessManager;

pub struct DiscordGuard;

impl DiscordGuard {
    pub fn init() -> crate::Result<DiscordGuard> {
        Ok(DiscordGuard)
    }

    pub async fn retry_if_not_ready(&self) -> bool {
        false
    }

    pub async fn set_activity(
        &self,
        _msg: &str,
        _reconnect_if_fail: bool,
        _pool: &SqlitePool,
    ) -> crate::Result<()> {
        Ok(())
    }

    pub async fn force_set_activity(
        &self,
        _msg: &str,
        _reconnect_if_fail: bool,
    ) -> crate::Result<()> {
        Ok(())
    }

    pub async fn clear_activity(
        &self,
        _reconnect_if_fail: bool,
    ) -> crate::Result<()> {
        Ok(())
    }

    pub async fn clear_to_default(
        &self,
        _reconnect_if_fail: bool,
        _pool: &SqlitePool,
        _process_manager: &ProcessManager,
    ) -> crate::Result<()> {
        Ok(())
    }
}
