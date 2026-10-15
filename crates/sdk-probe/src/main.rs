//! Independent consumer of the game's unmodified Steamworks SDK.
//! The caller must keep SkinOs's authenticated host alive and bound the process lifetime.
mod input;

use serde::Serialize;
use std::{
    ffi::{CStr, CString, OsString, c_char, c_int, c_void},
    io::Write,
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, Instant},
};

const USER_FACTORY: &CStr = c"SteamAPI_SteamUser_v023";
const REQUIRED_SYMBOLS: &[&str] = &[
    "SteamAPI_InitFlat",
    "SteamAPI_Shutdown",
    "SteamAPI_RunCallbacks",
    "SteamAPI_SteamUser_v023",
    "SteamAPI_ISteamUser_GetSteamID",
    "SteamAPI_ISteamUser_BLoggedOn",
];
const USAGE: &str = "skinos-sdk-probe --library PATH --appid ID --expected-steamid ID --client-library PATH [--inspect-only] [--input-check]\n--input-check performs a seven-second native SteamInput006 scan in the verified SDK AppID context.\nDynamic mode requires the caller's isolated runtime sandbox and SKINOS_RUNTIME_DATA_ROOT + DYLD_INSERT_LIBRARIES storage adapter. The authenticated SkinOs host must already be alive. This probe never logs in or launches Steam.";

type Result<T> = std::result::Result<T, String>;
#[derive(Debug)]
struct Options {
    library: PathBuf,
    client_library: PathBuf,
    appid: u32,
    expected_steamid: u64,
    inspect_only: bool,
    input_check: bool,
}
#[derive(Serialize)]
struct Report {
    mode: &'static str,
    sdk_initialized: bool,
    logged_on: bool,
    steamid: Option<String>,
    expected_matches: bool,
    client_library_matches: bool,
    init_result: Option<i32>,
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<input::InputReport>,
}
impl Report {
    fn empty(mode: &'static str) -> Self {
        Self {
            mode,
            sdk_initialized: false,
            logged_on: false,
            steamid: None,
            expected_matches: false,
            client_library_matches: false,
            init_result: None,
            error: None,
            input: None,
        }
    }
    fn success(&self) -> bool {
        self.sdk_initialized
            && self.logged_on
            && self.expected_matches
            && self.client_library_matches
            && self.input.as_ref().is_none_or(input::InputReport::success)
    }
}
fn parse(args: Vec<OsString>) -> Result<Options> {
    let mut library = None;
    let mut client_library = None;
    let mut appid = None;
    let mut expected = None;
    let mut inspect_only = false;
    let mut input_check = false;
    let mut args = args.into_iter();
    while let Some(flag) = args.next() {
        match flag.to_str() {
            Some("--library") => {
                library = Some(PathBuf::from(args.next().ok_or("Missing --library value")?))
            }
            Some("--client-library") => {
                client_library = Some(PathBuf::from(
                    args.next().ok_or("Missing --client-library value")?,
                ))
            }
            Some("--appid") => {
                appid = Some(
                    args.next()
                        .and_then(|s| s.to_str().and_then(|s| s.parse::<u32>().ok()))
                        .filter(|id| *id > 0)
                        .ok_or("Invalid AppID")?,
                )
            }
            Some("--expected-steamid") => {
                expected = Some(
                    args.next()
                        .and_then(|s| s.to_str().and_then(|s| s.parse::<u64>().ok()))
                        .filter(|id| *id > 0)
                        .ok_or("Invalid SteamID")?,
                )
            }
            Some("--inspect-only") => inspect_only = true,
            Some("--input-check") => input_check = true,
            Some("--help" | "-h") => return Err(USAGE.into()),
            _ => return Err("Unknown probe option".into()),
        }
    }
    let canonical_file = |path: Option<PathBuf>, name: &str| -> Result<PathBuf> {
        let path = path
            .ok_or_else(|| format!("Missing {name}"))?
            .canonicalize()
            .map_err(|_| format!("Cannot resolve {name}"))?;
        if !path.is_file() {
            return Err(format!("{name} must refer to a file"));
        }
        Ok(path)
    };
    let library = canonical_file(library, "--library")?;
    let client_library = canonical_file(client_library, "--client-library")?;
    if library.file_name() != Some(std::ffi::OsStr::new("libsteam_api.dylib")) {
        return Err("SDK library must be the game's libsteam_api.dylib".into());
    }
    if client_library.file_name() != Some(std::ffi::OsStr::new("steamclient.dylib")) {
        return Err("Client library must be the staged steamclient.dylib".into());
    }
    Ok(Options {
        library,
        client_library,
        appid: appid.ok_or("Missing --appid")?,
        expected_steamid: expected.ok_or("Missing --expected-steamid")?,
        inspect_only,
        input_check,
    })
}

