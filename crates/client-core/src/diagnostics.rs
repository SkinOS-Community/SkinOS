//! Read-only, bounded launch diagnostics. Paths never come from an IPC request.
//! Only fixed log filenames inside SkinOS's own verified run are read.
use anyhow::{Context, Result, ensure};
use regex::Regex;
use serde_json::{Value, json};
use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::OpenOptionsExt,
    },
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_TEXT: usize = 200 * 1024;
const MAX_TAIL: usize = 24 * 1024;
const LOG_NAMES: [&str; 10] = [
    "worker.stderr.log",
    "compat-game.stderr.log",
    "compat-sdk.stderr.log",
    "game.stderr.log",
    "sdk-probe.stderr.log",
    "worker.stdout.log",
    "compat-game.stdout.log",
    "compat-sdk.stdout.log",
    "game.stdout.log",
    "broker.stderr.log",
];

struct Directory {
    file: File,
    path: PathBuf,
}
struct LogBytes {
    bytes: Vec<u8>,
    modified: u64,
    truncated: bool,
}

impl Directory {
    fn open(path: &Path) -> Result<Self> {
        ensure!(
            !path.is_symlink(),
            "Diagnostic directory cannot be a symlink"
        );
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
            .open(path)?;
        Ok(Self {
            file,
            path: path.canonicalize()?,
        })
    }
    fn child(&self, name: &str) -> Result<Self> {
        let file = self.open_file(name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
        Ok(Self {
            file,
            path: self.path.join(name),
        })
    }
    fn open_file(&self, name: &str, flags: i32, mode: u32) -> Result<File> {
        ensure!(
            !name.is_empty() && !name.contains('/') && !matches!(name, "." | ".."),
            "Invalid diagnostic filename"
        );
        let name = CString::new(name)?;
        // Relative openat + NOFOLLOW confines reads/writes to the already-open
        // owned directory even if another process renames a parent directory.
        let descriptor = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                mode,
            )
        };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
    fn read(&self, name: &str, limit: usize, tail: bool) -> Result<Option<LogBytes>> {
        let mut file = match self.open_file(name, libc::O_RDONLY, 0) {
            Ok(file) => file,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file(),
            "Diagnostic source is not a regular file"
        );
        let truncated = metadata.len() > limit as u64;
        if tail && truncated {
            file.seek(SeekFrom::End(-(limit as i64)))?;
        }
        let mut bytes = Vec::new();
        file.take(limit as u64).read_to_end(&mut bytes)?;
        if tail && truncated {
            // Do not reveal a partial secret whose key/JWT prefix was cut off.
            let start = bytes
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|index| index + 1)
                .unwrap_or(bytes.len());
            bytes.drain(..start);
        }
        Ok(Some(LogBytes {
            bytes,
            truncated,
            modified: timestamp(metadata.modified().unwrap_or(UNIX_EPOCH)),
        }))
    }
    fn write_json(&self, name: &str, value: &Value) -> Result<()> {
        let temporary = format!(
            ".{name}.{}.{}.tmp",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let mut file = self.open_file(
            &temporary,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )?;
        file.write_all(&serde_json::to_vec(value)?)?;
        file.sync_all()?;
        let source = CString::new(temporary)?;
        let destination = CString::new(name)?;
        let result = unsafe {
            libc::renameat(
                self.file.as_raw_fd(),
                source.as_ptr(),
                self.file.as_raw_fd(),
                destination.as_ptr(),
            )
        };
        if result != 0 {
            unsafe {
                libc::unlinkat(self.file.as_raw_fd(), source.as_ptr(), 0);
            }
            return Err(std::io::Error::last_os_error().into());
        }
        self.file.sync_all()?;
        Ok(())
    }
}

pub fn record_launch_error(appid: u32, error: &str) -> Result<()> {
    record_error_at(&crate::session::data_dir()?, appid, error)
}

fn record_error_at(root: &Path, appid: u32, error: &str) -> Result<()> {
    ensure!(appid > 0, "Invalid AppID");
    let directory = error_directory(root, true)?.context("Diagnostic directory unavailable")?;
    let (message, truncated) = bounded_text(&sanitize(error), 16 * 1024);
    directory.write_json(&format!("{appid}.json"), &json!({"appid":appid,"updated_at":timestamp(SystemTime::now()),"stage":"core","message":message,"truncated":truncated}))
}

pub fn clear_launch_error(appid: u32) -> Result<()> {
    ensure!(appid > 0, "Invalid AppID");
    let Some(directory) = error_directory(&crate::session::data_dir()?, false)? else {
        return Ok(());
    };
    let name = CString::new(format!("{appid}.json"))?;
    if unsafe { libc::unlinkat(directory.file.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error.into());
        }
    }
    Ok(())
}

