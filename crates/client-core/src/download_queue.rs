//! Account-scoped download scheduling. Disk state contains intent only, never a session.
use crate::model::{Event, Request};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;
use skinos_downloader::{InstallControl, InstallInterrupted, InstallPlatform};
use std::{
    collections::HashMap, future::Future, path::PathBuf, pin::Pin, sync::Arc, time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

pub type DownloadFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;
#[derive(Clone)]
pub struct DownloadJob {
    pub steamid: String,
    pub appid: u32,
    pub is_update: bool,
    pub platform: Option<InstallPlatform>,
    pub request_id: Option<String>,
}
pub trait DownloadExecutor: Send + Sync + 'static {
    fn validate(&self, job: &DownloadJob) -> Result<InstallPlatform>;
    fn can_start(&self, job: &DownloadJob) -> bool;
    fn resource_key(&self, job: &DownloadJob) -> Result<String>;
    fn cleanup_completed(
        &self,
        _job: DownloadJob,
        _events: mpsc::UnboundedSender<Event>,
    ) -> DownloadFuture {
        Box::pin(async { Ok(()) })
    }
    fn run(
        &self,
        job: DownloadJob,
        control: InstallControl,
        events: mpsc::UnboundedSender<Event>,
    ) -> DownloadFuture;
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DownloadState {
    Queued,
    Preparing,
    Verifying,
    Downloading,
    Installing,
    Pausing,
    Paused,
    Cancelling,
    Cancelled,
    Completed,
    Failed,
}
impl DownloadState {
    fn active(self) -> bool {
        matches!(
            self,
            Self::Preparing
                | Self::Verifying
                | Self::Downloading
                | Self::Installing
                | Self::Pausing
                | Self::Cancelling
        )
    }
    fn unfinished(self) -> bool {
        self.active() || matches!(self, Self::Queued | Self::Paused)
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct DownloadItem {
    pub appid: u32,
    pub state: DownloadState,
    pub queue_position: usize,
    pub is_update: bool,
    pub platform: Option<InstallPlatform>,
    pub progress: Option<f64>,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub bytes_per_second: f64,
    pub eta_seconds: Option<u64>,
    pub reused_bytes: u64,
    pub processed_bytes: u64,
    pub install_total_bytes: u64,
    pub files_completed: u64,
    pub total_files: u64,
    pub chunks_completed: u64,
    pub total_chunks: u64,
    pub message: Option<String>,
    pub error: Option<String>,
    pub remove_requested: bool,
    #[serde(skip)]
    request_id: Option<String>,
    #[serde(skip)]
    attempt_base: u64,
    #[serde(skip)]
    attempt_wire_seen: u64,
}
impl DownloadItem {
    fn new(appid: u32, is_update: bool, state: DownloadState) -> Self {
        Self {
            appid,
            state,
            queue_position: 0,
            is_update,
            platform: None,
            progress: None,
            downloaded_bytes: 0,
            total_bytes: 0,
            bytes_per_second: 0.0,
            eta_seconds: None,
            reused_bytes: 0,
            processed_bytes: 0,
            install_total_bytes: 0,
            files_completed: 0,
            total_files: 0,
            chunks_completed: 0,
            total_chunks: 0,
            message: None,
            error: None,
            remove_requested: false,
            request_id: None,
            attempt_base: 0,
            attempt_wire_seen: 0,
        }
    }
    fn job(&self, steamid: &str) -> DownloadJob {
        DownloadJob {
            steamid: steamid.into(),
            appid: self.appid,
            is_update: self.is_update,
            platform: self.platform,
            request_id: self.request_id.clone(),
        }
    }
    fn progress(&mut self, value: &serde_json::Value) {
        // Late progress cannot overwrite an accepted interruption state.
        if !matches!(
            self.state,
            DownloadState::Pausing | DownloadState::Cancelling
        ) {
            self.state = match value["phase"].as_str().unwrap_or("") {
                "preparing" => DownloadState::Preparing,
                "verifying" => DownloadState::Verifying,
                "downloading" => DownloadState::Downloading,
                "installing" | "committing" | "complete" => DownloadState::Installing,
                _ => self.state,
            };
        }
        self.progress = value["progress"]
            .as_f64()
            .filter(|v| v.is_finite())
            .map(|v| v.clamp(0.0, 1.0));
        self.attempt_wire_seen = self
            .attempt_wire_seen
            .max(value["downloaded_bytes"].as_u64().unwrap_or(0));
        self.downloaded_bytes = self.attempt_base.saturating_add(self.attempt_wire_seen);
        if let Some(total) = value["total_bytes"].as_u64().filter(|v| *v > 0) {
            self.total_bytes = self
                .attempt_base
                .saturating_add(total)
                .max(self.downloaded_bytes);
        }
        if self.state == DownloadState::Downloading && self.total_bytes > 0 {
            self.progress =
                Some((self.downloaded_bytes as f64 / self.total_bytes as f64).clamp(0.0, 1.0));
        }
        self.bytes_per_second = value["bytes_per_second"]
            .as_f64()
            .filter(|v| v.is_finite() && *v >= 0.0)
            .unwrap_or(0.0);
        self.eta_seconds = value["eta_seconds"].as_u64();
        self.reused_bytes = value["reused_bytes"].as_u64().unwrap_or(self.reused_bytes);
        self.processed_bytes = value["processed_bytes"]
            .as_u64()
            .unwrap_or(self.processed_bytes);
        self.install_total_bytes = value["install_total_bytes"]
            .as_u64()
            .unwrap_or(self.install_total_bytes);
        self.files_completed = value["files_completed"]
            .as_u64()
            .unwrap_or(self.files_completed);
        self.total_files = value["total_files"].as_u64().unwrap_or(self.total_files);
        self.chunks_completed = value["chunks_completed"]
            .as_u64()
            .unwrap_or(self.chunks_completed);
        self.total_chunks = value["total_chunks"].as_u64().unwrap_or(self.total_chunks);
        self.message = value["message"].as_str().map(str::to_owned);
        if matches!(
            self.state,
            DownloadState::Pausing | DownloadState::Cancelling
        ) {
            self.bytes_per_second = 0.0;
            self.eta_seconds = None;
        }
    }
}
#[derive(Serialize, Deserialize)]
struct Intent {
    appid: u32,
    is_update: bool,
    #[serde(default)]
    platform: Option<InstallPlatform>,
    state: DownloadState,
    #[serde(default)]
    downloaded_bytes: u64,
    #[serde(default)]
    total_bytes: u64,
    #[serde(default)]
    remove_requested: bool,
}
#[derive(Serialize, Deserialize)]
struct SavedQueue {
    steamid: String,
    max_concurrent: usize,
    revision: u64,
    items: Vec<Intent>,
}

/// All persisted account intents participate in the cache reservation. An older
/// paused/cancelled/failed attempt must not lose resumable data during another
/// account's cleanup. Malformed queue state fails closed.
pub(crate) fn completed_cache_cleanup_allowed(
    data_root: &std::path::Path,
    appid: u32,
) -> Result<bool> {
    let directory = data_root.join("download-queues");
    ensure!(
        !directory.is_symlink(),
        "Download queue directory cannot be a symlink"
    );
    let mut completed = false;
    for entry in std::fs::read_dir(directory).context("Cannot verify download queue states")? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&path)?;
        ensure!(
            metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.len() <= 4 * 1024 * 1024,
            "Cannot verify download queue file"
        );
        let queue: SavedQueue = serde_json::from_slice(&std::fs::read(&path)?)
            .context("Cannot verify download queue contents")?;
        for item in queue.items.iter().filter(|item| item.appid == appid) {
            if item.state != DownloadState::Completed {
                return Ok(false);
            }
            completed = true;
        }
    }
    Ok(completed)
}
enum Command {
    Request(Request),
    Executor(Arc<dyn DownloadExecutor>),
    SuspendConnection,
    Reconnect(Arc<dyn DownloadExecutor>),
    RemoveIdle {
        appid: u32,
        reply: oneshot::Sender<Result<()>>,
    },
    Shutdown,
}
enum JobEvent {
    Event(u64, u32, Event),
    Finished(u64, u32, Result<()>),
}
struct Active {
    run_id: u64,
    control: InstallControl,
    resource: String,
    task: JoinHandle<()>,
}
#[derive(Clone)]
pub struct DownloadQueueHandle {
    commands: mpsc::UnboundedSender<Command>,
}
impl DownloadQueueHandle {
    /// Uninstall uses this only after it has reserved the app and destination.
    /// This operation never interrupts an active writer or touches game files.
    pub async fn remove_if_idle(&self, appid: u32) -> Result<()> {
        let (reply, received) = oneshot::channel();
        self.commands
            .send(Command::RemoveIdle { appid, reply })
            .map_err(|_| anyhow::anyhow!("Очередь загрузок недоступна"))?;
        received.await.context("Очередь загрузок завершила сеанс")?
    }
}
pub struct DownloadQueue {
    steamid: String,
    commands: mpsc::UnboundedSender<Command>,
    task: Option<JoinHandle<()>>,
}
impl DownloadQueue {
    pub fn handle(&self) -> DownloadQueueHandle {
        DownloadQueueHandle {
            commands: self.commands.clone(),
        }
    }
    pub fn start(
        steamid: String,
        path: PathBuf,
        executor: Arc<dyn DownloadExecutor>,
        events: mpsc::UnboundedSender<Event>,
    ) -> Result<Self> {
        let actor = Actor::load(steamid.clone(), path, executor, events)?;
        let (commands, rx) = mpsc::unbounded_channel();
        Ok(Self {
            steamid,
            commands,
            task: Some(tokio::spawn(actor.run(rx))),
        })
    }
    pub fn request(&self, request: Request) -> Result<()> {
        self.commands
            .send(Command::Request(request))
            .map_err(|_| anyhow::anyhow!("Очередь загрузок недоступна"))
    }
    pub fn update_executor(&self, executor: Arc<dyn DownloadExecutor>) {
        let _ = self.commands.send(Command::Executor(executor));
    }
    pub fn suspend_connection(&self) {
        let _ = self.commands.send(Command::SuspendConnection);
    }
    pub fn reconnect(&self, steamid: &str, executor: Arc<dyn DownloadExecutor>) -> Result<()> {
        ensure!(
            steamid == self.steamid,
            "Cannot reconnect another account's download queue"
        );
        self.commands
            .send(Command::Reconnect(executor))
            .map_err(|_| anyhow::anyhow!("Download queue has stopped"))
    }
    pub async fn shutdown(mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}
impl Drop for DownloadQueue {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
    }
}
struct Actor {
    steamid: String,
    path: PathBuf,
    executor: Arc<dyn DownloadExecutor>,
    events: mpsc::UnboundedSender<Event>,
    items: Vec<DownloadItem>,
    max_concurrent: usize,
    revision: u64,
    next_run: u64,
    active: HashMap<u32, Active>,
    pending_removals: HashMap<u32, String>,
    stopping: bool,
    connected: bool,
    resume_after_connection: std::collections::HashSet<u32>,
}
impl Actor {
    fn load(
        steamid: String,
        path: PathBuf,
        executor: Arc<dyn DownloadExecutor>,
        events: mpsc::UnboundedSender<Event>,
    ) -> Result<Self> {
        ensure!(
            steamid.len() == 17 && steamid.bytes().all(|v| v.is_ascii_digit()),
            "Invalid queue account"
        );
        ensure!(!path.is_symlink(), "Queue file cannot be a symlink");
        let saved = if path.exists() {
            Some(serde_json::from_slice::<SavedQueue>(&std::fs::read(
                &path,
            )?)?)
        } else {
            None
        };
        let mut actor = Self {
            steamid,
            path,
            executor,
            events,
            items: vec![],
            max_concurrent: 2,
            revision: 0,
            next_run: 0,
            active: HashMap::new(),
            pending_removals: HashMap::new(),
            stopping: false,
            connected: true,
            resume_after_connection: Default::default(),
        };
        if let Some(saved) = saved {
            ensure!(
                saved.steamid == actor.steamid,
                "Queue belongs to a different account"
            );
            actor.max_concurrent = saved.max_concurrent.clamp(1, 4);
            actor.revision = saved.revision;
            let mut ids = std::collections::HashSet::new();
            for intent in saved.items {
                ensure!(
                    intent.appid > 0 && ids.insert(intent.appid),
                    "Invalid duplicate queue item"
                );
                // No worker survives this process restart. A durable removal
                // intent can now be finalized without touching staging/game files.
                if intent.remove_requested {
                    continue;
                }
                let state = if intent.state == DownloadState::Cancelling {
                    // Cancellation was already accepted before the process
                    // stopped; restarting must not turn that intent into Pause.
                    DownloadState::Cancelled
                } else if intent.state.unfinished() {
                    DownloadState::Paused
                } else {
                    intent.state
                };
                let mut row = DownloadItem::new(intent.appid, intent.is_update, state);
                row.platform = intent.platform;
                // Old queues did not record a platform. Resolve once using game
                // evidence/defaults, never compatibility preferences, and keep
                // interrupted jobs paused until the user resumes them.
                if row.platform.is_none() {
                    match actor.executor.validate(&row.job(&actor.steamid)) {
                        Ok(platform) => row.platform = Some(platform),
                        Err(error) => row.error = Some(error.to_string()),
                    }
                }
                row.downloaded_bytes = intent.downloaded_bytes;
                row.total_bytes = intent.total_bytes;
                if row.total_bytes > 0 {
                    row.progress = Some(
                        (row.downloaded_bytes as f64 / row.total_bytes as f64).clamp(0.0, 1.0),
                    );
                }
                if state == DownloadState::Paused {
                    row.message = Some("Загрузка приостановлена после завершения сеанса".into());
                }
                actor.items.push(row);
            }
        }
        actor.reindex();
        actor.persist()?;
        Ok(actor)
    }
    fn reindex(&mut self) {
        for (position, row) in self.items.iter_mut().enumerate() {
            row.queue_position = position;
        }
    }
    fn snapshot(&mut self, id: Option<&str>) {
        self.revision = self.revision.saturating_add(1);
        self.reindex();
        let _=self.events.send(Event::new(id,"download_queue",json!({"steamid":self.steamid,"items":self.items,"max_concurrent":self.max_concurrent,"revision":self.revision})));
    }
    fn persist(&self) -> Result<()> {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let parent = self.path.parent().context("Queue directory missing")?;
        ensure!(!parent.is_symlink(), "Queue directory cannot be a symlink");
        std::fs::create_dir_all(parent)?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        ensure!(!self.path.is_symlink(), "Queue file cannot be a symlink");
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let temp = self
            .path
            .with_extension(format!("{}-{nonce}.tmp", std::process::id()));
        let value = SavedQueue {
            steamid: self.steamid.clone(),
            max_concurrent: self.max_concurrent,
            revision: self.revision,
            items: self
                .items
                .iter()
                .map(|row| Intent {
                    appid: row.appid,
                    is_update: row.is_update,
                    platform: row.platform,
                    state: row.state,
                    downloaded_bytes: row.downloaded_bytes,
                    total_bytes: row.total_bytes,
                    remove_requested: row.remove_requested,
                })
                .collect(),
        };
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        let result = (|| {
            file.write_all(&serde_json::to_vec(&value)?)?;
            file.sync_all()?;
            std::fs::rename(&temp, &self.path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temp);
        }
        result
    }
    fn save_or_report(&self) {
        if let Err(error) = self.persist() {
            let _ = self.events.send(Event::error(
                None,
                "download_queue_save_failed",
                &format!("Не удалось сохранить очередь: {error}"),
                true,
            ));
        }
    }
    fn remove_if_idle(&mut self, appid: u32) -> Result<()> {
        ensure!(appid > 0, "Invalid AppID");
        ensure!(
            !self.active.contains_key(&appid),
            "Дождитесь завершения или остановки загрузки этой игры"
        );
        let Some(index) = self.items.iter().position(|row| row.appid == appid) else {
            return Ok(());
        };
        let row = self.items.remove(index);
        if let Err(error) = self.persist() {
            self.items.insert(index, row);
            return Err(error);
        }
        self.pending_removals.remove(&appid);
        self.snapshot(None);
        Ok(())
    }
    fn apply(&mut self, request: &Request) -> Result<()> {
        // A rejected save must not leave a new/resumed job executable or change
        // ordering/limits behind the UI's error response. Active interruption
        // commands cannot be rolled back; apply_inner reports their save error
        // separately while acknowledging the real accepted state.
        let before_items = self.items.clone();
        let before_limit = self.max_concurrent;
        let result = self.apply_inner(request);
        if result.is_err()
            && !matches!(
                request.command.as_str(),
                "pause_download" | "cancel_download"
            )
        {
            self.items = before_items;
            self.max_concurrent = before_limit;
        }
        result
    }
    fn apply_inner(&mut self, request: &Request) -> Result<()> {
        if request.command == "get_download_queue" {
            self.snapshot(Some(&request.id));
            return Ok(());
        }
        if request.command == "set_download_limit" {
            let limit = request.payload["limit"]
                .as_u64()
                .filter(|v| (1..=4).contains(v))
                .context("Число одновременных загрузок должно быть от 1 до 4")?;
            self.max_concurrent = limit as usize;
            self.persist()?;
            self.snapshot(Some(&request.id));
            return Ok(());
        }
        let appid = request.payload["appid"]
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .filter(|v| *v > 0)
            .context("Invalid AppID")?;
        let position = self.items.iter().position(|row| row.appid == appid);
        if let Some(index) = position
            && self.items[index].remove_requested
        {
            ensure!(
                request.command == "remove_download" && !self.pending_removals.contains_key(&appid),
                "Дождитесь удаления записи из очереди"
            );
        }
        match request.command.as_str() {
            "remove_download" => {
                let index = position.context("Игра отсутствует в очереди")?;
                if let Some(control) = self.active.get(&appid).map(|active| active.control.clone())
                {
                    // Persist the intent before accepting an irreversible
                    // cancellation. A failed save leaves this job untouched.
                    self.items[index].remove_requested = true;
                    self.persist()?;
                    self.pending_removals.insert(appid, request.id.clone());
                    if control.cancel() {
                        self.items[index].state = DownloadState::Cancelling;
                        self.items[index].message =
                            Some("Остановка загрузки перед удалением записи".into());
                    } else {
                        // The atomic commit already owns its gate. Drain it;
                        // removing the queue entry never deletes installed files.
                        self.items[index].message =
                            Some("Удаление записи после завершения установки".into());
                    }
                    self.items[index].bytes_per_second = 0.0;
                    self.items[index].eta_seconds = None;
                    self.save_or_report();
                    // The correlated acknowledgement is deferred until the
                    // worker has exited and the removed row is saved to disk.
                    self.snapshot(None);
                    return Ok(());
                }
                self.items.remove(index);
                self.persist()?;
            }
            "install_game" | "update_game" => {
                if let Some(index) = position {
                    ensure!(
                        !self.items[index].state.unfinished(),
                        "Эта игра уже находится в очереди загрузок"
                    );
                }
                let mut row = DownloadItem::new(
                    appid,
                    request.command == "update_game",
                    DownloadState::Queued,
                );
                row.request_id = Some(request.id.clone());
                row.platform = request
                    .payload
                    .get("platform")
                    .map(|value| {
                        serde_json::from_value(value.clone())
                            .context("Версия игры должна быть macos или windows")
                    })
                    .transpose()?;
                row.platform = Some(self.executor.validate(&row.job(&self.steamid))?);
                if let Some(index) = position {
                    self.items.remove(index);
                }
                self.items.push(row);
                self.persist()?;
                let _ = self.events.send(Event::new(
                    Some(&request.id),
                    "download_enqueued",
                    json!({"appid":appid,"queue_id":format!("{}-{appid}",self.steamid),"platform":self.items.last().and_then(|row| row.platform)}),
                ));
            }
            "pause_download" | "cancel_download" => {
                let index = position.context("Игра отсутствует в очереди")?;
                let cancel = request.command == "cancel_download";
                if let Some(active) = self.active.get(&appid) {
                    if matches!(
                        self.items[index].state,
                        DownloadState::Pausing | DownloadState::Cancelling
                    ) {
                        bail!("Дождитесь завершения текущего изменения загрузки");
                    }
                    let accepted = if cancel {
                        active.control.cancel()
                    } else {
                        active.control.pause()
                    };
                    ensure!(
                        accepted,
                        "Игра уже завершает установку; дождитесь окончания записи"
                    );
                    self.items[index].state = if cancel {
                        DownloadState::Cancelling
                    } else {
                        DownloadState::Pausing
                    };
                } else {
                    let state = self.items[index].state;
                    ensure!(state != DownloadState::Completed, "Загрузка уже завершена");
                    self.items[index].state = if cancel {
                        DownloadState::Cancelled
                    } else {
                        DownloadState::Paused
                    };
                }
                self.items[index].bytes_per_second = 0.0;
                self.items[index].eta_seconds = None;
                self.items[index].error = None;
                self.save_or_report();
            }
            "resume_download" => {
                let index = position.context("Игра отсутствует в очереди")?;
                ensure!(
                    matches!(
                        self.items[index].state,
                        DownloadState::Paused | DownloadState::Cancelled | DownloadState::Failed
                    ),
                    "Эта загрузка уже выполняется или ожидает запуска"
                );
                self.items[index].platform = Some(
                    self.executor
                        .validate(&self.items[index].job(&self.steamid))?,
                );
                self.items[index].state = DownloadState::Queued;
                self.items[index].error = None;
                self.items[index].message = Some("Ожидание запуска".into());
                self.items[index].request_id = Some(request.id.clone());
                self.persist()?;
            }
            "move_download" => {
                let index = position.context("Игра отсутствует в очереди")?;
                ensure!(
                    self.items[index].state == DownloadState::Queued,
                    "Менять порядок можно только для ожидающих загрузок"
                );
                let target = request.payload["position"]
                    .as_u64()
                    .and_then(|v| usize::try_from(v).ok())
                    .filter(|v| *v < self.items.len())
                    .context("Invalid queue position")?;
                let row = self.items.remove(index);
                self.items.insert(target, row);
                self.persist()?;
            }
            _ => bail!("Unknown download command"),
        }
        self.snapshot(Some(&request.id));
        Ok(())
    }
    fn start_ready(&mut self, sink: &mpsc::UnboundedSender<JobEvent>) {
        if self.stopping || !self.connected {
            return;
        }
        for index in 0..self.items.len() {
            if self.active.len() >= self.max_concurrent {
                break;
            }
            if self.items[index].state != DownloadState::Queued {
                continue;
            }
            let job = self.items[index].job(&self.steamid);
            if !self.executor.can_start(&job) {
                continue;
            }
            let resource = match self.executor.resource_key(&job) {
                Ok(value) => value,
                Err(error) => {
                    self.items[index].state = DownloadState::Failed;
                    self.items[index].error = Some(error.to_string());
                    self.snapshot(None);
                    self.save_or_report();
                    continue;
                }
            };
            if self
                .active
                .values()
                .any(|active| active.resource == resource)
            {
                continue;
            }
            self.next_run += 1;
            let run_id = self.next_run;
            let control = InstallControl::new();
            let appid = job.appid;
            let (tx, rx) = mpsc::unbounded_channel();
            let future = self.executor.run(job, control.clone(), tx);
            let task = tokio::spawn(relay_job(future, rx, sink.clone(), run_id, appid));
            self.active.insert(
                appid,
                Active {
                    run_id,
                    control,
                    resource,
                    task,
                },
            );
            self.items[index].state = DownloadState::Preparing;
            self.items[index].attempt_base = self.items[index].downloaded_bytes;
            self.items[index].attempt_wire_seen = 0;
            self.items[index].error = None;
            self.items[index].message = Some("Подготовка загрузки".into());
            self.save_or_report();
            self.snapshot(None);
        }
    }
    fn suspend_connection(&mut self) {
        self.connected = false;
        for row in &mut self.items {
            if matches!(
                row.state,
                DownloadState::Preparing
                    | DownloadState::Verifying
                    | DownloadState::Downloading
                    | DownloadState::Installing
            ) && let Some(active) = self.active.get(&row.appid)
                && active.control.pause()
            {
                self.resume_after_connection.insert(row.appid);
                row.state = DownloadState::Pausing;
                row.bytes_per_second = 0.0;
                row.eta_seconds = None;
                row.message = Some("Ожидаем подключения к Steam".into());
            }
        }
        self.save_or_report();
        self.snapshot(None);
    }
    fn resume_connection_items(&mut self) {
        if !self.connected {
            return;
        }
        for row in &mut self.items {
            if self.resume_after_connection.contains(&row.appid)
                && !self.active.contains_key(&row.appid)
            {
                self.resume_after_connection.remove(&row.appid);
                if matches!(row.state, DownloadState::Paused | DownloadState::Failed) {
                    row.state = DownloadState::Queued;
                    row.error = None;
                    row.message = Some("Подключение восстановлено".into());
                }
            }
        }
    }
    async fn cleanup_completed(&self, appid: u32, request_id: Option<&str>) -> Result<()> {
        ensure!(
            !self.active.contains_key(&appid),
            "Загрузка ещё выполняется"
        );
        let row = self
            .items
            .iter()
            .find(|row| row.appid == appid)
            .context("Игра отсутствует в очереди")?;
        ensure!(
            row.state == DownloadState::Completed,
            "Очистка доступна только после успешной установки"
        );
        // A failed save must never turn a transient in-memory success into
        // permission to delete resumable on-disk state.
        self.persist()?;
        let mut job = row.job(&self.steamid);
        job.request_id = request_id.map(str::to_owned);
        self.executor
            .cleanup_completed(job, self.events.clone())
            .await
    }
    async fn cleanup_completed_best_effort(&self, appid: u32) {
        if let Err(error) = self.cleanup_completed(appid, None).await {
            let _ = self.events.send(Event::new(
                None,
                "download_cache_cleanup_warning",
                json!({"appid":appid,"message":error.to_string()}),
            ));
        }
    }
    async fn run(mut self, mut commands: mpsc::UnboundedReceiver<Command>) {
        let (sink, mut incoming) = mpsc::unbounded_channel();
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        self.snapshot(None);
        // Catch up only explicitly completed jobs after an upgrade/restart.
        // No worker starts and no new queue command is accepted until this
        // completed-state reservation is released.
        let completed: Vec<u32> = self
            .items
            .iter()
            .filter(|row| row.state == DownloadState::Completed)
            .map(|row| row.appid)
            .collect();
        for appid in completed {
            self.cleanup_completed_best_effort(appid).await;
        }
        loop {
            self.start_ready(&sink);
            if self.stopping && self.active.is_empty() {
                self.save_or_report();
                break;
            }
            tokio::select! {
                command=commands.recv(),if !self.stopping=>match command {
                    Some(Command::Request(request))=>{
                        if request.command == "cleanup_download_cache" {
                            let result = match request.payload["appid"].as_u64().and_then(|id| u32::try_from(id).ok()).filter(|id| *id > 0) {
                                Some(appid) => self.cleanup_completed(appid, Some(&request.id)).await,
                                None => Err(anyhow::anyhow!("Invalid AppID")),
                            };
                            if let Err(error) = result { let _=self.events.send(Event::error(Some(&request.id), "download_cache_cleanup_failed", &error.to_string(), true)); }
                            continue;
                        }
                        if matches!(request.command.as_str(), "pause_download" | "cancel_download" | "remove_download")
                            && let Some(appid)=request.payload["appid"].as_u64().and_then(|value|u32::try_from(value).ok()) {
                            self.resume_after_connection.remove(&appid);
                        }
                        if let Err(error)=self.apply(&request){let _=self.events.send(Event::error(Some(&request.id),"download_queue_failed",&error.to_string(),true));}
                    }
                    Some(Command::Executor(executor))=>{self.executor=executor;}
                    Some(Command::SuspendConnection)=>{self.suspend_connection();}
                    Some(Command::Reconnect(executor))=>{self.executor=executor;self.connected=true;self.resume_connection_items();self.save_or_report();self.snapshot(None);}
                    Some(Command::RemoveIdle{appid,reply})=>{let _=reply.send(self.remove_if_idle(appid));}
                    Some(Command::Shutdown)|None=>{
                        self.stopping=true;
                        for row in &mut self.items {
                            if let Some(active)=self.active.get(&row.appid) {if active.control.pause(){row.state=DownloadState::Pausing;}}
                            else if row.state==DownloadState::Queued {row.state=DownloadState::Paused;}
                        }
                        self.save_or_report();self.snapshot(None);
                    }
                },
                message=incoming.recv()=>if let Some(message)=message {self.handle_job(message).await;},
                _=tick.tick(),if !self.stopping && self.items.iter().any(|row|row.state==DownloadState::Queued)=>{},
            }
        }
    }
    async fn handle_job(&mut self, message: JobEvent) {
        let (run_id, appid) = match &message {
            JobEvent::Event(run, id, _) | JobEvent::Finished(run, id, _) => (*run, *id),
        };
        if self
            .active
            .get(&appid)
            .is_none_or(|active| active.run_id != run_id)
        {
            return;
        }
        let Some(index) = self.items.iter().position(|row| row.appid == appid) else {
            return;
        };
        match message {
            JobEvent::Event(_, _, event) => {
                if event.event == "install_progress" {
                    self.items[index].progress(&event.payload);
                    self.snapshot(None);
                }
                let _ = self.events.send(event);
            }
            JobEvent::Finished(_, _, result) => {
                if let Some(active) = self.active.remove(&appid) {
                    let _ = active.task.await;
                }
                let succeeded = result.is_ok();
                let row = &mut self.items[index];
                row.bytes_per_second = 0.0;
                row.eta_seconds = None;
                match result {
                    Ok(()) => {
                        row.state = DownloadState::Completed;
                        row.progress = Some(1.0);
                        row.message = Some("Установлено".into());
                    }
                    Err(error) => match error.downcast_ref::<InstallInterrupted>() {
                        Some(InstallInterrupted::Paused) => {
                            row.state = DownloadState::Paused;
                            row.message = Some("Приостановлено".into());
                        }
                        Some(InstallInterrupted::Cancelled) => {
                            row.state = DownloadState::Cancelled;
                            row.message = Some(
                                "Отменено; загруженные данные сохранены для повторной попытки"
                                    .into(),
                            );
                        }
                        None => {
                            row.state = DownloadState::Failed;
                            row.error = Some(format!("{error:#}"));
                            row.message = None;
                        }
                    },
                }
                if self.resume_after_connection.contains(&appid)
                    && row.state == DownloadState::Failed
                {
                    row.state = DownloadState::Paused;
                    row.error = None;
                    row.message = Some("Ожидаем подключения к Steam".into());
                }
                let request_id = row.request_id.clone();
                let failed = row.error.clone();
                self.resume_connection_items();
                let removal = self.pending_removals.remove(&appid);
                if let Some(removal_id) = removal.as_deref() {
                    let removed = self.items.remove(index);
                    match self.persist() {
                        Ok(()) => self.snapshot(Some(removal_id)),
                        Err(error) => {
                            // Retain the stopped row and its durable removal
                            // intent. The user can retry removing the record;
                            // restarting will also finish that intent safely.
                            self.items.insert(index, removed);
                            self.snapshot(None);
                            let _ = self.events.send(Event::error(
                                Some(removal_id),
                                "download_queue_save_failed",
                                &format!(
                                    "Загрузка остановлена, но запись пока не удалена: {error}"
                                ),
                                true,
                            ));
                        }
                    }
                } else {
                    self.save_or_report();
                    self.snapshot(None);
                }
                if succeeded {
                    if self
                        .items
                        .iter()
                        .any(|row| row.appid == appid && row.state == DownloadState::Completed)
                    {
                        self.cleanup_completed_best_effort(appid).await;
                    }
                    let _ = self.events.send(Event::new(
                        request_id.as_deref(),
                        "operation_complete",
                        json!({"appid":appid,"queue_id":format!("{}-{appid}",self.steamid)}),
                    ));
                } else if let Some(error) = failed.filter(|_| removal.is_none()) {
                    let _ = self.events.send(Event::error(
                        request_id.as_deref(),
                        "download_failed",
                        &error,
                        true,
                    ));
                }
            }
        }
    }
}
async fn relay_job(
    future: DownloadFuture,
    mut events: mpsc::UnboundedReceiver<Event>,
    sink: mpsc::UnboundedSender<JobEvent>,
    run_id: u64,
    appid: u32,
) {
    let mut future = tokio::spawn(future);
    let mut open = true;
    loop {
        tokio::select! {
            result=&mut future=>{while let Ok(event)=events.try_recv(){let _=sink.send(JobEvent::Event(run_id,appid,event));}let result=result.unwrap_or_else(|error|Err(anyhow::anyhow!("Download task failed: {error}")));let _=sink.send(JobEvent::Finished(run_id,appid,result));break;}
            event=events.recv(),if open=>match event{Some(event)=>{let _=sink.send(JobEvent::Event(run_id,appid,event));},None=>open=false},
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use tokio::sync::Semaphore;
    const ACCOUNT: &str = "76561198000000001";
    struct Mock {
        account: String,
        active: Arc<AtomicUsize>,
        maximum: Arc<AtomicUsize>,
        started: mpsc::UnboundedSender<u32>,
        gates: Arc<Mutex<HashMap<u32, Arc<Semaphore>>>>,
        completed: Arc<Mutex<Vec<u32>>>,
        cleanup_calls: Arc<Mutex<Vec<u32>>>,
        cleanup_fails: Arc<AtomicBool>,
        resources: HashMap<u32, String>,
    }
    impl DownloadExecutor for Mock {
        fn validate(&self, job: &DownloadJob) -> Result<InstallPlatform> {
            ensure!(job.steamid == self.account, "wrong account");
            Ok(job.platform.unwrap_or(InstallPlatform::Macos))
        }
        fn can_start(&self, _: &DownloadJob) -> bool {
            true
        }
        fn resource_key(&self, job: &DownloadJob) -> Result<String> {
            Ok(self
                .resources
                .get(&job.appid)
                .cloned()
                .unwrap_or_else(|| job.appid.to_string()))
        }
        fn cleanup_completed(
            &self,
            job: DownloadJob,
            _: mpsc::UnboundedSender<Event>,
        ) -> DownloadFuture {
            let calls = self.cleanup_calls.clone();
            let fails = self.cleanup_fails.clone();
            Box::pin(async move {
                calls.lock().unwrap().push(job.appid);
                ensure!(
                    !fails.load(Ordering::SeqCst),
                    "fixture cache cleanup failure"
                );
                Ok(())
            })
        }
        fn run(
            &self,
            job: DownloadJob,
            control: InstallControl,
            events: mpsc::UnboundedSender<Event>,
        ) -> DownloadFuture {
            let (active, maximum, started, gates, completed) = (
                self.active.clone(),
                self.maximum.clone(),
                self.started.clone(),
                self.gates.clone(),
                self.completed.clone(),
            );
            Box::pin(async move {
                struct Count(Arc<AtomicUsize>);
                impl Drop for Count {
                    fn drop(&mut self) {
                        self.0.fetch_sub(1, Ordering::SeqCst);
                    }
                }
                let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                maximum.fetch_max(count, Ordering::SeqCst);
                let _count = Count(active);
                let gate = Arc::new(Semaphore::new(0));
                gates.lock().unwrap().insert(job.appid, gate.clone());
                let _ = started.send(job.appid);
                let _=events.send(Event::new(job.request_id.as_deref(),"install_progress",json!({"appid":job.appid,"phase":"preparing","downloaded_bytes":0,"total_bytes":0,"progress":null})));
                let _=events.send(Event::new(job.request_id.as_deref(),"install_progress",json!({"appid":job.appid,"phase":"downloading","downloaded_bytes":100,"total_bytes":1000,"progress":0.1,"bytes_per_second":20.0})));
                loop {
                    tokio::select! {
                        permit=gate.acquire()=>{permit?.forget();control.checkpoint()?;break;}
                        _=tokio::time::sleep(Duration::from_millis(5))=>{
                            if let Err(reason)=control.checkpoint(){
                                // A previously queued progress callback must not undo pausing/cancelling.
                                let _=events.send(Event::new(job.request_id.as_deref(),"install_progress",json!({"appid":job.appid,"phase":"downloading","downloaded_bytes":100,"total_bytes":1000,"progress":0.1})));
                                return Err(reason.into());
                            }
                        }
                    }
                }
                completed.lock().unwrap().push(job.appid);
                let _ = events.send(Event::new(
                    job.request_id.as_deref(),
                    "install_complete",
                    json!({"appid":job.appid}),
                ));
                Ok(())
            })
        }
    }
    fn mock(
        account: &str,
        resources: HashMap<u32, String>,
    ) -> (Arc<Mock>, mpsc::UnboundedReceiver<u32>) {
        let (started, rx) = mpsc::unbounded_channel();
        (
            Arc::new(Mock {
                account: account.into(),
                active: Arc::new(AtomicUsize::new(0)),
                maximum: Arc::new(AtomicUsize::new(0)),
                started,
                gates: Default::default(),
                completed: Default::default(),
                cleanup_calls: Default::default(),
                cleanup_fails: Default::default(),
                resources,
            }),
            rx,
        )
    }
    fn request(id: &str, command: &str, payload: serde_json::Value) -> Request {
        Request {
            id: id.into(),
            command: command.into(),
            payload,
        }
    }
    fn send(queue: &DownloadQueue, id: &str, command: &str, appid: u32) {
        queue
            .request(request(id, command, json!({"appid":appid})))
            .unwrap();
    }
    async fn started(rx: &mut mpsc::UnboundedReceiver<u32>) -> u32 {
        tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap()
    }
    async fn snapshot(
        rx: &mut mpsc::UnboundedReceiver<Event>,
        predicate: impl Fn(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let event = rx.recv().await.unwrap();
                if event.event == "download_queue" && predicate(&event.payload) {
                    return event.payload;
                }
            }
        })
        .await
        .unwrap()
    }
    fn state(value: &serde_json::Value, appid: u32) -> Option<&str> {
        value["items"]
            .as_array()?
            .iter()
            .find(|v| v["appid"] == appid)?["state"]
            .as_str()
    }
    fn finish(mock: &Mock, appid: u32) {
        mock.gates
            .lock()
            .unwrap()
            .get(&appid)
            .unwrap()
            .add_permits(1);
    }
    #[tokio::test]
    async fn parallel_slots_cancel_blocked_job_reorder_and_reject_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let (executor, mut starts) = mock(ACCOUNT, HashMap::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let queue = DownloadQueue::start(
            ACCOUNT.into(),
            dir.path().join("queue.json"),
            executor.clone(),
            tx,
        )
        .unwrap();
        send(&queue, "one", "install_game", 1);
        assert_eq!(started(&mut starts).await, 1);
        send(&queue, "two", "install_game", 2);
        assert_eq!(started(&mut starts).await, 2);
        send(&queue, "three", "install_game", 3);
        send(&queue, "four", "install_game", 4);
        snapshot(&mut rx, |v| state(v, 4) == Some("queued")).await;
        send(&queue, "duplicate", "install_game", 1);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let event = rx.recv().await.unwrap();
                if event.id.as_deref() == Some("duplicate") {
                    assert_eq!(event.event, "error");
                    break;
                }
            }
        })
        .await
        .unwrap();
        queue
            .request(request(
                "move",
                "move_download",
                json!({"appid":4,"position":0}),
            ))
            .unwrap();
        let reordered = snapshot(&mut rx, |v| v["items"][0]["appid"] == 4).await;
        assert_eq!(reordered["max_concurrent"], 2);
        assert!(starts.try_recv().is_err(), "reorder preempted active jobs");
        send(&queue, "cancel", "cancel_download", 1);
        snapshot(&mut rx, |v| state(v, 1) == Some("cancelled")).await;
        assert_eq!(started(&mut starts).await, 4);
        assert!(!executor.completed.lock().unwrap().contains(&1));
        finish(&executor, 2);
        snapshot(&mut rx, |v| state(v, 2) == Some("completed")).await;
        assert_eq!(started(&mut starts).await, 3);
        assert_eq!(executor.maximum.load(Ordering::SeqCst), 2);
        queue.shutdown().await;
        assert_eq!(executor.active.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn connection_recovery_resumes_active_jobs_but_not_user_pauses_or_cancels() {
        let dir = tempfile::tempdir().unwrap();
        let (old, mut starts) = mock(ACCOUNT, HashMap::new());
        let (tx, mut events) = mpsc::unbounded_channel();
        let queue = DownloadQueue::start(
            ACCOUNT.into(),
            dir.path().join("queue.json"),
            old.clone(),
            tx,
        )
        .unwrap();
        send(&queue, "one", "install_game", 1);
        assert_eq!(started(&mut starts).await, 1);
        send(&queue, "two", "install_game", 2);
        assert_eq!(started(&mut starts).await, 2);
        send(&queue, "pause-two", "pause_download", 2);
        snapshot(&mut events, |value| state(value, 2) == Some("paused")).await;
        send(&queue, "three", "install_game", 3);
        assert_eq!(started(&mut starts).await, 3);
        queue.suspend_connection();
        snapshot(&mut events, |value| {
            state(value, 1) == Some("paused") && state(value, 3) == Some("paused")
        })
        .await;
        assert_eq!(old.active.load(Ordering::SeqCst), 0);
        send(&queue, "cancel-three", "cancel_download", 3);
        snapshot(&mut events, |value| state(value, 3) == Some("cancelled")).await;
        let (fresh, mut fresh_starts) = mock(ACCOUNT, HashMap::new());
        assert!(queue.reconnect("76561198000000999", fresh.clone()).is_err());
        queue.reconnect(ACCOUNT, fresh.clone()).unwrap();
        assert_eq!(started(&mut fresh_starts).await, 1);
        let value = snapshot(&mut events, |value| state(value, 1) == Some("downloading")).await;
        assert_eq!(state(&value, 2), Some("paused"));
        assert_eq!(state(&value, 3), Some("cancelled"));
        assert!(
            tokio::time::timeout(Duration::from_millis(30), fresh_starts.recv())
                .await
                .is_err()
        );
        queue.shutdown().await;
        assert_eq!(fresh.active.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn reconnect_before_pause_finishes_waits_for_old_writer_to_exit() {
        let dir = tempfile::tempdir().unwrap();
        let (old, mut starts) = mock(ACCOUNT, HashMap::new());
        let (tx, mut events) = mpsc::unbounded_channel();
        let queue = DownloadQueue::start(
            ACCOUNT.into(),
            dir.path().join("queue.json"),
            old.clone(),
            tx,
        )
        .unwrap();
        send(&queue, "one", "install_game", 1);
        assert_eq!(started(&mut starts).await, 1);
        let (fresh, mut fresh_starts) = mock(ACCOUNT, HashMap::new());
        queue.suspend_connection();
        queue.reconnect(ACCOUNT, fresh.clone()).unwrap();
        assert_eq!(started(&mut fresh_starts).await, 1);
        assert_eq!(old.active.load(Ordering::SeqCst), 0);
        snapshot(&mut events, |value| state(value, 1) == Some("downloading")).await;
        queue.shutdown().await;
        assert_eq!(fresh.active.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn pause_releases_slot_and_resume_keeps_cumulative_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let (executor, mut starts) = mock(ACCOUNT, HashMap::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let queue = DownloadQueue::start(
            ACCOUNT.into(),
            dir.path().join("queue.json"),
            executor.clone(),
            tx,
        )
        .unwrap();
        queue
            .request(request("limit", "set_download_limit", json!({"limit":1})))
            .unwrap();
        snapshot(&mut rx, |v| v["max_concurrent"] == 1).await;
        send(&queue, "one", "install_game", 1);
        assert_eq!(started(&mut starts).await, 1);
        snapshot(&mut rx, |v| v["items"][0]["downloaded_bytes"] == 100).await;
        send(&queue, "two", "install_game", 2);
        send(&queue, "pause", "pause_download", 1);
        let paused = snapshot(&mut rx, |v| state(v, 1) == Some("paused")).await;
        assert_eq!(paused["items"][0]["downloaded_bytes"], 100);
        assert_eq!(started(&mut starts).await, 2);
        send(&queue, "resume", "resume_download", 1);
        send(&queue, "cancel-two", "cancel_download", 2);
        assert_eq!(started(&mut starts).await, 1);
        let resumed = snapshot(&mut rx, |v| {
            state(v, 1) == Some("downloading") && v["items"][0]["downloaded_bytes"] == 200
        })
        .await;
        assert_eq!(resumed["items"][0]["total_bytes"], 1100);
        assert_eq!(executor.maximum.load(Ordering::SeqCst), 1);
        queue.shutdown().await;
        assert_eq!(executor.active.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn same_destination_is_serialized_while_other_game_runs() {
        let dir = tempfile::tempdir().unwrap();
        let (executor, mut starts) = mock(
            ACCOUNT,
            HashMap::from([(1, "shared".into()), (2, "shared".into())]),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let queue = DownloadQueue::start(
            ACCOUNT.into(),
            dir.path().join("queue.json"),
            executor.clone(),
            tx,
        )
        .unwrap();
        send(&queue, "one", "install_game", 1);
        assert_eq!(started(&mut starts).await, 1);
        send(&queue, "two", "install_game", 2);
        send(&queue, "three", "install_game", 3);
        assert_eq!(started(&mut starts).await, 3);
        snapshot(&mut rx, |v| {
            state(v, 2) == Some("queued") && state(v, 3) == Some("downloading")
        })
        .await;
        finish(&executor, 1);
        assert_eq!(started(&mut starts).await, 2);
        queue.shutdown().await;
        assert_eq!(executor.maximum.load(Ordering::SeqCst), 2);
    }
    #[tokio::test]
    async fn shutdown_drains_then_restart_is_paused_and_account_bound() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("account.json");
        let (executor, mut starts) = mock(ACCOUNT, HashMap::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let queue =
            DownloadQueue::start(ACCOUNT.into(), path.clone(), executor.clone(), tx).unwrap();
        send(&queue, "one", "install_game", 7);
        assert_eq!(started(&mut starts).await, 7);
        snapshot(&mut rx, |v| state(v, 7) == Some("downloading")).await;
        queue.shutdown().await;
        assert_eq!(executor.active.load(Ordering::SeqCst), 0);
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("connection"));
        assert!(!saved.contains("token"));
        assert!(!saved.contains("error"));
        let (other, _) = mock("76561198000000002", HashMap::new());
        let (tx, _) = mpsc::unbounded_channel();
        assert!(DownloadQueue::start("76561198000000002".into(), path.clone(), other, tx).is_err());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let queue = DownloadQueue::start(ACCOUNT.into(), path, executor.clone(), tx).unwrap();
        let recovered = snapshot(&mut rx, |v| state(v, 7) == Some("paused")).await;
        assert_eq!(recovered["steamid"], ACCOUNT);
        assert!(starts.try_recv().is_err());
        send(&queue, "resume", "resume_download", 7);
        assert_eq!(started(&mut starts).await, 7);
        queue.shutdown().await;
        assert_eq!(executor.active.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn interrupted_cancel_intent_restores_cancelled_and_pause_stays_paused() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("queue.json");
        let value = SavedQueue {
            steamid: ACCOUNT.into(),
            max_concurrent: 2,
            revision: 5,
            items: vec![
                Intent {
                    appid: 1,
                    is_update: false,
                    platform: None,
                    state: DownloadState::Cancelling,
                    downloaded_bytes: 12,
                    total_bytes: 50,
                    remove_requested: false,
                },
                Intent {
                    appid: 2,
                    is_update: false,
                    platform: None,
                    state: DownloadState::Pausing,
                    downloaded_bytes: 20,
                    total_bytes: 70,
                    remove_requested: false,
                },
            ],
        };
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let (executor, _) = mock(ACCOUNT, HashMap::new());
        let (tx, _) = mpsc::unbounded_channel();
        let actor = Actor::load(ACCOUNT.into(), path, executor, tx).unwrap();
        assert_eq!(actor.items[0].state, DownloadState::Cancelled);
        assert_eq!(actor.items[1].state, DownloadState::Paused);
        assert_eq!(actor.items[0].downloaded_bytes, 12);
    }
    #[test]
    fn cleanup_gate_checks_all_accounts_and_preserves_every_resumable_state() {
        let root = tempfile::tempdir().unwrap();
        let queues = root.path().join("download-queues");
        std::fs::create_dir(&queues).unwrap();
        let write = |name: &str, appid: u32, state: DownloadState| {
            std::fs::write(
                queues.join(name),
                serde_json::to_vec(&json!({
                    "steamid":ACCOUNT,"max_concurrent":2,"revision":1,
                    "items":[{"appid":appid,"is_update":false,"state":state}]
                }))
                .unwrap(),
            )
            .unwrap();
        };
        assert!(!completed_cache_cleanup_allowed(root.path(), 1).unwrap());
        write("first.json", 1, DownloadState::Completed);
        assert!(completed_cache_cleanup_allowed(root.path(), 1).unwrap());
        for state in [
            DownloadState::Queued,
            DownloadState::Downloading,
            DownloadState::Preparing,
            DownloadState::Verifying,
            DownloadState::Installing,
            DownloadState::Pausing,
            DownloadState::Paused,
            DownloadState::Cancelling,
            DownloadState::Cancelled,
            DownloadState::Failed,
        ] {
            write("second.json", 1, state);
            assert!(!completed_cache_cleanup_allowed(root.path(), 1).unwrap());
        }
        write("second.json", 2, DownloadState::Paused);
        assert!(completed_cache_cleanup_allowed(root.path(), 1).unwrap());
        std::fs::write(queues.join("second.json"), b"malformed").unwrap();
        assert!(completed_cache_cleanup_allowed(root.path(), 1).is_err());
    }
    #[tokio::test]
    async fn cleanup_requires_completed_state_and_failure_keeps_install_successful() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("queue.json");
        let (executor, mut started_rx) = mock(ACCOUNT, HashMap::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut actor =
            Actor::load(ACCOUNT.into(), path.clone(), executor.clone(), tx.clone()).unwrap();
        for state in [
            DownloadState::Paused,
            DownloadState::Failed,
            DownloadState::Cancelled,
            DownloadState::Queued,
            DownloadState::Downloading,
        ] {
            actor.items = vec![DownloadItem::new(1, false, state)];
            assert!(actor.cleanup_completed(1, None).await.is_err());
        }
        assert!(executor.cleanup_calls.lock().unwrap().is_empty());
        actor.items.clear();
        actor.persist().unwrap();
        drop(actor);
        executor.cleanup_fails.store(true, Ordering::SeqCst);
        let queue =
            DownloadQueue::start(ACCOUNT.into(), path.clone(), executor.clone(), tx).unwrap();
        send(&queue, "install", "install_game", 1);
        assert_eq!(started(&mut started_rx).await, 1);
        finish(&executor, 1);
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let event = rx.recv().await.unwrap();
                if event.event == "operation_complete" {
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(*executor.cleanup_calls.lock().unwrap(), [1]);
        let saved: SavedQueue = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(saved.items[0].state, DownloadState::Completed);
        queue.shutdown().await;
    }
    #[tokio::test]
    async fn startup_cleanup_targets_only_previously_completed_rows() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("queue.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({
                "steamid":ACCOUNT,"max_concurrent":2,"revision":1,"items":[
                    {"appid":1,"is_update":false,"state":"completed"},
                    {"appid":2,"is_update":false,"state":"paused"},
                    {"appid":3,"is_update":false,"state":"failed"},
                    {"appid":4,"is_update":false,"state":"cancelled"}
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        let (executor, _) = mock(ACCOUNT, HashMap::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let queue = DownloadQueue::start(ACCOUNT.into(), path, executor.clone(), tx).unwrap();
        // A correlated snapshot is handled only after startup cleanup finishes.
        send(&queue, "ready", "get_download_queue", 1);
        tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(event) = rx.recv().await {
                if event.id.as_deref() == Some("ready") {
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(*executor.cleanup_calls.lock().unwrap(), [1]);
        queue.shutdown().await;
    }
    #[test]
    fn explicit_platform_survives_queue_restart_and_resume() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("queue.json");
        let (executor, _) = mock(ACCOUNT, HashMap::new());
        let (tx, _) = mpsc::unbounded_channel();
        let mut actor =
            Actor::load(ACCOUNT.into(), path.clone(), executor.clone(), tx.clone()).unwrap();
        actor
            .apply(&request(
                "enqueue",
                "install_game",
                json!({"appid":1,"platform":"windows"}),
            ))
            .unwrap();
        assert_eq!(actor.items[0].platform, Some(InstallPlatform::Windows));
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["items"][0]["platform"], "windows");
        drop(actor);
        // The executor's automatic default is macOS. A restart/resume must
        // retain the explicit Windows selection rather than recompute it.
        let mut actor = Actor::load(ACCOUNT.into(), path, executor, tx).unwrap();
        assert_eq!(actor.items[0].state, DownloadState::Paused);
        assert_eq!(actor.items[0].platform, Some(InstallPlatform::Windows));
        actor
            .apply(&request(
                "resume",
                "resume_download",
                json!({"appid":1,"platform":"macos"}),
            ))
            .unwrap();
        let job = actor.items[0].job(ACCOUNT);
        assert_eq!(job.platform, Some(InstallPlatform::Windows));
        assert_eq!(actor.items[0].state, DownloadState::Queued);
    }
    #[test]
    fn legacy_queue_freezes_default_while_paused_and_invalid_platform_cannot_enqueue() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("queue.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({
                "steamid": ACCOUNT, "max_concurrent": 2, "revision": 1,
                "items": [{"appid":1,"is_update":false,"state":"downloading"}]
            }))
            .unwrap(),
        )
        .unwrap();
        let (executor, _) = mock(ACCOUNT, HashMap::new());
        let (tx, _) = mpsc::unbounded_channel();
        let mut actor = Actor::load(ACCOUNT.into(), path.clone(), executor, tx).unwrap();
        assert_eq!(actor.items[0].state, DownloadState::Paused);
        assert_eq!(actor.items[0].platform, Some(InstallPlatform::Macos));
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["items"][0]["platform"], "macos");
        for invalid in [json!("linux"), json!("unknown"), json!(null), json!(42)] {
            assert!(
                actor
                    .apply(&request(
                        "invalid",
                        "install_game",
                        json!({"appid":2,"platform":invalid})
                    ))
                    .is_err()
            );
            assert_eq!(actor.items.len(), 1);
        }
    }
    #[test]
    fn failed_persistence_rolls_back_enqueue_resume_reorder_and_limit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("queue.json");
        let (executor, _) = mock(ACCOUNT, HashMap::new());
        let (tx, _) = mpsc::unbounded_channel();
        let mut actor = Actor::load(ACCOUNT.into(), path.clone(), executor, tx).unwrap();
        // Atomic rename onto a directory must fail on every test filesystem.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(
            actor
                .apply(&request("enqueue", "install_game", json!({"appid":1})))
                .is_err()
        );
        assert!(
            actor.items.is_empty(),
            "failed enqueue left an executable job"
        );
        actor
            .items
            .push(DownloadItem::new(1, false, DownloadState::Paused));
        assert!(
            actor
                .apply(&request("resume", "resume_download", json!({"appid":1})))
                .is_err()
        );
        assert_eq!(actor.items[0].state, DownloadState::Paused);
        actor.items[0].state = DownloadState::Queued;
        actor
            .items
            .push(DownloadItem::new(2, false, DownloadState::Queued));
        assert!(
            actor
                .apply(&request(
                    "move",
                    "move_download",
                    json!({"appid":2,"position":0})
                ))
                .is_err()
        );
        assert_eq!(
            actor.items.iter().map(|row| row.appid).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(
            actor
                .apply(&request("limit", "set_download_limit", json!({"limit":4})))
                .is_err()
        );
        assert_eq!(actor.max_concurrent, 2);
    }
    #[tokio::test]
    async fn accepted_pause_is_reported_even_when_saving_intent_fails() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("queue.json");
        let (executor, _) = mock(ACCOUNT, HashMap::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut actor = Actor::load(ACCOUNT.into(), path.clone(), executor, tx).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        let control = InstallControl::new();
        actor
            .items
            .push(DownloadItem::new(1, false, DownloadState::Downloading));
        actor.active.insert(
            1,
            Active {
                run_id: 1,
                control: control.clone(),
                resource: "one".into(),
                task: tokio::spawn(async {}),
            },
        );
        actor
            .apply(&request("pause", "pause_download", json!({"appid":1})))
            .unwrap();
        assert_eq!(control.checkpoint(), Err(InstallInterrupted::Paused));
        assert_eq!(actor.items[0].state, DownloadState::Pausing);
        let warning = rx.recv().await.unwrap();
        assert_eq!(warning.event, "error");
        assert_eq!(warning.payload["code"], "download_queue_save_failed");
        let ack = rx.recv().await.unwrap();
        assert_eq!(ack.id.as_deref(), Some("pause"));
        assert_eq!(ack.event, "download_queue");
    }
    #[test]
    fn stale_progress_cannot_replace_interruption_and_unknown_totals_keep_bytes() {
        let mut row = DownloadItem::new(1, false, DownloadState::Pausing);
        row.downloaded_bytes = 50;
        row.total_bytes = 100;
        row.attempt_base = 50;
        row.progress(&json!({"phase":"preparing","downloaded_bytes":0,"total_bytes":0,"bytes_per_second":999}));
        assert_eq!(row.state, DownloadState::Pausing);
        assert_eq!(row.downloaded_bytes, 50);
        assert_eq!(row.total_bytes, 100);
        assert_eq!(row.bytes_per_second, 0.0);
    }
    #[tokio::test]
    async fn remove_active_waits_for_writer_drain_then_acks_without_deleting_data() {
        struct DrainExecutor {
            started: mpsc::UnboundedSender<()>,
            stopped: mpsc::UnboundedSender<()>,
            release: Arc<Semaphore>,
            active: Arc<AtomicUsize>,
        }
        impl DownloadExecutor for DrainExecutor {
            fn validate(&self, job: &DownloadJob) -> Result<InstallPlatform> {
                Ok(job.platform.unwrap_or(InstallPlatform::Macos))
            }
            fn can_start(&self, _: &DownloadJob) -> bool {
                true
            }
            fn resource_key(&self, job: &DownloadJob) -> Result<String> {
                Ok(job.appid.to_string())
            }
            fn run(
                &self,
                job: DownloadJob,
                control: InstallControl,
                events: mpsc::UnboundedSender<Event>,
            ) -> DownloadFuture {
                let (started, stopped, release, active) = (
                    self.started.clone(),
                    self.stopped.clone(),
                    self.release.clone(),
                    self.active.clone(),
                );
                Box::pin(async move {
                    struct Count(Arc<AtomicUsize>);
                    impl Drop for Count {
                        fn drop(&mut self) {
                            self.0.fetch_sub(1, Ordering::SeqCst);
                        }
                    }
                    active.fetch_add(1, Ordering::SeqCst);
                    let _count = Count(active);
                    let _ = started.send(());
                    let reason = loop {
                        if let Err(reason) = control.checkpoint() {
                            break reason;
                        }
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    };
                    let _ = stopped.send(());
                    let _=events.send(Event::new(job.request_id.as_deref(),"install_progress",json!({"appid":job.appid,"phase":"downloading","downloaded_bytes":100,"total_bytes":1000})));
                    release.acquire().await?.forget();
                    Err(reason.into())
                })
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("queue.json");
        let staging = directory.path().join("staging.fixture");
        let installed = directory.path().join("installed.fixture");
        std::fs::write(&staging, b"keep partial data").unwrap();
        std::fs::write(&installed, b"keep game").unwrap();
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let (stopped_tx, mut stopped_rx) = mpsc::unbounded_channel();
        let active = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(Semaphore::new(0));
        let executor = Arc::new(DrainExecutor {
            started: started_tx,
            stopped: stopped_tx,
            release: release.clone(),
            active: active.clone(),
        });
        let (tx, mut rx) = mpsc::unbounded_channel();
        let queue = DownloadQueue::start(ACCOUNT.into(), path.clone(), executor, tx).unwrap();
        send(&queue, "install", "install_game", 1);
        tokio::time::timeout(Duration::from_secs(2), started_rx.recv())
            .await
            .unwrap()
            .unwrap();
        send(&queue, "remove", "remove_download", 1);
        tokio::time::timeout(Duration::from_secs(2), stopped_rx.recv())
            .await
            .unwrap()
            .unwrap();
        let cancelling = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let event = rx.recv().await.unwrap();
                assert_ne!(
                    event.id.as_deref(),
                    Some("remove"),
                    "removal was acknowledged before the writer stopped"
                );
                if event.event == "download_queue" && state(&event.payload, 1) == Some("cancelling")
                {
                    break event;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(cancelling.payload["items"][0]["remove_requested"], true);
        assert_eq!(active.load(Ordering::SeqCst), 1);
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["items"][0]["remove_requested"], true);
        release.add_permits(1);
        let ack = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let event = rx.recv().await.unwrap();
                if event.id.as_deref() == Some("remove") {
                    break event;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(ack.event, "download_queue");
        assert_eq!(state(&ack.payload, 1), None);
        assert_eq!(active.load(Ordering::SeqCst), 0);
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(saved["items"].as_array().unwrap().is_empty());
        assert_eq!(std::fs::read(staging).unwrap(), b"keep partial data");
        assert_eq!(std::fs::read(installed).unwrap(), b"keep game");
        queue.shutdown().await;
    }
    #[tokio::test]
    async fn removal_completion_race_and_late_old_events_cannot_resurrect_a_row() {
        let directory = tempfile::tempdir().unwrap();
        let (executor, _) = mock(ACCOUNT, HashMap::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut actor = Actor::load(
            ACCOUNT.into(),
            directory.path().join("queue.json"),
            executor,
            tx,
        )
        .unwrap();
        actor
            .items
            .push(DownloadItem::new(1, false, DownloadState::Installing));
        actor.active.insert(
            1,
            Active {
                run_id: 1,
                control: InstallControl::new(),
                resource: "one".into(),
                task: tokio::spawn(async {}),
            },
        );
        actor
            .apply(&request("remove", "remove_download", json!({"appid":1})))
            .unwrap();
        // The job completed before cancellation reached it. The installed game
        // is untouched, but its queue row still disappears after the join.
        actor.handle_job(JobEvent::Finished(1, 1, Ok(()))).await;
        assert!(actor.items.is_empty());
        assert!(!actor.active.contains_key(&1));
        let mut acknowledged = false;
        while let Ok(event) = rx.try_recv() {
            if event.id.as_deref() == Some("remove") {
                assert_eq!(event.event, "download_queue");
                assert!(event.payload["items"].as_array().unwrap().is_empty());
                acknowledged = true;
            }
        }
        assert!(acknowledged);
        actor
            .items
            .push(DownloadItem::new(1, false, DownloadState::Preparing));
        actor.active.insert(
            1,
            Active {
                run_id: 2,
                control: InstallControl::new(),
                resource: "one".into(),
                task: tokio::spawn(async {}),
            },
        );
        actor
            .handle_job(JobEvent::Event(
                1,
                1,
                Event::new(
                    None,
                    "install_progress",
                    json!({"appid":1,"phase":"downloading","downloaded_bytes":999}),
                ),
            ))
            .await;
        actor.handle_job(JobEvent::Finished(1, 1, Ok(()))).await;
        assert_eq!(actor.items[0].state, DownloadState::Preparing);
        assert_eq!(actor.items[0].downloaded_bytes, 0);
        assert!(actor.active.contains_key(&1));
    }
    #[tokio::test]
    async fn remove_persistence_failures_neither_cancel_early_nor_claim_record_removed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("queue.json");
        let (executor, _) = mock(ACCOUNT, HashMap::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut actor = Actor::load(ACCOUNT.into(), path.clone(), executor, tx).unwrap();
        actor
            .items
            .push(DownloadItem::new(1, false, DownloadState::Downloading));
        let control = InstallControl::new();
        actor.active.insert(
            1,
            Active {
                run_id: 1,
                control: control.clone(),
                resource: "one".into(),
                task: tokio::spawn(async {}),
            },
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(
            actor
                .apply(&request(
                    "remove-rejected",
                    "remove_download",
                    json!({"appid":1})
                ))
                .is_err()
        );
        assert!(control.checkpoint().is_ok());
        assert!(!actor.items[0].remove_requested);
        assert!(actor.pending_removals.is_empty());
        std::fs::remove_dir(&path).unwrap();
        actor.persist().unwrap();
        actor
            .apply(&request("remove", "remove_download", json!({"appid":1})))
            .unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        actor
            .handle_job(JobEvent::Finished(
                1,
                1,
                Err(InstallInterrupted::Cancelled.into()),
            ))
            .await;
        assert_eq!(actor.items[0].state, DownloadState::Cancelled);
        assert!(actor.items[0].remove_requested);
        assert!(!actor.active.contains_key(&1));
        let mut failed = false;
        while let Ok(event) = rx.try_recv() {
            if event.id.as_deref() == Some("remove") {
                assert_eq!(event.event, "error");
                failed = true;
            }
        }
        assert!(failed);
        std::fs::remove_dir(&path).unwrap();
        actor
            .apply(&request("retry", "remove_download", json!({"appid":1})))
            .unwrap();
        assert!(actor.items.is_empty());
    }
    #[tokio::test]
    async fn uninstall_handle_rejects_active_but_removes_idle_and_missing_idempotently() {
        let directory = tempfile::tempdir().unwrap();
        let (executor, mut starts) = mock(ACCOUNT, HashMap::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let queue = DownloadQueue::start(
            ACCOUNT.into(),
            directory.path().join("queue.json"),
            executor.clone(),
            tx,
        )
        .unwrap();
        let handle = queue.handle();
        queue
            .request(request("limit", "set_download_limit", json!({"limit":1})))
            .unwrap();
        snapshot(&mut rx, |v| v["max_concurrent"] == 1).await;
        send(&queue, "one", "install_game", 1);
        assert_eq!(started(&mut starts).await, 1);
        assert!(handle.remove_if_idle(1).await.is_err());
        assert_eq!(executor.active.load(Ordering::SeqCst), 1);
        send(&queue, "two", "install_game", 2);
        snapshot(&mut rx, |v| state(v, 2) == Some("queued")).await;
        handle.remove_if_idle(2).await.unwrap();
        snapshot(&mut rx, |v| state(v, 2).is_none()).await;
        handle.remove_if_idle(2).await.unwrap();
        assert!(starts.try_recv().is_err());
        queue.shutdown().await;
        assert!(
            handle.remove_if_idle(1).await.is_err(),
            "a stopped account queue must not act on a new session"
        );
    }
    #[test]
    fn idle_removal_rolls_back_on_save_failure_and_pending_removal_survives_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("queue.json");
        let (executor, _) = mock(ACCOUNT, HashMap::new());
        let (tx, _) = mpsc::unbounded_channel();
        let mut actor = Actor::load(ACCOUNT.into(), path.clone(), executor.clone(), tx).unwrap();
        actor
            .items
            .push(DownloadItem::new(1, false, DownloadState::Paused));
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(actor.remove_if_idle(1).is_err());
        assert_eq!(actor.items.len(), 1);
        assert!(
            actor
                .apply(&request("remove", "remove_download", json!({"appid":1})))
                .is_err()
        );
        assert_eq!(actor.items.len(), 1);
        std::fs::remove_dir(&path).unwrap();
        actor.items[0].remove_requested = true;
        actor.persist().unwrap();
        let (tx, _) = mpsc::unbounded_channel();
        let loaded = Actor::load(ACCOUNT.into(), path.clone(), executor, tx).unwrap();
        assert!(loaded.items.is_empty());
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert!(saved["items"].as_array().unwrap().is_empty());
    }
}
