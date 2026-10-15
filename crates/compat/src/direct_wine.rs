//! Read-only construction of a direct, owned Wine feeder command.
//! This never prepares a prefix, installs a driver or changes SDK/game launch.
use crate::LaunchPlan;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    os::unix::fs::PermissionsExt,
    path::{Component, Path, PathBuf},
    process::Command,
};

type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriverArchitecture {
    Arm64,
    X64,
}
impl DriverArchitecture {
    pub fn label(self) -> &'static str {
        match self {
            Self::Arm64 => "arm64",
            Self::X64 => "x64",
        }
    }
    fn machine(self) -> u16 {
        match self {
            Self::Arm64 => 0xaa64,
            Self::X64 => 0x8664,
        }
    }
}

/// A prepared prefix and its selected runner; all paths keep their original
/// spelling. In particular loader canonicalization can change Wine's helper
/// identity, so Command.program and WINELOADER are the same literal staged path.
#[derive(Clone, Debug)]
pub struct Context {
    pub loader: PathBuf,
    pub server: PathBuf,
    pub prefix: PathBuf,
    pub runner_root: PathBuf,
    pub unix_architecture: &'static str,
    pub driver_arch: DriverArchitecture,
    pub winedevice: PathBuf,
    pub environment: BTreeMap<String, String>,
}
impl Context {
    pub fn from_plan(plan: &LaunchPlan, home: &Path) -> Result<Self> {
        Self::from_plan_with_environment(plan, home, std::env::vars_os().collect())
    }
    fn from_plan_with_environment(
        plan: &LaunchPlan,
        home: &Path,
        inherited: BTreeMap<OsString, OsString>,
    ) -> Result<Self> {
        absolute_clean(&plan.runner_root, "runner root")?;
        absolute_clean(&plan.prefix_dir, "prefix directory")?;
        absolute_clean(home, "home directory")?;
        let loader = PathBuf::from(
            plan.env
                .get("SKINOS_WINELOADER")
                .ok_or("Launch plan is missing its staged Wine loader")?,
        );
        absolute_clean(&loader, "staged Wine loader")?;
        let (unix_architecture, expected_arch) = if loader
            == plan
                .runner_root
                .join("skinos-launcher/aarch64-unix/Wine.app/Contents/MacOS/wine")
        {
            ("aarch64-unix", DriverArchitecture::Arm64)
        } else if loader
            == plan
                .runner_root
                .join("skinos-launcher/x86_64-unix/Wine.app/Contents/MacOS/wine")
        {
            ("x86_64-unix", DriverArchitecture::X64)
        } else {
            return Err(
                "Direct Wine helpers require the exact staged loader of the selected runner".into(),
            );
        };
        let metadata = fs::symlink_metadata(&loader)
            .map_err(|error| format!("Cannot inspect staged Wine loader: {error}"))?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
            return Err(
                "Staged Wine loader must be an executable copy or hardlink, not a symlink".into(),
            );
        }
        let unix = plan.runner_root.join("lib/wine").join(unix_architecture);
        let windows = plan.runner_root.join("lib/wine/x86_64-windows");
        if !unix.is_dir() || !windows.is_dir() {
            return Err("Selected Wine runtime directories are missing".into());
        }
        let server = match expected_arch {
            DriverArchitecture::Arm64 => plan.runner_root.join("bin/wineserver-arm64"),
            DriverArchitecture::X64 => {
                let primary = plan.runner_root.join("bin/wineserver");
                if executable(&primary) {
                    primary
                } else {
                    plan.runner_root.join("bin/wineserver-x86")
                }
            }
        };
        if !executable(&server) {
            return Err("Matching wineserver is missing from the selected runner".into());
        }
        let prefix = plan.prefix_dir.join("pfx");
        if !prefix.is_dir() || !prefix.join("system.reg").is_file() {
            return Err("Prepare this prefix through the normal NotProton flow before starting a direct helper".into());
        }
        let system32 = prefix.join("drive_c/windows/system32");
        let winedevice = system32.join("winedevice.exe");
        let driver_arch = match pe_machine(&winedevice)? {
            0xaa64 => DriverArchitecture::Arm64,
            0x8664 => DriverArchitecture::X64,
            _ => return Err("Unsupported winedevice architecture in selected prefix".into()),
        };
        if driver_arch != expected_arch
            || pe_machine(&system32.join("ntdll.dll"))? != driver_arch.machine()
        {
            return Err("Selected prefix winedevice/ntdll architecture does not match the staged Wine loader".into());
        }
        // Explicit runner settings are copied; inherited Steam/auth/SDK and DLL
        // injection contexts are not helper requirements and are not propagated.
        let mut environment = BTreeMap::new();
        for name in [
            "USER",
            "LOGNAME",
            "TMPDIR",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "__CF_USER_TEXT_ENCODING",
        ] {
            if let Some(value) = inherited.get(std::ffi::OsStr::new(name)) {
                environment.insert(
                    name.into(),
                    value
                        .to_str()
                        .ok_or("Helper OS environment is not UTF-8")?
                        .into(),
                );
            }
        }
        environment.extend(
            plan.env
                .iter()
                .filter(|(name, _)| !sdk_only_environment(name))
                .map(|(name, value)| (name.clone(), value.clone())),
        );
        environment.insert("HOME".into(), path_text(home)?);
        environment.insert("WINELOADER".into(), path_text(&loader)?);
        environment.insert("WINESERVER".into(), path_text(&server)?);
        environment.insert("WINEPREFIX".into(), path_text(&prefix)?);
        environment.insert("CX_ROOT".into(), path_text(&plan.runner_root)?);
        environment.insert(
            "CX_HOME".into(),
            path_text(&home.join("Library/Application Support/CrossOver"))?,
        );
        environment.insert("WINEARCH".into(), "win64".into());
        environment.insert(
            "PATH".into(),
            format!(
                "{}:/usr/bin:/bin:/usr/sbin:/sbin",
                path_text(&plan.runner_root.join("bin"))?
            ),
        );
        let mut dll_directories = vec![];
        let prefix_steam = prefix.join("drive_c/Program Files (x86)/Steam");
        if prefix_steam.is_dir() {
            dll_directories.push(prefix_steam);
        }
        dll_directories.extend([windows, unix]);
        environment.insert(
            "WINEDLLPATH".into(),
            dll_directories
                .iter()
                .map(|path| path_text(path))
                .collect::<Result<Vec<_>>>()?
                .join(":"),
        );
        let msync = match plan.env.get("WINEMSYNC") {
            Some(value) => value.clone(),
            None => {
                let marker = plan.prefix_dir.join("notproton-msync");
                if marker.is_file() {
                    let mut bytes = Vec::new();
                    File::open(marker)
                        .map_err(|e| e.to_string())?
                        .take(32)
                        .read_to_end(&mut bytes)
                        .map_err(|e| e.to_string())?;
                    String::from_utf8(bytes)
                        .map_err(|_| "Invalid saved Wine MSync mode")?
                        .trim()
                        .into()
                } else {
                    "0".into()
                }
            }
        };
        if !matches!(msync.as_str(), "0" | "1") {
            return Err("Unsupported saved Wine MSync mode".into());
        }
        environment.insert("WINEMSYNC".into(), msync);
        environment
            .entry("WINEDEBUG".into())
            .or_insert_with(|| "err+all,fixme-all".into());
        let d3dshared = plan
            .runner_root
            .join("lib64/apple_gptk/external/libd3dshared.dylib");
        if d3dshared.is_file() {
            environment.insert(
                "CX_APPLEGPTK_LIBD3DSHARED_PATH".into(),
                path_text(&d3dshared)?,
            );
        }
        Ok(Self {
            loader,
            server,
            prefix,
            runner_root: plan.runner_root.clone(),
            unix_architecture,
            driver_arch,
            winedevice,
            environment,
        })
    }

    /// Returns only `feeder.exe --feed`. Installation remains an independent,
    /// explicitly authorized operation, and SDK/game commands keep their shim.
    /// The caller must add its profile-lock CLOEXEC/pre_exec closure before spawn.
    pub fn feeder_command(&self, feeder: &Path) -> Result<Command> {
        self.input_command(feeder, None, false)
    }

    /// Install/reuse only this packaged Wine bus and enter --feed in the same
    /// foreground process. The caller verifies all three manifest hashes.
    pub fn ensure_feeder_command(&self, feeder: &Path, inf: &Path) -> Result<Command> {
        self.ensure_input_command(feeder, inf, false)
    }

    /// Explicit experimental feedback transport; ordinary input commands never
    /// select it. The caller must verify the separate experiment manifest.
    pub fn ensure_feedback_feeder_command(&self, feeder: &Path, inf: &Path) -> Result<Command> {
        self.ensure_input_command(feeder, inf, true)
    }

    fn ensure_input_command(&self, feeder: &Path, inf: &Path, feedback: bool) -> Result<Command> {
        absolute_clean(inf, "owned input INF")?;
        if inf.file_name().and_then(|name| name.to_str()) != Some("skinosinput.inf")
            || inf.parent() != feeder.parent()
            || !inf.is_file()
        {
            return Err("Input INF must be the packaged sibling of the feeder".into());
        }
        let driver = inf.with_file_name("skinosinput.sys");
        if pe_machine(&driver)? != self.driver_arch.machine() {
            return Err(
                "Input driver architecture must match the selected prefix winedevice".into(),
            );
        }
        self.input_command(feeder, Some(inf), feedback)
    }

    fn input_command(&self, feeder: &Path, inf: Option<&Path>, feedback: bool) -> Result<Command> {
        absolute_clean(feeder, "owned feeder executable")?;
        if feeder.file_name().and_then(|name| name.to_str()) != Some("feeder.exe") {
            return Err("Direct Wine input command requires the owned feeder.exe".into());
        }
        if pe_machine(feeder)? != self.driver_arch.machine() {
            return Err("Feeder architecture must match the selected prefix winedevice".into());
        }
        let mut command = Command::new(&self.loader);
        command
            .env_clear()
            .envs(&self.environment)
            .arg(windows_path(feeder)?)
            .current_dir(feeder.parent().ok_or("Missing feeder directory")?);
        if let Some(inf) = inf {
            command
                .arg(if feedback {
                    "--ensure-feedback"
                } else {
                    "--ensure-and-feed"
                })
                .arg(windows_path(inf)?);
        } else {
            command.arg("--feed");
        }
        Ok(command)
    }
}

