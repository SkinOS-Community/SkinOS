use serde_json::json;
use std::cell::RefCell;
use std::ffi::{CStr, CString, OsString, c_char, c_int, c_void};
use std::io::BufRead;
use std::io::Write;
use std::os::unix::{ffi::OsStrExt, process::CommandExt};
use std::path::Path;
use std::process::ExitCode;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, Ordering},
};
use std::time::{Duration, Instant};

#[path = "worker/game_input_session.rs"]
mod game_input_session;
#[path = "worker/launch_selection.rs"]
mod launch_selection;
#[path = "worker/native_process.rs"]
mod native_process;
#[path = "worker/rumble_feedback_session.rs"]
mod rumble_feedback_session;
#[path = "worker/rumble_fixture_session.rs"]
mod rumble_fixture_session;
#[path = "worker/rumble_sink.rs"]
mod rumble_sink;
#[path = "worker/wine_input_session.rs"]
mod wine_input_session;

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
    fn dlclose(handle: *mut c_void) -> c_int;
    fn dladdr(address: *const c_void, info: *mut DlInfo) -> c_int;
    fn fcntl(fd: i32, command: i32, ...) -> i32;
    fn close(fd: i32) -> i32;
    fn kill(pid: i32, signal: i32) -> i32;
    fn sysctl(
        name: *mut c_int,
        namelen: u32,
        oldp: *mut c_void,
        oldlenp: *mut usize,
        newp: *mut c_void,
        newlen: usize,
    ) -> c_int;
}
#[link(name = "proc")]
unsafe extern "C" {
    fn proc_pidpath(pid: i32, buffer: *mut c_void, buffersize: u32) -> i32;
    fn proc_name(pid: i32, buffer: *mut c_void, buffersize: u32) -> i32;
    fn proc_listallpids(buffer: *mut c_void, buffersize: i32) -> i32;
    fn proc_pidinfo(pid: i32, flavor: i32, arg: u64, buffer: *mut c_void, buffersize: i32) -> i32;
}

#[repr(C)]
struct DlInfo {
    filename: *const c_char,
    base: *mut c_void,
    symbol: *const c_char,
    symbol_address: *mut c_void,
}

// Current OpenSteamworks NativeCallbackMsg_t: two i32s, pointer, i32.
// Payload is deliberately never read or persisted by this lifecycle probe.
#[repr(C)]
struct Callback {
    user: i32,
    callback: i32,
    parameter: *mut c_void,
    parameter_bytes: i32,
}

fn event(name: &str, ok: bool, extra: serde_json::Value) {
    let _ = writeln!(
        std::io::stdout(),
        "SKINOS_EVENT {}",
        json!({"event":name,"ok":ok,"detail":extra})
    );
    let _ = std::io::stdout().flush();
}

unsafe fn symbol(handle: *mut c_void, name: &CStr) -> Result<*mut c_void, String> {
    // SAFETY: handle is a successful dlopen result; name is NUL-terminated.
    let pointer = unsafe { dlsym(handle, name.as_ptr()) };
    if pointer.is_null() {
        Err(format!("Missing export {}", name.to_string_lossy()))
    } else {
        Ok(pointer)
    }
}

unsafe fn method(object: *mut c_void, index: usize) -> *const c_void {
    // SAFETY: caller supplies an interface returned by the fingerprint-pinned
    // runtime and an index supported by the reviewed source/disassembly.
    unsafe { *(*(object as *const *const *const c_void)).add(index) }
}

pub fn execute(args: &[OsString]) -> ExitCode {
    if args.len() != 4
        || std::env::var_os("SKINOS_RUNTIME_WORKER").as_deref() != Some(std::ffi::OsStr::new("1"))
    {
        eprintln!("Internal sandbox worker; use probe.");
        return ExitCode::FAILURE;
    }
    let result = run(
        Path::new(&args[0]),
        Path::new(&args[1]),
        Path::new(&args[2]),
        &args[3].to_string_lossy(),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            event("error", false, json!({"message":error}));
            ExitCode::FAILURE
        }
    }
}

fn run(runtime: &Path, state: &Path, canary: &Path, phase: &str) -> Result<(), String> {
    let owner = state.join(format!("runtime-owner-{}.next", std::process::id()));
    std::fs::write(&owner, format!("{}\n", std::process::id())).map_err(|e| e.to_string())?;
    std::fs::rename(owner, state.join("runtime-owner.pid")).map_err(|e| e.to_string())?;
    let profile_lock_fd = if phase == "live" {
        let lock_fd = std::env::var("SKINOS_PROFILE_LOCK_FD")
            .ok()
            .and_then(|value| value.parse::<i32>().ok())
            .filter(|fd| *fd > 2)
            .ok_or("Account profile lock was not handed off")?;
        if unsafe { fcntl(lock_fd, 1, 0) } < 0 {
            return Err("Account profile lock did not survive exec".into());
        }
        // The worker owns this lock. Wine servers and game subprocesses must
        // not keep it alive after the runtime session has ended.
        if std::env::var_os("SKINOS_TRACE_READY").is_some() && unsafe { fcntl(lock_fd, 2, 1) } < 0 {
            return Err("Could not isolate the account lock from game children".into());
        }
        Some(lock_fd)
    } else {
        None
    };
    if let Some(root) = std::env::var_os("SKINOS_GAME_DATA_ROOT") {
        use std::os::unix::fs::OpenOptionsExt;
        let root = Path::new(&root);
        let temporary = root.join(format!(".skinos-save-check-{}.tmp", std::process::id()));
        let renamed = root.join(format!(".skinos-save-check-{}.done", std::process::id()));
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|e| format!("Native save folder is not writable: {e}"))?;
        file.write_all(b"SkinOs atomic save permission check")
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        std::fs::rename(&temporary, &renamed)
            .map_err(|e| format!("Native atomic save rename denied: {e}"))?;
        std::fs::remove_file(renamed).map_err(|e| e.to_string())?;
        event(
            "native_save_permission",
            true,
            json!({"write":true,"rename":true,"existing_files_changed":false}),
        );
    }
    let read_denied = std::fs::read(canary).is_err();
    let alias = Path::new("/System/Volumes/Data")
        .join(canary.strip_prefix("/").map_err(|e| e.to_string())?);
    let alias_read_denied = std::fs::read(alias).is_err();
    let write_denied = std::fs::write(canary, b"unexpected-write").is_err();
    let tcp_denied = std::net::TcpStream::connect_timeout(
        &"127.0.0.1:9".parse().unwrap(),
        Duration::from_millis(100),
    )
    .is_err_and(|e| matches!(e.raw_os_error(), Some(1 | 13)));
    let state_write = std::fs::write(state.join("worker-alive.txt"), b"isolated worker").is_ok();
    let guard = read_denied
        && alias_read_denied
        && write_denied
        && (tcp_denied || phase == "live")
        && state_write;
    event(
        "sandbox_guard",
        guard,
        json!({"protected_read_denied":read_denied,"alias_read_denied":alias_read_denied,"protected_write_denied":write_denied,"network_denied":tcp_denied,"state_write_allowed":state_write}),
    );
    if !guard {
        return Err("Sandbox guard failed; refusing to load library.".into());
    }
    if std::env::var_os("SKINOS_TRACE_READY").is_none() {
        // Re-exec only ourselves after sandbox application. macOS strips DYLD
        // overrides when launching protected system tools such as sandbox-exec.
        // The telemetry dylib never enters any other application.
        let error = std::process::Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
            .arg("__worker")
            .arg(runtime)
            .arg(state)
            .arg(canary)
            .arg(phase)
            .env("DYLD_INSERT_LIBRARIES", state.join("libskinos_trace.dylib"))
            .env("SKINOS_TRACE_READY", "1")
            .exec();
        return Err(format!("Could not activate worker telemetry: {error}"));
    }
    // The connected writer is configured before Valve can create any threads
    // or load its output component. It is CLOEXEC and never adopted from a
    // parent/game process. The session outlives the entire engine teardown.
    let output_selection =
        crate::wine_input_output::OutputSelection::from_worker_environment(phase == "live")?;
    let feedback_qa =
        crate::input_fixture_rumble::validate_feedback_environment(output_selection.is_game())?;
    let feedback_product = std::env::var("SKINOS_INPUT_FEEDBACK").ok();
    let feedback_mode = match (feedback_qa, feedback_product.as_deref()) {
        (true, None | Some("")) => Some(rumble_feedback_session::Mode::FixtureQa),
        (false, Some("1")) if output_selection.is_game() => {
            Some(rumble_feedback_session::Mode::Product)
        }
        (false, None | Some("")) => None,
        _ => return Err("Feedback mode does not match this game input session".into()),
    };
    let wine_output =
        RefCell::new(unsafe { wine_input_session::Session::create(state, output_selection)? });
    let rumble_feedback = RefCell::new(if let Some(mode) = feedback_mode {
        let epoch = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_nanos() as u64)
            .max(1);
        wine_output
            .borrow_mut()
            .enable_experimental_feedback(epoch)?;
        Some(rumble_feedback_session::Session::new(epoch, mode))
    } else {
        None
    });
    let game_input = RefCell::new(if output_selection.uses_production_preparation() {
        let root =
            std::env::var_os("SKINOS_RUNTIME_DATA_ROOT").ok_or("Owned input data root missing")?;
        Some(game_input_session::Session::capture_before_load(
            Path::new(&root),
        )?)
    } else {
        None
    });
    let rumble_fixture = RefCell::new(
        if crate::input_fixture_rumble::validate_environment(output_selection.is_diagnostic())? {
            let root = std::env::var_os("SKINOS_RUNTIME_DATA_ROOT")
                .ok_or("Owned rumble data root missing")?;
            Some(rumble_fixture_session::Session::capture(Path::new(&root))?)
        } else {
            None
        },
    );
    // Fail closed throughout bootstrap: merely staging binaries is not enough
    // to allow an original Valve launchd service to activate implicitly.
    unsafe {
        let configured = dlsym(
            (-2_isize) as *mut c_void,
            c"skinos_ipc_is_configured".as_ptr(),
        );
        if configured.is_null() {
            return Err(
                "Owned IPC routing adapter is unavailable; runtime startup is blocked".into(),
            );
        }
        let configured: unsafe extern "C" fn() -> i32 = std::mem::transmute(configured);
        if configured() != 1 {
            return Err("Owned IPC namespace is not configured; runtime startup is blocked".into());
        }
    }
    let path = CString::new(runtime.as_os_str().as_encoded_bytes()).map_err(|e| e.to_string())?;
    event("loading", true, json!({"phase":phase}));
    // SAFETY: occurs only after sandbox guards in the separate bounded worker.
    let library = unsafe { dlopen(path.as_ptr(), 2 | 4) }; // RTLD_NOW | RTLD_LOCAL, Darwin.
    if library.is_null() {
        // SAFETY: dlerror returns a thread-local string, copied immediately.
        let message = unsafe {
            let error = dlerror();
            if error.is_null() {
                "unknown dlopen error".into()
            } else {
                CStr::from_ptr(error).to_string_lossy().into_owned()
            }
        };
        return Err(message);
    }
    event("loaded", true, json!({}));
    if phase == "load" {
        // SAFETY: no interface pointers or handles have been created.
        let result = unsafe { dlclose(library) };
        event("unloaded", result == 0, json!({}));
        return Ok(());
    }
    // These flat signatures are grounded in pinned OpenSteamworks source and
    // audited arm64 exported wrappers, not guessed internal vtable offsets.
    unsafe {
        let factory: unsafe extern "C" fn(*const c_char, *mut i32) -> *mut c_void =
            std::mem::transmute(symbol(library, c"CreateInterface")?);
        let mut factory_code = -1;
        let engine = factory(
            c"CLIENTENGINE_INTERFACE_VERSION005".as_ptr(),
            &mut factory_code,
        );
        event(
            "factory",
            !engine.is_null(),
            json!({"version":"CLIENTENGINE_INTERFACE_VERSION005","return_code":factory_code}),
        );
        if engine.is_null() {
            return Err("Client engine factory unavailable.".into());
        }
        let mut image = DlInfo {
            filename: std::ptr::null(),
            base: std::ptr::null_mut(),
            symbol: std::ptr::null(),
            symbol_address: std::ptr::null_mut(),
        };
        if dladdr(factory as *const c_void, &mut image) != 0 {
            let table = *(engine as *const *const *const c_void);
            let slots: Vec<_> = (0..=49).map(|index| json!({"index":index,"offset":format!("0x{:x}",(*table.add(index) as usize).wrapping_sub(image.base as usize))})).collect();
            event("engine_layout", true, json!({"slots":slots}));
        }
        if phase == "factory" {
            return Ok(());
        }
        let create_global: unsafe extern "C" fn(*mut i32) -> i32 =
            std::mem::transmute(symbol(library, c"Steam_CreateGlobalUser")?);
        let release_user: unsafe extern "C" fn(i32, i32) =
            std::mem::transmute(symbol(library, c"Steam_ReleaseUser")?);
        let release_pipe: unsafe extern "C" fn(i32) -> bool =
            std::mem::transmute(symbol(library, c"Steam_BReleaseSteamPipe")?);
        let get_callback: unsafe extern "C" fn(i32, *mut Callback) -> bool =
            std::mem::transmute(symbol(library, c"Steam_BGetCallback")?);
        let free_callback: unsafe extern "C" fn(i32) =
            std::mem::transmute(symbol(library, c"Steam_FreeLastCallback")?);
        let is_connected: unsafe extern "C" fn(i32, i32) -> bool =
            std::mem::transmute(symbol(library, c"Steam_BConnected")?);
        let is_logged_on: unsafe extern "C" fn(i32, i32) -> bool =
            std::mem::transmute(symbol(library, c"Steam_BLoggedOn")?);
        event("creating_global_user", true, json!({}));
        let mut pipe = 0;
        let user = create_global(&mut pipe);
        event(
            "global_user",
            user != 0 && pipe != 0,
            json!({"user_handle":user,"pipe_handle":pipe}),
        );
        if user == 0 || pipe == 0 {
            return Err("Global user creation failed.".into());
        }
        let fixture_log = if !output_selection.is_product_diagnostic()
            && std::env::var("SKINOS_INPUT_CONFIG").ok().as_deref() == Some("1")
        {
            let root = std::env::var_os("SKINOS_RUNTIME_DATA_ROOT")
                .ok_or("Owned runtime data root missing")?;
            Some(crate::input_fixture_log::Cursor::capture(Path::new(&root))?)
        } else {
            None
        };
        // Explicit owned neutral QA remains available on every route; product
        // diagnostics do not enable it and never run the active fixture sequence.
        let input_fixture = RefCell::new(crate::input_fixture::Fixture::from_environment(state)?);
        if let Some(fixture) = &*input_fixture.borrow() {
            if let Some(scope) = rumble_fixture.borrow_mut().as_mut() {
                scope.attached(fixture)?;
            }
            if let Some(scope) = rumble_feedback.borrow_mut().as_mut() {
                scope.attached(fixture)?;
            }
            event(
                "steam_input_fixture",
                true,
                json!({
                    "fixture":true,"mode":"neutral","sdl_joystick_id":fixture.joystick_id,
                    "sdl_version":fixture.sdl_version,"sdl_is_gamepad":fixture.sdl_is_gamepad,
                    "active_input":false,"physical_device_verified":false
                }),
            );
        }
        let get_client_user: unsafe extern "C" fn(*mut c_void, i32, i32) -> *mut c_void =
            std::mem::transmute(method(engine, 8));
        let client_user = get_client_user(engine, user, pipe);
        if client_user.is_null() {
            return Err("Client user interface unavailable".into());
        }
        let slots: Vec<_> = [1,4,6,10,54,55,56,57,58,67,74,181].into_iter().map(|index| json!({"index":index,"offset":format!("0x{:x}",(method(client_user,index) as usize).wrapping_sub(image.base as usize))})).collect();
        event(
            "user_layout",
            !client_user.is_null(),
            json!({"slots":slots}),
        );
        let get_friends: unsafe extern "C" fn(*mut c_void, i32, i32) -> *mut c_void =
            std::mem::transmute(method(engine, 13));
        let friends = get_friends(engine, user, pipe);
        if friends.is_null() {
            return Err("Client friends interface unavailable".into());
        }
        let slots:Vec<_>=(0..7).map(|index|json!({"index":index,"offset":format!("0x{:x}",(method(friends,index) as usize).wrapping_sub(image.base as usize))})).collect();
        event("friends_layout", true, json!({"slots":slots}));
        if phase == "global" {
            let set: unsafe extern "C" fn(*mut c_void, u32) =
                std::mem::transmute(method(friends, 5));
            let get: unsafe extern "C" fn(*mut c_void) -> u32 =
                std::mem::transmute(method(friends, 4));
            set(friends, 7);
            let actual = get(friends);
            event(
                "offline_invisible_probe",
                actual == 0 || actual == 7,
                json!({"requested":7,"actual":actual,"offline":true}),
            );
        }
        let get_manager: unsafe extern "C" fn(*mut c_void, i32, i32) -> *mut c_void =
            std::mem::transmute(method(engine, 43));
        let manager = get_manager(engine, user, pipe);
        if manager.is_null() {
            return Err("App manager unavailable".into());
        }
        let slots:Vec<_>=[2,4,5,75,77,83].into_iter().map(|index|json!({"index":index,"offset":format!("0x{:x}",(method(manager,index) as usize).wrapping_sub(image.base as usize))})).collect();
        event("manager_layout", true, json!({"slots":slots}));
        let get_apps: unsafe extern "C" fn(*mut c_void, i32, i32) -> *mut c_void =
            std::mem::transmute(method(engine, 17));
        let apps = get_apps(engine, user, pipe);
        if apps.is_null() {
            return Err("Client apps unavailable".into());
        }
        let slots:Vec<_>=[4,7].into_iter().map(|index|json!({"index":index,"offset":format!("0x{:x}",(method(apps,index) as usize).wrapping_sub(image.base as usize))})).collect();
        event("apps_layout", true, json!({"slots":slots}));
        let get_utils: unsafe extern "C" fn(*mut c_void, i32) -> *mut c_void =
            std::mem::transmute(method(engine, 14));
        let utils = get_utils(engine, pipe);
        if !utils.is_null() {
            let slots:Vec<_>=[34,39,40,64].into_iter().map(|index|json!({"index":index,"offset":format!("0x{:x}",(method(utils,index) as usize).wrapping_sub(image.base as usize))})).collect();
            event("utils_layout", true, json!({"slots":slots}));
        }
        let get_storage: unsafe extern "C" fn(*mut c_void, i32, i32) -> *mut c_void =
            std::mem::transmute(method(engine, 24));
        let storage = get_storage(engine, user, pipe);
        if !storage.is_null() {
            let slots:Vec<_>=[24,25].into_iter().map(|index|json!({"index":index,"offset":format!("0x{:x}",(method(storage,index) as usize).wrapping_sub(image.base as usize))})).collect();
            event("storage_layout", true, json!({"slots":slots}));
        }
        let mut isolated_user_paths = 0;
        if !utils.is_null() {
            for (index, name) in [
                "install",
                "user_install_image",
                "user_persistent_storage",
                "managed_content",
            ]
            .into_iter()
            .enumerate()
            {
                let get_path: unsafe extern "C" fn(*mut c_void) -> *const c_char =
                    std::mem::transmute(method(utils, index));
                let value = get_path(utils);
                if !value.is_null() {
                    let path = CStr::from_ptr(value).to_string_lossy();
                    event("runtime_path", true, json!({"kind":name,"path":path}));
                    if (index == 1 || index == 2)
                        && Some(Path::new(path.as_ref()).as_os_str())
                            == std::env::var_os("SKINOS_RUNTIME_DATA_ROOT").as_deref()
                    {
                        isolated_user_paths += 1;
                    }
                }
            }
        }
        event(
            "storage_isolation",
            isolated_user_paths == 2,
            json!({"verified_getters":isolated_user_paths}),
        );
        if isolated_user_paths != 2 {
            return Err("Runtime storage getters did not match the isolated root.".into());
        }
        // Audited current Mac proxy stubs: 34 SetCurrentUIMode@0x8cde30,
        // 39 SetLauncherType@0x8ce548, 64 SetClientUIProcess@0x8d0f24.
        let launcher: unsafe extern "C" fn(*mut c_void, i32) =
            std::mem::transmute(method(utils, 39));
        let ui_mode: unsafe extern "C" fn(*mut c_void, i32) =
            std::mem::transmute(method(utils, 34));
        let ui_process: unsafe extern "C" fn(*mut c_void) = std::mem::transmute(method(utils, 64));
        launcher(utils, 5);
        ui_mode(utils, 7);
        ui_process(utils);
        event(
            "ui_process_registered",
            true,
            json!({"launcher_type":5,"ui_mode":7}),
        );
        let register = dlsym(
            (-2_isize) as *mut c_void,
            c"skinos_ipc_register_current_process".as_ptr(),
        );
        if register.is_null() {
            return Err("Owned broker registration adapter missing".into());
        }
        let register: unsafe extern "C" fn() -> i32 = std::mem::transmute(register);
        let registered = register() == 1;
        event(
            "broker_process_registered",
            registered,
            json!({"pid":std::process::id(),"executable":std::env::current_exe().map_err(|e|e.to_string())?,"real_query_verified":registered}),
        );
        if !registered {
            return Err("Genuine broker did not confirm this runtime PID/path".into());
        }
        event(
            "connection",
            true,
            json!({"connected":is_connected(user,pipe),"logged_on":is_logged_on(user,pipe)}),
        );
        let fixture_joystick_id = input_fixture
            .borrow()
            .as_ref()
            .map(|fixture| fixture.joystick_id);
        let live_result = if phase == "live" {
            Some(live_session(
                client_user,
                pipe,
                user,
                get_callback,
                free_callback,
                is_logged_on,
                &LiveRuntime {
                    profile_lock_fd: profile_lock_fd
                        .ok_or("Profile lock missing from live runtime")?,
                    friends,
                    controls: Arc::new(RuntimeControls::default()),
                    library,
                    engine,
                    image_base: image.base as usize,
                    fixture_log: fixture_log.as_ref(),
                    fixture_joystick_id,
                    fixture: &input_fixture,
                    rumble_fixture: &rumble_fixture,
                    rumble_feedback: &rumble_feedback,
                    wine_output: &wine_output,
                    game_input: &game_input,
                    manager,
                    state,
                    user,
                },
            ))
        } else {
            None
        };
        let start = Instant::now();
        let mut callback_count = 0;
        while start.elapsed() < Duration::from_millis(300) {
            let mut callback = Callback {
                user: 0,
                callback: 0,
                parameter: std::ptr::null_mut(),
                parameter_bytes: 0,
            };
            if get_callback(pipe, &mut callback) {
                callback_count += 1;
                event(
                    "callback",
                    true,
                    json!({"id":callback.callback,"payload_bytes":callback.parameter_bytes}),
                );
                free_callback(pipe);
            }
            // Retain any failure for finish while still completing the normal
            // original fixture/user/engine cleanup sequence below.
            let _ = wine_output.borrow_mut().check();
            std::thread::sleep(Duration::from_millis(10));
        }
        event("callbacks_pumped", true, json!({"count":callback_count}));
        if let Some(scope) = rumble_feedback.borrow_mut().as_mut() {
            let cleanup_pump = || {
                let frame: unsafe extern "C" fn(*mut c_void) =
                    std::mem::transmute(method(engine, 19));
                frame(engine);
                let mut callback = Callback {
                    user: 0,
                    callback: 0,
                    parameter: std::ptr::null_mut(),
                    parameter_bytes: 0,
                };
                while get_callback(pipe, &mut callback) {
                    free_callback(pipe);
                }
            };
            scope.finish_source(input_fixture.borrow().as_ref(), cleanup_pump);
        }
        let game_input_result = game_input
            .borrow_mut()
            .as_mut()
            .map(|scope| scope.finish_before_shutdown())
            .transpose();
        if let Err(error) = &game_input_result {
            let _ = wine_output.borrow_mut().record_input_failure(error.clone());
        }
        game_input.borrow_mut().take();
        drop(input_fixture);
        if let Some(scope) = rumble_fixture.borrow_mut().as_mut() {
            let completion = scope.finish_after_detach().and_then(|payload| {
                payload
                    .map(|payload| wine_output.borrow_mut().defer_diagnostic(payload))
                    .transpose()
            });
            if let Err(error) = completion {
                let _ = wine_output.borrow_mut().record_input_failure(error);
            }
        }
        release_user(pipe, user);
        let released = release_pipe(pipe);
        event("released", released, json!({}));
        // Slot 49 is grounded in pinned IClientEngine.h and this Mac binary's
        // pipe-count guard + global-thread shutdown disassembly at 0xa024dc.
        let shutdown: unsafe extern "C" fn(*mut c_void) -> bool =
            std::mem::transmute(method(engine, 49));
        let stopped = shutdown(engine);
        event("shutdown", stopped, json!({}));
        if stopped {
            event("unloaded", dlclose(library) == 0, json!({}));
        }
        // OutputLease keeps the output image resident across Valve's dlclose;
        // finish drains/join the relay before its final stats observation.
        let wine_output_result = wine_output.borrow_mut().finish(stopped);
        if let Some(scope) = rumble_feedback.borrow_mut().as_mut() {
            scope.finalize(
                state,
                stopped,
                live_result.as_ref().is_some_and(|result| result.is_ok()),
                wine_output.borrow().output_final_report(),
                wine_output
                    .borrow()
                    .zero_controller_finished_without_relay(),
            );
        }
        if let Some(result) = live_result {
            result?;
        }
        wine_output_result?;
    }
    Ok(())
}