fn error_directory(root: &Path, create: bool) -> Result<Option<Directory>> {
    if !root.exists() && !create {
        return Ok(None);
    }
    ensure!(
        !root.is_symlink(),
        "SkinOS data directory cannot be a symlink"
    );
    if create {
        std::fs::create_dir_all(root)?;
    }
    let directory = Directory::open(root)?;
    if create {
        let name = CString::new("diagnostics")?;
        let result = unsafe { libc::mkdirat(directory.file.as_raw_fd(), name.as_ptr(), 0o700) };
        if result != 0
            && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
        {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    match directory.child("diagnostics") {
        Ok(directory) => Ok(Some(directory)),
        Err(error)
            if !create
                && error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

pub fn load_game_log(appid: u32) -> Result<Value> {
    load_from(&crate::session::data_dir()?, appid)
}

fn load_from(root: &Path, appid: u32) -> Result<Value> {
    ensure!(appid > 0, "Invalid AppID");
    let mut text = format!(
        "Диагностика запуска · AppID {appid}\nСекреты и токены скрыты. Показаны последние записи выбранного запуска.\n"
    );
    let mut updated = 0;
    let mut truncated = false;
    let mut log_id = format!("game-{appid}-no-run");
    if let Some(directory) = error_directory(root, false)?
        && let Some(file) = directory.read(&format!("{appid}.json"), 32 * 1024, false)?
        && !file.truncated
        && let Ok(error) = serde_json::from_slice::<Value>(&file.bytes)
        && error["appid"].as_u64() == Some(u64::from(appid))
    {
        updated = file.modified;
        truncated |= error["truncated"].as_bool().unwrap_or(false);
        text.push_str("\n=== Ошибка подготовки/запуска (core) ===\n");
        text.push_str(&sanitize(
            error["message"].as_str().unwrap_or("Ошибка без описания"),
        ));
        text.push('\n');
    }
    if let Some((name, run)) = latest_run(root, appid)? {
        log_id = name.clone();
        text.push_str(&format!("\nЗапуск: {name}\n"));
        if let Ok(Some(file)) = run.read("report.json", 256 * 1024, false) {
            updated = updated.max(file.modified);
            truncated |= file.truncated;
            if !file.truncated
                && let Ok(report) = serde_json::from_slice::<Value>(&file.bytes)
            {
                text.push_str("\n=== Результат runtime ===\n");
                for key in [
                    "exit_code",
                    "termination_signal",
                    "timed_out",
                    "authenticated",
                    "game_launch_verified",
                    "clean_release_completed",
                ] {
                    if let Some(value) = report.get(key) {
                        text.push_str(&format!("{key}: {value}\n"));
                    }
                }
                if let Some(events) = report["events"].as_array() {
                    for event in events
                        .iter()
                        .filter(|event| event["event"] == "error" || event["ok"] == false)
                        .rev()
                        .take(8)
                    {
                        text.push_str(&format!("{}\n", format_event(event)));
                    }
                }
            }
        }
        for name in LOG_NAMES {
            match run.read(name, MAX_TAIL, true) {
                Ok(Some(file)) if !file.bytes.is_empty() => {
                    updated = updated.max(file.modified);
                    truncated |= file.truncated;
                    text.push_str(&format!(
                        "\n=== {name}{} ===\n",
                        if file.truncated {
                            " · последние записи"
                        } else {
                            ""
                        }
                    ));
                    let content = String::from_utf8_lossy(&file.bytes);
                    for line in content.lines() {
                        if name == "worker.stdout.log"
                            && let Ok(event) = serde_json::from_str::<Value>(
                                line.strip_prefix("SKINOS_EVENT ").unwrap_or(line),
                            )
                        {
                            text.push_str(&format_event(&event));
                        } else {
                            text.push_str(&sanitize(line));
                        }
                        text.push('\n');
                    }
                }
                Ok(_) => {}
                Err(_) => {
                    text.push_str(&format!(
                        "\n{name}: файл недоступен или исключён проверкой пути.\n"
                    ));
                }
            }
        }
    } else {
        text.push_str("\nПодходящий runtime-лог ещё не создан. Ошибки подготовки, если они есть, показаны выше.\n");
    }
    let (text, cut) = bounded_text(&text, MAX_TEXT);
    Ok(
        json!({"appid":appid,"log_id":log_id,"text":text,"updated_at":if updated == 0 { None } else { Some(updated) },"truncated":truncated || cut}),
    )
}

fn latest_run(root: &Path, appid: u32) -> Result<Option<(String, Directory)>> {
    if !root.exists() {
        return Ok(None);
    }
    let data = Directory::open(root)?;
    let runtime = match data.child("runtime") {
        Ok(runtime) => runtime,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error).context("SkinOS runtime diagnostics unavailable"),
    };
    let mut runs = Vec::new();
    for entry in std::fs::read_dir(&runtime.path)?.take(8192).flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !valid_run_name(&name) || !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(UNIX_EPOCH);
        let created = name
            .rsplit('-')
            .next()
            .and_then(|value| value.parse::<u128>().ok())
            .unwrap_or(0);
        runs.push((created, modified, name));
    }
    runs.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    for (_, _, name) in runs.into_iter().take(512) {
        let Ok(run) = runtime.child(&name) else {
            continue;
        };
        if run_matches(&run, appid) {
            return Ok(Some((name, run)));
        }
    }
    Ok(None)
}

fn valid_run_name(name: &str) -> bool {
    name.strip_prefix("probe-").is_some_and(|tail| {
        !tail.is_empty()
            && tail
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'-')
    })
}

fn run_matches(run: &Directory, appid: u32) -> bool {
    match run.read("launch-meta.json", 16 * 1024, false) {
        Ok(Some(file)) => {
            return !file.truncated
                && serde_json::from_slice::<Value>(&file.bytes)
                    .is_ok_and(|value| value["appid"].as_u64() == Some(u64::from(appid)));
        }
        Err(_) => return false,
        Ok(None) => {}
    }
    if let Ok(Some(file)) = run.read("report.json", 256 * 1024, false)
        && !file.truncated
        && let Ok(report) = serde_json::from_slice::<Value>(&file.bytes)
        && report["events"]
            .as_array()
            .is_some_and(|events| events.iter().any(|event| event_matches(event, appid)))
    {
        return true;
    }
    if let Ok(Some(file)) = run.read("worker.stdout.log", 64 * 1024, true) {
        return String::from_utf8_lossy(&file.bytes).lines().any(|line| {
            serde_json::from_str::<Value>(line.strip_prefix("SKINOS_EVENT ").unwrap_or(line))
                .is_ok_and(|event| event_matches(&event, appid))
        });
    }
    false
}

fn event_matches(event: &Value, appid: u32) -> bool {
    event["appid"].as_u64() == Some(u64::from(appid))
        || event["detail"]["appid"].as_u64() == Some(u64::from(appid))
}

fn format_event(event: &Value) -> String {
    let name = event["event"].as_str().unwrap_or("runtime");
    let result = match event["ok"].as_bool() {
        Some(true) => "OK",
        Some(false) => "ОШИБКА",
        None => "",
    };
    let detail = event
        .get("detail")
        .map(|value| value.to_string())
        .unwrap_or_default();
    sanitize(&format!("[{result}] {name}: {detail}"))
}

/// Removes credentials from bounded runtime/core error text before UI display.
pub fn redact(value: &str) -> String {
    sanitize(value)
}

fn sanitize(value: &str) -> String {
    static JWT: OnceLock<Regex> = OnceLock::new();
    static SECRET: OnceLock<Regex> = OnceLock::new();
    static BEARER: OnceLock<Regex> = OnceLock::new();
    static ANSI: OnceLock<Regex> = OnceLock::new();
    let jwt = JWT
        .get_or_init(|| Regex::new(r"\bey[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+").unwrap());
    let secret = SECRET.get_or_init(|| Regex::new(r#"(?i)([A-Za-z0-9_-]*(?:token|password|passwd|secret|api[_-]?key|authorization|cookie|sessionid|steamloginsecure|steamrefresh_steam)[A-Za-z0-9_-]*[\"']?\s*[:=]\s*).*$"#).unwrap());
    let bearer = BEARER.get_or_init(|| Regex::new(r"(?i)\bBearer\s+\S+").unwrap());
    let ansi = ANSI.get_or_init(|| Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]").unwrap());
    value
        .lines()
        .map(|line| {
            let clean = ansi.replace_all(line, "");
            let clean = jwt.replace_all(&clean, "[TOKEN HIDDEN]");
            let clean = bearer.replace_all(&clean, "Bearer [HIDDEN]");
            let clean = secret.replace_all(&clean, "${1}[HIDDEN]");
            clean
                .chars()
                .filter(|ch| *ch == '\t' || !ch.is_control())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn bounded_text(text: &str, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (text.to_owned(), false);
    }
    let mut end = limit.saturating_sub(64);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (
        format!("{}\n[Лог сокращён до допустимого размера]\n", &text[..end]),
        true,
    )
}
fn timestamp(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn run(root: &Path, name: &str, appid: u32) -> PathBuf {
        let path = root.join("runtime").join(name);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            path.join("launch-meta.json"),
            json!({"appid":appid}).to_string(),
        )
        .unwrap();
        path
    }

    #[test]
    fn appid_selection_and_error_details_do_not_mix_games() {
        let root = tempfile::tempdir().unwrap();
        let chosen = run(root.path(), "probe-1-1", 42);
        std::fs::write(
            chosen.join("worker.stderr.log"),
            "SDK initialization failed: missing export",
        )
        .unwrap();
        let other = run(root.path(), "probe-2-2", 43);
        std::fs::write(other.join("worker.stderr.log"), "OTHER GAME PRIVATE LOG").unwrap();
        let data = load_from(root.path(), 42).unwrap();
        assert_eq!(data["log_id"], "probe-1-1");
        assert!(data["text"].as_str().unwrap().contains("missing export"));
        assert!(!data["text"].as_str().unwrap().contains("OTHER GAME"));
    }

    #[test]
    fn legacy_events_match_without_reading_credentials_or_external_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let path = run(root.path(), "probe-1-1", 42);
        std::fs::remove_file(path.join("launch-meta.json")).unwrap();
        std::fs::write(
            path.join("worker.stdout.log"),
            "SKINOS_EVENT {\"event\":\"appinfo_requested\",\"ok\":true,\"detail\":{\"appid\":42}}\n",
        )
        .unwrap();
        let secret = root.path().join("real-secret.token");
        std::fs::write(&secret, "DO NOT READ ME").unwrap();
        symlink(&secret, path.join("game.stderr.log")).unwrap();
        std::fs::write(path.join("credentials.json"), "DO NOT READ ME EITHER").unwrap();
        let data = load_from(root.path(), 42).unwrap();
        assert_eq!(data["log_id"], "probe-1-1");
        assert!(!data["text"].as_str().unwrap().contains("DO NOT READ"));
        let outer = tempfile::tempdir().unwrap();
        symlink(outer.path(), root.path().join("runtime/probe-9-9")).unwrap();
        assert_eq!(load_from(root.path(), 42).unwrap()["log_id"], "probe-1-1");
    }

    #[test]
    fn secrets_are_removed_without_hiding_useful_failures() {
        let value = "error: actual SDK failure\nJWT eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjMifQ.signature\nAuthorization: Bearer opaque\nCookie: steamLoginSecure=opaque; x=other\n{\"access_token\":\"unsafe\"}\nAWS_SECRET_ACCESS_KEY=abc\nrefresh_token=opaque\npassword: aaa\n\x1b[31mRED\x1b[0m";
        let text = sanitize(value);
        for secret in ["eyJhbGci", "opaque", "unsafe", "abc", "aaa", "\x1b"] {
            assert!(!text.contains(secret), "secret pattern remained");
        }
        assert!(text.contains("actual SDK failure"));
        assert!(text.contains("RED"));
    }

    #[test]
    fn bounded_tail_discards_partial_first_line() {
        let root = tempfile::tempdir().unwrap();
        let path = run(root.path(), "probe-1-1", 42);
        let huge = format!(
            "refresh_token={}\nvisible final failure\n",
            "x".repeat(MAX_TAIL * 2)
        );
        std::fs::write(path.join("game.stderr.log"), huge).unwrap();
        let data = load_from(root.path(), 42).unwrap();
        assert_eq!(data["truncated"], true);
        assert!(
            data["text"]
                .as_str()
                .unwrap()
                .contains("visible final failure")
        );
        assert!(!data["text"].as_str().unwrap().contains("xxxx"));
        assert!(data["text"].as_str().unwrap().len() <= MAX_TEXT);
    }

    #[test]
    fn preparation_errors_are_private_and_available_without_a_runtime() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        record_error_at(
            root.path(),
            42,
            "Account profile is busy\nrefresh_token=should-never-be-written",
        )
        .unwrap();
        let path = root.path().join("diagnostics/42.json");
        assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(
            root.path()
                .join("diagnostics")
                .metadata()
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("should-never-be-written")
        );
        let data = load_from(root.path(), 42).unwrap();
        assert!(
            data["text"]
                .as_str()
                .unwrap()
                .contains("Account profile is busy")
        );
        assert_eq!(data["log_id"], "game-42-no-run");
        assert_eq!(
            load_from(root.path(), 43).unwrap()["updated_at"],
            Value::Null
        );
    }

    #[test]
    fn symlinked_diagnostic_directory_is_never_written() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), root.path().join("diagnostics")).unwrap();
        assert!(record_error_at(root.path(), 42, "failure").is_err());
        assert!(!outside.path().join("42.json").exists());
    }
}