fn sdk_only_environment(name: &str) -> bool {
    name.starts_with("Steam")
        || name.starts_with("SKINOS_PROBE_")
        || name.starts_with("SKINOS_EXPECTED_")
        || name.starts_with("SKINOS_WINDOWS_")
        || name.starts_with("SKINOS_RUNTIME_")
        || matches!(
            name,
            "DYLD_INSERT_LIBRARIES"
                | "DYLD_LIBRARY_PATH"
                | "DYLD_FALLBACK_LIBRARY_PATH"
                | "STEAM_DYLD_INSERT_LIBRARIES"
                | "SKINOS_GAME_DYLD_INSERT_LIBRARIES"
                | "SKINOS_INPUT_OUTPUT_FD"
                | "SKINOS_COMPONENT_ROLE"
                | "WINEDLLOVERRIDES"
        )
}
fn executable(path: &Path) -> bool {
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}
fn absolute_clean(path: &Path, description: &str) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(format!(
            "{description} must be an absolute path without traversal"
        ));
    }
    path_text(path).map(|_| ())
}
fn path_text(path: &Path) -> Result<String> {
    let text = path.to_str().ok_or("Wine helper path is not UTF-8")?;
    if text.contains(['\0', ':']) {
        return Err("Wine helper path contains an unsupported NUL or colon".into());
    }
    Ok(text.into())
}
fn windows_path(path: &Path) -> Result<String> {
    Ok(format!("Z:{}", path_text(path)?.replace('/', "\\")))
}
fn pe_machine(path: &Path) -> Result<u16> {
    let mut file =
        File::open(path).map_err(|e| format!("Cannot read PE header {}: {e}", path.display()))?;
    let mut dos = [0_u8; 64];
    file.read_exact(&mut dos)
        .map_err(|_| "Truncated PE DOS header")?;
    if &dos[..2] != b"MZ" {
        return Err("Invalid PE DOS signature".into());
    }
    let offset = u32::from_le_bytes(dos[60..64].try_into().unwrap());
    if !(64..=1024 * 1024).contains(&offset) {
        return Err("Invalid bounded PE header offset".into());
    }
    file.seek(SeekFrom::Start(u64::from(offset)))
        .map_err(|e| e.to_string())?;
    let mut header = [0_u8; 6];
    file.read_exact(&mut header)
        .map_err(|_| "Truncated PE header")?;
    if &header[..4] != b"PE\0\0" {
        return Err("Invalid PE signature".into());
    }
    Ok(u16::from_le_bytes(header[4..6].try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pe(path: &Path, machine: u16) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut bytes = vec![0; 134];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[60..64].copy_from_slice(&128_u32.to_le_bytes());
        bytes[128..132].copy_from_slice(b"PE\0\0");
        bytes[132..134].copy_from_slice(&machine.to_le_bytes());
        fs::write(path, bytes).unwrap();
    }
    fn executable_file(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"offline fixture, never executed").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fn fixture(root: &Path, arch: DriverArchitecture) -> (LaunchPlan, PathBuf) {
        let runner = root.join("runner");
        let unix = if arch == DriverArchitecture::Arm64 {
            "aarch64-unix"
        } else {
            "x86_64-unix"
        };
        let loader = runner
            .join("skinos-launcher")
            .join(unix)
            .join("Wine.app/Contents/MacOS/wine");
        executable_file(&loader);
        fs::create_dir_all(runner.join("lib/wine").join(unix)).unwrap();
        fs::create_dir_all(runner.join("lib/wine/x86_64-windows")).unwrap();
        executable_file(&runner.join(if arch == DriverArchitecture::Arm64 {
            "bin/wineserver-arm64"
        } else {
            "bin/wineserver"
        }));
        let prefix = root.join("prefix");
        pe(
            &prefix.join("pfx/drive_c/windows/system32/winedevice.exe"),
            arch.machine(),
        );
        pe(
            &prefix.join("pfx/drive_c/windows/system32/ntdll.dll"),
            arch.machine(),
        );
        fs::write(prefix.join("pfx/system.reg"), "offline registry").unwrap();
        let feeder = root.join(arch.label()).join("feeder.exe");
        pe(&feeder, arch.machine());
        let plan = LaunchPlan {
            program: "/bin/sh".into(),
            args: vec!["run".into(), "waitforexitandrun".into(), "game.exe".into()],
            env: BTreeMap::from([
                ("SKINOS_WINELOADER".into(), path_text(&loader).unwrap()),
                ("CX_GRAPHICS_BACKEND".into(), "dxmt".into()),
            ]),
            working_dir: root.into(),
            read_roots: vec![],
            write_roots: vec![],
            execute_roots: vec![],
            prefix_dir: prefix,
            runner_root: runner,
            sdk_probe: "sdk-probe32.exe".into(),
            windows_sdk: None,
            sdk_args: vec![
                "run".into(),
                "waitforexitandrun".into(),
                "sdk-probe32.exe".into(),
            ],
            sdk_env: BTreeMap::from([("SKINOS_PROBE_ARCH".into(), "x86".into())]),
        };
        (plan, feeder)
    }
    #[test]
    fn staged_loader_and_winedevice_define_helper_not_game_sdk_architecture() {
        for arch in [DriverArchitecture::Arm64, DriverArchitecture::X64] {
            let root = tempfile::tempdir().unwrap();
            let (plan, feeder) = fixture(root.path(), arch);
            let before = serde_json::to_value(&plan).unwrap();
            let context =
                Context::from_plan_with_environment(&plan, root.path(), BTreeMap::new()).unwrap();
            assert_eq!(context.driver_arch, arch);
            let command = context.feeder_command(&feeder).unwrap();
            assert_eq!(
                command.get_program(),
                std::ffi::OsStr::new(&plan.env["SKINOS_WINELOADER"])
            );
            let env: BTreeMap<_, _> = command
                .get_envs()
                .map(|(key, value)| {
                    (
                        key.to_string_lossy().into_owned(),
                        value.map(|value| value.to_string_lossy().into_owned()),
                    )
                })
                .collect();
            assert_eq!(
                env["WINELOADER"].as_ref(),
                Some(&plan.env["SKINOS_WINELOADER"])
            );
            assert_eq!(
                env["WINEPREFIX"].as_ref(),
                Some(&path_text(&plan.prefix_dir.join("pfx")).unwrap())
            );
            assert_eq!(env["CX_GRAPHICS_BACKEND"].as_deref(), Some("dxmt"));
            assert!(!env.contains_key("SKINOS_PROBE_ARCH"));
            assert_eq!(
                command.get_args().collect::<Vec<_>>(),
                vec![
                    std::ffi::OsStr::new(&windows_path(&feeder).unwrap()),
                    std::ffi::OsStr::new("--feed")
                ]
            );
            assert_eq!(serde_json::to_value(&plan).unwrap(), before);
        }
    }
    #[test]
    fn literal_staging_path_is_preserved_even_with_symlinked_parent_directory() {
        let root = tempfile::tempdir().unwrap();
        let actual = root.path().join("actual");
        fs::create_dir(&actual).unwrap();
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&actual, &alias).unwrap();
        let (plan, feeder) = fixture(&alias, DriverArchitecture::Arm64);
        let context = Context::from_plan_with_environment(&plan, &alias, BTreeMap::new()).unwrap();
        let command = context.feeder_command(&feeder).unwrap();
        assert!(command.get_program().to_string_lossy().contains("/alias/"));
        assert_eq!(
            context.environment["WINELOADER"],
            plan.env["SKINOS_WINELOADER"]
        );
    }
    #[test]
    fn inherited_runner_and_sdk_context_cannot_override_selected_environment() {
        let root = tempfile::tempdir().unwrap();
        let (mut plan, feeder) = fixture(root.path(), DriverArchitecture::Arm64);
        plan.env.insert("SteamAppId".into(), "999".into());
        plan.env
            .insert("DYLD_INSERT_LIBRARIES".into(), "/unrelated.dylib".into());
        let inherited = BTreeMap::from([
            ("WINELOADER".into(), "/other/wine".into()),
            ("WINESERVER".into(), "/other/server".into()),
            ("WINEPREFIX".into(), "/other/prefix".into()),
            ("PATH".into(), "/other/bin".into()),
            ("LANG".into(), "C".into()),
        ]);
        let context = Context::from_plan_with_environment(&plan, root.path(), inherited).unwrap();
        assert_eq!(context.environment["LANG"], "C");
        assert_eq!(
            context.environment["WINELOADER"],
            plan.env["SKINOS_WINELOADER"]
        );
        assert!(!context.environment.contains_key("SteamAppId"));
        assert!(!context.environment.contains_key("DYLD_INSERT_LIBRARIES"));
        assert!(context.feeder_command(&feeder).is_ok());
    }
    #[test]
    fn incomplete_or_wrong_architecture_prefix_is_rejected_without_preparation() {
        let root = tempfile::tempdir().unwrap();
        let (mut plan, feeder) = fixture(root.path(), DriverArchitecture::Arm64);
        pe(
            &plan
                .prefix_dir
                .join("pfx/drive_c/windows/system32/winedevice.exe"),
            0x8664,
        );
        assert!(Context::from_plan_with_environment(&plan, root.path(), BTreeMap::new()).is_err());
        plan.prefix_dir = root.path().join("not-created");
        assert!(Context::from_plan_with_environment(&plan, root.path(), BTreeMap::new()).is_err());
        assert!(!plan.prefix_dir.exists());
        assert!(feeder.is_file());
    }
    #[test]
    fn wrong_feeder_architecture_and_nonstaged_loader_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let (mut plan, feeder) = fixture(root.path(), DriverArchitecture::Arm64);
        let context =
            Context::from_plan_with_environment(&plan, root.path(), BTreeMap::new()).unwrap();
        pe(&feeder, 0x8664);
        assert!(context.feeder_command(&feeder).is_err());
        plan.env.insert(
            "SKINOS_WINELOADER".into(),
            path_text(
                &plan
                    .runner_root
                    .join("lib/wine/aarch64-unix/wine.app/Contents/MacOS/wine"),
            )
            .unwrap(),
        );
        assert!(Context::from_plan_with_environment(&plan, root.path(), BTreeMap::new()).is_err());
    }
    #[test]
    fn ensure_and_feed_preserves_direct_loader_and_requires_sibling_matching_driver() {
        let root = tempfile::tempdir().unwrap();
        let (plan, feeder) = fixture(root.path(), DriverArchitecture::Arm64);
        let context =
            Context::from_plan_with_environment(&plan, root.path(), BTreeMap::new()).unwrap();
        let inf = feeder.with_file_name("skinosinput.inf");
        fs::write(&inf, "offline INF").unwrap();
        let driver = feeder.with_file_name("skinosinput.sys");
        pe(&driver, 0xaa64);
        let command = context.ensure_feeder_command(&feeder, &inf).unwrap();
        assert_eq!(command.get_program(), context.loader.as_os_str());
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            vec![
                windows_path(&feeder).unwrap(),
                "--ensure-and-feed".into(),
                windows_path(&inf).unwrap()
            ]
        );
        let feedback = context
            .ensure_feedback_feeder_command(&feeder, &inf)
            .unwrap();
        assert_eq!(feedback.get_program(), context.loader.as_os_str());
        assert_eq!(feedback.get_args().nth(1).unwrap(), "--ensure-feedback");
        pe(&driver, 0x8664);
        assert!(context.ensure_feeder_command(&feeder, &inf).is_err());
        assert!(
            context
                .ensure_feedback_feeder_command(&feeder, &inf)
                .is_err()
        );
        let other = root.path().join("skinosinput.inf");
        fs::write(&other, "other").unwrap();
        assert!(context.ensure_feeder_command(&feeder, &other).is_err());
        assert!(
            context
                .ensure_feedback_feeder_command(&feeder, &other)
                .is_err()
        );
    }
    #[test]
    fn carried_msync_is_read_without_overwriting_prefix_and_explicit_setting_wins() {
        let root = tempfile::tempdir().unwrap();
        let (mut plan, _) = fixture(root.path(), DriverArchitecture::X64);
        let marker = plan.prefix_dir.join("notproton-msync");
        fs::write(&marker, "1\n").unwrap();
        let context =
            Context::from_plan_with_environment(&plan, root.path(), BTreeMap::new()).unwrap();
        assert_eq!(context.environment["WINEMSYNC"], "1");
        assert_eq!(fs::read_to_string(&marker).unwrap(), "1\n");
        plan.env.insert("WINEMSYNC".into(), "0".into());
        assert_eq!(
            Context::from_plan_with_environment(&plan, root.path(), BTreeMap::new())
                .unwrap()
                .environment["WINEMSYNC"],
            "0"
        );
    }
}
