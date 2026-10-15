use crate::{
    model::{Event, Game, Request},
    session::{self, Account},
};
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use steam_cm_protocol::connection::Connection;
use steam_cm_protocol::{AuthEvent, AuthMethod, FriendsEvent, LoggedOn, RunCommand, SteamClient};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::{Mutex, mpsc},
    task::{JoinHandle, JoinSet},
};

pub type SharedConnection = Arc<Mutex<Connection>>;

#[derive(Clone)]
pub struct OperationContext {
    pub steamid: String,
    pub games: Vec<Game>,
    pub connection: SharedConnection,
    pub persona_state: u32,
    pub downloads: Option<crate::download_queue::DownloadQueueHandle>,
}

pub type OperationFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;
pub trait OperationAdapter: Send + Sync + 'static {
    fn download_executor(
        &self,
        _context: OperationContext,
    ) -> Option<Arc<dyn crate::download_queue::DownloadExecutor>> {
        None
    }
    fn set_presence(&self, _state: u32) {}
    fn run(
        &self,
        request: Request,
        context: OperationContext,
        events: mpsc::UnboundedSender<Event>,
    ) -> OperationFuture;
}
struct UnavailableAdapter;
impl OperationAdapter for UnavailableAdapter {
    fn run(
        &self,
        _: Request,
        _: OperationContext,
        _: mpsc::UnboundedSender<Event>,
    ) -> OperationFuture {
        Box::pin(async { bail!("This build does not include the installation or runtime adapter") })
    }
}

pub async fn serve() -> Result<()> {
    serve_with_adapter(Arc::new(UnavailableAdapter)).await
}

enum WorkerControl {
    Guard(String),
    RefreshLibrary,
    Social(Request),
    Stop,
}
enum WorkerEvent {
    Ui(Event),
    Operation(Event),
    Authenticated(LoggedOn, SharedConnection),
    Library(Vec<Game>),
    LibraryError(String),
    Ended(crate::reconnect::SessionFailure),
}
struct Worker {
    controls: mpsc::UnboundedSender<WorkerControl>,
    task: JoinHandle<()>,
}
impl Worker {
    async fn stop(self) {
        let _ = self.controls.send(WorkerControl::Stop);
        let mut task = self.task;
        if tokio::time::timeout(Duration::from_secs(2), &mut task)
            .await
            .is_err()
        {
            task.abort();
        }
    }
}

