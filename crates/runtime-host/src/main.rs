//! Dynamic experiments always run in a separately sandboxed worker process.
mod broker;
mod input;
mod input_config;
mod input_device_log;
mod input_devices;
mod input_filter_policy;
mod input_fixture;
mod input_fixture_log;
mod input_fixture_reconnect;
mod input_fixture_rumble;
mod input_fixture_sequence;
mod input_focus;
mod input_readiness;
mod input_settings;
mod installation;
mod native_launch;
mod native_lock_files;
mod profile;
mod runtime_bundle;
mod wine_input_output;
mod wine_input_relay;
mod worker;

use serde::Serialize;
use serde_json::Value;
use std::fs::{self, File};
use std::io::{BufRead, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const AUDITED_RUNTIME: &str = "035722a42c74989fa38c7efe0d735fdb38444e1102c92246652b5d913d019840";
const USAGE: &str = "Usage: skinos-runtime-host probe [--runtime PATH] [--state-root PATH] [--phase load|factory|global] [--json]\n\nRuns original Valve code in a bounded, sandboxed child process. No existing account state or network access. No Steam UI or game is started.";

type Result<T> = std::result::Result<T, String>;
unsafe extern "C" {
    fn fcntl(fd: i32, command: i32, ...) -> i32;
}

#[derive(Serialize)]
struct Report {
    schema_version: u32,
    mode: &'static str,
    phase: String,
    runtime_sha256: String,
    state_directory: PathBuf,
    stdout_log: PathBuf,
    stderr_log: PathBuf,
    timed_out: bool,
    exit_code: Option<i32>,
    termination_signal: Option<i32>,
    sandbox_guard_passed: bool,
    global_user_created: bool,
    clean_release_completed: bool,
    authenticated: bool,
    game_launch_verified: bool,
    events: Vec<Value>,
    broker_label: String,
    broker_service: String,
}

// This profile permits system libraries and the copied runtime, but no existing
// user files. Specific non-system Mach services (including Steam and Keychain)
// are denied; all networking and spawning other executables are denied.
const PROFILE: &str = r#"(version 1)
(allow default)
(deny network*)
(allow network-bind (local ip "localhost:57343"))
(allow network-inbound (local ip "localhost:57343"))
(allow network-bind (subpath (param "STATE_ROOT")))
(allow network-inbound (subpath (param "STATE_ROOT")))
(allow network-outbound (subpath (param "STATE_ROOT")))
(deny file-read-data
    (require-not
        (require-any
            (literal "/")
            (literal "/System")
            (literal "/System/Volumes")
            (subpath "/System/Library")
            (subpath "/System/Volumes/Preboot")
            (subpath "/System/Cryptexes")
            (subpath "/usr/lib")
            (subpath "/private/var/db/dyld")
            (subpath (param "STATE_ROOT"))
            (literal (param "WORKER"))
            ; SKINOS_EXTRA_READ
            (literal "/dev/null")
            (literal "/dev/random")
            (literal "/dev/urandom"))))
(deny file-write*
    (require-not (require-any (subpath (param "STATE_ROOT"))
        ; SKINOS_EXTRA_WRITE
    )))
(deny process-fork)
(deny process-exec
    (require-not (require-any (literal (param "WORKER"))
        ; SKINOS_EXTRA_EXEC
    )))
(deny mach-lookup
    (global-name-regex #"^com\.valvesoftware\.")
    (global-name-regex #"^com\.apple\.security")
    (global-name-regex #"^com\.apple\.account"))
(deny ipc-posix-shm)
(deny ipc-posix-sem)
"#;

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "__worker") {
        return worker::execute(&args[1..]);
    }
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[std::ffi::OsString]) -> Result<()> {
    if args
        .first()
        .is_some_and(|arg| arg == "inspect-native-launch")
    {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::io::stdin()
            .lock()
            .take(65537)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() > 65536 {
            return Err("Launch description exceeds protocol limit".into());
        }
        let request: Value =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid launch description")?;
        let plan = native_launch::NativeLaunchPlan::from_request(&request)?;
        let save_roots = installation::native_save_roots(&request)?;
        println!(
            "{}",
            serde_json::json!({
                "kind":plan.kind,"target":plan.target,"executable":plan.executable,
                "working_directory":plan.cwd,"interpreter":plan.interpreter,
                "architectures":plan.target_architectures,"sdk":plan.sdk,
                "sdk_probe_reason":plan.sdk_probe_reason,"save_roots":save_roots,
                "game_started":false,"directories_created":false
            })
        );
        return Ok(());
    }
    if args
        .first()
        .is_some_and(|arg| arg == "--prepare-bundled-runtime")
    {
        if args.len() != 1 {
            return Err("--prepare-bundled-runtime takes no arguments; set SKINOS_DATA_DIR for an isolated destination".into());
        }
        let runtime = runtime_bundle::prepare()?;
        println!(
            "{}",
            serde_json::json!({"prepared":true,"runtime_sha256":AUDITED_RUNTIME,
            "runtime_directory":runtime.parent(),"runtime_executed":false,"account_data_copied":false})
        );
        return Ok(());
    }
    if args.first().is_some_and(|arg| arg == "--print-trace-path") {
        println!("{}", trace_dylib()?.display());
        return Ok(());
    }
    if args
        .first()
        .is_some_and(|arg| arg == "--help" || arg == "-h")
    {
        println!("{USAGE}");
        return Ok(());
    }
    let live = args.first().is_some_and(|arg| arg == "run");
    if !live && args.first().is_none_or(|arg| arg != "probe") {
        return Err(USAGE.into());
    }
    let mut runtime = None;
    let mut state_root = PathBuf::from("outputs/runtime-state");
    let mut phase = String::from(if live { "live" } else { "global" });
    let mut json = false;
    let mut args = args[1..].iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--runtime") => {
                runtime = Some(PathBuf::from(args.next().ok_or("Missing runtime path")?))
            }
            Some("--state-root") => {
                state_root = PathBuf::from(args.next().ok_or("Missing state root")?)
            }
            Some("--phase") => {
                phase = args
                    .next()
                    .ok_or("Missing phase")?
                    .to_str()
                    .ok_or("Invalid phase")?
                    .to_owned()
            }
            Some("--json") => json = true,
            _ => return Err(format!("Unknown option: {}", arg.to_string_lossy())),
        }
    }
    if !matches!(phase.as_str(), "load" | "factory" | "global" | "live") {
        return Err("Phase must be load, factory, or global.".into());
    }
    let initial_command = if live {
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        if line.len() > 65536 {
            return Err("Command exceeds protocol limit".into());
        }
        let value: Value = serde_json::from_str(&line).map_err(|_| "Invalid command JSON")?;
        if value["command"] != "launch" && value["command"] != "authenticate" {
            return Err("Expected launch or authenticate command".into());
        }
        Some((line, value))
    } else {
        None
    };
    let requested_appid = initial_command
        .as_ref()
        .and_then(|(_, v)| v["appid"].as_u64())
        .unwrap_or(0);
    let compatibility: Option<skinos_compat::LaunchPlan> = initial_command
        .as_ref()
        .and_then(|(_, request)| request.get("compatibility"))
        .filter(|v| !v.is_null())
        .map(|value| serde_json::from_value(value.clone()).map_err(|e| e.to_string()))
        .transpose()?;
    let native_plan = initial_command
        .as_ref()
        .filter(|(_, request)| request["command"] == "launch" && compatibility.is_none())
        .map(|(_, request)| native_launch::NativeLaunchPlan::from_request(request))
        .transpose()?;
    let game_data = initial_command
        .as_ref()
        .map(|(_, request)| installation::prepare_native_save_roots(request))
        .transpose()?
        .unwrap_or_default();
    let runtime = runtime.map(Ok).unwrap_or_else(owned_runtime)?;
    if !cfg!(target_arch = "aarch64") {
        return Err("The current dynamic adapter is audited for arm64 only.".into());
    }
    if matches!(phase.as_str(), "global" | "live") {
        std::net::TcpListener::bind("127.0.0.1:57343")
            .map_err(|_| "Steam runtime IPC port is occupied. Close the other client before starting this host; no process was stopped.")?;
    }
    let inspection = skinos_runtime_probe::inspect(&runtime)?;
    if inspection.sha256 != AUDITED_RUNTIME {
        return Err("Runtime fingerprint is not audited for this dynamic experiment. Static inspection remains available.".into());
    }
    let root = fs::canonicalize(&state_root)
        .or_else(|_| {
            fs::create_dir_all(&state_root)?;
            fs::canonicalize(&state_root)
        })
        .map_err(|error| format!("Could not prepare state root: {error}"))?;
    let id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let state = root.join(format!("probe-{}-{id}", std::process::id()));
    fs::create_dir(&state).map_err(|e| e.to_string())?;
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    if let Some((_, request)) = &initial_command {
        let metadata = serde_json::json!({"appid":requested_appid,"created_at":SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),"command":request["command"],"compatibility":if request["compatibility"].is_null(){"native"}else{"notproton"}});
        fs::write(
            state.join("launch-meta.json"),
            serde_json::to_vec_pretty(&metadata).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }
    let account_profile = initial_command
        .as_ref()
        .map(|(_, request)| {
            profile::Profile::open(
                &root,
                request["steamid"]
                    .as_str()
                    .ok_or("Invalid profile SteamID")?,
            )
        })
        .transpose()?;
    let storage_root = account_profile
        .as_ref()
        .map(|p| p.root.as_path())
        .unwrap_or(&state);
    let data_root = account_profile
        .as_ref()
        .map(|p| p.data.clone())
        .unwrap_or_else(|| state.join("data"));
    let library_root = account_profile
        .as_ref()
        .map(|p| p.library.clone())
        .unwrap_or_else(|| state.join("library"));
    let install = initial_command
        .as_ref()
        .map(|(_, request)| installation::stage(request, storage_root))
        .transpose()?
        .flatten();
    fs::create_dir(state.join("runtime")).map_err(|e| e.to_string())?;
    fs::create_dir(state.join("tmp")).map_err(|e| e.to_string())?;
    fs::create_dir(state.join("data")).map_err(|e| e.to_string())?;
    fs::copy(trace_dylib()?, state.join("libskinos_trace.dylib")).map_err(|e| e.to_string())?;
    let sdk_probe = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .with_file_name("skinos-sdk-probe");
    if install.is_some() {
        if !sdk_probe.is_file() {
            return Err("Missing skinos-sdk-probe beside runtime host".into());
        }
        fs::copy(&sdk_probe, state.join("skinos-sdk-probe")).map_err(|e| e.to_string())?;
    }
    let source = inspection
        .canonical_path
        .parent()
        .ok_or("Runtime parent missing")?;
    for name in [
        "steamclient.dylib",
        "libtier0_s.dylib",
        "libvstdlib_s.dylib",
        "libaudio.dylib",
        "crashhandler.dylib",
        "ipcserver",
    ] {
        fs::create_dir_all(
            state
                .join("runtime")
                .join(name)
                .parent()
                .ok_or("Invalid runtime destination")?,
        )
        .map_err(|e| e.to_string())?;
        fs::copy(source.join(name), state.join("runtime").join(name))
            .map_err(|e| format!("Could not copy runtime component {name}: {e}"))?;
    }
    input::stage(source, &state.join("runtime"))?;
    let input_diagnostic = initial_command
        .as_ref()
        .is_some_and(|(_, request)| request["input_diagnostic"] == true);
    let requested_backend = std::env::var("SKINOS_INPUT_OUTPUT_BACKEND").ok();
    let output_selection = wine_input_output::OutputSelection::for_request(
        initial_command.as_ref().map(|(_, request)| request),
        requested_backend.as_deref(),
    )?;
    input_fixture_rumble::validate_environment(
        input_diagnostic && output_selection.is_diagnostic(),
    )?;
    input_fixture_rumble::validate_feedback_environment(output_selection.is_game())?;
    // Games and product diagnostics share binding, installation and teardown.
    const PRODUCT_GAME_INPUT_READY: bool = true;
    const PRODUCT_DIAGNOSTIC_INPUT_READY: bool = true;
    let product_ready = if output_selection.is_game() {
        PRODUCT_GAME_INPUT_READY
    } else {
        PRODUCT_DIAGNOSTIC_INPUT_READY
    };
    output_selection.require_ready(requested_backend.as_deref() == Some("wine"), product_ready)?;
    let output_backend = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .with_file_name("libsteaminput_wine.dylib");
    wine_input_output::stage(&state, &output_backend, output_selection)?;
    // Keep the framework's signed bundle metadata and resource dylib together.
    let copy = Command::new("/usr/bin/ditto")
        .arg(source.join("Frameworks/Breakpad.framework"))
        .arg(state.join("runtime/Frameworks/Breakpad.framework"))
        .output()
        .map_err(|e| e.to_string())?;
    if !copy.status.success() {
        return Err(format!(
            "Could not stage Breakpad framework: {}",
            String::from_utf8_lossy(&copy.stderr)
        ));
    }
    let copied = skinos_runtime_probe::inspect(&state.join("runtime/steamclient.dylib"))?;
    if copied.sha256 != AUDITED_RUNTIME {
        return Err("Runtime changed while being copied; refusing to load it.".into());
    }
    let host_executable = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|e| e.to_string())?;
    let worker = state.join("runtime/skinos-runtime-host");
    fs::copy(host_executable, &worker).map_err(|e| e.to_string())?;
    let mut broker = broker::Broker::start(
        &state,
        &state.join("runtime"),
        &state.join("libskinos_trace.dylib"),
    )?;
    let canary = root.join(format!("guard-canary-{id}.txt"));
    let canary_contents = format!("skinos-canary-{id}");
    fs::write(&canary, &canary_contents).map_err(|e| e.to_string())?;
    let stdout_log = state.join("worker.stdout.log");
    let stderr_log = state.join("worker.stderr.log");
    let mut profile = if live {
        format!("{PROFILE}\n(allow network*)\n(allow file-read-data (subpath \"/private/etc\"))\n")
    } else {
        PROFILE.to_owned()
    };
    let mut read_paths = Vec::new();
    let mut write_paths = Vec::new();
    let mut exec_paths = Vec::new();
    let mut parameters: Vec<(String, PathBuf)> = Vec::new();
    let mut grant = |path: PathBuf, prefix: &str, list: &mut Vec<String>| {
        let key = format!("{}_{}", prefix, parameters.len());
        list.push(format!("(subpath (param \"{key}\"))"));
        parameters.push((key, path));
    };
    if storage_root != state {
        grant(storage_root.to_path_buf(), "PROFILE_READ", &mut read_paths);
        grant(
            storage_root.to_path_buf(),
            "PROFILE_WRITE",
            &mut write_paths,
        );
    }
    for path in &game_data {
        grant(path.clone(), "SAVE_READ", &mut read_paths);
        grant(path.clone(), "SAVE_WRITE", &mut write_paths);
    }
    if let Some(path) = &install {
        grant(path.clone(), "GAME_READ", &mut read_paths);
        grant(path.clone(), "GAME_EXEC", &mut exec_paths);
        read_paths.push("(literal \"/bin/sh\")".into());
        exec_paths.push("(literal \"/bin/sh\")".into());
        grant(
            state.join("skinos-sdk-probe"),
            "PROBE_EXEC",
            &mut exec_paths,
        );
        profile = profile.replace("(deny process-fork)", "(allow process-fork)");
    }
    if let Some(plan) = &compatibility {
        for path in &plan.read_roots {
            grant(path.clone(), "COMPAT_READ", &mut read_paths);
        }
        for path in &plan.write_roots {
            grant(path.clone(), "COMPAT_WRITE", &mut write_paths);
        }
        for path in &plan.execute_roots {
            grant(path.clone(), "COMPAT_EXEC", &mut exec_paths);
            grant(path.clone(), "COMPAT_TOOL_READ", &mut read_paths);
        }
        for path in ["/private/tmp", "/private/var/folders", "/dev"] {
            grant(PathBuf::from(path), "WINE_READ", &mut read_paths);
            grant(PathBuf::from(path), "WINE_WRITE", &mut write_paths);
        }
        profile = profile
            .replace("(deny ipc-posix-shm)", "(allow ipc-posix-shm)")
            .replace("(deny ipc-posix-sem)", "(allow ipc-posix-sem)");
        // Wine resolves long DOS paths by enumerating each directory component.
        // Permit listing ancestor directories only, not reading their children.
        for path in directory_listing_ancestors(
            plan.read_roots
                .iter()
                .map(PathBuf::as_path)
                .chain([state.as_path(), storage_root]),
        ) {
            let key = format!("COMPAT_DIRECTORY_{}", parameters.len());
            read_paths.push(format!("(literal (param \"{key}\"))"));
            parameters.push((key, path));
        }
        // The compatibility runner is launched directly; it never hands off to Steam.app.
        profile.push_str("\n(deny process-exec (literal \"/usr/bin/open\"))\n");
    }
    if let Some(plan) = &native_plan {
        read_paths.push(native_lock_files::SOURCE2_LOCK_FILES.into());
        write_paths.push(native_lock_files::SOURCE2_LOCK_FILES.into());
        // A script uses its declared interpreter and a small set of system
        // path/platform utilities. Every grant names one executable, never a
        // general bin directory or a path supplied by launch arguments.
        let mut tools = Vec::new();
        if let Some(interpreter) = &plan.interpreter {
            tools.push(interpreter.clone());
            tools.extend(
                ["/usr/bin/basename", "/usr/bin/dirname", "/usr/bin/uname"].map(PathBuf::from),
            );
        }
        for path in tools {
            let key = format!("NATIVE_TOOL_{}", parameters.len());
            read_paths.push(format!("(literal (param \"{key}\"))"));
            exec_paths.push(format!("(literal (param \"{key}\"))"));
            parameters.push((key, path));
        }
        if plan.kind == "script"
            || plan
                .target_architectures
                .iter()
                .any(|arch| arch == "x86_64")
        {
            // Rosetta's system runtime is outside /System/Library. Intel game
            // executables require this read even though no user-data access is needed.
            read_paths
                .push("(literal \"/Library/Apple/usr/libexec/oah/libRosettaRuntime\")".into());
        }
        profile.push_str("\n(deny process-exec (literal \"/usr/bin/open\"))\n");
    }
    profile = profile
        .replace("; SKINOS_EXTRA_READ", &read_paths.join(" "))
        .replace("; SKINOS_EXTRA_WRITE", &write_paths.join(" "))
        .replace("; SKINOS_EXTRA_EXEC", &exec_paths.join(" "));
    fs::write(state.join("sandbox.sb"), profile).map_err(|e| e.to_string())?;
    let mut command = Command::new("/usr/bin/sandbox-exec");
    for (name, path) in parameters {
        command.arg("-D").arg(format!("{name}={}", path.display()));
    }
    command.env_remove("SKINOS_GAME_DATA_ROOT");
    command
        .arg("-D")
        .arg(format!("STATE_ROOT={}", state.display()))
        .arg("-D")
        .arg(format!("WORKER={}", worker.display()))
        .arg("-f")
        .arg(state.join("sandbox.sb"))
        .arg(&worker)
        .arg("__worker")
        .arg(state.join("runtime/steamclient.dylib"))
        .arg(&state)
        .arg(&canary)
        .arg(&phase)
        .current_dir(&state)
        .env("TMPDIR", state.join("tmp"))
        .env("SKINOS_RUNTIME_WORKER", "1")
        .env("SKINOS_RUNTIME_DATA_ROOT", &data_root)
        .env("SKINOS_RUNTIME_STATE_ROOT", &state)
        .env("SKINOS_RUNTIME_LIBRARY_ROOT", &library_root)
        .env("SKINOS_COMPONENT_ROLE", "runtime")
        .env("SKINOS_ADAPTER_PATH", state.join("libskinos_trace.dylib"))
        .env("SKINOS_IPC_SERVICE_NAME", &broker.service_name)
        .stdin(if live { Stdio::piped() } else { Stdio::null() })
        .stdout(if live {
            Stdio::piped()
        } else {
            File::create(&stdout_log).map_err(|e| e.to_string())?.into()
        })
        .stderr(File::create(&stderr_log).map_err(|e| e.to_string())?);
    command.env_remove("SKINOS_INPUT_OUTPUT_FD");
    command.env("SKINOS_INPUT_OUTPUT_MODE", output_selection.as_str());
    command.env_remove("SKINOS_INPUT_OUTPUT_BACKEND");
    if output_selection.enabled() {
        command.env("SKINOS_INPUT_OUTPUT_BACKEND", "wine");
    }
    // The structured per-game input selection owns this switch. An inherited
    // flag cannot arm feedback for native games, disabled input or diagnostics.
    // Explicit fixture QA uses its own capture mode through the same transport.
    command.env_remove("SKINOS_INPUT_FEEDBACK");
    if output_selection.is_game()
        && std::env::var("SKINOS_INPUT_FEEDBACK_TEST").ok().as_deref() != Some("1")
    {
        command.env("SKINOS_INPUT_FEEDBACK", "1");
    }
    command.env(
        "SKINOS_INPUT_DIAGNOSTIC",
        if input_diagnostic { "1" } else { "0" },
    );
    // Remove inherited Steam/DYLD overrides that might alter what gets loaded.
    for (name, _) in std::env::vars_os() {
        let name_text = name.to_string_lossy();
        if name_text.starts_with("Steam")
            || name_text.starts_with("STEAM_")
            || name_text.starts_with("DYLD_")
        {
            command.env_remove(name);
        }
    }
    if let Some((_, request)) = &initial_command {
        if request["command"] == "launch" {
            command.env(
                "SKINOS_GAME_ARGUMENTS_JSON",
                request["arguments"].to_string(),
            );
            command.env("SKINOS_LAUNCH_APP_ID", request["appid"].to_string());
            command.env(
                "SKINOS_GAME_BUNDLE",
                request["executable"].as_str().ok_or("Missing game path")?,
            );
            command.env(
                "SKINOS_GAME_WORKING_DIR",
                request["working_dir"]
                    .as_str()
                    .ok_or("Missing working directory")?,
            );
        }
    }
    if let Some(plan) = &native_plan {
        plan.configure(&mut command);
        if plan.kind == "script" {
            command.env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
        }
    }
    if let Some(profile) = &account_profile {
        let lock_fd = profile.lock_fd();
        command.env("SKINOS_PROFILE_LOCK_FD", lock_fd.to_string());
        unsafe {
            command.pre_exec(move || {
                if fcntl(lock_fd, 2, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let started = Instant::now();
    let mut child = command
        .spawn()
        .map_err(|e| format!("Sandbox worker could not start: {e}"))?;
    let owner_tmp = state.join("runtime-owner.next");
    fs::write(&owner_tmp, format!("{}\n", child.id())).map_err(|e| e.to_string())?;
    fs::rename(owner_tmp, state.join("runtime-owner.pid")).map_err(|e| e.to_string())?;
    let output_thread = if live {
        let mut input = child.stdin.take().ok_or("Worker input missing")?;
        if let Some((line, _)) = initial_command {
            write!(input, "{line}").map_err(|e| e.to_string())?;
            input.flush().map_err(|e| e.to_string())?;
        }
        std::thread::spawn(move || {
            for line in std::io::stdin()
                .lock()
                .lines()
                .map_while(std::result::Result::ok)
            {
                // Protocol secrets travel only through anonymous stdin pipes.
                if line.len() > 65536 {
                    break;
                }
                if writeln!(input, "{line}").is_err() {
                    break;
                }
            }
        });
        let output = child.stdout.take().ok_or("Worker output missing")?;
        let log_path = stdout_log.clone();
        Some(std::thread::spawn(move || {
            let mut file = File::create(log_path).ok();
            for line in std::io::BufReader::new(output)
                .lines()
                .map_while(std::result::Result::ok)
            {
                if let Some(protocol) = line.strip_prefix("SKINOS_LIVE ") {
                    let _ = writeln!(std::io::stdout(), "{protocol}");
                    let _ = std::io::stdout().flush();
                }
                if let Some(file) = &mut file {
                    let _ = writeln!(file, "{line}");
                }
            }
        }))
    } else {
        None
    };
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if !live && started.elapsed() > Duration::from_secs(15) {
            timed_out = true;
            child.kill().map_err(|e| e.to_string())?;
            break child.wait().map_err(|e| e.to_string())?;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    if let Some(thread) = output_thread {
        let _ = thread.join();
    }
    let canary_intact = fs::read_to_string(&canary).map_err(|e| e.to_string())? == canary_contents;
    fs::remove_file(&canary).map_err(|e| e.to_string())?;
    if !canary_intact {
        return Err("Sandbox canary was modified; dynamic experiment rejected.".into());
    }
    let text = fs::read_to_string(&stdout_log).map_err(|e| e.to_string())?;
    let events: Vec<Value> = text
        .lines()
        .filter_map(|line| line.strip_prefix("SKINOS_EVENT "))
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let has = |name| {
        events
            .iter()
            .any(|event| event["event"] == name && event["ok"] == true)
    };
    let report = Report {
        schema_version: 1,
        mode: "isolated_dynamic",
        phase,
        runtime_sha256: copied.sha256,
        state_directory: state.clone(),
        stdout_log,
        stderr_log,
        timed_out,
        exit_code: status.code(),
        termination_signal: status.signal(),
        sandbox_guard_passed: has("sandbox_guard"),
        global_user_created: has("global_user"),
        clean_release_completed: has("released") && has("shutdown") && has("unloaded"),
        authenticated: has("runtime_authenticated"),
        game_launch_verified: status.success()
            && !events.iter().any(|event| event["event"] == "game_failed")
            && has("game_running")
            && (has("sdk_verified") || has("compat_sdk_verified")),
        events,
        broker_label: broker.label.clone(),
        broker_service: broker.service_name.clone(),
    };
    broker.stop();
    let report_json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    fs::write(state.join("report.json"), &report_json).map_err(|e| e.to_string())?;
    if live && !status.success() {
        let message = if let Some(message) = report
            .events
            .iter()
            .rev()
            .find(|event| event["event"] == "error")
            .and_then(|event| event["detail"]["message"].as_str())
        {
            message.to_owned()
        } else if let Some(signal) = status.signal() {
            format!("Runtime helper terminated with signal {signal}")
        } else {
            format!(
                "Runtime helper exited with status {}",
                status.code().unwrap_or(-1)
            )
        };
        let _ = writeln!(
            std::io::stdout(),
            "{}",
            serde_json::json!({"event":"launch_status","payload":{"appid":requested_appid,"state":"failed","message":message}})
        );
    }
    if live {
    } else if json {
        println!("{report_json}");
    } else {
        println!("Report: {}", state.join("report.json").display());
        println!(
            "Guard: {}; global user: {}; released: {}; exit: {:?}; timeout: {}",
            report.sandbox_guard_passed,
            report.global_user_created,
            report.clean_release_completed,
            report.exit_code,
            report.timed_out
        );
    }
    if !status.success() || !report.sandbox_guard_passed {
        return Err(
            "Runtime worker did not complete successfully; inspect its report/logs.".into(),
        );
    }
    Ok(())
}

fn directory_listing_ancestors<'a>(
    paths: impl IntoIterator<Item = &'a std::path::Path>,
) -> Vec<PathBuf> {
    let mut directories = std::collections::BTreeSet::new();
    for path in paths {
        for ancestor in path.ancestors().skip(1) {
            if ancestor.is_absolute() && ancestor.is_dir() {
                directories.insert(ancestor.to_path_buf());
            }
        }
    }
    directories.into_iter().collect()
}

fn trace_dylib() -> Result<PathBuf> {
    let sibling = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .with_file_name("libskinos_trace.dylib");
    if sibling.is_file() {
        return Ok(sibling);
    }
    let built = PathBuf::from(env!("SKINOS_TRACE_DYLIB"));
    if built.is_file() {
        Ok(built)
    } else {
        Err("Missing libskinos_trace.dylib beside runtime host".into())
    }
}

fn owned_runtime() -> Result<PathBuf> {
    runtime_bundle::prepare()
}

#[cfg(test)]
mod directory_policy_tests {
    use super::*;
    #[test]
    fn ancestor_listing_does_not_grant_file_contents() {
        let root =
            std::env::temp_dir().join(format!("skinos-directory-policy-{}", std::process::id()));
        let game = root.join("Steam/steamapps/common/Game");
        fs::create_dir_all(&game).unwrap();
        let root = root.canonicalize().unwrap();
        let game = game.canonicalize().unwrap();
        let secret = root.join("Steam/private.fixture");
        fs::write(&secret, "private fixture").unwrap();
        let ancestors = directory_listing_ancestors([game.as_path()]);
        let literals = ancestors
            .iter()
            .map(|p| format!("(literal {})", serde_json::to_string(p).unwrap()))
            .collect::<Vec<_>>()
            .join(" ");
        let policy = format!(
            "(version 1)(allow default)(deny file-read-data (require-all (subpath {}) (require-not (require-any (subpath {}) {literals}))))",
            serde_json::to_string(&root).unwrap(),
            serde_json::to_string(&game).unwrap()
        );
        let path = root.join("policy.sb");
        fs::write(&path, policy).unwrap();
        let list = Command::new("/usr/bin/sandbox-exec")
            .arg("-f")
            .arg(&path)
            .arg("/bin/ls")
            .arg(root.join("Steam"))
            .output()
            .unwrap();
        assert!(
            list.status.success(),
            "ancestor directory could not be listed"
        );
        let content = Command::new("/usr/bin/sandbox-exec")
            .arg("-f")
            .arg(&path)
            .arg("/bin/cat")
            .arg(&secret)
            .output()
            .unwrap();
        assert!(
            !content.status.success(),
            "ancestor permission exposed a file"
        );
        assert!(content.stdout.is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}
