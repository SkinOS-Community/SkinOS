//! A native launch is selected by Steam metadata; filesystem discovery only
//! validates that selection and locates a uniquely associated Steamworks SDK.
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct NativeLaunchPlan {
    pub install_root: PathBuf,
    pub target: PathBuf,
    pub executable: PathBuf,
    pub kind: &'static str,
    pub cwd: PathBuf,
    pub interpreter: Option<PathBuf>,
    pub interpreter_argument: Option<String>,
    pub target_architectures: Vec<String>,
    pub sdk: Option<PathBuf>,
    pub sdk_architectures: Vec<String>,
    pub sdk_probe_reason: &'static str,
}

impl NativeLaunchPlan {
    pub fn from_request(request: &Value) -> Result<Self, String> {
        let install_root = canonical_directory(request, "install_dir")?;
        let target = canonical_member(&install_root, field(request, "executable")?)?;
        let cwd = fs::canonicalize(field(request, "working_dir")?)
            .map_err(|e| format!("Native working directory is unavailable: {e}"))?;
        if !cwd.starts_with(&install_root) {
            return Err("Native working directory escapes the selected installation".into());
        }
        if !cwd.is_dir() {
            return Err("Native working directory is not a directory".into());
        }
        let arguments = request["arguments"]
            .as_array()
            .ok_or("Native launch arguments must be an array")?;
        if arguments.len() > 128
            || arguments.iter().any(|arg| {
                arg.as_str()
                    .is_none_or(|text| text.contains('\0') || text.len() > 16 * 1024)
            })
        {
            return Err("Native launch arguments are invalid or too large".into());
        }
        let (kind, executable, interpreter, interpreter_argument) = if target.is_dir() {
            if target.extension().is_none_or(|ext| ext != "app") {
                return Err("Selected native directory is not an application bundle".into());
            }
            let plist = canonical_member(&target, &target.join("Contents/Info.plist"))?;
            if fs::metadata(&plist).map_err(|e| e.to_string())?.len() > 1024 * 1024 {
                return Err("Native bundle Info.plist exceeds the metadata limit".into());
            }
            let output = Command::new("/usr/bin/plutil")
                .args(["-extract", "CFBundleExecutable", "raw", "-o", "-", "--"])
                .arg(plist)
                .output()
                .map_err(|e| format!("Cannot inspect native bundle executable: {e}"))?;
            if !output.status.success() {
                return Err("Native bundle has no valid CFBundleExecutable".into());
            }
            let name = String::from_utf8(output.stdout)
                .map_err(|_| "Native bundle executable name is not UTF-8")?;
            let name = name.trim_end_matches(['\n', '\r']);
            if name.is_empty() || name.contains(['/', '\\', '\0']) || matches!(name, "." | "..") {
                return Err("Native bundle executable name is invalid".into());
            }
            let executable = canonical_member(&target, &target.join("Contents/MacOS").join(name))?;
            if !executable.is_file() || macho_architectures(&executable)?.is_empty() {
                return Err("Native bundle executable is not a Mach-O file".into());
            }
            ("bundle", executable, None, None)
        } else if target.is_file() {
            if !macho_architectures(&target)?.is_empty() {
                ("binary", target.clone(), None, None)
            } else {
                let (interpreter, argument) = script_interpreter(&target)?;
                ("script", target.clone(), Some(interpreter), argument)
            }
        } else {
            return Err(
                "Selected native target is not an ordinary file or application bundle".into(),
            );
        };
        let target_architectures = if kind == "script" {
            vec![]
        } else {
            macho_architectures(&executable)?
        };
        let (candidate, mut sdk_probe_reason) =
            resolve_sdk(&install_root, &target, &executable, kind);
        let sdk_architectures = candidate
            .as_ref()
            .and_then(|path| macho_architectures(path).ok())
            .unwrap_or_default();
        let sdk = if candidate.is_some() && !sdk_architectures.iter().any(|arch| arch == "arm64") {
            sdk_probe_reason = "incompatible";
            None
        } else {
            candidate
        };
        Ok(Self {
            install_root,
            target,
            executable,
            kind,
            cwd,
            interpreter,
            interpreter_argument,
            target_architectures,
            sdk,
            sdk_architectures,
            sdk_probe_reason,
        })
    }

