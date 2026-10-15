use crate::{
    daemon::{OperationAdapter, OperationContext, OperationFuture},
    model::{Event, Game, Request},
    session, settings,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::json;
use std::{
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::Arc,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    sync::{Mutex, mpsc},
};
use zeroize::Zeroize;

#[derive(Default)]
pub struct MvpOperations {
    runtime: Arc<Mutex<()>>,
    apps: Arc<std::sync::Mutex<std::collections::HashSet<u32>>>,
    controls: RuntimeRegistry,
    destinations: DestinationRegistry,
    uninstall_plans: Arc<crate::uninstall::Confirmations>,
}

type DestinationRegistry = Arc<std::sync::Mutex<std::collections::HashSet<PathBuf>>>;
struct DestinationOperation {
    path: PathBuf,
    registry: DestinationRegistry,
}
impl DestinationOperation {
    fn acquire(registry: DestinationRegistry, path: PathBuf) -> Result<Self> {
        let path = path.canonicalize().unwrap_or(path);
        ensure!(
            registry
                .lock()
                .map_err(|_| anyhow::anyhow!("Destination lock unavailable"))?
                .insert(path.clone()),
            "Эта папка игры уже используется"
        );
        Ok(Self { path, registry })
    }
}
impl Drop for DestinationOperation {
    fn drop(&mut self) {
        if let Ok(mut registry) = self.registry.lock() {
            registry.remove(&self.path);
        }
    }
}

struct MvpDownloader {
    context: OperationContext,
    apps: Arc<std::sync::Mutex<std::collections::HashSet<u32>>>,
    destinations: DestinationRegistry,
}
impl MvpDownloader {
    fn destination(&self, appid: u32) -> Result<PathBuf> {
        let game = self
            .context
            .games
            .iter()
            .find(|game| game.appid == appid)
            .context("Игра отсутствует в библиотеке текущего аккаунта")?;
        skinos_downloader::library::installation_directory(
            &settings::load()?.steamapps_dir,
            appid,
            game.installdir.as_deref().unwrap_or(&appid.to_string()),
        )
    }
}
impl crate::download_queue::DownloadExecutor for MvpDownloader {
    fn validate(&self, job: &crate::download_queue::DownloadJob) -> Result<()> {
        ensure!(
            job.steamid == self.context.steamid,
            "Download account changed"
        );
        let mut game = self
            .context
            .games
            .iter()
            .find(|game| game.appid == job.appid)
            .cloned()
            .context("Игра отсутствует в библиотеке текущего аккаунта")?;
        annotate_installed(std::slice::from_mut(&mut game));
        if job.is_update {
            ensure!(game.installed_dir.is_some(), "Сначала установите игру");
        }
        platform(&game, &settings::game(job.appid)?)?;
        self.destination(job.appid)?;
        Ok(())
    }
    fn can_start(&self, job: &crate::download_queue::DownloadJob) -> bool {
        if self
            .apps
            .lock()
            .map_or(true, |apps| apps.contains(&job.appid))
        {
            return false;
        }
        let Ok(path) = self.destination(job.appid) else {
            return true;
        };
        let path = path.canonicalize().unwrap_or(path);
        self.destinations
            .lock()
            .is_ok_and(|paths| !paths.contains(&path))
    }
    fn resource_key(&self, job: &crate::download_queue::DownloadJob) -> Result<String> {
        let path = self.destination(job.appid)?;
        Ok(path
            .canonicalize()
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned())
    }
    fn run(
        &self,
        job: crate::download_queue::DownloadJob,
        control: skinos_downloader::InstallControl,
        events: mpsc::UnboundedSender<Event>,
    ) -> crate::download_queue::DownloadFuture {
        let mut context = self.context.clone();
        let apps = self.apps.clone();
        let destinations = self.destinations.clone();
        Box::pin(async move {
            ensure!(job.steamid == context.steamid, "Download account changed");
            control.checkpoint()?;
            {
                let mut active = apps
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Operation lock unavailable"))?;
                ensure!(
                    active.insert(job.appid),
                    "Эта игра уже запущена или обновляется"
                );
            }
            let _app = AppOperation {
                appid: job.appid,
                apps,
            };
            annotate_installed(&mut context.games);
            let game = context
                .games
                .iter()
                .find(|game| game.appid == job.appid)
                .context("Игра отсутствует в библиотеке текущего аккаунта")?
                .clone();
            if job.is_update {
                ensure!(game.installed_dir.is_some(), "Сначала установите игру");
            }
            let config = settings::load()?;
            let platform = platform(&game, &settings::game(job.appid)?)?;
            let destination = skinos_downloader::library::installation_directory(
                &config.steamapps_dir,
                job.appid,
                game.installdir.as_deref().unwrap_or(&job.appid.to_string()),
            )?;
            let _destination = DestinationOperation::acquire(destinations, destination.clone())?;
            let id = job.request_id.clone();
            let sink = events.clone();
            let progress = Arc::new(move |progress: skinos_downloader::InstallProgress| {
                let _ = sink.send(Event::new(
                    id.as_deref(),
                    "install_progress",
                    serde_json::to_value(progress).unwrap_or_default(),
                ));
            });
            let record = skinos_downloader::install(
                context.connection,
                skinos_downloader::InstallRequest {
                    appid: job.appid,
                    steamid: context.steamid.clone(),
                    destination,
                    platform,
                    steamapps_dir: Some(config.steamapps_dir),
                    download_region_id: config.download_region_id,
                },
                progress,
                control,
            )
            .await?;
            let _=events.send(Event::new(job.request_id.as_deref(),"install_complete",json!({"appid":job.appid,"install_dir":record.install_dir,"build_id":record.build_id})));
            if let Some(game) = context
                .games
                .iter_mut()
                .find(|game| game.appid == job.appid)
            {
                game.available_build_id = Some(record.build_id);
            }
            annotate_installed(&mut context.games);
            let _ = session::cache_library(&context.steamid, &context.games);
            let _ = events.send(Event::new(
                job.request_id.as_deref(),
                "library",
                json!({"steamid":context.steamid,"games":context.games,"cached":false}),
            ));
            Ok(())
        })
    }
}

type RuntimeRegistry =
    Arc<std::sync::Mutex<std::collections::HashMap<u32, mpsc::UnboundedSender<RuntimeControl>>>>;
#[derive(Clone, Copy)]
enum RuntimeControl {
    Stop,
    Presence(u32),
}
struct RuntimeSession {
    events: mpsc::UnboundedSender<Event>,
    controls: RuntimeRegistry,
}
struct RuntimeRegistration {
    appid: u32,
    controls: RuntimeRegistry,
}
impl Drop for RuntimeRegistration {
    fn drop(&mut self) {
        if let Ok(mut registry) = self.controls.lock() {
            registry.remove(&self.appid);
        }
    }
}

fn enqueue_stop(controls: &RuntimeRegistry, appid: u32) -> Result<()> {
    let channel = controls
        .lock()
        .map_err(|_| anyhow::anyhow!("Runtime control unavailable"))?
        .get(&appid)
        .cloned()
        .context("Эта игра уже завершилась")?;
    channel
        .send(RuntimeControl::Stop)
        .context("Движок игры уже завершился")
}

struct AppOperation {
    appid: u32,
    apps: Arc<std::sync::Mutex<std::collections::HashSet<u32>>>,
}
impl Drop for AppOperation {
    fn drop(&mut self) {
        if let Ok(mut apps) = self.apps.lock() {
            apps.remove(&self.appid);
        }
    }
}

pub fn installation_directory(appid: u32) -> Result<PathBuf> {
    let root = settings::load()?.steamapps_dir;
    Ok(skinos_downloader::library::read_appmanifest(&root, appid)?.install_dir)
}

fn installed(game: &Game) -> Option<skinos_downloader::InstalledGame> {
    settings::load()
        .ok()
        .and_then(|s| {
            skinos_downloader::library::read_appmanifest(&s.steamapps_dir, game.appid).ok()
        })
        .or_else(|| {
            session::data_dir().ok().and_then(|r| {
                skinos_downloader::read_install(&r.join("games").join(game.appid.to_string())).ok()
            })
        })
}

fn is_windows(entry: &crate::model::LaunchEntry) -> bool {
    entry
        .oslist
        .as_deref()
        .is_none_or(|v| v.is_empty() || v.split(',').any(|os| os.trim() == "windows"))
}
fn is_macos(entry: &crate::model::LaunchEntry) -> bool {
    entry
        .oslist
        .as_deref()
        .is_some_and(|v| v.split(',').any(|os| os.trim() == "macos"))
        && entry.osarch.as_deref() != Some("32")
}
fn is_default_public(entry: &crate::model::LaunchEntry) -> bool {
    entry
        .launch_type
        .as_deref()
        .is_none_or(|v| v.is_empty() || v == "default")
        && entry.betakey.as_deref().is_none_or(str::is_empty)
}
fn installed_platform(game: &Game, dir: &Path) -> Option<String> {
    // Ignore tools/editors, beta branches and Linux launch entries. A native app
    // bundle is unambiguous even when PICS lists several OSes on one entry.
    let available = game
        .launch
        .iter()
        .filter(|e| is_default_public(e))
        .filter_map(|e| {
            let path = relative_game_path(dir, &e.executable).ok()?;
            if is_macos(e) && path.extension().is_some_and(|v| v == "app") && path.is_dir() {
                Some("macos")
            } else if is_windows(e)
                && path.is_file()
                && path
                    .extension()
                    .is_some_and(|v| v.eq_ignore_ascii_case("exe"))
            {
                Some("windows")
            } else if is_macos(e) && path.is_file() {
                Some("macos")
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    if available.contains(&"macos") {
        Some("macos".into())
    } else {
        available.first().map(|v| (*v).into())
    }
}

fn launch_entry<'a>(
    game: &'a Game,
    dir: &Path,
    windows: bool,
) -> Result<&'a crate::model::LaunchEntry> {
    game.launch
        .iter()
        .filter(|e| is_default_public(e))
        .filter(|e| if windows { is_windows(e) } else { is_macos(e) })
        .find(|e| {
            relative_game_path(dir, &e.executable).is_ok_and(|p| {
                if windows {
                    p.is_file()
                } else {
                    p.is_file() || p.extension().is_some_and(|v| v == "app") && p.is_dir()
                }
            })
        })
        .with_context(|| {
            if windows {
                "Не найдены установленные Windows-файлы. Установите Windows-версию через NotProton"
            } else {
                "Не найдены установленные macOS-файлы игры"
            }
        })
}

fn launch_arguments(
    entry: &crate::model::LaunchEntry,
    options: &skinos_compat::GameSettings,
    windows: bool,
) -> Result<Vec<String>> {
    let mut arguments = skinos_compat::parse_arguments(entry.arguments.as_deref().unwrap_or(""))
        .map_err(anyhow::Error::msg)?;
    // NotProton appends its custom arguments when constructing the runner plan.
    if !windows {
        arguments.extend(
            skinos_compat::parse_arguments(&options.launch_arguments)
                .map_err(anyhow::Error::msg)?,
        );
    }
    Ok(arguments)
}

pub fn annotate_installed(games: &mut [Game]) {
    for game in games {
        game.windows_supported = game
            .launch
            .iter()
            .filter(|e| is_default_public(e))
            .any(is_windows);
        game.macos_supported = game
            .launch
            .iter()
            .filter(|e| is_default_public(e))
            .any(is_macos);
        game.linux_supported = game
            .launch
            .iter()
            .filter(|e| is_default_public(e))
            .any(|e| {
                e.oslist
                    .as_deref()
                    .is_some_and(|v| v.split(',').any(|os| os.trim() == "linux"))
            });
        let record = installed(game);
        game.installed_dir = record
            .as_ref()
            .map(|r| r.install_dir.to_string_lossy().into_owned());
        game.installed_build_id = record.as_ref().map(|r| r.build_id.clone());
        game.installed_platform = record
            .as_ref()
            .and_then(|r| installed_platform(game, &r.install_dir));
        game.update_available = game
            .available_build_id
            .as_ref()
            .zip(game.installed_build_id.as_ref())
            .is_some_and(|(a, b)| a != b);
    }
}

fn platform(
    game: &Game,
    settings: &skinos_compat::GameSettings,
) -> Result<skinos_downloader::InstallPlatform> {
    use skinos_downloader::InstallPlatform;
    match settings.compatibility.as_str() {
        "notproton" => {
            ensure!(game.windows_supported, "У игры нет Windows-варианта");
            Ok(InstallPlatform::Windows)
        }
        "native" => {
            ensure!(game.macos_supported, "У игры нет macOS-варианта");
            Ok(InstallPlatform::Macos)
        }
        "auto"
            if game.installed_platform.as_deref() == Some("windows") || !game.macos_supported =>
        {
            ensure!(game.windows_supported, "Нет совместимого варианта запуска");
            Ok(InstallPlatform::Windows)
        }
        "auto" => Ok(InstallPlatform::Macos),
        _ => bail!("Неизвестный способ запуска"),
    }
}

impl OperationAdapter for MvpOperations {
    fn download_executor(
        &self,
        context: OperationContext,
    ) -> Option<Arc<dyn crate::download_queue::DownloadExecutor>> {
        Some(Arc::new(MvpDownloader {
            context,
            apps: self.apps.clone(),
            destinations: self.destinations.clone(),
        }))
    }
    fn set_presence(&self, state: u32) {
        if let Ok(controls) = self.controls.lock() {
            for channel in controls.values() {
                let _ = channel.send(RuntimeControl::Presence(state));
            }
        }
    }
    fn run(
        &self,
        request: Request,
        mut context: OperationContext,
        events: mpsc::UnboundedSender<Event>,
    ) -> OperationFuture {
        let destinations = self.destinations.clone();
        let runtime_lock = self.runtime.clone();
        let apps = self.apps.clone();
        let controls = self.controls.clone();
        let uninstall_plans = self.uninstall_plans.clone();
        Box::pin(async move {
            annotate_installed(&mut context.games);
            if request.command == "check_updates" {
                let only = request.payload.get("appid").and_then(|v| v.as_u64());
                let mut updates = vec![];
                for game in context.games.iter().filter(|g| {
                    g.installed_dir.is_some() && only.is_none_or(|id| id == u64::from(g.appid))
                }) {
                    let installed = installed(game).context("Установленная игра недоступна")?;
                    let options = settings::game(game.appid)?;
                    let build = skinos_downloader::latest_build(
                        &context.connection,
                        game.appid,
                        platform(game, &options)?,
                    )
                    .await?;
                    updates.push(json!({"appid":game.appid,"installed_build_id":installed.build_id,"available_build_id":build,"update_available":installed.build_id!=build}));
                    let _ = events.send(Event::new(
                        Some(&request.id),
                        "updates",
                        json!({"games":updates,"complete":false}),
                    ));
                }
                let _ = events.send(Event::new(
                    Some(&request.id),
                    "updates",
                    json!({"games":updates,"complete":true}),
                ));
                return Ok(());
            }
            let appid = request
                .payload
                .get("appid")
                .and_then(|v| v.as_u64())
                .and_then(|v| u32::try_from(v).ok())
                .filter(|v| *v > 0)
                .context("Missing valid AppID")?;
            if request.command == "stop_game" {
                enqueue_stop(&controls, appid)?;
                let _ = events.send(Event::new(
                    Some(&request.id),
                    "stop_requested",
                    json!({"appid":appid}),
                ));
                return Ok(());
            }
            {
                let mut active = apps
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Operation lock unavailable"))?;
                ensure!(
                    active.insert(appid),
                    "Эта игра уже запущена или обновляется"
                );
            }
            let _app = AppOperation { appid, apps };
            let game = context
                .games
                .iter()
                .find(|g| g.appid == appid)
                .context("Игра отсутствует в библиотеке текущего аккаунта")?
                .clone();
            if matches!(
                request.command.as_str(),
                "get_uninstall_plan" | "uninstall_game"
            ) {
                let config = settings::load()?;
                let library = config.steamapps_dir.clone();
                let record = installed(&game).context("Игра уже удалена с устройства")?;
                let _destination = DestinationOperation::acquire(destinations, record.install_dir)?;
                if request.command == "get_uninstall_plan" {
                    let legacy = session::data_dir()?.join("games");
                    let root = library.clone();
                    let plan = tokio::task::spawn_blocking(move || {
                        skinos_downloader::library::uninstall_plan(&root, appid, Some(&legacy))
                    })
                    .await??;
                    let payload = uninstall_plans.insert(&context.steamid, &library, plan)?;
                    let _ = events.send(Event::new(Some(&request.id), "uninstall_plan", payload));
                    return Ok(());
                }
                let token = request.payload["confirmation_token"]
                    .as_str()
                    .context("Сначала подтвердите удаление игры в приложении")?;
                let plan = uninstall_plans.take(token, &context.steamid, appid, &library)?;
                if let Some(downloads) = &context.downloads {
                    downloads.remove_if_idle(appid).await?;
                }
                // The blocking transaction owns both guards. Logout/quit may
                // drop this future, but cannot release the paths while deletion
                // is still in progress on the blocking pool.
                let result = tokio::task::spawn_blocking(move || {
                    let (_app, _destination) = (_app, _destination);
                    skinos_downloader::library::uninstall(&plan)
                })
                .await?;
                if let Ok(outcome) = &result {
                    let _ = events.send(Event::new(
                        Some(&request.id),
                        "game_uninstalled",
                        serde_json::to_value(outcome)?,
                    ));
                }
                // A disk error can leave a tombstone after detaching the
                // manifest. Refresh facts even on failure, without claiming
                // successful deletion or leaving stale installed state.
                annotate_installed(&mut context.games);
                let _ = session::cache_library(&context.steamid, &context.games);
                let _ = events.send(Event::new(
                    Some(&request.id),
                    "library",
                    json!({"steamid":context.steamid,"games":context.games,"cached":false}),
                ));
                return result.map(|_| ());
            }
            let options = settings::game(appid)?;
            let platform = platform(&game, &options)?;
            match request.command.as_str() {
                "launch_game" | "diagnose_input" => {
                    let _exclusive = runtime_lock
                        .try_lock_owned()
                        .context("Сначала закройте текущую игру")?;
                    let record = installed(&game).context("Сначала установите игру")?;
                    let _destination =
                        DestinationOperation::acquire(destinations, record.install_dir.clone())?;
                    let result = launch(
                        &request,
                        &context,
                        &game,
                        &record,
                        platform,
                        &options,
                        RuntimeSession { events, controls },
                    )
                    .await;
                    if let Err(error) = &result {
                        let _ =
                            crate::diagnostics::record_launch_error(appid, &format!("{error:#}"));
                    }
                    result
                }
                _ => bail!("Unknown game operation"),
            }
        })
    }
}

fn relative_game_path(root: &Path, value: &str) -> Result<PathBuf> {
    let normalized = value.replace('\\', "/");
    let path = Path::new(&normalized);
    ensure!(
        !path.as_os_str().is_empty()
            && path
                .components()
                .all(|part| matches!(part, Component::Normal(_) | Component::CurDir)),
        "Invalid game launch path"
    );
    let result = root
        .join(path)
        .canonicalize()
        .context("Game launch path is missing")?;
    ensure!(
        result.starts_with(root.canonicalize()?),
        "Game launch path escapes installation"
    );
    Ok(result)
}

fn host_binary() -> Result<PathBuf> {
    let own = std::env::current_exe()?;
    let host = own
        .parent()
        .context("Backend executable parent missing")?
        .join("skinos-runtime-host");
    ensure!(
        host.is_file(),
        "Модуль запуска ещё не включён в эту сборку приложения"
    );
    Ok(host)
}

async fn launch(
    request: &Request,
    context: &OperationContext,
    game: &Game,
    record: &skinos_downloader::InstalledGame,
    platform: skinos_downloader::InstallPlatform,
    options: &skinos_compat::GameSettings,
    runtime: RuntimeSession,
) -> Result<()> {
    let RuntimeSession { events, controls } = runtime;
    crate::diagnostics::clear_launch_error(game.appid)?;
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    controls
        .lock()
        .map_err(|_| anyhow::anyhow!("Runtime control unavailable"))?
        .insert(game.appid, control_tx);
    let _registration = RuntimeRegistration {
        appid: game.appid,
        controls,
    };
    let destination = &record.install_dir;
    let windows = platform == skinos_downloader::InstallPlatform::Windows;
    let input_diagnostic = request.command == "diagnose_input";
    ensure!(
        !input_diagnostic || windows,
        "Для этой проверки выберите установленную Windows-игру через NotProton"
    );
    ensure!(
        record.appid == game.appid,
        "Installed game identity mismatch"
    );
    let entry = launch_entry(game, destination, windows)?;
    let executable = relative_game_path(destination, &entry.executable)?;
    let working_dir = match entry.workingdir.as_deref().filter(|v| !v.is_empty()) {
        Some(value) => relative_game_path(destination, value)?,
        None => destination.canonicalize()?,
    };
    let arguments = launch_arguments(entry, options, windows)?;
    let compatibility = if windows {
        let config = settings::load()?;
        let prepare = skinos_compat::PrepareRequest {
            home: PathBuf::from(std::env::var_os("HOME").context("User home unavailable")?),
            data_dir: session::data_dir()?,
            steamapps_dir: config.steamapps_dir,
            crossover_path: config.crossover_path,
            appid: game.appid,
            install_dir: destination.clone(),
            executable: executable.clone(),
            arguments: arguments.clone(),
            working_dir: working_dir.clone(),
            settings: options.clone(),
        };
        Some(
            tokio::task::spawn_blocking(move || skinos_compat::prepare_launch(&prepare))
                .await?
                .map_err(anyhow::Error::msg)?,
        )
    } else {
        None
    };
    let mut initial_persona = context.persona_state;
    while let Ok(control) = control_rx.try_recv() {
        match control {
            RuntimeControl::Presence(state) => initial_persona = state,
            RuntimeControl::Stop => {
                let _ = events.send(Event::new(
                    Some(&request.id),
                    "launch_status",
                    json!({"appid":game.appid,"state":"exited","steamid":context.steamid}),
                ));
                return Ok(());
            }
        }
    }
    let account = session::list_accounts()?
        .into_iter()
        .find(|a| a.steamid == context.steamid)
        .context("Saved account is missing")?;
    let token = session::load_refresh_token(&context.steamid)?;
    let mut command = tokio::process::Command::new(host_binary()?);
    command
        .arg("run")
        .arg("--state-root")
        .arg(session::data_dir()?.join("runtime"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .context("Cannot start SkinOs runtime host")?;
    let mut stderr = child.stderr.take().context("Missing runtime diagnostics")?;
    let stderr_task = tokio::spawn(async move {
        let mut tail = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = stderr.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            tail.extend_from_slice(&chunk[..n]);
            if tail.len() > 32768 {
                tail.drain(..tail.len() - 32768);
            }
        }
        Ok::<_, std::io::Error>(String::from_utf8_lossy(&tail).into_owned())
    });
    let mut input = child.stdin.take().context("Missing runtime input")?;
    let output = child.stdout.take().context("Missing runtime events")?;
    let mut launch = json!({"protocol_version":1,"command":"launch","appid":game.appid,"steamid":context.steamid,
        "account_name":account.account_name,"refresh_token":token.expose_secret(),"install_dir":destination,
        "executable":executable,"arguments":arguments,"working_dir":working_dir,
        "name":game.name,"build_id":record.build_id,"depots":record.depots,"compatibility":compatibility,"persona_state":initial_persona});
    launch["input_diagnostic"] = json!(input_diagnostic);
    // A requested diagnostic temporarily enables the same input path without
    // changing the user's saved per-game preference.
    launch["steam_input"] = json!(windows && (options.steam_input || input_diagnostic));
    let mut bytes = serde_json::to_vec(&launch)?;
    if let serde_json::Value::String(mut secret) = launch["refresh_token"].take() {
        secret.zeroize();
    }
    bytes.push(b'\n');
    let written = input.write_all(&bytes).await;
    bytes.zeroize();
    if let Err(error) = written {
        drop(input);
        let _ = child.wait().await;
        let detail = stderr_task
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
        if !detail.trim().is_empty() {
            bail!("{}", crate::diagnostics::redact(&detail));
        }
        return Err(error.into());
    }
    let mut lines = BufReader::new(output).lines();
    let mut seen_running = false;
    let mut seen_input_diagnostic = false;
    let mut host_error = false;
    let mut failure_message = None;
    loop {
        let line = tokio::select! {
            line=lines.next_line()=>line?,
            control=control_rx.recv()=>{
                if let Some(control)=control {
                    let value=match control {
                        RuntimeControl::Stop=>json!({"command":"stop","appid":game.appid}),
                        RuntimeControl::Presence(state)=>json!({"command":"set_presence","appid":game.appid,"state":state}),
                    };
                    let mut bytes=serde_json::to_vec(&value)?;bytes.push(b'\n');
                    input.write_all(&bytes).await.context("Game control pipe closed")?;
                    input.flush().await?;
                }
                continue;
            }
        };
        let Some(line) = line else { break };
        ensure!(line.len() <= 1_048_576, "Runtime event exceeds size limit");
        let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let Some(kind) = event.get("event").and_then(|v| v.as_str()) else {
            continue;
        };
        let payload = event.get("payload").cloned().unwrap_or_default();
        match kind {
            "launch_status" => {
                ensure!(
                    payload.get("appid").and_then(|v| v.as_u64()) == Some(game.appid as u64),
                    "Runtime returned a different game identity"
                );
                if payload.get("state").and_then(|v| v.as_str()) == Some("running") {
                    ensure!(
                        payload.get("steamid").and_then(|v| v.as_str())
                            == Some(context.steamid.as_str()),
                        "Runtime did not confirm the expected Steam account"
                    );
                    seen_running = true;
                }
                if payload.get("state").and_then(|v| v.as_str()) == Some("failed") {
                    host_error = true;
                    failure_message = payload
                        .get("message")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned);
                }
                let kind = if input_diagnostic {
                    "steam_input_status"
                } else {
                    kind
                };
                let _ = events.send(Event::new(Some(&request.id), kind, payload));
            }
            "steam_input_diagnostics" if input_diagnostic => {
                ensure!(
                    payload["appid"].as_u64() == Some(u64::from(game.appid))
                        && payload["steamid"].as_str() == Some(context.steamid.as_str()),
                    "Steam Input diagnostic identity mismatch"
                );
                seen_input_diagnostic = payload["summary"]["api_initialized"] == true
                    && payload["summary"]["identity_verified"] == true
                    && payload["summary"]["appid_verified"] == true
                    && payload["summary"]["appid"].as_u64() == Some(u64::from(game.appid))
                    && payload["summary"]["scan_completed"] == true;
                let _ = events.send(Event::new(Some(&request.id), kind, payload));
            }
            "steam_input_status" | "steam_input_rumble_status" if !input_diagnostic => {
                ensure!(
                    payload["appid"].as_u64() == Some(u64::from(game.appid)),
                    "Steam Input status belongs to another game"
                );
                // A controller transport error must not turn a running game
                // into a launch failure. Bind the status to this launch owner.
                let mut payload = payload;
                payload["steamid"] = json!(context.steamid);
                let _ = events.send(Event::new(Some(&request.id), kind, payload));
            }
            "error" => {
                host_error = true;
                failure_message = payload
                    .get("message")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned);
            }
            _ => {}
        }
    }
    drop(input);
    let status = child.wait().await?;
    let stderr = stderr_task
        .await
        .context("Runtime diagnostics reader failed")??;
    if let Some(message) = failure_message {
        bail!("{}", crate::diagnostics::redact(&message));
    }
    if !status.success() && !stderr.trim().is_empty() {
        bail!("{}", crate::diagnostics::redact(&stderr));
    }
    ensure!(
        status.success(),
        "Игровой движок завершился с ошибкой ({status}). Подробности — в журнале запуска"
    );
    ensure!(
        if input_diagnostic {
            seen_input_diagnostic
        } else {
            seen_running
        },
        "Игровой движок завершился до подтверждения запуска. Откройте журнал запуска"
    );
    ensure!(
        !host_error,
        "Игровой движок сообщил об ошибке. Откройте журнал запуска"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stop_only_reaches_the_registered_game_and_ended_channels_fail() {
        let registry: RuntimeRegistry = Arc::default();
        let (sender, mut receiver) = mpsc::unbounded_channel();
        registry.lock().unwrap().insert(42, sender);
        assert!(enqueue_stop(&registry, 43).is_err());
        assert!(receiver.try_recv().is_err());
        enqueue_stop(&registry, 42).unwrap();
        assert!(matches!(receiver.try_recv().unwrap(), RuntimeControl::Stop));
        drop(receiver);
        assert!(enqueue_stop(&registry, 42).is_err());
    }
    fn entry(exe: &str, os: &str) -> crate::model::LaunchEntry {
        crate::model::LaunchEntry {
            executable: exe.into(),
            arguments: Some("--base 'a b'".into()),
            workingdir: None,
            launch_type: Some("default".into()),
            oslist: Some(os.into()),
            osarch: None,
            betakey: None,
        }
    }
    fn game(launch: Vec<crate::model::LaunchEntry>) -> Game {
        serde_json::from_value(json!({"appid":42,"name":"Game","playtime_minutes":0,"last_played":0,"app_type":"game",
            "installdir":"Game","installed_dir":null,"macos_supported":true,"windows_supported":true,"header_url":"","launch":launch})).unwrap()
    }
    #[test]
    fn detects_actual_installed_platform_and_ignores_beta_tools() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tool.exe"), b"MZ").unwrap();
        std::fs::create_dir(dir.path().join("Game.app")).unwrap();
        let mut tool = entry("tool.exe", "windows");
        tool.launch_type = Some("editor".into());
        let mut beta = entry("tool.exe", "windows");
        beta.betakey = Some("private".into());
        let g = game(vec![
            tool,
            beta,
            entry("game.exe", "windows"),
            entry("Game.app", "windows,macos"),
        ]);
        assert_eq!(installed_platform(&g, dir.path()).as_deref(), Some("macos"));
        assert_eq!(
            launch_entry(&g, dir.path(), false).unwrap().executable,
            "Game.app"
        );
        assert!(launch_entry(&g, dir.path(), true).is_err());
        std::fs::write(dir.path().join("game.exe"), b"MZ").unwrap();
        assert_eq!(
            launch_entry(&g, dir.path(), true).unwrap().executable,
            "game.exe"
        );
        let only_windows = game(vec![entry("game.exe", "windows")]);
        assert_eq!(
            installed_platform(&only_windows, dir.path()).as_deref(),
            Some("windows")
        );
    }
    #[test]
    fn native_arguments_append_once_and_windows_custom_is_left_to_notproton() {
        let options = skinos_compat::GameSettings {
            launch_arguments: "--custom 'c d'".into(),
            ..Default::default()
        };
        let entry = entry("game.exe", "windows");
        assert_eq!(
            launch_arguments(&entry, &options, false).unwrap(),
            ["--base", "a b", "--custom", "c d"]
        );
        assert_eq!(
            launch_arguments(&entry, &options, true).unwrap(),
            ["--base", "a b"]
        );
    }
    #[test]
    fn auto_respects_installed_windows_and_native_override_requires_support() {
        use skinos_downloader::InstallPlatform;
        let mut g = game(vec![entry("game.exe", "windows")]);
        g.installed_platform = Some("windows".into());
        assert_eq!(
            platform(&g, &Default::default()).unwrap(),
            InstallPlatform::Windows
        );
        g.macos_supported = false;
        let native = skinos_compat::GameSettings {
            compatibility: "native".into(),
            ..Default::default()
        };
        assert!(platform(&g, &native).is_err());
    }
    #[test]
    fn selected_executable_cannot_escape_game_through_symlink() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("game.exe"), b"outside").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
            assert!(relative_game_path(root.path(), "link/game.exe").is_err());
        }
        assert!(relative_game_path(root.path(), "../outside").is_err());
    }
}