fn inspect(options: &Options) -> Result<()> {
    let output = std::process::Command::new("/usr/bin/nm")
        .arg("-arch")
        .arg(std::env::consts::ARCH.replace("aarch64", "arm64"))
        .arg("-gU")
        .arg(&options.library)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("Cannot inspect SDK symbols for this architecture".into());
    }
    let symbols = String::from_utf8_lossy(&output.stdout);
    for symbol in REQUIRED_SYMBOLS.iter().chain(
        input::REQUIRED_SYMBOLS
            .iter()
            .filter(|_| options.input_check),
    ) {
        if !symbols
            .lines()
            .any(|line| line.split_whitespace().last() == Some(format!("_{symbol}").as_str()))
        {
            return Err(format!("Required SDK export absent: {symbol}"));
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn _dyld_image_count() -> u32;
    fn _dyld_get_image_name(index: u32) -> *const c_char;
}

struct Library(*mut c_void);
impl Library {
    unsafe fn open(path: &Path) -> Result<Self> {
        use std::os::unix::ffi::OsStrExt;
        let path =
            CString::new(path.as_os_str().as_bytes()).map_err(|_| "Library path contains NUL")?;
        // SAFETY: valid NUL-terminated path; call is confined to this probe process.
        let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
        if handle.is_null() {
            let error = unsafe { libc::dlerror() };
            return Err(if error.is_null() {
                "dlopen failed".into()
            } else {
                unsafe { CStr::from_ptr(error) }
                    .to_string_lossy()
                    .into_owned()
            });
        }
        Ok(Self(handle))
    }
    unsafe fn symbol(&self, name: &CStr) -> Result<*mut c_void> {
        // SAFETY: handle remains loaded and name is statically NUL-terminated.
        let pointer = unsafe { libc::dlsym(self.0, name.as_ptr()) };
        if pointer.is_null() {
            Err(format!("Missing SDK export {}", name.to_string_lossy()))
        } else {
            Ok(pointer)
        }
    }
}
// Deliberately keep images loaded until exit: the SDK may retain background TLS.
// SteamAPI_Shutdown releases the SDK session before the operating system unloads images.

#[cfg(target_os = "macos")]
fn client_images_match(expected: &Path) -> bool {
    let mut found = false;
    // SAFETY: dyld image names are process-owned and valid during this single-thread query.
    for i in 0..unsafe { _dyld_image_count() } {
        let pointer = unsafe { _dyld_get_image_name(i) };
        if pointer.is_null() {
            continue;
        }
        let text = unsafe { CStr::from_ptr(pointer) }.to_string_lossy();
        let path = Path::new(text.as_ref());
        if path.file_name() == Some(std::ffi::OsStr::new("steamclient.dylib")) {
            found = true;
            if path.canonicalize().ok().as_deref() != Some(expected) {
                return false;
            }
        }
    }
    found
}
#[cfg(not(target_os = "macos"))]
fn client_images_match(_: &Path) -> bool {
    false
}

struct ScopeExit<F: FnOnce()>(Option<F>);
impl<F: FnOnce()> ScopeExit<F> {
    fn new(action: F) -> Self {
        Self(Some(action))
    }
}
impl<F: FnOnce()> Drop for ScopeExit<F> {
    fn drop(&mut self) {
        if let Some(action) = self.0.take() {
            action();
        }
    }
}

fn run(options: &Options) -> Result<Report> {
    let data_root = std::env::var_os("SKINOS_RUNTIME_DATA_ROOT").ok_or(
        "Missing isolated runtime data root; run this probe inside the SkinOs host sandbox",
    )?;
    if !Path::new(&data_root).is_dir() {
        return Err("Isolated runtime data root is absent".into());
    }
    // Account data persists independently of the disposable per-run binary state.
    let state_root = std::env::var_os("SKINOS_RUNTIME_STATE_ROOT")
        .ok_or("Missing explicit per-run runtime state root")?;
    let state_root = Path::new(&state_root)
        .canonicalize()
        .map_err(|_| "Cannot resolve per-run runtime state root")?;
    let runtime_root = state_root
        .join("runtime")
        .canonicalize()
        .map_err(|_| "Cannot resolve isolated runtime binary directory")?;
    if !runtime_root.starts_with(&state_root) || !options.client_library.starts_with(&runtime_root)
    {
        return Err(
            "Client library must be staged inside the isolated runtime state directory".into(),
        );
    }

    if std::env::var_os("DYLD_INSERT_LIBRARIES").is_none() {
        return Err(
            "Missing runtime storage adapter; original Steam state must not be selected".into(),
        );
    }
    // SAFETY: set only this process's app context, before loading libraries or
    // creating threads. HOME and system-wide environment are never modified.
    unsafe {
        std::env::set_var("SteamAppId", options.appid.to_string());
        std::env::set_var("SteamGameId", options.appid.to_string());
    }
    std::env::set_current_dir(
        options
            .client_library
            .parent()
            .ok_or("Missing runtime directory")?,
    )
    .map_err(|e| e.to_string())?;
    // SAFETY: libraries are selected explicitly, original unmodified Valve files.
    let _client = unsafe { Library::open(&options.client_library) }?;
    let sdk = unsafe { Library::open(&options.library) }?;
    type InitFlat = unsafe extern "C" fn(*mut c_char) -> c_int;
    type NoArgs = unsafe extern "C" fn();
    type UserFactory = unsafe extern "C" fn() -> *mut c_void;
    type GetSteamId = unsafe extern "C" fn(*mut c_void) -> u64;
    type LoggedOn = unsafe extern "C" fn(*mut c_void) -> bool;
    // Signatures from Valve's steam_api.h/steam_api_flat.h, exports checked with nm.
    let init: InitFlat = unsafe { std::mem::transmute(sdk.symbol(c"SteamAPI_InitFlat")?) };
    let shutdown: NoArgs = unsafe { std::mem::transmute(sdk.symbol(c"SteamAPI_Shutdown")?) };
    let callbacks: NoArgs = unsafe { std::mem::transmute(sdk.symbol(c"SteamAPI_RunCallbacks")?) };
    let factory: UserFactory = unsafe { std::mem::transmute(sdk.symbol(USER_FACTORY)?) };
    let get_id: GetSteamId =
        unsafe { std::mem::transmute(sdk.symbol(c"SteamAPI_ISteamUser_GetSteamID")?) };
    let logged_on: LoggedOn =
        unsafe { std::mem::transmute(sdk.symbol(c"SteamAPI_ISteamUser_BLoggedOn")?) };
    let mut report = Report::empty("sdk");
    let mut error = [0 as c_char; 1024];
    let result = unsafe { init(error.as_mut_ptr()) };
    report.init_result = Some(result);
    report.sdk_initialized = result == 0;
    report.client_library_matches = client_images_match(&options.client_library);
    if result != 0 {
        let end = error.iter().position(|c| *c == 0).unwrap_or(error.len());
        report.error = Some(
            String::from_utf8_lossy(&error[..end].iter().map(|c| *c as u8).collect::<Vec<_>>())
                .into_owned(),
        );
        return Ok(report);
    }
    let sdk_shutdown = ScopeExit::new(|| unsafe { shutdown() });
    // An init success alone is insufficient: use the original versioned user factory.
    let user = unsafe { factory() };
    if user.is_null() {
        report.error = Some("SteamAPI_InitFlat succeeded but SteamUser023 is unavailable".into());
    } else {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            unsafe { callbacks() };
            let steamid = unsafe { get_id(user) };
            report.logged_on = unsafe { logged_on(user) };
            report.steamid = (steamid != 0).then(|| steamid.to_string());
            report.expected_matches = steamid == options.expected_steamid;
            if report.success() || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    if options.input_check {
        let input_report = if report.success() {
            input::check(&sdk, options.appid, callbacks, &|| unsafe {
                get_id(user) == options.expected_steamid && logged_on(user)
            })
        } else {
            let mut unavailable = input::InputReport::empty();
            unavailable.error =
                Some("Native input scan requires a verified SDK account and client image".into());
            unavailable
        };
        if let Some(error) = &input_report.error {
            report.error = Some(error.clone());
        }
        report.input = Some(input_report);
        report.client_library_matches = client_images_match(&options.client_library);
    }
    drop(sdk_shutdown);
    if !report.client_library_matches {
        report.error = Some("SDK loaded a different Steam client library".into());
    }
    Ok(report)
}

fn main() -> ExitCode {
    if std::env::args()
        .skip(1)
        .any(|arg| arg == "--help" || arg == "-h")
    {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let options = match parse(std::env::args_os().skip(1).collect()) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    if options.inspect_only {
        let mut report = Report::empty("static");
        report.error = inspect(&options).err();
        println!("{}", serde_json::to_string(&report).unwrap());
        return if report.error.is_none() {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(2)
        };
    }
    // Valve SDK diagnostic prints go to stderr, preserving one JSON record on stdout.
    use std::os::fd::FromRawFd;
    let output = unsafe { libc::dup(libc::STDOUT_FILENO) };
    if output < 0 {
        return ExitCode::from(2);
    }
    let mut report_output = unsafe { std::fs::File::from_raw_fd(output) };
    if unsafe { libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) } < 0 {
        return ExitCode::from(2);
    }
    let mut report = match run(&options) {
        Ok(report) => report,
        Err(error) => {
            let mut report = Report::empty("sdk");
            report.error = Some(error);
            report
        }
    };
    if !report.success() && report.error.as_deref() == Some("") {
        report.error = Some("Steamworks initialization failed".into());
    }
    let _ = serde_json::to_writer(&mut report_output, &report);
    let _ = report_output.write_all(b"\n");
    let _ = report_output.flush();
    if report.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn init_success_without_identity_is_not_probe_success() {
        let mut report = Report::empty("sdk");
        report.sdk_initialized = true;
        assert!(!report.success());
        report.logged_on = true;
        report.expected_matches = true;
        assert!(!report.success());
        report.client_library_matches = true;
        assert!(report.success());
    }
    #[test]
    fn default_report_preserves_original_schema_and_input_mode_requires_scan() {
        let mut report = Report::empty("sdk");
        report.sdk_initialized = true;
        report.logged_on = true;
        report.expected_matches = true;
        report.client_library_matches = true;
        assert!(report.success());
        assert!(
            serde_json::to_value(&report)
                .unwrap()
                .get("input")
                .is_none()
        );
        report.input = Some(input::InputReport::empty());
        assert!(!report.success());
        let input = report.input.as_mut().unwrap();
        input.api_initialized = true;
        input.appid_verified = true;
        input.appid = 648800;
        input.scan_completed = true;
        assert!(report.success());
    }
    #[test]
    fn sdk_shutdown_guard_runs_once_when_optional_probe_fails() {
        let calls = std::cell::Cell::new(0);
        let result: Result<()> = {
            let _guard = ScopeExit::new(|| calls.set(calls.get() + 1));
            Err("Missing optional input export".into())
        };
        assert!(result.is_err());
        assert_eq!(calls.get(), 1);
    }
    #[test]
    fn missing_parameters_do_not_run_a_library() {
        assert!(parse(vec![]).is_err());
    }
}