fn live_status(appid: u32, state: &str, steamid: Option<u64>, message: Option<&str>) {
    let _ = writeln!(
        std::io::stdout(),
        "SKINOS_LIVE {}",
        json!({"event":"launch_status","payload":{"appid":appid,"state":state,"steamid":steamid.map(|v|v.to_string()),"message":message}})
    );
    let _ = std::io::stdout().flush();
}

fn live_session(
    client_user: *mut c_void,
    pipe: i32,
    user: i32,
    get_callback: unsafe extern "C" fn(i32, *mut Callback) -> bool,
    free_callback: unsafe extern "C" fn(i32),
    is_logged_on: unsafe extern "C" fn(i32, i32) -> bool,
    runtime: &LiveRuntime<'_>,
) -> Result<(), String> {
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    if line.len() > 65536 {
        return Err("Command exceeds protocol limit".into());
    }
    let request: serde_json::Value =
        serde_json::from_str(&line).map_err(|_| "Invalid command JSON")?;
    runtime.wine_output.borrow().validate_mode(&request)?;
    let appid = request["appid"].as_u64().unwrap_or(0) as u32;
    let steamid = request["steamid"]
        .as_str()
        .and_then(|v| v.parse::<u64>().ok())
        .or_else(|| request["steamid"].as_u64())
        .ok_or("Missing SteamID")?;
    let account = CString::new(
        request["account_name"]
            .as_str()
            .ok_or("Missing account name")?,
    )
    .map_err(|_| "Invalid account name")?;
    let token = CString::new(
        request["refresh_token"]
            .as_str()
            .ok_or("Missing refresh token")?,
    )
    .map_err(|_| "Invalid token")?;
    let persona = request["persona_state"]
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(7);
    if !valid_persona(persona) {
        return Err("Invalid persona state".into());
    }
    runtime.controls.persona.store(persona, Ordering::SeqCst);
    apply_presence(runtime, persona, false)?;
    let controls = runtime.controls.clone();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            match parse_control(&line, appid) {
                Some(ControlCommand::Stop) => controls.stop.store(true, Ordering::SeqCst),
                Some(ControlCommand::Presence(value)) => {
                    controls.persona.store(value, Ordering::SeqCst)
                }
                None => {}
            }
        }
    });
    live_status(appid, "authenticating", None, None);
    unsafe {
        let set_token: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) =
            std::mem::transmute(method(client_user, 56));
        let log_on: unsafe extern "C" fn(*mut c_void, u64) -> i32 =
            std::mem::transmute(method(client_user, 1));
        let get_steamid: unsafe extern "C" fn(*mut c_void) -> u64 =
            std::mem::transmute(method(client_user, 10));
        set_token(client_user, token.as_ptr(), account.as_ptr());
        let result = log_on(client_user, steamid);
        event("logon_started", result == 1, json!({"result":result}));
        if result != 1 {
            live_status(appid, "failed", None, Some("Runtime rejected login start"));
            return Err(format!("LogOn returned EResult {result}"));
        }
        let deadline = Instant::now() + Duration::from_secs(45);
        while Instant::now() < deadline {
            let mut callback = Callback {
                user: 0,
                callback: 0,
                parameter: std::ptr::null_mut(),
                parameter_bytes: 0,
            };
            while get_callback(pipe, &mut callback) {
                // Only IDs/sizes are diagnostic; credentials/payloads never logged.
                event(
                    "auth_callback",
                    true,
                    json!({"id":callback.callback,"payload_bytes":callback.parameter_bytes}),
                );
                free_callback(pipe);
            }
            if is_logged_on(user, pipe) {
                let actual = get_steamid(client_user);
                if actual != steamid {
                    live_status(
                        appid,
                        "failed",
                        Some(actual),
                        Some("Runtime identity mismatch"),
                    );
                    return Err("Runtime identity mismatch".into());
                }
                apply_presence(
                    runtime,
                    runtime.controls.persona.load(Ordering::SeqCst),
                    true,
                )?;
                live_status(appid, "authenticated", Some(actual), None);
                event(
                    "runtime_authenticated",
                    true,
                    json!({"steamid":actual.to_string()}),
                );
                if request["command"] == "authenticate" {
                    return Ok(());
                }
                // Temporary, explicitly enabled diagnostic only; the guard is
                // restored while its authenticated user/pipe are still alive.
                let mut input_settings = if request["input_diagnostic"] == true
                    && !runtime.wine_output.borrow().uses_production_preparation()
                {
                    crate::input_settings::attach(runtime.engine, user, pipe, runtime.image_base)?
                } else {
                    None
                };
                let result = launch_game(
                    &request,
                    runtime,
                    client_user,
                    pipe,
                    actual,
                    get_callback,
                    free_callback,
                );
                let restored = input_settings
                    .as_mut()
                    .map(|guard| guard.restore())
                    .transpose();
                return result.and(restored.map(|_| ()));
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    live_status(appid, "failed", None, Some("Runtime login timed out"));
    Err("Runtime login timed out".into())
}

#[derive(Default)]
struct RuntimeControls {
    stop: AtomicBool,
    persona: AtomicU32,
}

struct LiveRuntime<'a> {
    profile_lock_fd: i32,
    friends: *mut c_void,
    controls: Arc<RuntimeControls>,
    library: *mut c_void,
    engine: *mut c_void,
    image_base: usize,
    fixture_log: Option<&'a crate::input_fixture_log::Cursor>,
    fixture_joystick_id: Option<u32>,
    fixture: &'a RefCell<Option<crate::input_fixture::Fixture>>,
    rumble_fixture: &'a RefCell<Option<rumble_fixture_session::Session>>,
    rumble_feedback: &'a RefCell<Option<rumble_feedback_session::Session>>,
    wine_output: &'a RefCell<wine_input_session::Session>,
    game_input: &'a RefCell<Option<game_input_session::Session>>,
    manager: *mut c_void,
    state: &'a Path,
    user: i32,
}

impl LiveRuntime<'_> {
    fn check_input_output(&self) -> Result<(), String> {
        self.wine_output.borrow_mut().check()
    }
    fn check_input_output_child(&self, child: &mut std::process::Child) -> Result<(), String> {
        if let Err(error) = self.check_input_output() {
            // This child belongs to this diagnostic invocation. Never choose
            // another game PID or terminate a Wine process group on failure.
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        Ok(())
    }
}