    pub fn configure(&self, command: &mut Command) {
        command
            .env("SKINOS_GAME_INSTALL_ROOT", &self.install_root)
            .env("SKINOS_GAME_BUNDLE", &self.target)
            .env("SKINOS_GAME_WORKING_DIR", &self.cwd)
            .env("SKINOS_NATIVE_TARGET_KIND", self.kind)
            .env("SKINOS_NATIVE_EXECUTABLE", &self.executable)
            .env_remove("SKINOS_NATIVE_INTERPRETER")
            .env_remove("SKINOS_NATIVE_INTERPRETER_ARGUMENT")
            .env_remove("SKINOS_NATIVE_SDK_PATH")
            .env("SKINOS_NATIVE_SDK_PROBE_SUPPORTED", "0")
            .env("SKINOS_NATIVE_SDK_PROBE_REASON", self.sdk_probe_reason);
        if let Some(sdk) = &self.sdk {
            command.env("SKINOS_NATIVE_SDK_PATH", sdk);
            if self.sdk_architectures.iter().any(|arch| arch == "arm64") {
                command.env("SKINOS_NATIVE_SDK_PROBE_SUPPORTED", "1");
            }
        }
        if let Some(interpreter) = &self.interpreter {
            command.env("SKINOS_NATIVE_INTERPRETER", interpreter);
        }
        if let Some(argument) = &self.interpreter_argument {
            command.env("SKINOS_NATIVE_INTERPRETER_ARGUMENT", argument);
        }
    }
}

fn field<'a>(request: &'a Value, name: &str) -> Result<&'a Path, String> {
    request[name]
        .as_str()
        .map(Path::new)
        .ok_or_else(|| format!("Native launch is missing {name}"))
}

fn canonical_directory(request: &Value, name: &str) -> Result<PathBuf, String> {
    let path = fs::canonicalize(field(request, name)?)
        .map_err(|e| format!("Native {name} is unavailable: {e}"))?;
    if !path.is_dir() {
        return Err(format!("Native {name} must be a directory"));
    }
    Ok(path)
}

fn canonical_member(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let path =
        fs::canonicalize(path).map_err(|e| format!("Native launch file is unavailable: {e}"))?;
    if !path.starts_with(root) || path == root {
        return Err("Native launch path escapes the selected installation or bundle".into());
    }
    Ok(path)
}

fn script_interpreter(path: &Path) -> Result<(PathBuf, Option<String>), String> {
    let mut head = [0_u8; 512];
    let len = File::open(path)
        .and_then(|mut file| file.read(&mut head))
        .map_err(|e| format!("Cannot inspect native script: {e}"))?;
    let head = &head[..len];
    let end = head.iter().position(|byte| *byte == b'\n').unwrap_or(len);
    if !head.starts_with(b"#!") || end == 512 {
        return Err(
            "Selected native file is neither Mach-O nor a script with a bounded shebang".into(),
        );
    }
    let line = std::str::from_utf8(&head[2..end])
        .map_err(|_| "Native script shebang is not UTF-8")?
        .trim();
    let mut fields = line.split_whitespace();
    let interpreter = fields
        .next()
        .ok_or("Native script interpreter is missing")?;
    let rest: Vec<&str> = fields.collect();
    let (interpreter, argument) = if interpreter == "/usr/bin/env" {
        // Resolve only a single explicit, known interpreter. No PATH lookup,
        // assignments, env -S splitting, or command-language evaluation.
        let [name] = rest.as_slice() else {
            return Err(
                "Native env shebang must name one supported interpreter without options".into(),
            );
        };
        let path = match *name {
            "sh" => "/bin/sh",
            "bash" => "/bin/bash",
            _ => return Err("Native env shebang names an unsupported interpreter".into()),
        };
        (path, None)
    } else {
        if rest.len() > 1 {
            return Err("Native script shebang has ambiguous interpreter options".into());
        }
        (interpreter, rest.first().map(|value| (*value).to_string()))
    };
    if !matches!(interpreter, "/bin/sh" | "/bin/bash") {
        return Err("Native script must declare /bin/sh or /bin/bash; other interpreters are not supported yet".into());
    }
    if argument.as_ref().is_some_and(|option| {
        !option.starts_with('-')
            || option.len() < 2
            || !option[1..].bytes().all(|flag| b"eufvx".contains(&flag))
    }) {
        return Err("Native shell shebang option is not supported".into());
    }
    let interpreter = fs::canonicalize(interpreter)
        .map_err(|e| format!("Native script interpreter is unavailable: {e}"))?;
    Ok((interpreter, argument))
}

