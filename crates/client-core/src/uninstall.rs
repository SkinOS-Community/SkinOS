//! Binds an uninstall confirmation to the exact server-side filesystem plan.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use skinos_downloader::library::UninstallPlan;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const PLAN_TTL: Duration = Duration::from_secs(15 * 60);
static NEXT_PLAN: AtomicU64 = AtomicU64::new(1);

struct Prepared {
    steamid: String,
    library: PathBuf,
    created: Instant,
    plan: UninstallPlan,
}

#[derive(Default)]
pub(crate) struct Confirmations {
    plans: Mutex<HashMap<String, Prepared>>,
}

impl Confirmations {
    pub fn insert(&self, steamid: &str, library: &Path, plan: UninstallPlan) -> Result<Value> {
        let mut payload = serde_json::to_value(&plan)?;
        let token = format!(
            "{}-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
            NEXT_PLAN.fetch_add(1, Ordering::Relaxed)
        );
        let mut plans = self
            .plans
            .lock()
            .map_err(|_| anyhow::anyhow!("Uninstall plan unavailable"))?;
        plans.retain(|_, p| {
            p.created.elapsed() < PLAN_TTL && !(p.steamid == steamid && p.plan.appid == plan.appid)
        });
        ensure!(
            plans.len() < 64,
            "Слишком много открытых подтверждений удаления"
        );
        plans.insert(
            token.clone(),
            Prepared {
                steamid: steamid.into(),
                library: library.to_path_buf(),
                created: Instant::now(),
                plan,
            },
        );
        payload["confirmation_token"] = Value::String(token);
        Ok(payload)
    }

    pub fn take(
        &self,
        token: &str,
        steamid: &str,
        appid: u32,
        library: &Path,
    ) -> Result<UninstallPlan> {
        let mut plans = self
            .plans
            .lock()
            .map_err(|_| anyhow::anyhow!("Uninstall plan unavailable"))?;
        let prepared = plans
            .get(token)
            .context("Откройте подтверждение удаления заново")?;
        ensure!(
            prepared.steamid == steamid && prepared.plan.appid == appid,
            "Подтверждение относится к другой игре или аккаунту"
        );
        ensure!(
            prepared.created.elapsed() < PLAN_TTL && prepared.library == library,
            "План удаления устарел. Откройте подтверждение заново"
        );
        Ok(plans.remove(token).unwrap().plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf, UninstallPlan) {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("steamapps");
        std::fs::create_dir_all(library.join("common/Game")).unwrap();
        std::fs::write(library.join("common/Game/game.bin"), b"installed").unwrap();
        std::fs::write(
            library.join("appmanifest_42.acf"),
            b"\"AppState\" { \"appid\" \"42\" \"StateFlags\" \"4\" \"installdir\" \"Game\" }",
        )
        .unwrap();
        let plan = skinos_downloader::library::uninstall_plan(&library, 42, None).unwrap();
        (temp, library, plan)
    }

    #[test]
    fn confirmation_is_bound_to_account_game_library_and_is_single_use() {
        let (_temp, library, plan) = fixture();
        let store = Confirmations::default();
        let payload = store.insert("account-a", &library, plan).unwrap();
        let token = payload["confirmation_token"].as_str().unwrap();
        assert!(store.take(token, "account-b", 42, &library).is_err());
        assert!(store.take(token, "account-a", 43, &library).is_err());
        assert!(
            store
                .take(token, "account-a", 42, Path::new("/other/steamapps"))
                .is_err()
        );
        assert_eq!(
            store.take(token, "account-a", 42, &library).unwrap().appid,
            42
        );
        assert!(store.take(token, "account-a", 42, &library).is_err());
        assert!(library.join("common/Game/game.bin").is_file());
    }

    #[test]
    fn newer_preview_invalidates_old_confirmation_and_plans_expire() {
        let (_temp, library, plan) = fixture();
        let store = Confirmations::default();
        let old = store.insert("account", &library, plan.clone()).unwrap();
        let new = store.insert("account", &library, plan).unwrap();
        assert!(
            store
                .take(
                    old["confirmation_token"].as_str().unwrap(),
                    "account",
                    42,
                    &library
                )
                .is_err()
        );
        let token = new["confirmation_token"].as_str().unwrap();
        store.plans.lock().unwrap().get_mut(token).unwrap().created = Instant::now() - PLAN_TTL;
        assert!(store.take(token, "account", 42, &library).is_err());
        assert!(library.join("common/Game/game.bin").is_file());
    }
}