pub async fn serve_with_adapter(adapter: Arc<dyn OperationAdapter>) -> Result<()> {
    let (events, mut event_rx) = mpsc::unbounded_channel::<Event>();
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(event) = event_rx.recv().await {
            let mut line = serde_json::to_vec(&event)?;
            line.push(b'\n');
            stdout.write_all(&line).await?;
            stdout.flush().await?;
        }
        Ok::<_, anyhow::Error>(())
    });
    let mut input = BufReader::new(tokio::io::stdin()).lines();
    let (worker_events, mut worker_rx) = mpsc::unbounded_channel::<(u64, WorkerEvent)>();
    let mut worker: Option<Worker> = None;
    let mut epochs = crate::reconnect::SessionEpochs::default();
    let mut reconnect = crate::reconnect::Reconnect::default();
    let mut connected = false;
    let mut current: Option<Account> = None;
    let mut connection: Option<SharedConnection> = None;
    let mut games: Vec<Game> = vec![];
    let mut persona_state = 7_u32;
    let mut auth_request: Option<String> = None;
    let mut library_request: Option<String> = None;
    let mut operations = JoinSet::new();
    let mut downloads: Option<crate::download_queue::DownloadQueue> = None;
    emit(
        &events,
        Event::new(
            None,
            "ready",
            json!({"protocol_version":1,"backend":"skinos-core","version":env!("CARGO_PKG_VERSION")}),
        ),
    );

    loop {
        let retry_deadline = reconnect.deadline;
        tokio::select! {
            line = input.next_line() => {
                let Some(line) = line? else { break; };
                if line.len() > 1_048_576 {
                    emit(&events, Event::error(None,"invalid_request","Request exceeds one megabyte",false)); continue;
                }
                let request: Request = match serde_json::from_str(&line) {
                    Ok(request) => request,
                    Err(_) => { emit(&events,Event::error(None,"invalid_request","Invalid NDJSON request",false)); continue; }
                };
                if !request.payload.is_object() && !request.payload.is_null() {
                    emit(&events,Event::error(Some(&request.id),"invalid_request","payload must be an object",false)); continue;
                }
                let id = request.id.clone();
                match request.command.as_str() {
                    "get_settings" | "save_settings" => {
                        let result: Result<serde_json::Value> = (|| {
                            if request.command == "save_settings" {
                                let value = serde_json::from_value(request.payload["settings"].clone())?;
                                crate::settings::save(&value)?;
                            }
                            let settings = crate::settings::load()?;
                            let home = std::path::PathBuf::from(std::env::var_os("HOME").context("Home unavailable")?);
                            Ok(json!({"settings":settings,"download_regions":skinos_downloader::download_regions(),"crossovers":skinos_compat::discover_crossovers(&home),"notproton":skinos_compat::bundled_info()}))
                        })();
                        match result {
                            Ok(payload) => {
                                emit(&events,Event::new(Some(&id),"settings",payload));
                                if request.command == "save_settings" && let Some(account)=&current {
                                    crate::operations::annotate_installed(&mut games);
                                    emit(&events,Event::new(Some(&id),"library",json!({"steamid":account.steamid,"games":games,"cached":false})));
                                }
                            }
                            Err(error) => emit(&events,Event::error(Some(&id),"settings_failed",&error.to_string(),true)),
                        }
                    }
                    "get_game_settings" | "save_game_settings" => {
                        let result: Result<serde_json::Value> = (|| {
                            let appid=request.payload["appid"].as_u64().and_then(|v|u32::try_from(v).ok()).filter(|v|*v>0).context("Invalid AppID")?;
                            if request.command == "save_game_settings" {
                                let settings=serde_json::from_value(request.payload["settings"].clone())?;
                                crate::settings::save_game(appid,&settings)?;
                            }
                            Ok(json!({"appid":appid,"settings":crate::settings::game(appid)?}))
                        })();
                        match result {
                            Ok(payload)=>emit(&events,Event::new(Some(&id),"game_settings",payload)),
                            Err(error)=>emit(&events,Event::error(Some(&id),"settings_failed",&error.to_string(),true)),
                        }
                    }
                    "quit" => { emit(&events,Event::new(Some(&id),"status",json!({"state":"stopped"}))); break; }
                    "status" => {
                        match session::list_accounts() {
                            Ok(accounts) => emit(&events,Event::new(Some(&id),"status",json!({"state":if connected{"authenticated"}else if reconnect.deadline.is_some(){"reconnecting"}else if worker.is_some(){"connecting"}else if current.is_some(){"disconnected"}else{"signed_out"},"account":current,"accounts":accounts,"retry_in_seconds":reconnect.remaining_seconds(),"auto_login_disabled":session::auto_login_account().ok().flatten().is_none(),"generation":epochs.account}))),
                            Err(_) => emit(&events,Event::error(Some(&id),"account_registry","Cannot read saved SkinOs accounts",false)),
                        }
                    }
                    "login_qr" | "login_password" | "restore_session" | "auto_login" => {
                        if request.command == "auto_login" && (current.is_some() || worker.is_some() || reconnect.deadline.is_some()) {continue;}
                        // An explicit account/login choice supersedes any pending recovery,
                        // even if that new choice later has a missing saved credential.
                        if request.command != "auto_login" && !connected {
                            reconnect.cancel();epochs.new_transport();
                            if let Some(old)=worker.take(){old.stop().await;}
                        }
                        let automatic = if request.command == "auto_login" {
                            match session::auto_login_account() {
                                Ok(Some(account)) => Some(account),
                                Ok(None) => {emit(&events,Event::new(Some(&id),"status",json!({"state":"signed_out","auto_login_disabled":true})));continue;}
                                Err(error) => {emit(&events,Event::error(Some(&id),"no_session",&error.to_string(),false));continue;}
                            }
                        } else {None};
                        // Validate before cancelling a working connection. Tokens are read
                        // once per explicit login, then retained only in zeroized memory.
                        let method: Result<(AuthMethod, Option<Account>)> = (|| {
                            match request.command.as_str() {
                                "login_qr" => Ok((AuthMethod::Qr,None)),
                                "login_password" => Ok((AuthMethod::Credentials {
                                    account: required_string(&request,"account_name").or_else(|_|required_string(&request,"account"))?, password: required_string(&request,"password")?
                                },None)),
                                _ => {
                                    let account = if let Some(account)=automatic {account} else {
                                        let sid = required_string(&request,"steamid")?;
                                        session::list_accounts()?.into_iter().find(|account|account.steamid==sid).context("Saved account not found")?
                                    };
                                    let token=session::load_refresh_token(&account.steamid)?;
                                    Ok((AuthMethod::RefreshToken(token.expose_secret().to_owned()),Some(account)))
                                }
                            }
                        })();
                        let (method,restore_account) = match method {Ok(value)=>value,Err(error)=>{
                            let code=if matches!(request.command.as_str(),"restore_session"|"auto_login") {"no_session"} else {"auth_input"};
                            emit(&events,Event::error(Some(&id),code,&error.to_string(),false));continue;
                        }};
                        reconnect.cancel();
                        if let (AuthMethod::RefreshToken(token),Some(account))=(&method,&restore_account) {reconnect.remember(account.clone(),token);}
                        if let Some(queue)=downloads.take(){queue.shutdown().await;}
                        epochs.new_account();
                        operations.abort_all();
                        if let Some(old)=worker.take() {old.stop().await;}
                        connected=false;current=None;connection=None;games.clear();persona_state=7;adapter.set_presence(7);
                        auth_request=Some(id.clone());library_request=Some(id.clone());
                        emit(&events,Event::new(Some(&id),"status",json!({"state":"connecting","account":restore_account,"generation":epochs.account})));
                        if let Some(account)=&restore_account && let Ok(Some(mut cached))=session::load_library(&account.steamid) {
                            crate::operations::annotate_installed(&mut cached);
                            emit(&events,Event::new(Some(&id),"library",json!({"steamid":account.steamid,"games":cached,"cached":true})));
                        }
                        let hint=restore_account.map(|account|account.account_name);
                        worker=Some(spawn_session_worker(epochs.transport,id,method,hint,worker_events.clone()));
                    }
                    #[cfg(debug_assertions)]
                    "debug_disconnect" => {
                        if std::env::var("SKINOS_ENABLE_CONNECTION_DIAGNOSTICS").as_deref()!=Ok("1") {
                            emit(&events,Event::error(Some(&id),"diagnostics_disabled","Connection diagnostics are disabled",false));continue;
                        }
                        if connected && let Some(conn)=&connection {
                            let conn=conn.clone();let request_id=id.clone();
                            let (out,rx)=mpsc::unbounded_channel();
                            let future:OperationFuture=Box::pin(async move {
                                tokio::time::timeout(Duration::from_secs(10),async {conn.lock().await.debug_close_transport().await}).await.context("CM close diagnostic timed out")??;
                                let _=out.send(Event::new(Some(&request_id),"diagnostic",json!({"action":"cm_transport_closed"})));Ok(())
                            });
                            operations.spawn(relay_operation(future,rx,worker_events.clone(),epochs.account,id));
                        }else{emit(&events,Event::error(Some(&id),"not_authenticated","No authenticated CM connection to close",false));}
                    }
                    "retry_connection" => {
                        if !connected && worker.is_none() && reconnect.retry_now() {
                            emit(&events,Event::new(Some(&id),"status",json!({"state":"reconnecting","retry_in_seconds":0,"account":current})));
                        } else if connected {emit(&events,Event::new(Some(&id),"status",json!({"state":"authenticated","account":current})));}
                        else if worker.is_none() {emit(&events,Event::new(Some(&id),"status",json!({"state":"auth_required","message":"Войдите в Steam заново"})));}
                    }
                    "submit_guard" => {
                        match (worker.as_ref(), required_string(&request,"code")) {
                            (Some(w),Ok(code)) if current.is_none()=>{let _=w.controls.send(WorkerControl::Guard(code));}
                            _=>emit(&events,Event::error(Some(&id),"no_guard_flow","No Steam Guard flow is active",false)),
                        }
                    }
                    "cancel_auth" => {
                        if !connected {
                            reconnect.cancel();epochs.new_transport();
                            if let Some(old)=worker.take(){old.stop().await;}
                            auth_request=None;library_request=None;
                            emit(&events,Event::new(Some(&id),"status",json!({"state":if current.is_some(){"disconnected"}else{"signed_out"},"account":current})));
                        } else {emit(&events,Event::error(Some(&id),"already_authenticated","Authentication already completed",false));}
                    }
                    "logout" => {
                        if let Some(queue)=downloads.take(){queue.shutdown().await;}
                        reconnect.cancel();epochs.new_account();connected=false;
                        if let Err(error)=session::set_auto_login(false){emit(&events,Event::error(Some(&id),"session_preferences",&error.to_string(),false));}
                        operations.abort_all(); connection=None; persona_state=7; adapter.set_presence(7);
                        if let Some(old)=worker.take(){old.stop().await;}
                        if let Some(account)=current.take()
                            && request.payload.get("forget").and_then(|v|v.as_bool()).unwrap_or(false)
                            && session::remove_account(&account.steamid).is_err() {
                            emit(&events,Event::error(Some(&id),"keychain_delete","Signed out, but could not remove the saved session",true));
                        }
                        games.clear(); auth_request=None; library_request=None;
                        emit(&events,Event::new(Some(&id),"status",json!({"state":"signed_out","accounts":session::list_accounts().unwrap_or_default()})));
                    }
                    "refresh_library" => {
                        if !connected { emit(&events,Event::error(Some(&id),"not_authenticated","Подключение к Steam восстанавливается",true)); }
                        else if library_request.is_some(){emit(&events,Event::error(Some(&id),"library_busy","Library refresh is already in progress",true));}
                        else if let Some(w)=&worker {
                            library_request=Some(id.clone());
                            let _=w.controls.send(WorkerControl::RefreshLibrary);
                            emit(&events,Event::new(Some(&id),"status",json!({"state":"loading_library"})));
                        }
                    }
                    "get_friends" | "get_profile" | "set_presence" | "get_chat_history" | "send_message" | "get_notifications" => {
                        if connected && let Some(w)=&worker {
                            if w.controls.send(WorkerControl::Social(request)).is_err() {
                                emit(&events,Event::error(Some(&id),"steam_connection","The Steam session is no longer connected",true));
                            }
                        } else { emit(&events,Event::error(Some(&id),"not_authenticated","Sign in to use friends and chat",true)); }
                    }
                    "get_game_log" => {
                        let request_id=id.clone();
                        let appid=request.payload["appid"].as_u64().and_then(|value|u32::try_from(value).ok()).filter(|value|*value>0);
                        let (operation_events,operation_rx)=mpsc::unbounded_channel();
                        let future: OperationFuture=Box::pin(async move {
                            let result: Result<serde_json::Value>=async {
                                let appid=appid.context("Invalid AppID")?;
                                tokio::task::spawn_blocking(move || crate::diagnostics::load_game_log(appid)).await.context("Launch diagnostic reader failed")?
                            }.await;
                            let event=match result {Ok(payload)=>Event::new(Some(&request_id),"game_log",payload),Err(error)=>Event::error(Some(&request_id),"game_log_failed",&error.to_string(),true)};
                            let _=operation_events.send(event); Ok(())
                        });
                        operations.spawn(relay_operation(future,operation_rx,worker_events.clone(),epochs.account,id));
                    }
                    "get_game_details" | "get_web_session" => {
                        if connected && let (Some(account),Some(conn))=(&current,&connection) {
                            let steamid=account.steamid.clone(); let conn=conn.clone(); let request_id=id.clone();
                            let (operation_events,operation_rx)=mpsc::unbounded_channel();
                            let future: OperationFuture=Box::pin(async move {
                                let event_name=if request.command=="get_game_details" {"game_details"} else {"web_session"};
                                let result: Result<serde_json::Value>=async {
                                    if request.command=="get_game_details" {
                                        let appid=request.payload["appid"].as_u64().and_then(|v|u32::try_from(v).ok()).filter(|v|*v>0).context("Invalid AppID")?;
                                        crate::store::load_game_details(appid).await
                                    } else {
                                        let target=request.payload["target"].as_str().context("Missing web target")?;
                                        crate::web_session::create_web_session(conn,&steamid,target).await
                                    }
                                }.await;
                                let event=match result {Ok(payload)=>Event::new(Some(&request_id),event_name,payload),Err(error)=>Event::error(Some(&request_id),&format!("{event_name}_failed"),&error.to_string(),true)};
                                let _=operation_events.send(event); Ok(())
                            });
                            operations.spawn(relay_operation(future,operation_rx,worker_events.clone(),epochs.account,id));
                        } else {emit(&events,Event::error(Some(&id),"not_authenticated","Sign in before opening Steam pages",true));}
                    }
                    "install_game" | "update_game" | "get_download_queue" | "pause_download" | "resume_download" | "cancel_download" | "remove_download" | "move_download" | "set_download_limit" | "cleanup_download_cache" => {
                        if let Some(queue)=&downloads {
                            if let Err(error)=queue.request(request){emit(&events,Event::error(Some(&id),"download_queue_failed",&error.to_string(),true));}
                        }else{emit(&events,Event::error(Some(&id),"not_authenticated","Войдите в аккаунт перед управлением загрузками",true));}
                    }
                    "check_updates" | "launch_game" | "diagnose_input" | "stop_game" | "get_uninstall_plan" | "uninstall_game" => {
                        if (connected || request.command=="stop_game") && let (Some(account),Some(conn))=(&current,&connection) {
                            let context=OperationContext{steamid:account.steamid.clone(),games:games.clone(),connection:conn.clone(),persona_state,downloads:downloads.as_ref().map(|queue|queue.handle())};
                            let (operation_events, operation_rx)=mpsc::unbounded_channel();
                            let future=adapter.run(request,context,operation_events);
                            operations.spawn(relay_operation(future,operation_rx,worker_events.clone(),epochs.account,id));
                        } else {emit(&events,Event::error(Some(&id),"not_authenticated","Sign in before starting a game operation",true));}
                    }
                    _=>emit(&events,Event::error(Some(&id),"unknown_command","Unsupported backend command",false)),
                }
            }
            message=worker_rx.recv()=> {
                let Some((epoch,message))=message else{break;};
                if !epochs.accepts(epoch,matches!(&message,WorkerEvent::Operation(_))){continue;}
                match message {
                    WorkerEvent::Ui(event)=>{
                        if event.event=="presence" && let Some(value)=event.payload["state"].as_str().and_then(crate::social::presence_raw) {
                            persona_state=value; adapter.set_presence(value);
                        }
                        emit(&events,event);
                    },
                    WorkerEvent::Operation(mut event)=> {
                        if let Some(account)=&current
                            && apply_operation_state(&mut games,&account.steamid,&mut event) {
                            if event.event=="library" {
                                crate::operations::annotate_installed(&mut games);
                                event.payload["games"]=serde_json::to_value(&games).unwrap_or_default();
                            }
                            let _=session::cache_library(&account.steamid,&games);
                        }
                        if event.event=="library" && let (Some(queue),Some(account),Some(conn))=(&downloads,&current,&connection)
                            && let Some(executor)=adapter.download_executor(OperationContext{steamid:account.steamid.clone(),games:games.clone(),connection:conn.clone(),persona_state,downloads:downloads.as_ref().map(|queue|queue.handle())}) {queue.update_executor(executor);}
                        emit(&events,event);
                    },
                    WorkerEvent::Authenticated(mut login,conn)=> {
                        let account=Account{steamid:login.steamid.to_string(),account_name:login.account_name.clone(),last_used:SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()};
                        if reconnect.account().is_some_and(|expected|expected.steamid!=account.steamid) {
                            use zeroize::Zeroize;login.refresh_token.zeroize();reconnect.cancel();epochs.new_transport();
                            if let Some(old)=worker.take(){old.stop().await;}
                            emit(&events,Event::new(None,"status",json!({"state":"auth_required","message":"Steam вернул другой аккаунт. Войдите заново."})));continue;
                        }
                        reconnect.remember(account.clone(),&login.refresh_token);
                        connected=true;
                        let saved=session::save_account(account.clone(),&login.refresh_token).is_ok();
                        use zeroize::Zeroize; login.refresh_token.zeroize();
                        emit(&events,Event::new(auth_request.as_deref(),"authenticated",json!({"steamid":account.steamid,"account_name":account.account_name,"session_saved":saved,"generation":epochs.account})));
                        if !saved{emit(&events,Event::error(auth_request.as_deref(),"session_not_saved","Signed in, but SkinOS could not save this session",true));}
                        // Cached metadata is usable for selecting an operation only after
                        // this account's identity is verified. Steam still authorizes every
                        // depot/runtime operation; cached rows never grant a license.
                        if let Ok(Some(mut cached))=session::load_library(&account.steamid) {
                            crate::operations::annotate_installed(&mut cached);
                            games=cached;
                            emit(&events,Event::new(auth_request.as_deref(),"library",json!({"steamid":account.steamid,"games":games,"cached":true})));
                        }
                        current=Some(account); connection=Some(conn);
                        if let (Some(account),Some(conn))=(&current,&connection) {
                            let context=OperationContext{steamid:account.steamid.clone(),games:games.clone(),connection:conn.clone(),persona_state,downloads:downloads.as_ref().map(|queue|queue.handle())};
                            if let Some(executor)=adapter.download_executor(context) {
                                if let Some(queue)=&downloads {let _=queue.reconnect(&account.steamid,executor);}
                                else {
                                let (queue_events,mut queue_rx)=mpsc::unbounded_channel();
                                let sink=worker_events.clone();let epoch=epochs.account;
                                let path=session::data_dir().map(|root|root.join("download-queues").join(format!("{}.json",account.steamid)));
                                match path.and_then(|path|crate::download_queue::DownloadQueue::start(account.steamid.clone(),path,executor,queue_events)) {
                                    Ok(queue)=>{downloads=Some(queue);tokio::spawn(async move {while let Some(event)=queue_rx.recv().await{let _=sink.send((epoch,WorkerEvent::Operation(event)));}});}
                                    Err(error)=>emit(&events,Event::error(None,"download_queue_failed",&error.to_string(),true)),
                                }
                                }
                            }
                        }
                        emit(&events,Event::new(auth_request.as_deref(),"status",json!({"state":"loading_library"})));
                    }
                    WorkerEvent::Library(mut loaded)=> {
                        if let Some(account)=&current {
                            crate::operations::annotate_installed(&mut loaded);
                            games=loaded;
                            if session::cache_library(&account.steamid,&games).is_err(){emit(&events,Event::error(library_request.as_deref(),"cache_write","Library loaded but could not be cached",true));}
                            emit(&events,Event::new(library_request.as_deref(),"library",json!({"steamid":account.steamid,"games":games,"cached":false})));
                        }
                        if let (Some(queue),Some(account),Some(conn))=(&downloads,&current,&connection)
                            && let Some(executor)=adapter.download_executor(OperationContext{steamid:account.steamid.clone(),games:games.clone(),connection:conn.clone(),persona_state,downloads:downloads.as_ref().map(|queue|queue.handle())}) {queue.update_executor(executor);}
                        library_request=None;
                    }
                    WorkerEvent::LibraryError(error)=> {
                        emit(&events,Event::error(library_request.as_deref(),"library_failed",&error,true));
                        library_request=None;
                    }
                    WorkerEvent::Ended(failure)=> {
                        // A CM socket is not the owner of the game's private Valve runtime.
                        // Keep account operations and their epoch alive throughout recovery.
                        if let Some(queue)=&downloads {queue.suspend_connection();}
                        epochs.new_transport();connected=false;worker=None;library_request=None;
                        if reconnect.schedule(&failure) {
                            emit(&events,Event::new(auth_request.as_deref(),"status",json!({"state":"reconnecting","retry_in_seconds":10,"account":current.as_ref().or(reconnect.account()),"message":"Соединение со Steam прервано. Повторим через 10 секунд."})));
                        } else {
                            reconnect.cancel();
                            emit(&events,Event::error(auth_request.as_deref(),"auth_required",&failure.message,false));
                            emit(&events,Event::new(auth_request.as_deref(),"status",json!({"state":"auth_required","account":current,"message":"Сеанс Steam завершён. Войдите заново."})));
                        }
                    }
                }
            }
            _=tokio::time::sleep_until(retry_deadline.unwrap_or_else(tokio::time::Instant::now)),if retry_deadline.is_some()=>{
                if let Some((method,hint))=reconnect.begin_attempt() {
                    epochs.new_transport();
                    let id=format!("reconnect-{}",epochs.transport);
                    auth_request=Some(id.clone());library_request=Some(id.clone());
                    emit(&events,Event::new(Some(&id),"status",json!({"state":"connecting","reconnecting":true,"account":current.as_ref().or(reconnect.account())})));
                    worker=Some(spawn_session_worker(epochs.transport,id,method,Some(hint),worker_events.clone()));
                }
            }
            _=operations.join_next(),if !operations.is_empty()=>{}
            _=tokio::signal::ctrl_c()=>{break;}
        }
    }
    if let Some(queue) = downloads.take() {
        queue.shutdown().await;
    }
    operations.abort_all();
    while operations.join_next().await.is_some() {}
    if let Some(worker) = worker {
        worker.stop().await;
    }
    drop(events);
    writer.await??;
    Ok(())
}

