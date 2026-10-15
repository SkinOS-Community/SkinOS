//! A transient, user-domain launchd job for a copied genuine Valve IPC broker.
//! It never unloads, replaces, or queries the original Valve job.
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

unsafe extern "C" {
    fn geteuid() -> u32;
}
type Result<T> = std::result::Result<T, String>;

pub struct Broker {
    pub service_name: String,
    pub label: String,
    domain: String,
    active: bool,
}

fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn path_text(path: &Path) -> Result<String> {
    let text = path.to_str().ok_or("Broker path is not UTF-8")?;
    if text.chars().any(|c| c < ' ') {
        return Err("Broker path contains an XML control character".into());
    }
    Ok(xml(text))
}

/// All arguments are separate argv elements. No shell, root domain or global job edits.
fn launchctl(args: &[&std::ffi::OsStr]) -> Result<Output> {
    let mut child = Command::new("/bin/launchctl")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Cannot execute own broker lifecycle command: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().map_err(|e| e.to_string()),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(40)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Own broker lifecycle command timed out".into());
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.to_string());
            }
        }
    }
}

impl Broker {
    pub fn start(state: &Path, runtime_dir: &Path, adapter: &Path) -> Result<Self> {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let state = state
            .canonicalize()
            .map_err(|e| format!("Invalid broker state: {e}"))?;
        let runtime = runtime_dir
            .canonicalize()
            .map_err(|e| format!("Invalid broker runtime: {e}"))?;
        let adapter = adapter
            .canonicalize()
            .map_err(|e| format!("Invalid broker adapter: {e}"))?;
        let ipcserver = runtime
            .join("ipcserver")
            .canonicalize()
            .map_err(|e| format!("Copied ipcserver is missing: {e}"))?;
        if !runtime.starts_with(&state)
            || !adapter.starts_with(&state)
            || !ipcserver.starts_with(&state)
            || !ipcserver.is_file()
            || !adapter.is_file()
        {
            return Err(
                "Broker executable and adapter must be copied inside this runtime state".into(),
            );
        }
        fs::set_permissions(&ipcserver, fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
        let data = state.join("data");
        fs::create_dir_all(&data).map_err(|e| e.to_string())?;
        let nonce = format!(
            "{:x}-{:x}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_nanos()
        );
        let label = format!("dev.skinos.ipc.{nonce}");
        let service_name = format!("dev.skinos.ipctool.{nonce}");
        let domain = format!("gui/{}", unsafe { geteuid() });
        let owner_file = state.join("runtime-owner.pid");
        let mut owner = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&owner_file)
            .map_err(|e| format!("Cannot create broker owner file: {e}"))?;
        writeln!(owner, "{}", std::process::id())
            .and_then(|_| owner.sync_all())
            .map_err(|e| e.to_string())?;
        let plist_path = state.join("broker.plist");
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{label}</string>
<key>ProgramArguments</key><array><string>{program}</string></array>
<key>WorkingDirectory</key><string>{runtime}</string>
<key>MachServices</key><dict><key>{service_name}</key><true/></dict>
<key>RunAtLoad</key><true/>
<key>KeepAlive</key><false/>
<key>ProcessType</key><string>Background</string>
<key>ExitTimeOut</key><integer>3</integer>
<key>StandardOutPath</key><string>{stdout}</string>
<key>StandardErrorPath</key><string>{stderr}</string>
<key>EnvironmentVariables</key><dict>
<key>DYLD_INSERT_LIBRARIES</key><string>{adapter}</string>
<key>SKINOS_IPC_SERVICE_NAME</key><string>{service_name}</string>
<key>SKINOS_COMPONENT_ROLE</key><string>broker</string>
<key>SKINOS_BROKER_LABEL</key><string>{label}</string>
<key>SKINOS_BROKER_OWNER_FILE</key><string>{owner_file}</string>
<key>SKINOS_BROKER_PARENT_PID</key><string>{parent_pid}</string>
<key>SKINOS_RUNTIME_DATA_ROOT</key><string>{data}</string>
<key>SKINOS_ADAPTER_PATH</key><string>{adapter}</string>
</dict></dict></plist>
"#,
            label = xml(&label),
            service_name = xml(&service_name),
            program = path_text(&ipcserver)?,
            runtime = path_text(&runtime)?,
            stdout = path_text(&state.join("broker.stdout.log"))?,
            stderr = path_text(&state.join("broker.stderr.log"))?,
            adapter = path_text(&adapter)?,
            data = path_text(&data)?,
            owner_file = path_text(&owner_file)?,
            parent_pid = std::process::id()
        );
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&plist_path)
            .map_err(|e| format!("Cannot create private broker plist: {e}"))?;
        file.write_all(plist.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        let mut broker = Self {
            service_name,
            label,
            domain,
            active: true,
        };
        let result = launchctl(&[
            "bootstrap".as_ref(),
            broker.domain.as_ref(),
            plist_path.as_os_str(),
        ]);
        match result {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
                broker.stop();
                return Err(format!("Own broker bootstrap failed: {detail}"));
            }
            Err(error) => {
                broker.stop();
                return Err(error);
            }
        }
        // Read only our exact job; never look up the well-known Valve Mach service.
        let target = format!("{}/{}", broker.domain, broker.label);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(output) = launchctl(&["print".as_ref(), target.as_ref()]) {
                let text = String::from_utf8_lossy(&output.stdout);
                if output.status.success()
                    && text.lines().any(|line| {
                        line.trim()
                            .strip_prefix("pid = ")
                            .and_then(|pid| pid.parse::<u32>().ok())
                            .is_some_and(|pid| pid > 0)
                    })
                {
                    return Ok(broker);
                }
            }
            if Instant::now() >= deadline {
                broker.stop();
                return Err(
                    "Owned IPC broker did not remain running; inspect its private stderr log"
                        .into(),
                );
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    pub fn stop(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        // Nonce-generated values are never populated from external configuration.
        if !self.label.starts_with("dev.skinos.ipc.") {
            return;
        }
        let target = format!("{}/{}", self.domain, self.label);
        let _ = launchctl(&["bootout".as_ref(), target.as_ref()]);
    }
}
impl Drop for Broker {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_are_xml_escaped() {
        assert_eq!(xml("a&b<\"'>"), "a&amp;b&lt;&quot;&apos;&gt;");
    }
    #[test]
    fn controls_are_rejected() {
        assert!(path_text(&std::path::PathBuf::from("/tmp/a\nb")).is_err());
    }
}