fn launch_game(
    request: &serde_json::Value,
    runtime: &LiveRuntime<'_>,
    client_user: *mut c_void,
    pipe: i32,
    steamid: u64,
    get_callback: unsafe extern "C" fn(i32, *mut Callback) -> bool,
    free_callback: unsafe extern "C" fn(i32),
) -> Result<(), String> {
    let appid = request["appid"].as_u64().ok_or("Missing AppID")? as u32;
    let library_path = CString::new(
        std::env::var_os("SKINOS_RUNTIME_LIBRARY_ROOT")
            .ok_or("Private library root missing")?
            .as_encoded_bytes(),
    )
    .map_err(|e| e.to_string())?;
    unsafe {
        let get_storage: unsafe extern "C" fn(*mut c_void, i32, i32) -> *mut c_void =
            std::mem::transmute(method(runtime.engine, 24));
        let storage = get_storage(runtime.engine, runtime.user, pipe);
        if storage.is_null() {
            return Err("Remote storage interface unavailable".into());
        }
        let set_cloud: unsafe extern "C" fn(*mut c_void, u32, bool) =
            std::mem::transmute(method(storage, 25));
        let cloud_enabled: unsafe extern "C" fn(*mut c_void, u32) -> bool =
            std::mem::transmute(method(storage, 24));
        set_cloud(storage, appid, false);
        if cloud_enabled(storage, appid) {
            return Err("Could not disable experimental Cloud sync in private runtime".into());
        }
        event("experimental_cloud_disabled", true, json!({"appid":appid}));
        let subscribed: unsafe extern "C" fn(*mut c_void, u32) -> bool =
            std::mem::transmute(method(client_user, 181));
        // Current Mac wrapper dereferences x1 (0x8976ac): CGameID is by
        // reference here, unlike LogOn's value SteamID. The C# header omits in.
        let running: unsafe extern "C" fn(*mut c_void, *const u64) -> bool =
            std::mem::transmute(method(client_user, 67));
        let add_library: unsafe extern "C" fn(*mut c_void, *const c_char) -> u32 =
            std::mem::transmute(method(runtime.manager, 77));
        let refresh_libraries: unsafe extern "C" fn(*mut c_void) =
            std::mem::transmute(method(runtime.manager, 83));
        let install_state: unsafe extern "C" fn(*mut c_void, u32) -> u32 =
            std::mem::transmute(method(runtime.manager, 4));
        let get_apps: unsafe extern "C" fn(*mut c_void, i32, i32) -> *mut c_void =
            std::mem::transmute(method(runtime.engine, 17));
        let apps = get_apps(runtime.engine, runtime.user, pipe);
        if apps.is_null() {
            return Err("Client apps interface unavailable".into());
        }
        let request_info: unsafe extern "C" fn(*mut c_void, *const u32, i32) -> bool =
            std::mem::transmute(method(apps, 7));
        let launch_options: unsafe extern "C" fn(*mut c_void, u32, *mut i32, i32) -> i32 =
            std::mem::transmute(method(apps, 4));
        let run_frame: unsafe extern "C" fn(*mut c_void) =
            std::mem::transmute(method(runtime.engine, 19));
        let requested = request_info(apps, &appid, 1);
        event("appinfo_requested", requested, json!({"appid":appid}));
        let folder = add_library(runtime.manager, library_path.as_ptr());
        refresh_libraries(runtime.manager);
        event(
            "library_registered",
            true,
            json!({"folder_id":folder,"state":install_state(runtime.manager,appid)}),
        );
        let mut options = [0_i32; 32];
        let ready_deadline = Instant::now() + Duration::from_secs(60);
        let mut next_request = Instant::now() + Duration::from_secs(5);
        let mut rerequests = 0;
        let selected = loop {
            run_frame(runtime.engine);
            runtime.check_input_output()?;
            let mut callback = Callback {
                user: 0,
                callback: 0,
                parameter: std::ptr::null_mut(),
                parameter_bytes: 0,
            };
            while get_callback(pipe, &mut callback) {
                free_callback(pipe);
            }
            let count = launch_options(apps, appid, options.as_mut_ptr(), options.len() as i32);
            let owned = subscribed(client_user, appid);
            if owned && (count > 0 || !request["compatibility"].is_null()) {
                let count =
                    usize::try_from(count.max(0)).map_err(|_| "Invalid launch option count")?;
                let available = options
                    .get(..count)
                    .ok_or("Runtime launch option list exceeds its buffer")?;
                let requested = request
                    .get("launch_option_id")
                    .filter(|value| !value.is_null())
                    .map(|value| {
                        value
                            .as_u64()
                            .and_then(|id| u32::try_from(id).ok())
                            .ok_or("Invalid selected launch option ID")
                    })
                    .transpose()?;
                break launch_selection::select(
                    requested,
                    available,
                    !request["compatibility"].is_null(),
                )?;
            }
            if rerequests < 3 && Instant::now() > next_request {
                event(
                    "appinfo_pending",
                    true,
                    json!({"subscribed":owned,"launch_options":count,"install_state":install_state(runtime.manager,appid)}),
                );
                request_info(apps, &appid, 1);
                rerequests += 1;
                next_request = Instant::now() + Duration::from_secs(5);
            }
            if Instant::now() > ready_deadline {
                live_status(
                    appid,
                    "failed",
                    Some(steamid),
                    Some("Runtime ownership or launch metadata is unavailable"),
                );
                return Err("Runtime ownership/launch metadata timeout".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let state = install_state(runtime.manager, appid);
        event(
            "launch_ready",
            true,
            json!({"appid":appid,"launch_option":selected,"install_state":state,"subscribed":true}),
        );
        if !request["compatibility"].is_null() {
            return launch_compatibility(
                request,
                runtime,
                client_user,
                pipe,
                steamid,
                get_callback,
                free_callback,
            );
        }
        verify_sdk(request, runtime, appid, steamid)?;
        // The real SDK consumer briefly counts as this app. Its process has
        // exited; let Steam's own process monitor observe that before launch.
        let gameid = u64::from(appid);
        let settle = Instant::now() + Duration::from_secs(10);
        while running(client_user, &gameid) {
            run_frame(runtime.engine);
            runtime.check_input_output()?;
            let mut callback = Callback {
                user: 0,
                callback: 0,
                parameter: std::ptr::null_mut(),
                parameter_bytes: 0,
            };
            while get_callback(pipe, &mut callback) {
                free_callback(pipe);
            }
            if Instant::now() > settle {
                return Err("SDK probe exited but runtime app status did not settle".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        live_status(appid, "starting", Some(steamid), None);
        let launch: unsafe extern "C" fn(*mut c_void, *const u64, u32, u32, *const c_char) -> u64 =
            std::mem::transmute(method(runtime.manager, 2));
        let result: unsafe extern "C" fn(i32, u64, *mut c_void, i32, i32, *mut bool) -> bool =
            std::mem::transmute(symbol(runtime.library, c"Steam_GetAPICallResult")?);
        let call = launch(runtime.manager, &gameid, selected, 0, c"".as_ptr());
        if call == 0 {
            live_status(
                appid,
                "failed",
                Some(steamid),
                Some("Runtime refused launch request"),
            );
            return Err("Invalid launch call handle".into());
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut seen_running = false;
        let mut launch_result = false;
        let mut stop_attempt = None;
        let mut tracked: Option<TrackedGameProcess> = None;
        let script = std::env::var("SKINOS_NATIVE_TARGET_KIND").as_deref() == Ok("script");
        let mut script_tracker: Option<native_process::ScriptTracker> = None;
        let mut persona = runtime.controls.persona.load(Ordering::SeqCst);
        apply_presence(runtime, persona, true)?;
        loop {
            refresh_presence(runtime, &mut persona)?;
            if script
                && script_tracker.is_none()
                && let Some((pid, group, started)) = native_launcher_identity()?
            {
                script_tracker = Some(native_process::ScriptTracker::capture(
                    pid,
                    group,
                    u128::from(started),
                    Path::new(
                        request["install_dir"]
                            .as_str()
                            .ok_or("Installation missing")?,
                    ),
                    appid,
                    runtime.state,
                )?);
            }
            if seen_running
                && runtime.controls.stop.load(Ordering::SeqCst)
                && let Some(process) = tracked.as_ref()
                && process.is_current()
            {
                terminate_selected_game(appid, steamid, process.pid, &mut stop_attempt)?;
            }
            let mut callback = Callback {
                user: 0,
                callback: 0,
                parameter: std::ptr::null_mut(),
                parameter_bytes: 0,
            };
            while get_callback(pipe, &mut callback) {
                if callback.callback == 703
                    && callback.parameter_bytes >= 16
                    && !callback.parameter.is_null()
                {
                    let data = std::slice::from_raw_parts(callback.parameter.cast::<u8>(), 16);
                    let handle = u64::from_ne_bytes(data[0..8].try_into().unwrap());
                    let result_id = i32::from_ne_bytes(data[8..12].try_into().unwrap());
                    let size = u32::from_ne_bytes(data[12..16].try_into().unwrap());
                    if handle == call && result_id == 1270027 && (524..=1024).contains(&size) {
                        let mut buffer = vec![0_u8; size as usize];
                        let mut failed = false;
                        if result(
                            pipe,
                            call,
                            buffer.as_mut_ptr().cast(),
                            size as i32,
                            1270027,
                            &mut failed,
                        ) {
                            let error = u32::from_ne_bytes(buffer[8..12].try_into().unwrap());
                            event(
                                "launch_result",
                                !failed && error == 0,
                                json!({"app_error":error,"call_failed":failed}),
                            );
                            launch_result = !failed && error == 0;
                            if !launch_result {
                                free_callback(pipe);
                                let message = format!("Steam runtime LaunchApp error {error}");
                                live_status(appid, "failed", Some(steamid), Some(&message));
                                return Err(message);
                            }
                        }
                    }
                }
                free_callback(pipe);
            }
            let active = running(client_user, &gameid);
            if active && launch_result && !seen_running {
                let candidate = if script {
                    script_tracker
                        .as_ref()
                        .map(|tracker| tracker.find_game())
                        .transpose()?
                        .flatten()
                } else {
                    Some(verify_game_process()?)
                };
                if let Some(process) = candidate {
                    let pid = process.pid;
                    tracked = Some(process);
                    if !running(client_user, &gameid) {
                        return Err("Game exited during SDK verification".into());
                    }
                    seen_running = true;
                    event(
                        "game_running",
                        true,
                        json!({"appid":appid,"steamid":steamid.to_string(),"pid":pid}),
                    );
                    live_status(appid, "running", Some(steamid), None);
                }
            }
            if seen_running && !active {
                live_status(appid, "exited", Some(steamid), None);
                return Ok(());
            }
            if !seen_running && Instant::now() > deadline {
                live_status(
                    appid,
                    "failed",
                    Some(steamid),
                    Some("Runtime launch did not reach a running game"),
                );
                return Err("Launch timeout".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn launch_compatibility(
    request: &serde_json::Value,
    runtime: &LiveRuntime<'_>,
    client_user: *mut c_void,
    pipe: i32,
    steamid: u64,
    get_callback: unsafe extern "C" fn(i32, *mut Callback) -> bool,
    free_callback: unsafe extern "C" fn(i32),
) -> Result<(), String> {
    use std::process::{Command, Stdio};
    let mut plan: skinos_compat::LaunchPlan =
        serde_json::from_value(request["compatibility"].clone()).map_err(|e| e.to_string())?;
    let appid = request["appid"].as_u64().ok_or("Missing AppID")? as u32;
    let gameid = u64::from(appid);
    let account = request["account_name"].as_str().ok_or("Missing account")?;
    let pump = || unsafe {
        let frame: unsafe extern "C" fn(*mut c_void) =
            std::mem::transmute(method(runtime.engine, 19));
        frame(runtime.engine);
        let mut callback = Callback {
            user: 0,
            callback: 0,
            parameter: std::ptr::null_mut(),
            parameter_bytes: 0,
        };
        while get_callback(pipe, &mut callback) {
            free_callback(pipe);
        }
    };
    let game_input_mode = runtime.wine_output.borrow().is_game();
    let mut reconnect_test =
        crate::input_fixture_reconnect::Sequence::from_environment(game_input_mode)?;
    let mut reconnect_test_error = None;
    let production_input_mode = runtime.wine_output.borrow().uses_production_preparation();
    let product_diagnostic = runtime.wine_output.borrow().is_product_diagnostic();
    let input_runtime = game_input_session::Runtime {
        engine: runtime.engine,
        user: runtime.user,
        pipe,
        base: runtime.image_base,
    };
    if let Some(scope) = runtime.rumble_feedback.borrow_mut().as_mut() {
        scope.configure(appid);
    }
    if production_input_mode {
        let mut scope = runtime.game_input.borrow_mut();
        let scope = scope
            .as_mut()
            .ok_or("Game input scope was not captured before engine load")?;
        let overlay = unsafe { scope.prepare(input_runtime, runtime.state, &pump)? };
        plan.env.extend(overlay);
        if !scope.has_device() {
            runtime.wine_output.borrow_mut().note_no_gamepad()?;
        }
    }
    let command = |args: &[String], name: &str| -> Result<Command, String> {
        let mut command = Command::new(&plan.program);
        command
            .args(args)
            .envs(&plan.env)
            .current_dir(&plan.working_dir)
            .env(
                "STEAM_COMPAT_CLIENT_INSTALL_PATH",
                runtime.state.join("runtime"),
            )
            .env(
                "SKINOS_GAME_DYLD_INSERT_LIBRARIES",
                runtime.state.join("libskinos_trace.dylib"),
            )
            .env("SKINOS_COMPONENT_ROLE", "game")
            .env("SKINOS_EXPECTED_STEAMID", steamid.to_string())
            .env("SteamAppId", appid.to_string())
            .env("SteamGameId", appid.to_string())
            .env("SteamOverlayGameId", appid.to_string())
            .env("SteamClientLaunch", "1")
            .env("SteamEnv", "1")
            .env("SteamNoOverlayUI", "1")
            .env("SteamUser", account)
            .env("SteamAppUser", account)
            .env_remove("SKINOS_INPUT_OUTPUT_FD")
            .env_remove("SKINOS_INPUT_RUMBLE_TEST")
            .env_remove("SKINOS_INPUT_RUMBLE_FIXTURE_ID")
            .env_remove("SKINOS_INPUT_FEEDBACK_SINK")
            .env_remove("SKINOS_INPUT_FEEDBACK_TEST")
            .env_remove("SKINOS_INPUT_FEEDBACK")
            .env_remove("SKINOS_PREFIX_SESSION_READY")
            .env_remove("SKINOS_PREFIX_SESSION_RECEIPT")
            .stdin(Stdio::null())
            .stdout(
                std::fs::File::create(runtime.state.join(format!("{name}.stdout.log")))
                    .map_err(|e| e.to_string())?,
            )
            .stderr(
                std::fs::File::create(runtime.state.join(format!("{name}.stderr.log")))
                    .map_err(|e| e.to_string())?,
            );
        isolate_profile_lock(&mut command, runtime.profile_lock_fd)?;
        Ok(command)
    };
    let running: unsafe extern "C" fn(*mut c_void, *const u64) -> bool =
        unsafe { std::mem::transmute(method(client_user, 67)) };
    live_status(
        appid,
        "starting",
        Some(steamid),
        Some("Проверка Windows SteamAPI через NotProton"),
    );
    let prefix_receipt = runtime.state.join("prepared-prefix-session.receipt");
    let mut probe_command = command(&plan.sdk_args, "compat-sdk")?;
    probe_command
        .envs(&plan.sdk_env)
        .env_remove("SKINOS_PREFIX_SESSION_READY")
        .env_remove("SKINOS_PREFIX_SESSION_RECEIPT");
    if request["input_diagnostic"] == true || game_input_mode {
        probe_command.env("SKINOS_PREFIX_SESSION_RECEIPT", &prefix_receipt);
    }
    let mut probe = probe_command
        .spawn()
        .map_err(|e| format!("Could not start Windows SDK probe: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(90);
    let status = loop {
        pump();
        runtime.check_input_output_child(&mut probe)?;
        if let Some(status) = probe.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if Instant::now() > deadline {
            let _ = probe.kill();
            let _ = probe.wait();
            return Err("Windows SDK probe timed out; see compat-sdk.stderr.log".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let output = std::fs::read_to_string(runtime.state.join("compat-sdk.stdout.log"))
        .map_err(|e| e.to_string())?;
    let proof: serde_json::Value = output
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["event"] == "compat_sdk_proof")
        .unwrap_or_default();
    let verified = status.success() && valid_compat_proof(&proof, steamid);
    event("compat_sdk_verified", verified, proof.clone());
    std::fs::write(
        runtime.state.join("compat-sdk-proof.json"),
        serde_json::to_vec_pretty(&proof).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if !verified {
        let reason = proof["reason"]
            .as_str()
            .unwrap_or("no_valid_identity_proof");
        return Err(format!(
            "Проверка Windows Steam API: {reason} ({status}); журнал: {}",
            runtime.state.display()
        ));
    }
    let settle = Instant::now() + Duration::from_secs(15);
    while unsafe { running(client_user, &gameid) } {
        pump();
        runtime.check_input_output()?;
        if Instant::now() > settle {
            return Err("Windows SDK probe did not release game session".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if production_input_mode {
        let has_device = runtime
            .game_input
            .borrow()
            .as_ref()
            .is_some_and(|scope| scope.has_device());
        if has_device {
            attach_product_wine_input_output(runtime, &plan, &pump)?;
        }
        let mut scope = runtime.game_input.borrow_mut();
        unsafe {
            scope.as_mut().ok_or("Game input scope missing")?.activate(
                input_runtime,
                runtime.state,
                appid,
            )?;
        }
        runtime.check_input_output()?;
    }
    if request["input_diagnostic"] == true {
        if !product_diagnostic {
            attach_wine_input_output(runtime, &plan, &pump)?;
        }
        let mut input_focus = if product_diagnostic {
            None
        } else {
            unsafe { crate::input_focus::attach(runtime.engine, pipe, runtime.image_base, appid)? }
        };
        let config_proof = if !product_diagnostic
            && std::env::var("SKINOS_INPUT_CONFIG").ok().as_deref() == Some("1")
        {
            let joystick = runtime
                .fixture_joystick_id
                .ok_or("Stock layout diagnostic requires the owned SDL fixture")?;
            let log = runtime
                .fixture_log
                .ok_or("Fixture log cursor is unavailable")?;
            let deadline = Instant::now() + Duration::from_secs(5);
            let index = loop {
                pump();
                runtime.check_input_output()?;
                let snapshot = unsafe {
                    crate::input_readiness::read(runtime.engine, pipe, runtime.image_base)?
                };
                let fresh_log = log.controller_index(joystick)? == Some(0);
                if let Some(index) = snapshot.verified_fixture_index(fresh_log) {
                    event(
                        "steam_input_readiness",
                        true,
                        json!({"connected_controllers":snapshot.connected_controllers,"controller_zero_gamepad_index":snapshot.controller_zero_gamepad_index,"fresh_fixture_log_verified":fresh_log}),
                    );
                    break index;
                }
                if Instant::now() > deadline {
                    event(
                        "steam_input_readiness",
                        false,
                        json!({"connected_controllers":snapshot.connected_controllers,"controller_zero_gamepad_index":snapshot.controller_zero_gamepad_index,"fresh_fixture_log_verified":fresh_log}),
                    );
                    return Err(
                        "Owned Steam controller fixture is not ready for stock configuration"
                            .into(),
                    );
                }
                std::thread::sleep(Duration::from_millis(25));
            };
            unsafe {
                crate::input_config::attach(
                    runtime.engine,
                    pipe,
                    runtime.image_base,
                    runtime.state,
                    appid,
                    Some(index),
                )?
            }
        } else {
            None
        };
        runtime.check_input_output()?;
        observe_input_output_library(runtime.state, "after_configuration")?;
        let native_input = if product_diagnostic {
            None
        } else {
            native_input_diagnostic(runtime, appid, steamid, &pump)?
        };
        if native_input.is_some() {
            let deadline = Instant::now() + Duration::from_secs(10);
            while unsafe { running(client_user, &gameid) } {
                pump();
                runtime.check_input_output()?;
                if Instant::now() > deadline {
                    return Err("Native input diagnostic did not release its app session".into());
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        let name = if plan
            .sdk_probe
            .file_name()
            .is_some_and(|n| n == "sdk-probe32.exe")
        {
            "input-probe32.exe"
        } else {
            "input-probe.exe"
        };
        let input_probe = plan.sdk_probe.with_file_name(name);
        if !input_probe.is_file() || plan.sdk_args.len() != 3 {
            return Err(
                "Steam Input diagnostic probe is missing or has an invalid launcher".into(),
            );
        }
        let rumble_requested = runtime.rumble_fixture.borrow().is_some();
        if rumble_requested {
            rumble_fixture_session::verify_probe(&input_probe)?;
            let config = if product_diagnostic {
                runtime
                    .game_input
                    .borrow()
                    .as_ref()
                    .ok_or("Product rumble configuration scope missing")?
                    .configuration_proof()
                    .ok_or("Rumble stock configuration missing")?
            } else {
                serde_json::to_value(config_proof.as_ref()).map_err(|error| error.to_string())?
            };
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                pump();
                let focus = if product_diagnostic {
                    runtime
                        .game_input
                        .borrow()
                        .as_ref()
                        .ok_or("Product rumble focus scope missing")?
                        .focus_snapshot()?
                        .ok_or("Product rumble focus not attached")?
                } else {
                    let focus = input_focus
                        .as_ref()
                        .ok_or("Rumble diagnostic requires explicit app focus")?
                        .snapshot()?;
                    json!({"gameid":focus.gameid,"pid":focus.pid})
                };
                let progress = runtime.wine_output.borrow_mut().progress()?;
                let fixture = runtime.fixture.borrow();
                let fixture = fixture
                    .as_ref()
                    .ok_or("Rumble fixture detached before probe")?;
                let ready = unsafe {
                    runtime
                        .rumble_fixture
                        .borrow_mut()
                        .as_mut()
                        .ok_or("Rumble scope missing")?
                        .begin(rumble_fixture_session::Grounding {
                            runtime: input_runtime,
                            state: runtime.state,
                            appid,
                            fixture,
                            config: &config,
                            focus: &focus,
                            output: progress,
                        })?
                };
                if ready {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err("Rumble QA did not establish its exact owned fixture route".into());
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        let mut args = plan.sdk_args.clone();
        args[2] = input_probe.to_string_lossy().into_owned();
        let mut fixture_sequence = if product_diagnostic {
            None
        } else {
            crate::input_fixture_sequence::Sequence::from_environment(
                appid,
                runtime.fixture_joystick_id,
            )?
        };
        live_status(
            appid,
            "starting",
            Some(steamid),
            Some("Диагностика Steam Input через NotProton"),
        );
        let retain_shim = runtime.wine_output.borrow().enabled();
        let probe_name = input_probe
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("Input diagnostic executable name is invalid")?;
        let launched_after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_micros();
        let mut diagnostic_command = command(&args, "steam-input")?;
        diagnostic_command
            .envs(&plan.sdk_env)
            .env_remove("SKINOS_INPUT_RUMBLE_TEST")
            .env_remove("SKINOS_INPUT_RUMBLE_FIXTURE_ID")
            .env_remove("SKINOS_INPUT_FEEDBACK_SINK")
            .env_remove("SKINOS_INPUT_FEEDBACK_TEST")
            .env_remove("SKINOS_INPUT_FEEDBACK")
            // Only this second command may retain the prefix prepared by the
            // genuine identity-verified SDK probe above. The launcher compares
            // its fresh receipt/settings before skipping any reset operations.
            .env("SKINOS_PREFIX_SESSION_READY", "1")
            .env("SKINOS_PREFIX_SESSION_RECEIPT", &prefix_receipt)
            .env("SKINOS_INPUT_DURATION_MS", "7000")
            .env("SKINOS_INPUT_INTERVAL_MS", "100");
        if rumble_requested {
            diagnostic_command
                .env("SKINOS_INPUT_RUMBLE_TEST", "1")
                .env("SKINOS_STEAM_INPUT_FIXTURE", "neutral")
                .env(
                    "SKINOS_INPUT_RUMBLE_FIXTURE_ID",
                    runtime
                        .fixture_joystick_id
                        .ok_or("Rumble fixture identity missing")?
                        .to_string(),
                );
        }
        let mut diagnostic = diagnostic_command
            .spawn()
            .map_err(|e| format!("Could not start Steam Input diagnostic: {e}"))?;
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut tracked_probe: Option<TrackedGameProcess> = None;
        let mut actual_probe_exit = None;
        let mut termination_observation = None;
        let status = loop {
            pump();
            if product_diagnostic {
                let topology = runtime
                    .game_input
                    .borrow_mut()
                    .as_mut()
                    .ok_or("Product diagnostic input scope missing")?
                    .check_topology();
                if let Err(error) = topology {
                    let _ = runtime.wine_output.borrow_mut().record_input_failure(error);
                }
            }
            runtime.check_input_output_child(&mut diagnostic)?;
            if retain_shim && tracked_probe.is_none() {
                match find_compat_game(&plan, probe_name, appid, launched_after) {
                    Ok(Some(process)) => {
                        event(
                            "steam_input_probe_process",
                            true,
                            json!({
                                "pid":process.pid,"start_seconds":process.start.0,
                                "start_microseconds":process.start.1,"prefix_appid_verified":true
                            }),
                        );
                        tracked_probe = Some(process);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        let _ = diagnostic.kill();
                        let _ = diagnostic.wait();
                        return Err(error);
                    }
                }
            }
            if let Some(sequence) = &mut fixture_sequence {
                if let Err(error) = poll_fixture_sequence(
                    runtime,
                    sequence,
                    input_focus.as_ref(),
                    config_proof.as_ref(),
                ) {
                    let _ = diagnostic.kill();
                    let _ = diagnostic.wait();
                    return Err(error);
                }
            }
            let shim_status = diagnostic.try_wait().map_err(|e| e.to_string())?;
            if !retain_shim {
                if let Some(status) = shim_status {
                    break Some(status);
                }
            } else if let Some(process) = &tracked_probe {
                actual_probe_exit = process.monitored_exit().or(actual_probe_exit);
                termination_observation = if actual_probe_exit.is_some() {
                    Some("kernel_zombie")
                } else {
                    process.termination_observation()
                };
                if termination_observation.is_some() {
                    break shim_status;
                }
            }
            if shim_status.is_some_and(|status| !status.success()) {
                break shim_status;
            }
            if Instant::now() > deadline {
                let _ = diagnostic.kill();
                let _ = diagnostic.wait();
                return Err("Steam Input diagnostic timed out".into());
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        if retain_shim {
            let probe = json!({"pid":tracked_probe.as_ref().map(|process| process.pid),
                "start_seconds":tracked_probe.as_ref().map(|process| process.start.0),
                "start_microseconds":tracked_probe.as_ref().map(|process| process.start.1),
                "termination_observation":termination_observation,"exit":actual_probe_exit,
                "prefix_appid_verified":tracked_probe.is_some()});
            runtime
                .wine_output
                .borrow_mut()
                .retain_shim(diagnostic, probe)?;
            if termination_observation.is_none() {
                return Err("Input diagnostic exited without observing its verified probe process termination".into());
            }
        }
        // The child can flush its final release sample and exit between two
        // polls. Inspect that final bounded log before validating the sequence.
        if let Some(sequence) = &mut fixture_sequence {
            poll_fixture_sequence(
                runtime,
                sequence,
                input_focus.as_ref(),
                config_proof.as_ref(),
            )?;
        }
        let output = std::fs::read_to_string(runtime.state.join("steam-input.stdout.log"))
            .map_err(|e| e.to_string())?;
        if output.len() > 2 * 1024 * 1024 {
            return Err("Steam Input diagnostic exceeded output limit".into());
        }
        let rows = output
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .collect::<Vec<_>>();
        let summary = rows
            .iter()
            .find(|row| row["event"] == "steam_input_probe_summary")
            .cloned()
            .unwrap_or_default();
        if status.is_some_and(|status| !status.success())
            || actual_probe_exit.is_some_and(ProcessExit::failed)
            || summary["scan_completed"] != true
            || summary["identity_verified"] != true
            || summary["api_initialized"] != true
            || summary["appid_verified"] != true
            || summary["appid"].as_u64() != Some(u64::from(appid))
        {
            let reason = rows
                .iter()
                .find(|row| row["event"] == "steam_input_probe_error")
                .and_then(|row| row["reason"].as_str())
                .unwrap_or("no_valid_input_report");
            return Err(format!(
                "Steam Input diagnostic failed: {reason} (shim={status:?}, probe={actual_probe_exit:?})"
            ));
        }
        let rumble_proof = if rumble_requested {
            let deadline = Instant::now() + Duration::from_millis(250);
            loop {
                let readback = {
                    let fixture = runtime.fixture.borrow();
                    runtime
                        .rumble_fixture
                        .borrow()
                        .as_ref()
                        .ok_or("Rumble scope missing")?
                        .readback(
                            fixture
                                .as_ref()
                                .ok_or("Rumble fixture detached before readback")?,
                            appid,
                            &rows,
                        )
                };
                match readback {
                    Ok(proof) => break Some(proof),
                    Err(error)
                        if error == "Rumble callback is still executing; retry closed readback"
                            && Instant::now() < deadline =>
                    {
                        pump();
                        runtime.check_input_output()?;
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => return Err(error),
                }
            }
        } else {
            None
        };
        let fixture =
            std::env::var("SKINOS_STEAM_INPUT_FIXTURE").ok().as_deref() == Some("neutral");
        let fixture_activity = fixture_sequence
            .as_ref()
            .map(|sequence| sequence.finish())
            .transpose();
        if fixture_activity.is_err() {
            if let Some(fixture) = runtime.fixture.borrow_mut().as_mut() {
                let _ = fixture.reset_gamepad_test_state();
            }
        }
        let fixture_activity = fixture_activity?;
        let focus_snapshot = if product_diagnostic {
            runtime
                .game_input
                .borrow()
                .as_ref()
                .ok_or("Product diagnostic input scope missing")?
                .focus_snapshot()?
        } else {
            input_focus
                .as_ref()
                .map(|guard| guard.snapshot())
                .transpose()?
                .map(|snapshot| json!({"gameid":snapshot.gameid,"pid":snapshot.pid}))
        };
        observe_input_output_library(runtime.state, "after_windows_scan")?;
        runtime.check_input_output()?;
        if let Some(guard) = &mut input_focus {
            guard.restore()?;
        }
        let config_proof = if product_diagnostic {
            runtime
                .game_input
                .borrow()
                .as_ref()
                .ok_or("Product diagnostic input scope missing")?
                .configuration_proof()
                .unwrap_or(serde_json::Value::Null)
        } else {
            serde_json::to_value(config_proof).map_err(|error| error.to_string())?
        };
        let mut payload = json!({"appid":appid,"steamid":steamid.to_string(),"fixture":fixture,"summary":summary,"samples":rows,"native_input":native_input,"focus_snapshot":focus_snapshot,"config_proof":config_proof,"fixture_activity":fixture_activity,"product_diagnostic":product_diagnostic});
        if let Some(proof) = rumble_proof {
            payload["rumble_fixture"] = proof;
        }
        if rumble_requested {
            if !retain_shim {
                return Err("Rumble QA requires strict owned output and SDK teardown".into());
            }
            runtime
                .rumble_fixture
                .borrow_mut()
                .as_mut()
                .ok_or("Rumble scope missing")?
                .defer(payload)?;
        } else if retain_shim {
            runtime.wine_output.borrow_mut().defer_diagnostic(payload)?;
        } else {
            publish_input_diagnostic(runtime.state, payload)?;
        }
        return Ok(());
    }
    if let Some(scope) = runtime.rumble_feedback.borrow_mut().as_mut() {
        scope.configure(appid);
        if !scope.is_qa()
            && !runtime
                .game_input
                .borrow()
                .as_ref()
                .is_some_and(|input| input.has_device())
        {
            scope.inactive_no_source();
        }
    }
    if runtime
        .rumble_feedback
        .borrow()
        .as_ref()
        .is_some_and(|scope| scope.needs_start())
    {
        let start_feedback = (|| -> Result<(), String> {
            if plan.windows_sdk.is_none() {
                return Err("Feedback requires this game's genuine Windows SDK".into());
            }
            let root = plan
                .sdk_probe
                .parent()
                .ok_or("Feedback SDK payload root missing")?
                .join("input-rumble-sink-experiment");
            let name = if plan
                .sdk_probe
                .file_name()
                .is_some_and(|name| name == "sdk-probe32.exe")
            {
                "input-probe32.exe"
            } else {
                "input-probe.exe"
            };
            let probe = rumble_sink::VerifiedProbe::from_experiment(&root, name)?;
            let mut args = plan.sdk_args.clone();
            if args.len() != 3 {
                return Err("Feedback SDK launcher shape changed".into());
            }
            args[2] = probe.path().to_string_lossy().into_owned();
            let mut sink_command = command(&args, "steam-input-feedback-sdk")?;
            sink_command
                .envs(&plan.sdk_env)
                .env("SKINOS_PREFIX_SESSION_READY", "1")
                .env("SKINOS_PREFIX_SESSION_RECEIPT", &prefix_receipt);
            let epoch = runtime
                .rumble_feedback
                .borrow()
                .as_ref()
                .expect("scope present")
                .epoch;
            let tracker_plan = plan.clone();
            let tracker_name = name.to_owned();
            let binding_deadline = Instant::now() + Duration::from_secs(5);
            loop {
                pump();
                if let Err(error) = poll_game_input_topology(runtime, input_runtime, appid) {
                    let _ = runtime
                        .wine_output
                        .borrow_mut()
                        .record_input_failure(error.clone());
                    return Err(error);
                }
                let route = collect_rumble_feedback_route(runtime, input_runtime, appid)?;
                if route.is_some()
                    && runtime
                        .rumble_feedback
                        .borrow()
                        .as_ref()
                        .expect("scope present")
                        .has_driver_binding()
                {
                    break;
                }
                if Instant::now() >= binding_deadline {
                    return Err("Feedback driver binding or parent route was not verified".into());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let launched_after = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_micros();
            let sink = rumble_sink::Session::start(
                sink_command,
                &probe,
                rumble_sink::Context::new(appid, steamid, epoch)?,
                move |_| {
                    Ok(Box::new(rumble_feedback_session::Tracker::new(
                        tracker_plan,
                        appid,
                        tracker_name,
                        launched_after,
                    )))
                },
            )?;
            runtime
                .rumble_feedback
                .borrow_mut()
                .as_mut()
                .expect("scope present")
                .start(sink)?;
            let deadline = Instant::now() + Duration::from_secs(12);
            loop {
                pump();
                if poll_rumble_feedback(runtime, input_runtime, appid)? {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err("Feedback SDK sink readiness timed out".into());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(())
        })();
        if let Err(error) = start_feedback {
            fail_rumble_feedback(runtime, error, &pump);
        }
    }
    live_status(
        appid,
        "starting",
        Some(steamid),
        Some("Запуск через NotProton"),
    );
    let launched_after = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_micros();
    let mut game_command = command(&plan.args, "compat-game")?;
    if game_input_mode {
        game_command
            .env("SKINOS_PREFIX_SESSION_READY", "1")
            .env("SKINOS_PREFIX_SESSION_RECEIPT", &prefix_receipt);
    }
    let mut child = game_command.spawn().map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(90);
    let expected = Path::new(request["executable"].as_str().ok_or("Missing executable")?)
        .file_name()
        .ok_or("Missing filename")?
        .to_string_lossy()
        .to_ascii_lowercase();
    let mut seen = false;
    let mut stop_attempt = None;
    let mut tracked: Option<TrackedGameProcess> = None;
    let mut diagnostics = WineDiagnostics::default();
    let mut monitored_exit = None;
    let mut game_termination = None;
    let mut game_termination_reported = false;
    let mut terminal_since = None;
    let mut input_degradation_reported = false;
    let mut persona = runtime.controls.persona.load(Ordering::SeqCst);
    apply_presence(runtime, persona, true)?;
    loop {
        refresh_presence(runtime, &mut persona)?;
        // Diagnose before processing Stop: a user pressing Stop after a real
        // fault must not turn the crash into a successful requested shutdown.
        if let Some(fatal) = diagnostics.poll(&runtime.state.join("compat-game.stderr.log"))? {
            event(
                "game_crash_detected",
                false,
                json!({"appid":appid,"reason":fatal.message(),"running_was_verified":seen}),
            );
            live_status(appid, "failed", Some(steamid), Some(fatal.message()));
            if tracked.is_none() {
                tracked = find_compat_game(&plan, &expected, appid, launched_after)
                    .ok()
                    .flatten();
            }
            let launcher_exit = child
                .try_wait()
                .map_err(|error| error.to_string())?
                .map(ProcessExit::from_status);
            monitored_exit = tracked
                .as_ref()
                .and_then(TrackedGameProcess::monitored_exit)
                .or(monitored_exit);
            cleanup_failed_compatibility(
                appid,
                steamid,
                tracked.as_ref(),
                &plan,
                &mut child,
                &pump,
            );
            return compat_failure(
                appid,
                steamid,
                fatal.message(),
                seen,
                stop_attempt.is_some(),
                launcher_exit,
                monitored_exit,
            );
        }
        if let Some(process) = tracked.as_ref() {
            monitored_exit = process.monitored_exit().or(monitored_exit);
            if game_input_mode && game_termination.is_none() {
                game_termination = if monitored_exit.is_some() {
                    Some("kernel_zombie")
                } else {
                    process.termination_observation()
                };
            }
        }
        if seen
            && runtime.controls.stop.load(Ordering::SeqCst)
            && let Some(process) = tracked.as_ref()
        {
            terminate_compat_game(appid, steamid, process, &plan, &mut stop_attempt)?;
        }
        if let Some(sequence) = &mut reconnect_test
            && reconnect_test_error.is_none()
        {
            match sequence.poll(runtime.state, runtime.fixture, seen) {
                Ok(Some(proof)) => event("steam_input_fixture_reconnect", true, proof),
                Ok(None) => {}
                Err(error) => {
                    reconnect_test_error = Some(error.clone());
                    let _ = runtime.wine_output.borrow_mut().record_input_failure(error);
                }
            }
        }
        pump();
        if game_input_mode {
            if !input_degradation_reported
                && let Err(error) = poll_game_input_topology(runtime, input_runtime, appid)
            {
                let _ = runtime.wine_output.borrow_mut().record_input_failure(error);
            }
            if let Err(error) = runtime.check_input_output() {
                if !seen {
                    if tracked.is_none() {
                        tracked = find_compat_game(&plan, &expected, appid, launched_after)
                            .ok()
                            .flatten();
                    }
                    cleanup_failed_compatibility(
                        appid,
                        steamid,
                        tracked.as_ref(),
                        &plan,
                        &mut child,
                        &pump,
                    );
                    return Err(error);
                }
                if !input_degradation_reported {
                    input_degradation_reported = true;
                    let _ = writeln!(
                        std::io::stdout(),
                        "SKINOS_LIVE {}",
                        json!({"event":"steam_input_status",
                        "payload":{"appid":appid,"state":"failed","message":error,"game_continues":true}})
                    );
                    let _ = std::io::stdout().flush();
                }
            }
        }
        let feedback_enabled = runtime
            .rumble_feedback
            .borrow()
            .as_ref()
            .is_some_and(|scope| scope.poll_needed());
        if feedback_enabled && let Err(error) = poll_rumble_feedback(runtime, input_runtime, appid)
        {
            fail_rumble_feedback(runtime, error, &pump);
        }
        let active = unsafe { running(client_user, &gameid) };
        if game_input_mode && game_termination.is_some() && !game_termination_reported {
            game_termination_reported = true;
            event(
                "game_process_terminated",
                true,
                json!({"appid":appid,"pid":tracked.as_ref().map(|process|process.pid),
                    "termination_observation":game_termination,"exit":monitored_exit,
                    "sdk_app_still_active":active,"prefix_appid_verified":true}),
            );
        }
        if active
            && !seen
            && let Some(process) = find_compat_game(&plan, &expected, appid, launched_after)?
        {
            let pid = process.pid;
            tracked = Some(process);
            seen = true;
            if game_input_mode {
                runtime.wine_output.borrow_mut().mark_game_running();
            }
            event(
                "game_running",
                true,
                json!({"appid":appid,"steamid":steamid.to_string(),"pid":pid,"compatibility":"notproton"}),
            );
            live_status(appid, "running", Some(steamid), None);
        }
        let status = child.try_wait().map_err(|e| e.to_string())?;
        let launcher_exit = status.map(ProcessExit::from_status);
        if compatibility_terminal_gate(seen, game_input_mode, active, game_termination.is_some()) {
            // An input feeder or a game service may keep the SDK AppID active
            // after the independently tracked game PID exits. Wait for late
            // fatal diagnostics, not unrelated Wine service shutdown. Legacy
            // launches retain their existing SDK-based terminal condition.
            let ended = terminal_since.get_or_insert_with(Instant::now);
            if ended.elapsed() >= Duration::from_millis(350) {
                let fatal = diagnostics.poll(&runtime.state.join("compat-game.stderr.log"))?;
                if let Err(reason) = classify_compat_exit(
                    fatal,
                    stop_attempt.is_some(),
                    launcher_exit,
                    monitored_exit,
                ) {
                    cleanup_failed_compatibility(
                        appid,
                        steamid,
                        tracked.as_ref(),
                        &plan,
                        &mut child,
                        &pump,
                    );
                    return compat_failure(
                        appid,
                        steamid,
                        &reason,
                        seen,
                        stop_attempt.is_some(),
                        launcher_exit,
                        monitored_exit,
                    );
                }
                event(
                    "game_exit",
                    true,
                    json!({"appid":appid,"requested_stop":stop_attempt.is_some(),"launcher_exit":launcher_exit,"monitored_exit":monitored_exit,
                        "termination_observation":game_termination,"sdk_app_still_active":active}),
                );
                if game_input_mode {
                    let process = tracked
                        .as_ref()
                        .ok_or("Game input launcher has no verified game process")?;
                    let metadata = json!({"pid":process.pid,"start_seconds":process.start.0,"start_microseconds":process.start.1,
                        "termination_observation":game_termination,"exit":monitored_exit,
                        "prefix_appid_verified":true,"requested_stop":stop_attempt.is_some()});
                    runtime.wine_output.borrow_mut().retain_game_shim(
                        child,
                        metadata,
                        stop_attempt.is_some(),
                    )?;
                }
                if let Some(sequence) = &reconnect_test {
                    if let Some(error) = reconnect_test_error.take() {
                        return Err(error);
                    }
                    event("steam_input_fixture_reconnect", true, sequence.finish()?);
                }
                live_status(appid, "exited", Some(steamid), None);
                return Ok(());
            }
        } else {
            terminal_since = None;
        }
        if !seen && status.is_some() && !active {
            return Err(format!(
                "NotProton exited before game initialization; diagnostics: {}",
                runtime.state.display()
            ));
        }
        if !seen && Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "NotProton game launch timed out; diagnostics: {}",
                runtime.state.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(if feedback_enabled {
            20
        } else {
            100
        }));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WineFatal {
    PageFault,
    UnhandledException,
}
impl WineFatal {
    fn message(self) -> &'static str {
        match self {
            Self::PageFault => {
                "Игра аварийно завершилась: Wine сообщил необработанную ошибку доступа к памяти. См. compat-game.stderr.log"
            }
            Self::UnhandledException => {
                "Игра аварийно завершилась: Wine сообщил необработанное исключение. См. compat-game.stderr.log"
            }
        }
    }
}
fn wine_fatal_diagnostic(text: &str) -> Option<WineFatal> {
    for line in text.lines().map(str::trim_start) {
        let fault = line
            .strip_prefix("wine: Unhandled page fault ")
            .or_else(|| line.strip_prefix("Unhandled exception: page fault "));
        if let Some(fault) = fault
            && [
                "on read access to ",
                "on write access to ",
                "on execute access to ",
            ]
            .iter()
            .any(|prefix| fault.starts_with(prefix))
        {
            return Some(WineFatal::PageFault);
        }
        if let Some(exception) = line.strip_prefix("wine: Unhandled exception ") {
            let code = exception.split_whitespace().next().unwrap_or_default();
            let code = code.strip_prefix("0x").unwrap_or(code);
            if !code.is_empty()
                && code.len() <= 16
                && code.bytes().all(|byte| byte.is_ascii_hexdigit())
                && (exception.contains(" in thread ") || exception.contains(" at address "))
            {
                return Some(WineFatal::UnhandledException);
            }
        }
    }
    None
}
#[derive(Default)]
struct WineDiagnostics {
    fingerprint: Option<(u64, Option<std::time::SystemTime>)>,
    fatal: Option<WineFatal>,
}
impl WineDiagnostics {
    fn poll(&mut self, path: &Path) -> Result<Option<WineFatal>, String> {
        if self.fatal.is_some() {
            return Ok(self.fatal);
        }
        use std::io::{Read, Seek, SeekFrom};
        const TAIL_LIMIT: u64 = 128 * 1024;
        let mut file = std::fs::File::open(path)
            .map_err(|error| format!("Cannot read game diagnostics: {error}"))?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        let fingerprint = (metadata.len(), metadata.modified().ok());
        if self.fingerprint == Some(fingerprint) {
            return Ok(None);
        }
        self.fingerprint = Some(fingerprint);
        let offset = metadata.len().saturating_sub(TAIL_LIMIT);
        file.seek(SeekFrom::Start(offset))
            .map_err(|error| error.to_string())?;
        let mut tail = Vec::with_capacity(TAIL_LIMIT as usize);
        file.take(TAIL_LIMIT)
            .read_to_end(&mut tail)
            .map_err(|error| error.to_string())?;
        let start = if offset > 0 {
            tail.iter()
                .position(|b| *b == b'\n')
                .map_or(tail.len(), |n| n + 1)
        } else {
            0
        };
        self.fatal = wine_fatal_diagnostic(&String::from_utf8_lossy(&tail[start..]));
        Ok(self.fatal)
    }
}
#[derive(Clone, Copy, Debug, serde::Serialize, PartialEq, Eq)]
struct ProcessExit {
    code: Option<i32>,
    signal: Option<i32>,
}
impl ProcessExit {
    fn from_status(status: std::process::ExitStatus) -> Self {
        use std::os::unix::process::ExitStatusExt;
        Self {
            code: status.code(),
            signal: status.signal(),
        }
    }
    fn failed(self) -> bool {
        self.signal.is_some() || self.code.is_some_and(|value| value != 0)
    }
    fn expected_after_stop(self) -> bool {
        matches!(self.signal, Some(9 | 15)) || matches!(self.code, Some(137 | 143))
    }
}
/// `verified_game_exit` comes only from the previously scoped process's kernel
/// zombie/ESRCH or PID-starttime replacement proof. Path inspection failure and
/// the launcher's exit cannot authorize ordinary-game input teardown.
fn compatibility_terminal_gate(
    seen: bool,
    game_input_mode: bool,
    sdk_app_active: bool,
    verified_game_exit: bool,
) -> bool {
    seen && if game_input_mode {
        verified_game_exit
    } else {
        !sdk_app_active
    }
}

fn classify_compat_exit(
    fatal: Option<WineFatal>,
    stop_signal_sent: bool,
    launcher: Option<ProcessExit>,
    game: Option<ProcessExit>,
) -> Result<(), String> {
    if let Some(fatal) = fatal {
        return Err(fatal.message().into());
    }
    if let Some(exit) =
        game.filter(|exit| exit.failed() && !(stop_signal_sent && exit.expected_after_stop()))
    {
        return Err(format!(
            "Игра завершилась с ошибкой (код {:?}, сигнал {:?}). См. compat-game.stderr.log",
            exit.code, exit.signal
        ));
    }
    if let Some(exit) =
        launcher.filter(|exit| exit.failed() && !(stop_signal_sent && exit.expected_after_stop()))
    {
        return Err(format!(
            "NotProton завершился с ошибкой (код {:?}, сигнал {:?}). См. compat-game.stderr.log",
            exit.code, exit.signal
        ));
    }
    Ok(())
}
fn compat_failure(
    appid: u32,
    steamid: u64,
    reason: &str,
    seen: bool,
    stop_sent: bool,
    launcher: Option<ProcessExit>,
    game: Option<ProcessExit>,
) -> Result<(), String> {
    event(
        "game_failed",
        false,
        json!({"appid":appid,"reason":reason,"running_was_verified":seen,"stop_signal_sent":stop_sent,"launcher_exit":launcher,"monitored_exit":game}),
    );
    live_status(appid, "failed", Some(steamid), Some(reason));
    Err(reason.to_owned())
}

fn cleanup_failed_compatibility(
    appid: u32,
    steamid: u64,
    process: Option<&TrackedGameProcess>,
    plan: &skinos_compat::LaunchPlan,
    child: &mut std::process::Child,
    pump: &impl Fn(),
) {
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut attempt = None;
    let mut cleanup_error = None;
    while let Some(process) = process {
        if !process.is_current() {
            break;
        }
        match terminate_scoped_compat_game(
            appid,
            steamid,
            process,
            plan,
            &mut attempt,
            TerminationCause::CrashCleanup,
        ) {
            Ok(true) => {}
            Ok(false) => {
                cleanup_error =
                    Some("Game identity no longer matches; refusing cleanup signal".to_owned());
                break;
            }
            Err(error) => {
                cleanup_error = Some(error);
                break;
            }
        }
        pump();
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // The direct launch shim is ours; Wine helpers may intentionally survive.
    // Never kill a process group or rescan to select another game's PID.
    let mut launcher_exited = child.try_wait().ok().flatten().is_some();
    if !launcher_exited {
        let _ = child.kill();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            launcher_exited = child.try_wait().ok().flatten().is_some();
            if launcher_exited {
                break;
            }
            pump();
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    let game_alive = process.is_some_and(TrackedGameProcess::is_current);
    event(
        "game_failure_cleanup",
        process.is_some() && !game_alive && launcher_exited && cleanup_error.is_none(),
        json!({"appid":appid,"game_identity_verified":process.is_some(),"game_alive_after":game_alive,"launcher_exited":launcher_exited,"error":cleanup_error}),
    );
}

fn valid_compat_proof(proof: &serde_json::Value, steamid: u64) -> bool {
    let initialized = match proof["proof_kind"].as_str() {
        Some("game_sdk") => proof["sdk_initialized"] == true,
        Some("steamclient_bridge") => proof["bridge_initialized"] == true,
        _ => false,
    };
    initialized
        && proof["ok"] == true
        && proof["logged_on"] == true
        && proof["expected_matches"] == true
        && proof["steamid"].as_str() == Some(&steamid.to_string())
}

#[derive(Debug, PartialEq, Eq)]
enum ControlCommand {
    Stop,
    Presence(u32),
}
fn parse_control(line: &str, appid: u32) -> Option<ControlCommand> {
    if line.len() > 4096 {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    if value["appid"].as_u64() != Some(u64::from(appid)) {
        return None;
    }
    match value["command"].as_str()? {
        "stop" => Some(ControlCommand::Stop),
        "set_presence" => value["state"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| valid_persona(*n))
            .map(ControlCommand::Presence),
        _ => None,
    }
}

fn valid_persona(state: u32) -> bool {
    matches!(state, 0 | 1 | 2 | 3 | 7)
}

fn apply_presence(runtime: &LiveRuntime<'_>, state: u32, logged_on: bool) -> Result<(), String> {
    // OpenSteamworks pinned header orders Get/SetPersonaState at slots 4/5.
    // The runtime SHA pin plus offline layout probe bind these to this Mac ABI.
    unsafe {
        let set: unsafe extern "C" fn(*mut c_void, u32) =
            std::mem::transmute(method(runtime.friends, 5));
        let get: unsafe extern "C" fn(*mut c_void) -> u32 =
            std::mem::transmute(method(runtime.friends, 4));
        set(runtime.friends, state);
        let actual = get(runtime.friends);
        // Offline GetPersonaState is Offline even after queuing Invisible for
        // logon. Confirm the effective value after authentication.
        let valid = actual == state || (!logged_on && actual == 0);
        event(
            "runtime_presence",
            valid,
            json!({"requested":state,"actual":actual,"logged_on":logged_on}),
        );
        if !valid {
            return Err("Runtime could not apply the selected persona state".into());
        }
    }
    Ok(())
}
fn refresh_presence(runtime: &LiveRuntime<'_>, previous: &mut u32) -> Result<(), String> {
    let desired = runtime.controls.persona.load(Ordering::SeqCst);
    if desired != *previous {
        apply_presence(runtime, desired, true)?;
        *previous = desired;
    }
    Ok(())
}

/// Shared by the live stop path and its isolated process regression. Only a
/// verified process from this exact launch can reach the signal helper.
fn terminate_compat_game(
    appid: u32,
    steamid: u64,
    process: &TrackedGameProcess,
    plan: &skinos_compat::LaunchPlan,
    attempt: &mut Option<(i32, Instant, bool)>,
) -> Result<bool, String> {
    terminate_scoped_compat_game(
        appid,
        steamid,
        process,
        plan,
        attempt,
        TerminationCause::UserStop,
    )
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum TerminationCause {
    UserStop,
    CrashCleanup,
}
fn terminate_scoped_compat_game(
    appid: u32,
    steamid: u64,
    process: &TrackedGameProcess,
    plan: &skinos_compat::LaunchPlan,
    attempt: &mut Option<(i32, Instant, bool)>,
    cause: TerminationCause,
) -> Result<bool, String> {
    let Some(current) = TrackedGameProcess::capture(process.pid) else {
        return Ok(false);
    };
    if !process.same_instance(&current)
        || !wrapper_allowed(&current.executable, plan)
        || !process_matches_compatibility(process.pid, plan, appid)
        || !process.is_current()
    {
        return Ok(false);
    }
    signal_selected_game(appid, steamid, process.pid, attempt, cause)?;
    Ok(true)
}

fn terminate_selected_game(
    appid: u32,
    steamid: u64,
    pid: i32,
    attempt: &mut Option<(i32, Instant, bool)>,
) -> Result<(), String> {
    signal_selected_game(appid, steamid, pid, attempt, TerminationCause::UserStop)
}
fn signal_selected_game(
    appid: u32,
    steamid: u64,
    pid: i32,
    attempt: &mut Option<(i32, Instant, bool)>,
    cause: TerminationCause,
) -> Result<(), String> {
    if pid <= 1 || pid == std::process::id() as i32 {
        return Err("Invalid game PID".into());
    }
    match attempt {
        None => {
            if unsafe { kill(pid, 15) } != 0
                && std::io::Error::last_os_error().raw_os_error() != Some(3)
            {
                return Err(format!(
                    "Cannot terminate selected game: {}",
                    std::io::Error::last_os_error()
                ));
            }
            *attempt = Some((pid, Instant::now(), false));
            event(
                if cause == TerminationCause::UserStop {
                    "game_stop_requested"
                } else {
                    "game_failure_cleanup_requested"
                },
                true,
                json!({"appid":appid,"pid":pid}),
            );
            if cause == TerminationCause::UserStop {
                live_status(appid, "stopping", Some(steamid), None);
            }
        }
        Some((selected, started, forced))
            if *selected == pid && !*forced && started.elapsed() > Duration::from_secs(5) =>
        {
            if unsafe { kill(pid, 9) } != 0
                && std::io::Error::last_os_error().raw_os_error() != Some(3)
            {
                return Err(format!(
                    "Cannot finish selected game: {}",
                    std::io::Error::last_os_error()
                ));
            }
            *forced = true;
            event(
                if cause == TerminationCause::UserStop {
                    "game_stop_forced"
                } else {
                    "game_failure_cleanup_forced"
                },
                true,
                json!({"appid":appid,"pid":pid}),
            );
        }
        _ => {}
    }
    Ok(())
}

// Layout from the installed macOS SDK sys/proc_info.h (136 bytes, start fields
// at offsets 120 and 128). Only PID and start time leave this private buffer.
#[repr(C)]
#[derive(Default)]
struct ProcBsdInfo {
    fields: [u32; 12],
    comm: [u8; 16],
    name: [u8; 32],
    tail: [u32; 6],
    start_seconds: u64,
    start_microseconds: u64,
}
fn process_start(pid: i32) -> Option<(u64, u64)> {
    let mut info = ProcBsdInfo::default();
    let size = std::mem::size_of::<ProcBsdInfo>() as i32;
    let actual = unsafe { proc_pidinfo(pid, 3, 0, (&mut info as *mut ProcBsdInfo).cast(), size) };
    if actual != size || info.fields[3] != pid as u32 {
        return None;
    }
    Some((info.start_seconds, info.start_microseconds))
}
fn process_executable(pid: i32) -> Option<std::path::PathBuf> {
    let mut path = [0u8; 4096];
    if unsafe { proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) } <= 0 {
        return None;
    }
    let path = CStr::from_bytes_until_nul(&path).ok()?;
    Some(Path::new(std::ffi::OsStr::from_bytes(path.to_bytes())).to_owned())
}
struct TrackedGameProcess {
    pid: i32,
    start: (u64, u64),
    executable: std::path::PathBuf,
    executable_identity: (u64, u64),
}
fn executable_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    Some((metadata.dev(), metadata.ino()))
}
impl TrackedGameProcess {
    fn capture(pid: i32) -> Option<Self> {
        let start = process_start(pid)?;
        let executable = process_executable(pid)?;
        let executable_identity = executable_identity(&executable)?;
        if process_start(pid)? != start {
            return None;
        }
        Some(Self {
            pid,
            start,
            executable,
            executable_identity,
        })
    }
    fn same_instance(&self, actual: &Self) -> bool {
        // Wine's per-app Mach-O wrappers are hardlinks. proc_pidpath can switch
        // to another link name when a service starts, although PID, start time
        // and the actual executable vnode are unchanged. Compare that vnode.
        actual.pid == self.pid
            && actual.start == self.start
            && actual.executable_identity == self.executable_identity
    }
    fn is_current(&self) -> bool {
        Self::capture(self.pid).is_some_and(|actual| self.same_instance(&actual))
    }
    fn monitored_exit(&self) -> Option<ProcessExit> {
        // A game can be a Wine-server child, so waitpid is not available to us.
        // If we catch its zombie state, record the kernel's wait status while
        // confirming PID/starttime. Once it is reaped, leave this metadata null.
        let mut info = ProcBsdInfo::default();
        let size = std::mem::size_of::<ProcBsdInfo>() as i32;
        let actual =
            unsafe { proc_pidinfo(self.pid, 3, 0, (&mut info as *mut ProcBsdInfo).cast(), size) };
        if actual != size
            || info.fields[3] != self.pid as u32
            || (info.start_seconds, info.start_microseconds) != self.start
            || info.fields[1] != 5
        {
            return None;
        }
        use std::os::unix::process::ExitStatusExt;
        Some(ProcessExit::from_status(
            std::process::ExitStatus::from_raw(info.fields[2] as i32),
        ))
    }
    fn termination_observation(&self) -> Option<&'static str> {
        if self.monitored_exit().is_some() {
            return Some("kernel_zombie");
        }
        if let Some(start) = process_start(self.pid) {
            return (start != self.start).then_some("pid_reused");
        }
        // An unavailable executable path/stat is not an exit. Only ESRCH from
        // the kernel (or a changed start time above) proves the tracked instance
        // is gone when its short zombie interval was not observed.
        let result = unsafe { kill(self.pid, 0) };
        (result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(3))
            .then_some("kernel_esrch")
    }
    fn start_micros(&self) -> u128 {
        u128::from(self.start.0) * 1_000_000 + u128::from(self.start.1)
    }
}
fn wrapper_allowed(path: &Path, plan: &skinos_compat::LaunchPlan) -> bool {
    path.starts_with(&plan.runner_root)
        || plan
            .execute_roots
            .iter()
            .filter(|root| {
                root.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("winetemp-"))
            })
            .any(|root| path.starts_with(root))
}

/// This buffer can contain unrelated private environment values. Never debug,
/// stringify or log it; wipe all bytes even on parsing or sysctl failures.
struct PrivateProcessArgs(Vec<u8>);
impl Drop for PrivateProcessArgs {
    fn drop(&mut self) {
        for byte in &mut self.0 {
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        std::sync::atomic::compiler_fence(Ordering::SeqCst);
    }
}
fn process_arguments(pid: i32) -> Option<PrivateProcessArgs> {
    let mut limit = 0i32;
    let mut size = std::mem::size_of::<i32>();
    let mut argmax = [1i32, 8]; // CTL_KERN, KERN_ARGMAX
    if unsafe {
        sysctl(
            argmax.as_mut_ptr(),
            2,
            (&mut limit as *mut i32).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
        || !(4096..=16_777_216).contains(&limit)
    {
        return None;
    }
    let mut buffer = PrivateProcessArgs(vec![0u8; limit as usize]);
    let mut size = buffer.0.len();
    let mut args = [1i32, 49, pid]; // CTL_KERN, KERN_PROCARGS2, PID
    if unsafe {
        sysctl(
            args.as_mut_ptr(),
            3,
            buffer.0.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
        || size > buffer.0.len()
    {
        return None;
    }
    // Leave allocation length intact so Drop wipes the entire reserved buffer.
    buffer.0[size..].fill(0);
    Some(buffer)
}
fn process_matches_compatibility(pid: i32, plan: &skinos_compat::LaunchPlan, appid: u32) -> bool {
    let Some(buffer) = process_arguments(pid) else {
        return false;
    };
    procargs_match(
        &buffer.0,
        plan.prefix_dir.join("pfx").as_os_str().as_bytes(),
        appid,
    )
}
fn procargs_match(bytes: &[u8], expected_prefix: &[u8], appid: u32) -> bool {
    let Some(raw) = bytes.get(..4).and_then(|v| v.try_into().ok()) else {
        return false;
    };
    let argc = i32::from_ne_bytes(raw);
    if !(1..=65536).contains(&argc) {
        return false;
    }
    let mut offset = 4usize;
    let Some(exec_end) = bytes[offset..].iter().position(|b| *b == 0) else {
        return false;
    };
    if exec_end == 0 {
        return false;
    }
    offset += exec_end + 1;
    // KERN_PROCARGS2 places padding after the executable before argv[0].
    while bytes.get(offset) == Some(&0) {
        offset += 1;
    }
    // Consume exactly argc strings. Values resembling ENV=... in argv are not
    // environment variables, and empty arguments still count toward argc.
    for _ in 0..argc {
        let Some(end) = bytes
            .get(offset..)
            .and_then(|v| v.iter().position(|b| *b == 0))
        else {
            return false;
        };
        offset += end + 1;
    }
    // Wine rewrites its argv in place and clears the unused tail. On macOS
    // KERN_PROCARGS2 keeps that NUL padding between the exact argc strings and
    // the environment (observed with Soundpad: argc=2, empty argv[1], 100 NULs).
    // Only skip it AFTER counting every argv entry, never scan argv for env keys.
    while bytes.get(offset) == Some(&0) {
        offset += 1;
    }
    let expected_appid = appid.to_string();
    let mut prefix_seen = false;
    let mut appid_seen = false;
    while let Some(rest) = bytes.get(offset..) {
        let Some(end) = rest.iter().position(|b| *b == 0) else {
            return false;
        };
        if end == 0 {
            break;
        }
        let entry = &rest[..end];
        if let Some(value) = entry.strip_prefix(b"WINEPREFIX=") {
            if prefix_seen || value != expected_prefix {
                return false;
            }
            prefix_seen = true;
        } else if let Some(value) = entry.strip_prefix(b"SteamAppId=") {
            if appid_seen || value != expected_appid.as_bytes() {
                return false;
            }
            appid_seen = true;
        }
        offset += end + 1;
    }
    prefix_seen && appid_seen
}

fn find_compat_game(
    plan: &skinos_compat::LaunchPlan,
    filename: &str,
    appid: u32,
    launched_after: u128,
) -> Result<Option<TrackedGameProcess>, String> {
    // /bin/ps is setuid on macOS and cannot be executed in this sandbox.
    // Query the kernel directly; require both the actual Windows process name
    // and the selected runner's executable wrapper path.
    let count = unsafe { proc_listallpids(std::ptr::null_mut(), 0) };
    if count <= 0 {
        return Err("Cannot enumerate game processes".into());
    }
    let mut pids = vec![0i32; (count as usize + 128).min(100_000)];
    let filled = unsafe { proc_listallpids(pids.as_mut_ptr().cast(), (pids.len() * 4) as i32) };
    if filled < 0 {
        return Err("Cannot enumerate game processes".into());
    }
    for pid in pids
        .into_iter()
        .take(filled as usize)
        .filter(|pid| *pid > 0)
    {
        let mut name = [0u8; 1024];
        if unsafe { proc_name(pid, name.as_mut_ptr().cast(), name.len() as u32) } <= 0 {
            continue;
        }
        let Ok(name) = CStr::from_bytes_until_nul(&name) else {
            continue;
        };
        if !name.to_string_lossy().eq_ignore_ascii_case(filename) {
            continue;
        }
        let Some(process) = TrackedGameProcess::capture(pid) else {
            continue;
        };
        if process.start_micros() >= launched_after
            && wrapper_allowed(&process.executable, plan)
            && process_matches_compatibility(pid, plan, appid)
            && process.is_current()
        {
            return Ok(Some(process));
        }
    }
    Ok(None)
}

fn native_launcher_identity() -> Result<Option<(i32, i32, u64)>, String> {
    unsafe {
        let pointer = dlsym((-2_isize) as *mut c_void, c"skinos_last_game_pid".as_ptr());
        if pointer.is_null() {
            return Err("Confined native launch adapter is unavailable".into());
        }
        let function: unsafe extern "C" fn() -> i32 = std::mem::transmute(pointer);
        let pid = function();
        if pid <= 0 {
            return Ok(None);
        }
        let group = dlsym(
            (-2_isize) as *mut c_void,
            c"skinos_last_game_group".as_ptr(),
        );
        let started = dlsym(
            (-2_isize) as *mut c_void,
            c"skinos_last_game_started_microseconds".as_ptr(),
        );
        if group.is_null() || started.is_null() {
            return Err("Native launch identity exports are missing".into());
        }
        let group: unsafe extern "C" fn() -> i32 = std::mem::transmute(group);
        let started: unsafe extern "C" fn() -> u64 = std::mem::transmute(started);
        Ok(Some((pid, group(), started())))
    }
}

fn verify_game_process() -> Result<TrackedGameProcess, String> {
    unsafe {
        let root = (-2_isize) as *mut c_void;
        let pid_function = dlsym(root, c"skinos_last_game_pid".as_ptr());
        let path_function = dlsym(root, c"skinos_last_game_executable".as_ptr());
        if pid_function.is_null() || path_function.is_null() {
            return Err("Confined native launch adapter did not publish a process".into());
        }
        let pid_function: unsafe extern "C" fn() -> i32 = std::mem::transmute(pid_function);
        let path_function: unsafe extern "C" fn() -> *const c_char =
            std::mem::transmute(path_function);
        let pid = pid_function();
        let expected = path_function();
        if pid <= 0 || expected.is_null() {
            return Err("No actual game process was launched".into());
        }
        let mut path = [0_u8; 4096];
        if proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) <= 0 {
            return Err("The game process is no longer running".into());
        }
        let actual =
            CStr::from_bytes_until_nul(&path).map_err(|_| "Invalid process executable path")?;
        if actual != CStr::from_ptr(expected) {
            return Err(
                "Tracked PID belongs to a bootstrap helper, not the selected game executable"
                    .into(),
            );
        }
        let process =
            TrackedGameProcess::capture(pid).ok_or("Cannot verify game process start time")?;
        if process.executable.as_os_str().as_bytes() != actual.to_bytes() {
            return Err("Game executable changed during verification".into());
        }
        Ok(process)
    }
}

/// CLOEXEC alone is insufficient when a loaded runtime changes descriptor
/// flags. Explicitly close this descriptor in the forked child immediately
/// before exec. The parent/worker must keep owning it, including its self-exec.
fn isolate_profile_lock(command: &mut std::process::Command, lock_fd: i32) -> Result<(), String> {
    if lock_fd <= 2 || unsafe { fcntl(lock_fd, 1, 0) } < 0 {
        return Err("Profile lock descriptor is not available for child isolation".into());
    }
    command.env_remove("SKINOS_PROFILE_LOCK_FD");
    unsafe {
        command.pre_exec(move || {
            // Only async-signal-safe operations are permitted after fork.
            // Never unlock with flock: that would release the parent's shared
            // open-file-description lock as well. Closing the child copy is enough.
            if close(lock_fd) != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(9) {
                    return Err(error);
                }
            }
            Ok(())
        });
    }
    Ok(())
}

// Optional differential probe using an unmodified native SDK against the same
// owned host and AppID as the Windows consumer. This is diagnostic-only; it does
// not replace the selected game's SDK proof or grant additional sandbox access.
fn publish_input_diagnostic(state: &Path, payload: serde_json::Value) -> Result<(), String> {
    let appid = payload["appid"]
        .as_u64()
        .and_then(|id| u32::try_from(id).ok())
        .ok_or("Input diagnostic AppID missing at publication")?;
    let steamid = payload["steamid"]
        .as_str()
        .and_then(|id| id.parse::<u64>().ok())
        .ok_or("Input diagnostic account identity missing at publication")?;
    std::fs::write(
        state.join("steam-input-proof.json"),
        serde_json::to_vec_pretty(&payload).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    event("steam_input_diagnostics", true, payload.clone());
    let _ = writeln!(
        std::io::stdout(),
        "SKINOS_LIVE {}",
        json!({"event":"steam_input_diagnostics","payload":payload})
    );
    let _ = std::io::stdout().flush();
    live_status(
        appid,
        "exited",
        Some(steamid),
        Some("Диагностика Steam Input завершена"),
    );
    Ok(())
}

fn poll_fixture_sequence(
    runtime: &LiveRuntime<'_>,
    sequence: &mut crate::input_fixture_sequence::Sequence,
    focus: Option<&crate::input_focus::FocusGuard>,
    config: Option<&crate::input_config::ConfigProof>,
) -> Result<(), String> {
    let result = (|| {
        let joystick = runtime
            .fixture_joystick_id
            .ok_or("Active fixture identity missing")?;
        let fresh_index = runtime
            .fixture_log
            .ok_or("Active fixture fresh log missing")?
            .controller_index(joystick)?;
        let config = config.ok_or("Active fixture stock configuration proof missing")?;
        if fresh_index != Some(config.controller_index)
            || config.controller_index != 0
            || !config.controller_index_verified_by_caller
        {
            return Err("Active fixture lost its fresh Valve controller-index identity".into());
        }
        let focus = focus
            .ok_or("Active fixture focus guard missing")?
            .snapshot()?;
        let mut fixture = runtime.fixture.borrow_mut();
        let fixture = fixture
            .as_mut()
            .ok_or("Owned SDL fixture has already detached")?;
        sequence.poll(
            &runtime.state.join("steam-input.stdout.log"),
            focus.gameid,
            config.calls_dispatched,
            fixture,
        )
    })();
    if result.is_err() {
        if let Some(fixture) = runtime.fixture.borrow_mut().as_mut() {
            let _ = fixture.reset_gamepad_test_state();
        }
    }
    result
}

fn attach_wine_input_output(
    runtime: &LiveRuntime<'_>,
    plan: &skinos_compat::LaunchPlan,
    pump: &impl Fn(),
) -> Result<(), String> {
    if !runtime.wine_output.borrow().enabled() {
        return Ok(());
    }
    let home = std::env::var_os("HOME").ok_or("Home unavailable for owned Wine helper context")?;
    let context = skinos_compat::direct_wine::Context::from_plan(plan, Path::new(&home))?;
    let feeder = wine_input_session::bundled_feeder(plan, context.driver_arch.label())?;
    let mut command = context.feeder_command(&feeder)?;
    command.env_remove("SKINOS_INPUT_OUTPUT_FD");
    isolate_profile_lock(&mut command, runtime.profile_lock_fd)?;
    // --feed only opens the existing driver. Relay::start requires its actual
    // root_present/root_started IOCTL and readiness; it never installs one.
    runtime.wine_output.borrow_mut().attach(command, pump)
}

fn attach_product_wine_input_output(
    runtime: &LiveRuntime<'_>,
    plan: &skinos_compat::LaunchPlan,
    pump: &impl Fn(),
) -> Result<(), String> {
    if !runtime.wine_output.borrow().uses_production_preparation() {
        return Err("Production feeder requested outside production input mode".into());
    }
    let home = std::env::var_os("HOME").ok_or("Home unavailable for owned Wine helper context")?;
    let context = skinos_compat::direct_wine::Context::from_plan(plan, Path::new(&home))?;
    let mut command = if runtime.rumble_feedback.borrow().is_some() {
        rumble_feedback_session::experimental_feeder(plan, &context)?
    } else {
        let (feeder, inf) = wine_input_session::bundled_driver(plan, context.driver_arch.label())?;
        context.ensure_feeder_command(&feeder, &inf)?
    };
    command.env_remove("SKINOS_INPUT_OUTPUT_FD");
    isolate_profile_lock(&mut command, runtime.profile_lock_fd)?;
    runtime.wine_output.borrow_mut().attach(command, pump)
}

fn poll_game_input_topology(
    runtime: &LiveRuntime<'_>,
    input: game_input_session::Runtime,
    appid: u32,
) -> Result<(), String> {
    let progress = runtime.wine_output.borrow_mut().progress()?;
    let payload = unsafe {
        runtime
            .game_input
            .borrow_mut()
            .as_mut()
            .ok_or("Game input scope missing")?
            .poll_game(input, runtime.state, appid, progress)?
    };
    if let Some(payload) = payload {
        event("game_input_connection", true, payload.clone());
        let _ = writeln!(
            std::io::stdout(),
            "SKINOS_LIVE {}",
            json!({"event":"steam_input_status","payload":payload})
        );
        let _ = std::io::stdout().flush();
    }
    Ok(())
}

fn collect_rumble_feedback_route(
    runtime: &LiveRuntime<'_>,
    input: game_input_session::Runtime,
    appid: u32,
) -> Result<Option<serde_json::Value>, String> {
    let progress = runtime.wine_output.borrow_mut().progress()?;
    let is_qa = runtime
        .rumble_feedback
        .borrow()
        .as_ref()
        .ok_or("Feedback scope missing")?
        .is_qa();
    let route = unsafe {
        let source = runtime.game_input.borrow();
        let source = source.as_ref().ok_or("Feedback source session missing")?;
        if is_qa {
            let fixture = runtime.fixture.borrow();
            let fixture = fixture.as_ref().ok_or("Feedback fixture missing")?;
            source.fixture_feedback_route(input, appid, fixture.joystick_id, progress)?
        } else {
            source
                .source_route(input, appid, progress)?
                .map(|route| route.proof())
        }
    };
    let mut feedback = runtime.rumble_feedback.borrow_mut();
    let feedback = feedback.as_mut().ok_or("Feedback scope missing")?;
    if is_qa && route.is_none() && feedback.ready() {
        return Err("Feedback parent output route was lost".into());
    }
    if let Some(batch) = runtime.wine_output.borrow().try_take_feedback(32) {
        feedback.accept(batch)?;
    }
    Ok(route)
}
fn poll_rumble_feedback(
    runtime: &LiveRuntime<'_>,
    input: game_input_session::Runtime,
    appid: u32,
) -> Result<bool, String> {
    let route = collect_rumble_feedback_route(runtime, input, appid)?;
    let fixture = runtime.fixture.borrow();
    let mut feedback = runtime.rumble_feedback.borrow_mut();
    let feedback = feedback.as_mut().ok_or("Feedback scope missing")?;
    feedback.poll(route, fixture.as_ref())?;
    Ok(feedback.ready())
}

fn fail_rumble_feedback(runtime: &LiveRuntime<'_>, error: String, pump: &impl Fn()) {
    runtime.wine_output.borrow_mut().disable_feedback(&error);
    if let Some(scope) = runtime.rumble_feedback.borrow_mut().as_mut() {
        scope.fail(error);
        scope.finish_source(runtime.fixture.borrow().as_ref(), pump);
    }
}

fn observe_input_output_library(state: &Path, stage: &str) -> Result<(), String> {
    if std::env::var("SKINOS_INPUT_TRACE").ok().as_deref() != Some("1") {
        return Ok(());
    }
    let path = CString::new(
        state
            .join("runtime/libsteaminput.dylib")
            .as_os_str()
            .as_bytes(),
    )
    .map_err(|_| "Invalid input output library path")?;
    let handle = unsafe { dlopen(path.as_ptr(), 2 | 16) }; // RTLD_NOW | RTLD_NOLOAD
    let loaded = !handle.is_null();
    if loaded {
        unsafe { dlclose(handle) };
    }
    event(
        "steam_input_output_library",
        true,
        json!({"stage":stage,"loaded":loaded,"observation_only":true}),
    );
    Ok(())
}

fn native_input_diagnostic(
    runtime: &LiveRuntime<'_>,
    appid: u32,
    steamid: u64,
    pump: &dyn Fn(),
) -> Result<Option<serde_json::Value>, String> {
    use std::process::{Command, Stdio};
    let Some(sdk) = std::env::var_os("SKINOS_INPUT_NATIVE_SDK") else {
        return Ok(None);
    };
    let sdk =
        std::fs::canonicalize(sdk).map_err(|_| "Cannot resolve native input diagnostic SDK")?;
    if !sdk.is_file() || sdk.file_name() != Some(std::ffi::OsStr::new("libsteam_api.dylib")) {
        return Err("Native input diagnostic requires an original libsteam_api.dylib".into());
    }
    let report_path = runtime.state.join("native-input-proof.json");
    let mut command = Command::new(runtime.state.join("skinos-sdk-probe"));
    command
        .env("SKINOS_COMPONENT_ROLE", "sdk")
        .env_remove("SKINOS_INPUT_OUTPUT_FD")
        .arg("--library")
        .arg(sdk)
        .arg("--appid")
        .arg(appid.to_string())
        .arg("--expected-steamid")
        .arg(steamid.to_string())
        .arg("--client-library")
        .arg(runtime.state.join("runtime/steamclient.dylib"))
        .arg("--input-check")
        .current_dir(runtime.state.join("runtime"))
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(&report_path).map_err(|e| e.to_string())?)
        .stderr(
            std::fs::File::create(runtime.state.join("native-input.stderr.log"))
                .map_err(|e| e.to_string())?,
        );
    isolate_profile_lock(&mut command, runtime.profile_lock_fd)?;
    let mut child = command
        .spawn()
        .map_err(|e| format!("Cannot start native input diagnostic: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        pump();
        runtime.check_input_output_child(&mut child)?;
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Native Steam Input diagnostic timed out".into());
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    if std::fs::metadata(&report_path)
        .map_err(|e| e.to_string())?
        .len()
        > 2 * 1024 * 1024
    {
        return Err("Native Steam Input report exceeded output limit".into());
    }
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&report_path).map_err(|e| e.to_string())?)
            .map_err(|_| "Invalid native Steam Input report")?;
    let input = report["input"].clone();
    let verified = status.success()
        && report["sdk_initialized"] == true
        && report["logged_on"] == true
        && report["expected_matches"] == true
        && report["client_library_matches"] == true
        && input["api_initialized"] == true
        && input["appid_verified"] == true
        && input["appid"].as_u64() == Some(u64::from(appid))
        && input["scan_completed"] == true;
    event("native_steam_input_diagnostic", verified, input.clone());
    if !verified {
        return Err("Native Steam Input diagnostic failed; inspect its local report".into());
    }
    Ok(Some(input))
}

fn verify_sdk(
    request: &serde_json::Value,
    runtime: &LiveRuntime<'_>,
    appid: u32,
    steamid: u64,
) -> Result<(), String> {
    use std::process::{Command, Stdio};
    let sdk = match std::env::var_os("SKINOS_NATIVE_SDK_PATH") {
        Some(path) => std::path::PathBuf::from(path),
        None => {
            event(
                "sdk_probe_skipped",
                true,
                json!({"reason":std::env::var("SKINOS_NATIVE_SDK_PROBE_REASON").unwrap_or_else(|_| "missing".into())}),
            );
            return Ok(());
        }
    };
    let install = std::fs::canonicalize(
        request["install_dir"]
            .as_str()
            .ok_or("Installation missing")?,
    )
    .map_err(|error| error.to_string())?;
    let sdk = sdk.canonicalize().map_err(|error| error.to_string())?;
    if !sdk.starts_with(install)
        || sdk
            .file_name()
            .is_none_or(|name| name != "libsteam_api.dylib")
    {
        return Err("Steamworks library escapes selected installation".into());
    }
    if std::env::var("SKINOS_NATIVE_SDK_PROBE_SUPPORTED").as_deref() != Ok("1") {
        event(
            "sdk_probe_skipped",
            true,
            json!({"reason":"sdk_architecture_not_supported_by_arm64_probe"}),
        );
        return Ok(());
    }
    let stderr = std::fs::File::create(runtime.state.join("sdk-probe.stderr.log"))
        .map_err(|e| e.to_string())?;
    let mut command = Command::new(runtime.state.join("skinos-sdk-probe"));
    command
        .env("SKINOS_COMPONENT_ROLE", "sdk")
        .arg("--library")
        .arg(sdk)
        .arg("--appid")
        .arg(appid.to_string())
        .arg("--expected-steamid")
        .arg(steamid.to_string())
        .arg("--client-library")
        .arg(runtime.state.join("runtime/steamclient.dylib"))
        .current_dir(runtime.state.join("runtime"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(stderr);
    isolate_profile_lock(&mut command, runtime.profile_lock_fd)?;
    let mut child = command
        .spawn()
        .map_err(|e| format!("Cannot start Steamworks SDK probe: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if child.try_wait().map_err(|e| e.to_string())?.is_some() {
            break;
        }
        // Keep the genuine engine serviced while the independent SDK performs
        // its IPC handshake. Do not consume/drop launch-result callbacks here.
        unsafe {
            let frame: unsafe extern "C" fn(*mut c_void) =
                std::mem::transmute(method(runtime.engine, 19));
            frame(runtime.engine);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Steamworks SDK initialization timed out".into());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|_| "Invalid Steamworks SDK report")?;
    let ok = output.status.success()
        && report["sdk_initialized"] == true
        && report["logged_on"] == true
        && report["expected_matches"] == true
        && report["client_library_matches"] == true;
    event("sdk_verified", ok, report.clone());
    std::fs::write(
        runtime.state.join("sdk-proof.json"),
        serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if !ok {
        live_status(
            appid,
            "failed",
            Some(steamid),
            Some("Game Steamworks SDK could not authenticate with the hosted runtime"),
        );
        return Err("Game Steamworks SDK check failed".into());
    }
    Ok(())
}

#[cfg(test)]
mod controls_and_process_tests {
    use super::*;
    fn args(argv: &[&str], env: &[&str]) -> Vec<u8> {
        let mut data = (argv.len() as i32).to_ne_bytes().to_vec();
        data.extend_from_slice(b"/runner/wine64\0\0\0");
        for value in argv.iter().chain(env.iter()) {
            data.extend_from_slice(value.as_bytes());
            data.push(0);
        }
        data.push(0);
        data
    }
    #[test]
    fn process_scope_requires_actual_environment_and_exact_prefix() {
        let prefix = b"/skin/compat/1/pfx";
        let valid = args(
            &["soundpad.exe", ""],
            &[
                "WINEPREFIX=/skin/compat/1/pfx",
                "SteamAppId=42",
                "OTHER=value",
            ],
        );
        assert!(procargs_match(&valid, prefix, 42));
        let fake = args(
            &[
                "soundpad.exe",
                "WINEPREFIX=/skin/compat/1/pfx",
                "SteamAppId=42",
            ],
            &["WINEPREFIX=/skin/compat/other/pfx", "SteamAppId=42"],
        );
        assert!(!procargs_match(&fake, prefix, 42));
        assert!(!procargs_match(
            &args(
                &["soundpad.exe"],
                &["WINEPREFIX=/skin/compat/1/pfx/other", "SteamAppId=42"]
            ),
            prefix,
            42
        ));
        assert!(!procargs_match(
            &args(
                &["soundpad.exe"],
                &["WINEPREFIX=/skin/compat/1/pfx", "SteamAppId=420"]
            ),
            prefix,
            42
        ));
        assert!(!procargs_match(
            &args(
                &["soundpad.exe"],
                &[
                    "WINEPREFIX=/skin/compat/1/pfx",
                    "WINEPREFIX=/skin/compat/other/pfx",
                    "SteamAppId=42"
                ]
            ),
            prefix,
            42
        ));
        assert!(!procargs_match(&valid[..valid.len() - 5], prefix, 42));
        assert!(!procargs_match(&[1, 2, 3], prefix, 42));
    }
    #[test]
    fn wine_fatal_signatures_exclude_benign_errors_and_client_assertions() {
        assert_eq!(
            wine_fatal_diagnostic(
                "wine: Unhandled page fault on read access to 00000000 at address 0040107F (thread 00ab), starting debugger..."
            ),
            Some(WineFatal::PageFault)
        );
        assert_eq!(
            wine_fatal_diagnostic(
                "Unhandled exception: page fault on write access to 0x00000000 in 32-bit code (0x0040107f)."
            ),
            Some(WineFatal::PageFault)
        );
        assert_eq!(
            wine_fatal_diagnostic(
                "wine: Unhandled exception 0xc0000005 in thread 00bc at address 0x0040107f"
            ),
            Some(WineFatal::UnhandledException)
        );
        for benign in [
            "0084:err:module:import_dll Library example.dll not found",
            "libSDL2: Assertion Failed: SDL_GetDisplayBounds",
            "src/common/steamclient.cpp Assertion Failed",
            "wine: configuration in '/prefix' has been updated.",
            "wineserver: shutting down",
            "00a8:warn:seh:dispatch_exception warning",
            "application quoted wine: Unhandled page fault on read access to 00000000",
            "wine: Unhandled exception message that is not a Wine exception report",
        ] {
            assert_eq!(
                wine_fatal_diagnostic(benign),
                None,
                "benign diagnostic was promoted to a fatal crash"
            );
        }
    }
    #[test]
    fn manual_stop_does_not_hide_a_crash_that_races_with_shutdown() {
        let zero = ProcessExit {
            code: Some(0),
            signal: None,
        };
        let term = ProcessExit {
            code: None,
            signal: Some(15),
        };
        let shell_term = ProcessExit {
            code: Some(143),
            signal: None,
        };
        let segfault = ProcessExit {
            code: None,
            signal: Some(11),
        };
        assert!(classify_compat_exit(None, false, Some(zero), Some(zero)).is_ok());
        assert!(classify_compat_exit(None, true, Some(shell_term), Some(term)).is_ok());
        assert!(classify_compat_exit(None, false, Some(shell_term), Some(term)).is_err());
        assert!(
            classify_compat_exit(Some(WineFatal::PageFault), true, Some(zero), Some(term)).is_err()
        );
        assert!(classify_compat_exit(None, true, None, Some(segfault)).is_err());
        assert!(
            classify_compat_exit(
                None,
                false,
                Some(ProcessExit {
                    code: Some(5),
                    signal: None
                }),
                None
            )
            .is_err()
        );
        // A missing wait status stays unknown; it is never fabricated as code 0.
        assert_eq!(
            serde_json::to_value(Option::<ProcessExit>::None).unwrap(),
            serde_json::Value::Null
        );
    }
    #[test]
    fn bounded_game_log_detects_late_fatal_and_retains_it_across_shutdown() {
        use std::io::Write;
        struct FileFixture(std::path::PathBuf);
        impl Drop for FileFixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let file = FileFixture(std::env::temp_dir().join(format!(
                "skinos-wine-fatal-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )));
        let mut content =
            b"wine: Unhandled page fault on read access to 00000000 at address 0040107F\n".to_vec();
        content.extend_from_slice(&vec![b'x'; 256 * 1024]);
        content.extend_from_slice(b"\n00ab:err:ignored benign warning\n");
        std::fs::write(&file.0, &content).unwrap();
        let mut diagnostics = WineDiagnostics::default();
        assert_eq!(
            diagnostics.poll(&file.0).unwrap(),
            None,
            "only the bounded tail should be inspected"
        );
        let mut append = std::fs::OpenOptions::new()
            .append(true)
            .open(&file.0)
            .unwrap();
        append
            .write_all(
                b"wine: Unhandled page fault on read access to 00000000 at address 0040107F\n",
            )
            .unwrap();
        drop(append);
        let fatal = diagnostics.poll(&file.0).unwrap();
        assert_eq!(fatal, Some(WineFatal::PageFault));
        assert!(
            classify_compat_exit(
                fatal,
                true,
                Some(ProcessExit {
                    code: Some(0),
                    signal: None
                }),
                None
            )
            .is_err()
        );
        std::fs::write(&file.0, b"wineserver: shutting down\n").unwrap();
        assert_eq!(
            diagnostics.poll(&file.0).unwrap(),
            fatal,
            "later shutdown must not erase a detected crash"
        );
    }
    #[test]
    fn wine_rewritten_argv_padding_does_not_hide_the_environment() {
        let prefix = b"/skin/compat/1/pfx";
        // Captured layout, synthetic values only: argv[0], an empty argv[1],
        // then 100 cleared bytes left by Wine before the environment starts.
        let mut padded = args(&["Soundpad.exe", ""], &[]);
        padded.pop();
        padded.extend_from_slice(&[0; 100]);
        padded.extend_from_slice(b"WINEPREFIX=/skin/compat/1/pfx\0SteamAppId=42\0\0");
        assert!(procargs_match(&padded, prefix, 42));
        assert!(!procargs_match(&padded, b"/skin/compat/other/pfx", 42));
        assert!(!procargs_match(&padded, prefix, 43));
        // Environment-looking argv values remain arguments even when Wine
        // leaves the same padding at the boundary.
        let mut fake = args(
            &[
                "Soundpad.exe",
                "WINEPREFIX=/skin/compat/1/pfx",
                "SteamAppId=42",
            ],
            &[],
        );
        fake.pop();
        fake.extend_from_slice(&[0; 100]);
        fake.extend_from_slice(b"WINEPREFIX=/skin/compat/other/pfx\0SteamAppId=43\0\0");
        assert!(!procargs_match(&fake, prefix, 42));
    }
    #[test]
    fn controls_reject_wrong_game_malformed_input_and_unknown_presence() {
        assert_eq!(
            parse_control(r#"{"command":"stop","appid":42}"#, 42),
            Some(ControlCommand::Stop)
        );
        assert_eq!(
            parse_control(r#"{"command":"set_presence","appid":42,"state":7}"#, 42),
            Some(ControlCommand::Presence(7))
        );
        for input in [
            r#"{"command":"stop","appid":43}"#,
            r#"{"command":"set_presence","appid":42,"state":8}"#,
            r#"{"command":"set_presence","appid":42,"state":-1}"#,
            r#"{"command":"set_presence","appid":42,"state":4294967297}"#,
            r#"{"command":"unknown","appid":42}"#,
            r#"{"command":"stop","appid":"42"}"#,
            r#"{"command":"stop","appid":42"#,
            "[]",
        ] {
            assert_eq!(parse_control(input, 42), None);
        }
        assert_eq!(parse_control(&" ".repeat(4097), 42), None);
    }
    #[test]
    fn native_procargs_layout_reads_only_child_environment() {
        // SIP-protected system executables redact their environment from this
        // sysctl; use our own unprotected binary, like the staged Wine runtime.
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "worker::controls_and_process_tests::process_inspection_child",
            ])
            .env("SKINOS_PROCESS_TEST_CHILD", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .env("WINEPREFIX", "/skinos-test/pfx")
            .env("SteamAppId", "42")
            .spawn()
            .unwrap();
        let matched = process_arguments(child.id() as i32)
            .is_some_and(|buffer| procargs_match(&buffer.0, b"/skinos-test/pfx", 42));
        let _ = child.kill();
        let _ = child.wait();
        assert!(
            matched,
            "native KERN_PROCARGS2 buffer did not match the reviewed layout"
        );
    }
    #[test]
    fn stop_terminates_only_the_verified_child_and_preserves_other_prefix() {
        use std::os::unix::process::ExitStatusExt;
        struct TestChild(std::process::Child);
        impl Drop for TestChild {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        fn child(prefix: &str) -> TestChild {
            TestChild(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "worker::controls_and_process_tests::process_inspection_child",
                    ])
                    .env("SKINOS_PROCESS_TEST_CHILD", "1")
                    .env("WINEPREFIX", prefix)
                    .env("SteamAppId", "42")
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .unwrap(),
            )
        }
        let mut selected = child("/skinos-stop-selected/pfx");
        let mut unrelated = child("/skinos-stop-other/pfx");
        let tracked = TrackedGameProcess::capture(selected.0.id() as i32).unwrap();
        let mut other = TrackedGameProcess::capture(unrelated.0.id() as i32).unwrap();
        let plan = skinos_compat::LaunchPlan {
            program: std::env::current_exe().unwrap(),
            args: vec![],
            env: Default::default(),
            working_dir: "/".into(),
            read_roots: vec![],
            write_roots: vec![],
            execute_roots: vec![],
            prefix_dir: "/skinos-stop-selected".into(),
            runner_root: tracked.executable.parent().unwrap().to_owned(),
            sdk_probe: "/unused".into(),
            windows_sdk: None,
            sdk_args: vec![],
            sdk_env: Default::default(),
        };
        let mut attempt = None;
        // Identical executable path is insufficient: the other prefix and an
        // incorrect AppID must both be refused without signalling either child.
        assert_eq!(tracked.executable, other.executable);
        assert!(!terminate_compat_game(42, 1, &other, &plan, &mut attempt).unwrap());
        assert!(!terminate_compat_game(43, 1, &tracked, &plan, &mut attempt).unwrap());
        let mut wrong_prefix = plan.clone();
        wrong_prefix.prefix_dir = "/different-prefix".into();
        assert!(!terminate_compat_game(42, 1, &tracked, &wrong_prefix, &mut attempt).unwrap());
        // A reused PID, even with the same executable and environment, is not
        // the original process. Reuse the selected PID with a forged starttime.
        other.pid = tracked.pid;
        other.start = (tracked.start.0, tracked.start.1.wrapping_add(1));
        assert!(!terminate_compat_game(42, 1, &other, &plan, &mut attempt).unwrap());
        assert!(attempt.is_none());
        assert!(selected.0.try_wait().unwrap().is_none());
        assert!(unrelated.0.try_wait().unwrap().is_none());
        assert_eq!(
            parse_control(r#"{"command":"stop","appid":42}"#, 42),
            Some(ControlCommand::Stop)
        );
        assert!(terminate_compat_game(42, 1, &tracked, &plan, &mut attempt).unwrap());
        let deadline = Instant::now() + Duration::from_secs(2);
        let status = loop {
            if let Some(status) = selected.0.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "selected test child did not exit after Stop"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(
            status.signal(),
            Some(15),
            "Stop must use graceful SIGTERM first"
        );
        assert_eq!(attempt.as_ref().unwrap().0, selected.0.id() as i32);
        assert!(
            unrelated.0.try_wait().unwrap().is_none(),
            "Stop touched another prefix"
        );
    }
    #[test]
    fn sdk_and_wine_child_explicitly_close_the_parent_profile_lock() {
        // Clearing CLOEXEC is process-wide. Run the lock owner in a dedicated
        // process so parallel tests cannot accidentally inherit this fixture FD.
        if std::env::var_os("SKINOS_LOCK_OWNER_TEST").is_none() {
            let output=std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact","worker::controls_and_process_tests::sdk_and_wine_child_explicitly_close_the_parent_profile_lock","--nocapture"])
                .env("SKINOS_LOCK_OWNER_TEST","1").output().unwrap();
            assert!(
                output.status.success(),
                "isolated lock-owner regression failed: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        use std::os::fd::AsRawFd;
        unsafe extern "C" {
            fn flock(fd: i32, operation: i32) -> i32;
        }
        struct OwnedChild(std::process::Child);
        impl Drop for OwnedChild {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        struct OwnedDirectory(std::path::PathBuf);
        impl Drop for OwnedDirectory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let directory = OwnedDirectory(std::env::temp_dir().join(format!(
                "skinos-child-lock-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )));
        std::fs::create_dir_all(&directory.0).unwrap();
        let path = directory.0.join("runtime.lock");
        let parent = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let fd = parent.as_raw_fd();
        assert_eq!(unsafe { flock(fd, 2 | 4) }, 0);
        // Reproduce the observed failure: CLOEXEC has been cleared by another
        // runtime component, so only our explicit child-close can protect it.
        assert_eq!(unsafe { fcntl(fd, 2, 0) }, 0);
        let contender = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let ready = directory.0.join("child-ready");
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "worker::controls_and_process_tests::profile_lock_inspection_child",
                "--nocapture",
            ])
            .env("SKINOS_LOCK_TEST_FD", fd.to_string())
            .env("SKINOS_LOCK_TEST_READY", &ready)
            .env("SKINOS_PROFILE_LOCK_FD", fd.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        isolate_profile_lock(&mut command, fd).unwrap();
        let mut child = OwnedChild(command.spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(3);
        while !ready.is_file() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "child failed lock-FD inspection"
            );
            assert!(Instant::now() < deadline, "child lock inspection timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            unsafe { fcntl(fd, 1, 0) } >= 0,
            "child isolation closed the parent FD"
        );
        assert_ne!(
            unsafe { flock(contender.as_raw_fd(), 2 | 4) },
            0,
            "child isolation unlocked the parent"
        );
        assert!(child.0.try_wait().unwrap().is_none());
        drop(parent);
        assert_eq!(
            unsafe { flock(contender.as_raw_fd(), 2 | 4) },
            0,
            "living child retained the runtime profile lock"
        );
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "child must still be alive when profile lock becomes free"
        );
    }
    #[test]
    fn profile_lock_inspection_child() {
        let Ok(value) = std::env::var("SKINOS_LOCK_TEST_FD") else {
            return;
        };
        let fd = value.parse::<i32>().unwrap();
        assert!(std::env::var_os("SKINOS_PROFILE_LOCK_FD").is_none());
        assert_eq!(
            unsafe { fcntl(fd, 1, 0) },
            -1,
            "parent profile lock FD survived child exec"
        );
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(9));
        std::fs::write(
            std::env::var_os("SKINOS_LOCK_TEST_READY").unwrap(),
            b"closed",
        )
        .unwrap();
        std::thread::sleep(Duration::from_secs(30));
    }
    #[test]
    fn process_inspection_child() {
        if std::env::var_os("SKINOS_PROCESS_TEST_CHILD").is_some() {
            std::thread::sleep(Duration::from_secs(30));
        }
    }
    #[test]
    fn game_input_terminal_gate_requires_real_game_exit_even_when_sdk_stays_active() {
        for active in [false, true] {
            assert!(compatibility_terminal_gate(true, true, active, true));
            assert!(!compatibility_terminal_gate(true, true, active, false));
            assert!(!compatibility_terminal_gate(false, true, active, true));
            assert!(!compatibility_terminal_gate(false, true, active, false));
        }
        // Non-input launches keep their established SDK termination behavior.
        for process_exited in [false, true] {
            assert!(!compatibility_terminal_gate(
                true,
                false,
                true,
                process_exited
            ));
            assert!(compatibility_terminal_gate(
                true,
                false,
                false,
                process_exited
            ));
            assert!(!compatibility_terminal_gate(
                false,
                false,
                false,
                process_exited
            ));
        }
    }
    #[test]
    fn unavailable_wrapper_does_not_mean_tracked_process_terminated() {
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "read line"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut process = loop {
            if let Some(process) = TrackedGameProcess::capture(pid) {
                break process;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        };
        process.executable = "/nonexistent/removed-wine-wrapper".into();
        process.executable_identity = (u64::MAX, u64::MAX);
        assert!(!process.is_current());
        assert_eq!(process.termination_observation(), None);
        assert!(!compatibility_terminal_gate(
            true,
            true,
            true,
            process.termination_observation().is_some()
        ));
        let actual_start = process.start;
        process.start = (actual_start.0.saturating_sub(1), actual_start.1);
        assert_eq!(process.termination_observation(), Some("pid_reused"));
        process.start = actual_start;
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(process.termination_observation(), Some("kernel_esrch"));
        assert!(compatibility_terminal_gate(
            true,
            true,
            true,
            process.termination_observation().is_some()
        ));
    }
    #[test]
    fn hardlinked_wine_wrapper_alias_keeps_identity_but_other_inode_does_not() {
        struct Directory(std::path::PathBuf);
        impl Drop for Directory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let directory = Directory(std::env::temp_dir().join(format!(
                "skinos-wrapper-identity-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )));
        std::fs::create_dir_all(&directory.0).unwrap();
        let game = directory.0.join("Soundpad.exe");
        let service = directory.0.join("SoundpadService.exe");
        let different = directory.0.join("Different.exe");
        std::fs::write(&game, b"fixture executable").unwrap();
        std::fs::hard_link(&game, &service).unwrap();
        std::fs::write(&different, b"fixture executable").unwrap();
        let expected = TrackedGameProcess {
            pid: 42,
            start: (123, 456),
            executable: game.clone(),
            executable_identity: executable_identity(&game).unwrap(),
        };
        let mut alias = TrackedGameProcess {
            pid: 42,
            start: (123, 456),
            executable: service.clone(),
            executable_identity: executable_identity(&service).unwrap(),
        };
        assert_ne!(expected.executable, alias.executable);
        assert!(expected.same_instance(&alias));
        alias.executable_identity = executable_identity(&different).unwrap();
        assert!(
            !expected.same_instance(&alias),
            "equal bytes in another inode are not the same executable"
        );
        alias.executable_identity = expected.executable_identity;
        alias.executable_identity.0 = alias.executable_identity.0.wrapping_add(1);
        assert!(
            !expected.same_instance(&alias),
            "inode alone cannot identify an executable across devices"
        );
        alias.executable_identity = expected.executable_identity;
        alias.start.1 += 1;
        assert!(!expected.same_instance(&alias));
        alias.start = expected.start;
        alias.pid += 1;
        assert!(!expected.same_instance(&alias));
    }
    #[test]
    fn native_process_identity_includes_start_time() {
        assert_eq!(std::mem::size_of::<ProcBsdInfo>(), 136);
        assert_eq!(std::mem::offset_of!(ProcBsdInfo, start_seconds), 120);
        let mut process = TrackedGameProcess::capture(std::process::id() as i32).unwrap();
        assert!(process.is_current());
        process.start.1 = process.start.1.wrapping_add(1);
        assert!(!process.is_current());
    }
}

#[cfg(test)]
mod compatibility_proof_tests {
    use super::*;
    #[test]
    fn legacy_bridge_requires_real_matching_identity_and_explicit_kind() {
        let mut p = json!({"proof_kind":"steamclient_bridge","bridge_initialized":true,"ok":true,"logged_on":true,"expected_matches":true,"steamid":"42"});
        assert!(valid_compat_proof(&p, 42));
        assert!(!valid_compat_proof(&p, 43));
        p["bridge_initialized"] = json!(false);
        assert!(!valid_compat_proof(&p, 42));
        p["sdk_initialized"] = json!(true);
        assert!(!valid_compat_proof(&p, 42));
        p["proof_kind"] = json!("game_sdk");
        assert!(valid_compat_proof(&p, 42));
        p["logged_on"] = json!(false);
        assert!(!valid_compat_proof(&p, 42));
    }
}