/// Operation workers receive a snapshot of the library. Merge their installation
/// and update facts into the actor's current rows without replacing newer owned
/// game metadata or reintroducing rows removed by a later library refresh.
fn apply_operation_state(games: &mut [Game], steamid: &str, event: &mut Event) -> bool {
    if event.event == "library" {
        if event.payload["steamid"].as_str() != Some(steamid) {
            return false;
        }
        let Ok(incoming) = serde_json::from_value::<Vec<Game>>(event.payload["games"].clone())
        else {
            return false;
        };
        for row in incoming {
            if let Some(game) = games.iter_mut().find(|g| g.appid == row.appid) {
                game.installed_dir = row.installed_dir;
                game.installed_build_id = row.installed_build_id;
                game.installed_platform = row.installed_platform;
                if let Some(build) = row.available_build_id
                    && game.available_build_id.as_ref().is_none_or(|old| {
                        old.parse::<u64>().unwrap_or(0) <= build.parse::<u64>().unwrap_or(0)
                    })
                {
                    game.available_build_id = Some(build);
                }
                game.update_available = game
                    .available_build_id
                    .as_ref()
                    .zip(game.installed_build_id.as_ref())
                    .is_some_and(|(a, b)| a != b);
            }
        }
        true
    } else if event.event == "updates" {
        let Some(updates) = event.payload["games"].as_array_mut() else {
            return false;
        };
        for row in updates {
            let Some(appid) = row["appid"].as_u64().and_then(|v| u32::try_from(v).ok()) else {
                continue;
            };
            if let Some(game) = games.iter_mut().find(|g| g.appid == appid) {
                game.available_build_id = row["available_build_id"].as_str().map(str::to_owned);
                // Installed state stays authoritative if a concurrent download
                // finished after this update check took its starting snapshot.
                game.update_available = game
                    .available_build_id
                    .as_ref()
                    .zip(game.installed_build_id.as_ref())
                    .is_some_and(|(a, b)| a != b);
                row["installed_build_id"] =
                    serde_json::to_value(&game.installed_build_id).unwrap_or_default();
                row["update_available"] = json!(game.update_available);
            }
        }
        true
    } else {
        false
    }
}