/// Read only the executable header, never load a game's code to classify it.
pub fn macho_architectures(path: &Path) -> Result<Vec<String>, String> {
    let mut bytes = [0_u8; 4096];
    let count = File::open(path)
        .and_then(|mut file| file.read(&mut bytes))
        .map_err(|e| format!("Cannot inspect native binary header: {e}"))?;
    let bytes = &bytes[..count];
    if bytes.len() < 8 {
        return Ok(vec![]);
    }
    let be = |offset: usize| u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap());
    let le = |offset: usize| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
    let mut cpus = vec![];
    match be(0) {
        0xfeedface | 0xfeedfacf => cpus.push(be(4)),
        0xcefaedfe | 0xcffaedfe => cpus.push(le(4)),
        magic @ (0xcafebabe | 0xbebafeca | 0xcafebabf | 0xbfbafeca) => {
            let little = matches!(magic, 0xbebafeca | 0xbfbafeca);
            let read = |offset| if little { le(offset) } else { be(offset) };
            let entries = read(4) as usize;
            let width = if matches!(magic, 0xcafebabf | 0xbfbafeca) {
                32
            } else {
                20
            };
            if entries == 0 || entries > 64 || 8 + entries * width > bytes.len() {
                return Err("Native universal binary has an invalid architecture table".into());
            }
            for index in 0..entries {
                cpus.push(read(8 + index * width));
            }
        }
        _ => return Ok(vec![]),
    }
    let mut result = BTreeSet::new();
    for cpu in cpus {
        result.insert(match cpu {
            0x01000007 => "x86_64".into(),
            0x0100000c => "arm64".into(),
            7 => "i386".into(),
            12 => "arm".into(),
            other => format!("cpu-{other}"),
        });
    }
    Ok(result.into_iter().collect())
}

fn resolve_sdk(
    root: &Path,
    target: &Path,
    executable: &Path,
    kind: &str,
) -> (Option<PathBuf>, &'static str) {
    resolve_sdk_with_limits(root, target, executable, kind, 100_000, 12)
}

fn resolve_sdk_with_limits(
    root: &Path,
    target: &Path,
    executable: &Path,
    kind: &str,
    max_entries: usize,
    max_depth: u8,
) -> (Option<PathBuf>, &'static str) {
    let Some(parent) = executable.parent() else {
        return (None, "invalid_sdk");
    };
    let nearby = parent.join("libsteam_api.dylib");
    if fs::symlink_metadata(&nearby).is_ok() {
        return match validate_sdk(root, &nearby) {
            Ok(path) => (Some(path), "ready"),
            Err(_) => (None, "invalid_sdk"),
        };
    }
    // Optional SDK verification never blocks a valid Steam launch. Incomplete
    // discovery cannot authorize an arbitrary library, so report why it skips.
    let scan_root = if kind == "bundle" { target } else { root };
    let mut stack = vec![(scan_root.to_path_buf(), 0_u8)];
    let mut candidates = BTreeSet::new();
    let mut visited = 0_usize;
    while let Some((directory, depth)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            return (None, "unavailable");
        };
        for entry in entries {
            visited += 1;
            if visited > max_entries {
                return (None, "bounded_scan");
            }
            let Ok(entry) = entry else {
                return (None, "unavailable");
            };
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                return (None, "unavailable");
            };
            if entry.file_name() == "libsteam_api.dylib" {
                let Ok(path) = validate_sdk(root, &path) else {
                    return (None, "invalid_sdk");
                };
                candidates.insert(path);
                if candidates.len() > 1 {
                    return (None, "ambiguous");
                }
            } else if metadata.is_dir() {
                if depth >= max_depth {
                    return (None, "bounded_scan");
                }
                stack.push((path, depth + 1));
            }
        }
    }
    match candidates.len() {
        0 => (None, "missing"),
        1 => (candidates.into_iter().next(), "ready"),
        _ => (None, "ambiguous"),
    }
}