/// Adapters cannot send directly to stdout: every event is checked by the session
/// actor before forwarding, including events already queued when logout occurs.
async fn relay_operation(
    future: OperationFuture,
    mut events: mpsc::UnboundedReceiver<Event>,
    sink: mpsc::UnboundedSender<(u64, WorkerEvent)>,
    generation: u64,
    request_id: String,
) {
    tokio::pin!(future);
    let mut events_open = true;
    loop {
        tokio::select! {
            result=&mut future => {
                // Completion can win select while final progress remains queued.
                // Preserve producer order before emitting a terminal error.
                while let Ok(event)=events.try_recv() {
                    let _=sink.send((generation,WorkerEvent::Operation(event)));
                }
                if let Err(error)=result {
                    let event=Event::error(Some(&request_id),"operation_failed",&error.to_string(),true);
                    let _=sink.send((generation,WorkerEvent::Operation(event)));
                } else {
                    let event=Event::new(Some(&request_id),"operation_complete",json!({}));
                    let _=sink.send((generation,WorkerEvent::Operation(event)));
                }
                break;
            }
            event=events.recv(), if events_open => {
                if let Some(event)=event {
                    let _=sink.send((generation,WorkerEvent::Operation(event)));
                } else { events_open=false; }
            }
        }
    }
}

fn required_string(request: &Request, name: &str) -> Result<String> {
    request
        .payload
        .get(name)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
        .with_context(|| format!("Missing {name}"))
}
fn emit(events: &mpsc::UnboundedSender<Event>, event: Event) {
    let _ = events.send(event);
}

fn spawn_session_worker(
    epoch: u64,
    id: String,
    method: AuthMethod,
    hint: Option<String>,
    sink: mpsc::UnboundedSender<(u64, WorkerEvent)>,
) -> Worker {
    let (controls, receiver) = mpsc::unbounded_channel();
    let task = tokio::spawn(session_worker(epoch, id, method, hint, receiver, sink));
    Worker { controls, task }
}

async fn session_worker(
    generation: u64,
    id: String,
    method: AuthMethod,
    hint: Option<String>,
    mut controls: mpsc::UnboundedReceiver<WorkerControl>,
    sink: mpsc::UnboundedSender<(u64, WorkerEvent)>,
) {
    let send = |event| {
        let _ = sink.send((generation, event));
    };
    let mut client = SteamClient::new();
    if let Some(hint) = hint {
        client.set_account_name_hint(hint);
    }
    let begin = tokio::time::timeout(Duration::from_secs(45), client.begin_auth(method));
    let mut auth = match begin.await {
        Ok(Ok(auth)) => auth,
        Ok(Err(error)) => {
            send(WorkerEvent::Ended(
                crate::reconnect::SessionFailure::from_protocol(error),
            ));
            return;
        }
        Err(_) => {
            send(WorkerEvent::Ended(
                crate::reconnect::SessionFailure::transport("Steam connection timed out"),
            ));
            return;
        }
    };
    let deadline = tokio::time::sleep(Duration::from_secs(180));
    tokio::pin!(deadline);
    let mut identity = None;
    let authenticated = loop {
        tokio::select! {
            control=controls.recv()=>match control {
                Some(WorkerControl::Guard(code))=>{if let Err(error)=client.submit_guard_code(code){send(WorkerEvent::Ui(Event::error(Some(&id),"guard_failed",&error.to_string(),true)));}}
                Some(WorkerControl::Stop)|None=>break false,
                _=>{}
            },
            event=auth.recv()=>match event {
                Some(AuthEvent::QrChallenge(url))=>send(WorkerEvent::Ui(Event::new(Some(&id),"auth_qr",json!({"url":url,"expires_in_seconds":180})))),
                Some(AuthEvent::GuardRequired(kind))=>send(WorkerEvent::Ui(Event::new(Some(&id),"guard_required",json!({"kind":match kind{steam_cm_protocol::GuardKind::EmailCode=>"email_code",steam_cm_protocol::GuardKind::DeviceCode=>"device_code",steam_cm_protocol::GuardKind::DeviceConfirmation=>"device_confirmation"}})))),
                Some(AuthEvent::Success(login))=>{if let Some(connection)=client.connection(){identity=Some((login.steamid,login.account_name.clone()));send(WorkerEvent::Authenticated(login,connection));break true;}else{break false;}}
                Some(AuthEvent::Failure(error))=>{send(WorkerEvent::Ended(crate::reconnect::SessionFailure::from_protocol(error)));break false;}
                None=>{send(WorkerEvent::Ended(crate::reconnect::SessionFailure::transport("Steam authentication ended before completion")));break false;}
            },
            _=&mut deadline=>{send(WorkerEvent::Ended(crate::reconnect::SessionFailure::transport("Sign-in timed out; request a new QR code")));break false;}
        }
    };
    if !authenticated {
        client.disconnect();
        return;
    }
    let Some((steamid, account_name)) = identity else {
        client.disconnect();
        return;
    };
    let mut social = crate::social::SocialState::new(steamid, account_name);
    send(WorkerEvent::Ui(social.profile_event(None)));
    send(WorkerEvent::Ui(Event::new(
        None,
        "presence",
        json!({"state":"invisible"}),
    )));
    let connection = client.connection().expect("authenticated connection");
    let mut social_tasks = JoinSet::new();
    let mut notification_state = match crate::notifications::NotificationState::load(steamid) {
        Ok(state) => state,
        Err(error) => {
            send(WorkerEvent::Ui(crate::notifications::failure(
                steamid,
                None,
                "notifications_storage_failed",
                &error.to_string(),
                true,
            )));
            crate::notifications::NotificationState::memory(steamid)
        }
    };
    let mut notification_tasks = JoinSet::new();
    let mut notification_waiters: Vec<String> = vec![];
    let mut notification_pending = false;
    let mut notification_library_settled = false;
    let mut notification_last_error: Option<String> = None;
    // First PICS/library load can hold the CM mutex for most of a notification
    // request's budget. Coalesce even early UI refreshes until that load settles;
    // real CM pushes continue filling the inbox immediately in the meantime.
    let mut notification_poll = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(90),
        Duration::from_secs(90),
    );
    notification_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let (commands, command_rx) = mpsc::unbounded_channel();
    let (friends, mut friend_rx) = mpsc::unbounded_channel();
    let _ = commands.send(RunCommand::RequestFriendData(vec![steamid]));
    let _ = commands.send(RunCommand::GetLibrary);
    {
        let run = client.run(command_rx, friends);
        tokio::pin!(run);
        loop {
            tokio::select! {
                result=&mut run=>{send(WorkerEvent::Ended(result.err().map(crate::reconnect::SessionFailure::from_protocol).unwrap_or_else(||crate::reconnect::SessionFailure::transport("Steam connection closed"))));break;}
                control=controls.recv()=>match control {
                    Some(WorkerControl::RefreshLibrary)=>{let _=commands.send(RunCommand::GetLibrary);}
                    Some(WorkerControl::Stop)|None=>break,
                    Some(WorkerControl::Guard(_))=>{}
                    Some(WorkerControl::Social(request))=>{
                        match request.command.as_str() {
                            "get_friends"=>{send(WorkerEvent::Ui(social.friends_event(Some(&request.id))));let _=commands.send(RunCommand::RequestFriendData(social.persona_ids()));}
                            "get_profile"=>{send(WorkerEvent::Ui(social.profile_event(Some(&request.id))));let _=commands.send(RunCommand::RequestFriendData(vec![steamid]));}
                            "get_notifications"=>{
                                if notification_waiters.len()>=8 {send(WorkerEvent::Ui(Event::error(Some(&request.id),"notifications_busy","Steam notifications are already loading",true)));}
                                else {
                                    notification_waiters.push(request.id);
                                    if notification_library_settled && !notification_pending {notification_pending=true;let conn=connection.clone();notification_tasks.spawn(async move {steam_cm_protocol::notifications::fetch(conn,steamid).await});}
                                }
                            }
                            _=>match social.validate(&request) {
                                Ok(())=>{social_tasks.spawn(crate::social::execute(request,connection.clone(),social.own_id()));}
                                Err(error)=>send(WorkerEvent::Ui(Event::error(Some(&request.id),"invalid_request",&error.to_string(),false))),
                            }
                        }
                    }
                },
                event=friend_rx.recv()=>match event {
                    Some(FriendsEvent::OwnedGames(games))=>{
                        send(WorkerEvent::Library(games.into_iter().map(Game::from).collect()));
                        if !notification_library_settled {notification_library_settled=true;notification_pending=true;let conn=connection.clone();notification_tasks.spawn(async move {steam_cm_protocol::notifications::fetch(conn,steamid).await});}
                    }
                    Some(FriendsEvent::LibraryError(error))=>{
                        send(WorkerEvent::LibraryError(error));
                        if !notification_library_settled {notification_library_settled=true;notification_pending=true;let conn=connection.clone();notification_tasks.spawn(async move {steam_cm_protocol::notifications::fetch(conn,steamid).await});}
                    }
                    Some(event)=>{
                        for notice in notification_state.social(&event,&social){send(WorkerEvent::Ui(notice));}
                        for event in social.handle(event) {send(WorkerEvent::Ui(event));}
                    }
                    None=>break,
                },
                _=notification_poll.tick()=>{
                    if notification_library_settled && !notification_pending {notification_pending=true;let conn=connection.clone();notification_tasks.spawn(async move {steam_cm_protocol::notifications::fetch(conn,steamid).await});}
                },
                result=notification_tasks.join_next(),if !notification_tasks.is_empty()=>{
                    notification_pending=false;
                    match result {
                        Some(Ok(Ok(batch)))=>{
                            notification_last_error=None;
                            send(WorkerEvent::Ui(crate::notifications::ready(steamid)));
                            for event in notification_state.ingest(batch,true,None){send(WorkerEvent::Ui(event));}
                            for id in notification_waiters.drain(..){send(WorkerEvent::Ui(notification_state.snapshot(Some(&id),false)));}
                        }
                        other=>{
                            let error=match other {Some(Ok(Err(error)))=>error.to_string(),Some(Err(error))=>format!("Notification worker failed: {error}"),_=>"Notification worker stopped".into()};
                            if notification_last_error.as_ref()!=Some(&error){send(WorkerEvent::Ui(crate::notifications::failure(steamid,None,"notifications_failed",&error,true)));}
                            for id in notification_waiters.drain(..){send(WorkerEvent::Ui(crate::notifications::failure(steamid,Some(&id),"notifications_failed",&error,true)));}
                            notification_last_error=Some(error);
                        }
                    }
                },
                result=social_tasks.join_next(),if !social_tasks.is_empty()=>{
                    if let Some(Ok(event))=result {
                        if event.event=="presence" && let Some(state)=event.payload["state"].as_str() {
                            social.apply_presence(state);
                            send(WorkerEvent::Ui(social.profile_event(None)));
                        }
                        send(WorkerEvent::Ui(event));
                    }
                }
            }
        }
    }
    social_tasks.abort_all();
    notification_tasks.abort_all();
    client.disconnect();
}