fn validate_sdk(root: &Path, candidate: &Path) -> Result<PathBuf, String> {
    let path = canonical_member(root, candidate)?;
    if !path.is_file() || macho_architectures(&path)?.is_empty() {
        return Err("Native Steamworks SDK is not a Mach-O library".into());
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "skinos-native-plan-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
        fn binary(&self, relative: &str) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, [0xcf, 0xfa, 0xed, 0xfe, 12, 0, 0, 1, 0, 0, 0, 0]).unwrap();
            path
        }
        fn request(&self, target: &Path) -> Value {
            json!({"install_dir":self.0,"executable":target,"working_dir":self.0,"arguments":["-test","two words"]})
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn binary_with_nearby_sdk_preserves_architecture() {
        let f = Fixture::new();
        let binary = f.binary("bin/game");
        let sdk = f.binary("bin/libsteam_api.dylib");
        let plan = NativeLaunchPlan::from_request(&f.request(&binary)).unwrap();
        assert_eq!(plan.kind, "binary");
        assert_eq!(plan.sdk, Some(sdk));
        assert_eq!(plan.sdk_architectures, ["arm64"]);
    }
    #[test]
    fn script_keeps_declared_launcher_and_discovers_unique_nested_sdk() {
        let f = Fixture::new();
        let script = f.0.join("launch.sh");
        fs::write(&script, b"#!/bin/sh\nexec ./bin/game \"$@\"\n").unwrap();
        let sdk = f.binary("bin/osx64/libsteam_api.dylib");
        let plan = NativeLaunchPlan::from_request(&f.request(&script)).unwrap();
        assert_eq!(plan.kind, "script");
        assert_eq!(plan.target, script);
        assert_eq!(plan.interpreter, Some(fs::canonicalize("/bin/sh").unwrap()));
        assert_eq!(plan.sdk, Some(sdk));
    }
    #[test]
    fn bundle_uses_info_plist_and_framework_sdk() {
        let f = Fixture::new();
        let app = f.0.join("Game.app");
        let executable = f.binary("Game.app/Contents/MacOS/Game");
        fs::write(app.join("Contents/Info.plist"),br#"<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleExecutable</key><string>Game</string></dict></plist>"#).unwrap();
        let sdk = f.binary("Game.app/Contents/Frameworks/libsteam_api.dylib");
        let plan = NativeLaunchPlan::from_request(&f.request(&app)).unwrap();
        assert_eq!(plan.kind, "bundle");
        assert_eq!(plan.executable, executable);
        assert_eq!(plan.sdk, Some(sdk));
    }
    #[test]
    fn no_sdk_is_a_valid_standalone_game() {
        let f = Fixture::new();
        let target = f.binary("game");
        assert!(
            NativeLaunchPlan::from_request(&f.request(&target))
                .unwrap()
                .sdk
                .is_none()
        );
    }
    #[test]
    fn ambiguous_sdk_does_not_pick_arbitrary_file() {
        let f = Fixture::new();
        let target = f.binary("game");
        f.binary("first/libsteam_api.dylib");
        f.binary("second/libsteam_api.dylib");
        let plan = NativeLaunchPlan::from_request(&f.request(&target)).unwrap();
        assert!(plan.sdk.is_none());
        assert_eq!(plan.sdk_probe_reason, "ambiguous");
    }
    #[test]
    fn executable_and_sdk_symlink_escapes_are_rejected() {
        let f = Fixture::new();
        let outside = Fixture::new();
        let external = outside.binary("game");
        let link = f.0.join("game");
        std::os::unix::fs::symlink(&external, &link).unwrap();
        assert!(
            NativeLaunchPlan::from_request(&f.request(&link))
                .unwrap_err()
                .contains("escapes")
        );
        let target = f.binary("actual-game");
        std::os::unix::fs::symlink(external, f.0.join("libsteam_api.dylib")).unwrap();
        let plan = NativeLaunchPlan::from_request(&f.request(&target)).unwrap();
        assert!(plan.sdk.is_none());
        assert_eq!(plan.sdk_probe_reason, "invalid_sdk");
    }
    #[test]
    fn cwd_outside_install_and_nonstring_arguments_are_rejected() {
        let f = Fixture::new();
        let target = f.binary("game");
        let mut request = f.request(&target);
        request["working_dir"] = json!("/Applications");
        assert!(NativeLaunchPlan::from_request(&request).is_err());
        request = f.request(&target);
        request["arguments"] = json!([1]);
        assert!(NativeLaunchPlan::from_request(&request).is_err());
    }
    #[test]
    fn sdk_scan_bounds_are_diagnostic_not_launch_failures() {
        let f = Fixture::new();
        let binary = f.binary("bin/game");
        f.binary("deeper/nested/libsteam_api.dylib");
        for (entries, depth) in [(1, 12), (100_000, 0)] {
            let (sdk, reason) =
                resolve_sdk_with_limits(&f.0, &binary, &binary, "binary", entries, depth);
            assert!(sdk.is_none());
            assert_eq!(reason, "bounded_scan");
        }
        let deep = f.0.join(["nested"; 14].join("/"));
        fs::create_dir_all(deep).unwrap();
        let plan = NativeLaunchPlan::from_request(&f.request(&binary)).unwrap();
        assert!(plan.sdk.is_none());
        assert_eq!(plan.sdk_probe_reason, "bounded_scan");
    }
    #[test]
    fn incompatible_sdk_does_not_block_native_target() {
        let f = Fixture::new();
        let binary = f.binary("game");
        fs::write(
            f.0.join("libsteam_api.dylib"),
            [0xcf, 0xfa, 0xed, 0xfe, 7, 0, 0, 1],
        )
        .unwrap();
        let plan = NativeLaunchPlan::from_request(&f.request(&binary)).unwrap();
        assert!(plan.sdk.is_none());
        assert_eq!(plan.sdk_probe_reason, "incompatible");
        assert_eq!(plan.sdk_architectures, ["x86_64"]);
    }
    #[test]
    fn bundle_executable_cannot_escape_bundle() {
        let f = Fixture::new();
        let binary = f.binary("outside/Game");
        let app = f.0.join("Game.app");
        fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        fs::write(app.join("Contents/Info.plist"), br#"<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleExecutable</key><string>Game</string></dict></plist>"#).unwrap();
        std::os::unix::fs::symlink(binary, app.join("Contents/MacOS/Game")).unwrap();
        assert!(
            NativeLaunchPlan::from_request(&f.request(&app))
                .unwrap_err()
                .contains("escapes")
        );
    }
    #[test]
    fn universal_header_reports_both_probe_architectures() {
        let f = Fixture::new();
        let path = f.0.join("universal");
        let mut bytes = vec![0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 2];
        for cpu in [0x01000007_u32, 0x0100000c] {
            bytes.extend_from_slice(&cpu.to_be_bytes());
            bytes.extend_from_slice(&[0; 16]);
        }
        fs::write(&path, bytes).unwrap();
        assert_eq!(macho_architectures(&path).unwrap(), ["arm64", "x86_64"]);
    }
    #[test]
    fn script_control_options_are_rejected_before_spawn() {
        let f = Fixture::new();
        let path = f.0.join("launcher");
        fs::write(&path, b"#!/bin/bash -c\nexit 0\n").unwrap();
        assert!(
            NativeLaunchPlan::from_request(&f.request(&path))
                .unwrap_err()
                .contains("option")
        );
        fs::write(&path, b"#!/bin/bash -eu\nexit 0\n").unwrap();
        assert_eq!(
            NativeLaunchPlan::from_request(&f.request(&path))
                .unwrap()
                .interpreter_argument
                .as_deref(),
            Some("-eu")
        );
    }
    #[test]
    fn env_shebang_only_resolves_a_single_fixed_interpreter() {
        let f = Fixture::new();
        let target = f.0.join("launch");
        fs::write(&target, b"#!/usr/bin/env bash\nexit 0\n").unwrap();
        assert_eq!(
            NativeLaunchPlan::from_request(&f.request(&target))
                .unwrap()
                .interpreter,
            Some(fs::canonicalize("/bin/bash").unwrap())
        );
        fs::write(&target, b"#!/usr/bin/env PATH=/tmp bash\nexit 0\n").unwrap();
        assert!(NativeLaunchPlan::from_request(&f.request(&target)).is_err());
    }
}