#[cfg(test)]
mod operation_event_tests {
    use super::*;
    fn fixture() -> Game {
        serde_json::from_value(json!({"appid":42,"name":"Fresh title","playtime_minutes":10,"last_played":0,"app_type":"game",
            "installdir":"Game","installed_dir":"/games/Game","macos_supported":true,"header_url":"", "launch":[],
            "installed_build_id":"2","available_build_id":"2"})).unwrap()
    }
    #[test]
    fn operation_results_update_actor_without_replacing_fresh_metadata() {
        let mut games = vec![fixture()];
        let mut update = Event::new(
            None,
            "updates",
            json!({"games":[{"appid":42,"installed_build_id":"1","available_build_id":"3","update_available":true}],"complete":true}),
        );
        assert!(apply_operation_state(&mut games, "123", &mut update));
        assert_eq!(games[0].installed_build_id.as_deref(), Some("2"));
        assert_eq!(update.payload["games"][0]["installed_build_id"], "2");
        assert!(games[0].update_available);
        let mut row = fixture();
        row.name = "Stale title".into();
        row.installed_build_id = Some("3".into());
        row.available_build_id = Some("3".into());
        let mut installed = Event::new(None, "library", json!({"steamid":"123","games":[row]}));
        assert!(apply_operation_state(&mut games, "123", &mut installed));
        assert_eq!(games[0].name, "Fresh title");
        assert!(!games[0].update_available);
        assert_eq!(games[0].installed_build_id.as_deref(), Some("3"));
        assert!(!apply_operation_state(
            &mut games,
            "other-account",
            &mut installed
        ));
    }
    #[tokio::test]
    async fn queued_events_keep_generation_and_precede_terminal_error() {
        let (tx, rx) = mpsc::unbounded_channel();
        tx.send(Event::new(
            Some("install"),
            "install_progress",
            json!({"progress":0.5}),
        ))
        .unwrap();
        tx.send(Event::new(
            Some("install"),
            "install_progress",
            json!({"progress":0.6}),
        ))
        .unwrap();
        drop(tx);
        let (sink, mut messages) = mpsc::unbounded_channel();
        relay_operation(
            Box::pin(async { bail!("transfer interrupted") }),
            rx,
            sink,
            7,
            "install".into(),
        )
        .await;
        let mut events = Vec::new();
        while let Some((generation, WorkerEvent::Operation(event))) = messages.recv().await {
            assert_eq!(generation, 7);
            events.push(event);
        }
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].payload["progress"], 0.5);
        assert_eq!(events[1].payload["progress"], 0.6);
        assert_eq!(events[2].payload["code"], "operation_failed");
    }
}
