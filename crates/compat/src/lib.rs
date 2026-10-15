//! Owned NotProton runner preparation and structured launch plans.
mod assets;
pub mod bootstrap;
pub mod direct_wine;
mod legacy;
pub mod native_delivery;
mod prefix;
mod sdk;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct GameSettings {
    pub compatibility: String,
    pub graphics: String,
    pub sync: String,
    pub high_resolution: bool,
    pub steam_input: bool,
    pub launch_arguments: String,
    pub crossover_path: Option<PathBuf>,
}
impl Default for GameSettings {
    fn default() -> Self {
        Self {
            compatibility: "auto".into(),
            graphics: "auto".into(),
            sync: "auto".into(),
            high_resolution: false,
            steam_input: false,
            launch_arguments: String::new(),
            crossover_path: None,
        }
    }
}
impl GameSettings {
    pub fn validate(&self) -> Result<()> {
        if !["auto", "native", "notproton"].contains(&self.compatibility.as_str()) {
            return Err("Unknown compatibility mode".into());
        }
        if !["auto", "d3dmetal", "dxmt", "dxvk", "wined3d"].contains(&self.graphics.as_str()) {
            return Err("Unknown graphics backend".into());
        }
        if !["auto", "msync", "none"].contains(&self.sync.as_str()) {
            return Err(
                "This NotProton version supports auto, MSync or no synchronization override".into(),
            );
        }
        parse_arguments(&self.launch_arguments)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CrossOverInstallation {
    pub path: PathBuf,
    pub name: String,
    pub version: String,
    pub preview: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotProtonInfo {
    pub revision: String,
    pub version: String,
}
pub fn bundled_info() -> NotProtonInfo {
    serde_json::from_str(include_str!("../../../third_party/notproton/UPSTREAM.json"))
        .expect("bundled metadata")
}
pub fn discover_crossovers(home: &Path) -> Vec<CrossOverInstallation> {
    let mut found = Vec::new();
    for directory in [PathBuf::from("/Applications"), home.join("Applications")] {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            if path.extension().is_none_or(|v| v != "app") || !name.starts_with("CrossOver") {
                continue;
            }
            if !path
                .join("Contents/SharedSupport/CrossOver/lib/wine")
                .is_dir()
            {
                continue;
            }
            let version = plist::Value::from_file(path.join("Contents/Info.plist"))
                .ok()
                .and_then(|value| {
                    value
                        .as_dictionary()?
                        .get("CFBundleShortVersionString")?
                        .as_string()
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| "Unknown".into());
            found.push(CrossOverInstallation {
                path,
                preview: name.contains("Preview"),
                name,
                version,
            });
        }
    }
    found.sort_by_key(|v| (!v.preview, v.path.clone()));
    found
}

pub struct PrepareRequest {
    pub home: PathBuf,
    pub data_dir: PathBuf,
    pub steamapps_dir: PathBuf,
    pub crossover_path: Option<PathBuf>,
    pub appid: u32,
    pub install_dir: PathBuf,
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub working_dir: PathBuf,
    pub settings: GameSettings,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LaunchPlan {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub working_dir: PathBuf,
    pub read_roots: Vec<PathBuf>,
    pub write_roots: Vec<PathBuf>,
    pub execute_roots: Vec<PathBuf>,
    pub prefix_dir: PathBuf,
    pub runner_root: PathBuf,
    pub sdk_probe: PathBuf,
    pub windows_sdk: Option<PathBuf>,
    pub sdk_args: Vec<String>,
    pub sdk_env: BTreeMap<String, String>,
}

fn sdk_launch_args(tool: &Path, sdk_probe: &Path) -> Vec<String> {
    vec![
        tool.join("run").to_string_lossy().into_owned(),
        // The probe needs the same real Windows client bootstrap as the game.
        // A raw Wine helper only worked after a prior launch populated the
        // prefix's client registry; fresh prefixes must work on their first run.
        "waitforexitandrun".into(),
        sdk_probe.to_string_lossy().into_owned(),
    ]
}

fn payload_root() -> Result<PathBuf> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let packaged = exe
        .parent()
        .ok_or("No executable directory")?
        .join("../Resources/notproton");
    for path in [
        packaged,
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/notproton"),
    ] {
        if path.join("skinos-notproton-setup").is_file() && path.join("run").is_file() {
            return path.canonicalize().map_err(|e| e.to_string());
        }
    }
    Err("NotProton payload is missing. Build it with scripts/build-notproton.py.".into())
}
fn copy_checked_payload(source: &Path, target: &Path) -> Result<()> {
    let resources = source.parent().ok_or("No NotProton resource directory")?;
    let delivery = native_delivery::Delivery::load(resources)?;
    let value: serde_json::Value = serde_json::from_slice(
        &fs::read(source.join("bridge-manifest.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let files = value["files"]
        .as_object()
        .ok_or("Malformed bridge manifest")?;
    for (name, hash) in files {
        let rel = Path::new(name);
        if rel.is_absolute()
            || rel
                .components()
                .any(|v| !matches!(v, std::path::Component::Normal(_)))
        {
            return Err("Unsafe bridge manifest path".into());
        }
        let expected_hash = hash.as_str().ok_or("Missing bridge hash")?;
        let data = if let Some(record) = delivery.as_ref().and_then(|value| value.bridge.get(name))
        {
            record.restore(resources, expected_hash, None)?
        } else {
            let from = source.join("bridge").join(format!("{name}.gz"));
            let packed =
                fs::read(&from).map_err(|e| format!("Missing bundled bridge {name}: {e}"))?;
            let mut data = Vec::new();
            flate2::read::GzDecoder::new(packed.as_slice())
                .take(256 * 1024 * 1024)
                .read_to_end(&mut data)
                .map_err(|e| format!("Invalid bundled bridge {name}: {e}"))?;
            data
        };
        if format!("{:x}", Sha256::digest(&data)) != expected_hash {
            return Err(format!(
                "Bundled NotProton bridge checksum mismatch: {name}"
            ));
        }
        let dest = target.join("bridge").join(rel);
        if fs::read(&dest).ok().as_deref() == Some(&data) {
            continue;
        }
        fs::create_dir_all(dest.parent().unwrap()).map_err(|e| e.to_string())?;
        let temp = dest.with_extension("skinos-new");
        fs::write(&temp, data).map_err(|e| e.to_string())?;
        fs::rename(temp, dest).map_err(|e| e.to_string())?;
    }
    Ok(())
}

// The ARM64 Wine app's raw executable is nested three levels below its Unix
// libraries. Upstream stages the loader beside those libraries for game launch;
// doing the same before prefix preparation also keeps helpers on ARM64 Wine.
fn stage_loader(runner: &Path, flavor: &str) -> Result<PathBuf> {
    let arch = if flavor == "fex" {
        "aarch64-unix"
    } else {
        "x86_64-unix"
    };
    let native = runner.join("lib/wine").join(arch);
    let original = if flavor == "fex" {
        native.join("wine.app/Contents/MacOS/wine")
    } else {
        native.join("wine")
    };
    let directory = runner
        .join("skinos-launcher")
        .join(arch)
        .join("Wine.app/Contents/MacOS");
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(&native).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_name() == "wine" || entry.file_name() == "wine.app" {
            continue;
        }
        let target = directory.join(entry.file_name());
        if fs::symlink_metadata(&target).is_err() {
            std::os::unix::fs::symlink(entry.path(), target).map_err(|e| e.to_string())?;
        }
    }
    let loader = directory.join("wine");
    if !loader.exists() {
        fs::hard_link(&original, &loader)
            .or_else(|_| fs::copy(&original, &loader).map(|_| ()))
            .map_err(|e| e.to_string())?;
    }
    migrate_loader_plist(&directory.parent().unwrap().join("Info.plist"))?;
    Ok(loader)
}

fn migrate_loader_plist(path: &Path) -> Result<()> {
    let mut dictionary = if path.exists() {
        plist::Value::from_file(path)
            .map_err(|e| format!("Invalid Wine launcher Info.plist: {e}"))?
            .into_dictionary()
            .ok_or("Wine launcher Info.plist is not a dictionary")?
    } else {
        plist::Dictionary::new()
    };
    let original = dictionary.clone();
    for (key, value) in [
        ("CFBundleIdentifier", "app.skinos.wine"),
        ("CFBundleExecutable", "wine"),
        ("CFBundlePackageType", "APPL"),
    ] {
        if !dictionary.contains_key(key) {
            dictionary.insert(key.into(), plist::Value::String(value.into()));
        }
    }
    for (key, value) in [
        (
            "NSMicrophoneUsageDescription",
            "This game would like to access your microphone.",
        ),
        (
            "NSCameraUsageDescription",
            "This game would like to access your camera.",
        ),
        (
            "NSLocalNetworkUsageDescription",
            "This game would like to access devices on your local network.",
        ),
    ] {
        dictionary.insert(key.into(), plist::Value::String(value.into()));
    }
    // Descriptions allow the normal system prompt; they grant no permissions.
    if dictionary != original {
        let mut bytes = Vec::new();
        plist::Value::Dictionary(dictionary)
            .to_writer_xml(&mut bytes)
            .map_err(|e| e.to_string())?;
        assets::atomic_write(path, &bytes)?;
    }
    Ok(())
}

fn wine_helper_directory(loader: &Path) -> Result<PathBuf> {
    // CrossOver's ntdll builds native per-program wrappers here. It uses the
    // Darwin user temp directory, not TMPDIR. Authorize only the exact directory
    // derived from the selected loader identity, never the whole temp tree.
    let output = Command::new("/usr/bin/getconf")
        .arg("DARWIN_USER_TEMP_DIR")
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("Cannot locate CrossOver's helper directory".into());
    }
    let temp = PathBuf::from(
        String::from_utf8(output.stdout)
            .map_err(|e| e.to_string())?
            .trim(),
    );
    let temp = temp.canonicalize().map_err(|e| e.to_string())?;
    let stat = fs::metadata(loader).map_err(|e| e.to_string())?;
    Ok(temp.join(format!(
        "winetemp-{}-{}-{}-{}",
        stat.ino(),
        stat.len(),
        stat.mtime(),
        stat.mtime_nsec()
    )))
}

pub fn prepare_launch(request: &PrepareRequest) -> Result<LaunchPlan> {
    request.settings.validate()?;
    if request.appid == 0 {
        return Err("App ID is required".into());
    }
    let install = request
        .install_dir
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let executable = if request.executable.is_absolute() {
        request.executable.clone()
    } else {
        install.join(&request.executable)
    };
    let executable = executable
        .canonicalize()
        .map_err(|e| format!("Game executable: {e}"))?;
    if !executable.starts_with(&install) || !executable.is_file() {
        return Err("Executable must be inside the installed game".into());
    }
    let selection = sdk::select(&install, &executable)?;
    let crossover = request
        .settings
        .crossover_path
        .clone()
        .or_else(|| request.crossover_path.clone())
        .or_else(|| {
            discover_crossovers(&request.home)
                .into_iter()
                .next()
                .map(|v| v.path)
        })
        .ok_or("Install CrossOver or CrossOver Preview to run Windows games")?;
    let payload = payload_root()?;
    let support = request.data_dir.join("notproton");
    fs::create_dir_all(&support).map_err(|e| e.to_string())?;
    let dependencies = bootstrap::dependency_payload(&payload, &request.data_dir)?;
    copy_checked_payload(&dependencies, &support)?;
    assets::stage_fonts(&dependencies, &support, &bundled_info())?;
    legacy::stage_shared_client(
        &support,
        &install,
        &request.steamapps_dir,
        &request.data_dir,
    )?;
    let output = Command::new(payload.join("skinos-notproton-setup"))
        .arg(&crossover)
        .arg(payload.join("NotProtonResources.bundle"))
        .arg(&support)
        .output()
        .map_err(|e| format!("Prepare CrossOver: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("Invalid NotProton setup response: {e}"))?;
    let build = value["build"].as_str().ok_or("Missing runner build")?;
    let flavor = value["flavor"].as_str().ok_or("Missing runner flavor")?;
    let runner_root = PathBuf::from(value["runner_root"].as_str().ok_or("Missing runner root")?);
    let wine_loader = stage_loader(&runner_root, flavor)?;
    let helper_directory = wine_helper_directory(&wine_loader)?;
    let tool = support.join("tools").join(build);
    fs::create_dir_all(&tool).map_err(|e| e.to_string())?;
    fs::copy(payload.join("run"), tool.join("run")).map_err(|e| e.to_string())?;
    fs::write(tool.join("build"), format!("{build}\n")).map_err(|e| e.to_string())?;
    fs::write(tool.join("flavor"), format!("{flavor}\n")).map_err(|e| e.to_string())?;
    let original_prefix = request
        .steamapps_dir
        .join("compatdata")
        .join(request.appid.to_string());
    let prefix_dir = prefix::select(&original_prefix, &support, request.appid, build)?;
    let mut env = BTreeMap::from([
        (
            "SKINOS_WINELOADER".into(),
            wine_loader.to_string_lossy().into_owned(),
        ),
        (
            "SKINOS_NOTPROTON_SUPPORT".into(),
            support.to_string_lossy().into_owned(),
        ),
        (
            "STEAM_COMPAT_DATA_PATH".into(),
            prefix_dir.to_string_lossy().into_owned(),
        ),
        (
            "STEAM_COMPAT_INSTALL_PATH".into(),
            install.to_string_lossy().into_owned(),
        ),
        ("STEAM_COMPAT_APP_ID".into(), request.appid.to_string()),
        (
            "STEAM_COMPAT_LIBRARY_PATHS".into(),
            request
                .steamapps_dir
                .parent()
                .unwrap_or(&request.steamapps_dir)
                .to_string_lossy()
                .into_owned(),
        ),
        (
            "NOTPROTON_RETINA".into(),
            u8::from(request.settings.high_resolution).to_string(),
        ),
    ]);
    // Opt-in diagnostic verbosity for integration checks, never a saved setting.
    if std::env::var_os("SKINOS_COMPAT_DEBUG").is_some() {
        env.insert("WINEDEBUG".into(), "+process,+module,+loaddll,+seh".into());
    }
    if request.settings.graphics != "auto" {
        env.insert(
            "CX_GRAPHICS_BACKEND".into(),
            request.settings.graphics.clone(),
        );
    }
    match request.settings.sync.as_str() {
        "msync" => {
            env.insert("WINEMSYNC".into(), "1".into());
        }
        "none" => {
            env.insert("WINEMSYNC".into(), "0".into());
        }
        _ => {}
    }
    let mut args = vec![
        tool.join("run").to_string_lossy().into_owned(),
        "waitforexitandrun".into(),
        executable.to_string_lossy().into_owned(),
    ];
    args.extend(request.arguments.clone());
    args.extend(parse_arguments(&request.settings.launch_arguments)?);
    let working_dir = if request.working_dir.is_absolute() {
        request.working_dir.clone()
    } else {
        install.join(&request.working_dir)
    };
    let working_dir = working_dir.canonicalize().map_err(|e| e.to_string())?;
    if !working_dir.starts_with(&install) {
        return Err("Working directory must be inside the game".into());
    }

    let sdk_probe = payload.join(selection.architecture.probe_name());
    if !sdk_probe.is_file() {
        return Err(format!(
            "В сборке отсутствует проверка Steam API для {}",
            selection.architecture.label()
        ));
    }
    let sdk_args = sdk_launch_args(&tool, &sdk_probe);
    let mut sdk_env = BTreeMap::new();
    sdk_env.insert(
        "SKINOS_PROBE_ARCH".into(),
        selection.architecture.label().into(),
    );
    let windows_sdk = selection.sdk;
    if let Some(path) = &windows_sdk {
        sdk_env.insert(
            "SKINOS_WINDOWS_SDK".into(),
            format!("Z:{}", path.to_string_lossy().replace('/', "\\")),
        );
    } else {
        sdk_env.insert(
            "SKINOS_WINDOWS_CLIENT".into(),
            format!(
                "C:\\Program Files (x86)\\Steam\\{}",
                selection.architecture.client_name()
            ),
        );
    }
    Ok(LaunchPlan {
        program: PathBuf::from("/bin/sh"),
        args,
        env,
        working_dir,
        read_roots: vec![
            support.clone(),
            payload.clone(),
            install.clone(),
            request.steamapps_dir.clone(),
            request.home.join("Library/Application Support/CrossOver"),
            request.home.join("Library/Preferences"),
            PathBuf::from("/Library/Preferences"),
            PathBuf::from("/usr/share"),
        ],
        write_roots: vec![
            install,
            support,
            prefix_dir.clone(),
            request.steamapps_dir.join("compatdata"),
            request.home.join("Library/Application Support/CrossOver"),
        ],
        execute_roots: vec![
            runner_root.clone(),
            helper_directory,
            PathBuf::from("/bin"),
            PathBuf::from("/usr/bin"),
            PathBuf::from("/usr/sbin"),
        ],
        prefix_dir,
        runner_root,
        sdk_probe,
        windows_sdk,
        sdk_args,
        sdk_env,
    })
}

/// Tokenize argument text without a shell, expansions or evaluation.
pub fn parse_arguments(input: &str) -> Result<Vec<String>> {
    let mut result = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escape = false;
    let mut started = false;
    for c in input.chars() {
        if c == '\0' {
            return Err("Launch arguments contain a NUL byte".into());
        }
        if escape {
            word.push(c);
            escape = false;
            started = true;
            continue;
        }
        if c == '\\' && quote != Some('\'') {
            escape = true;
            started = true;
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                word.push(c);
            }
            started = true;
        } else if c == '\'' || c == '"' {
            quote = Some(c);
            started = true;
        } else if c.is_whitespace() {
            if started {
                result.push(std::mem::take(&mut word));
                started = false;
            }
        } else {
            word.push(c);
            started = true;
        }
    }
    if escape || quote.is_some() {
        return Err("Launch arguments contain an unfinished quote or escape".into());
    }
    if started {
        result.push(word);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn bridge_staging_keeps_gzip_fallback_with_or_without_a_delivery_sidecar() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("Resources/notproton");
        let target = temp.path().join("cache");
        fs::create_dir_all(source.join("bridge")).unwrap();
        let data = b"development bridge bytes";
        let manifest = serde_json::json!({"files": {
            "steam.exe": format!("{:x}", Sha256::digest(data))
        }});
        fs::write(source.join("bridge-manifest.json"), manifest.to_string()).unwrap();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(data).unwrap();
        fs::write(
            source.join("bridge/steam.exe.gz"),
            encoder.finish().unwrap(),
        )
        .unwrap();
        copy_checked_payload(&source, &target).unwrap();
        assert_eq!(fs::read(target.join("bridge/steam.exe")).unwrap(), data);

        fs::write(
            source.parent().unwrap().join("native-delivery.json"),
            r#"{"schema_version":1,"runtime":{},"bridge":{}}"#,
        )
        .unwrap();
        fs::write(target.join("bridge/steam.exe"), b"stale cached bridge").unwrap();
        copy_checked_payload(&source, &target).unwrap();
        assert_eq!(fs::read(target.join("bridge/steam.exe")).unwrap(), data);
    }

    #[test]
    fn bridge_staging_restores_carrier_and_verifies_it_before_cache_writes() {
        let temp = tempfile::tempdir().unwrap();
        let resources = temp.path().join("Resources");
        let source = resources.join("notproton");
        let target = temp.path().join("cache");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(resources.join("NativeRuntimeCode")).unwrap();
        let data = b"original native bridge";
        let mut carrier = b"new-header".to_vec();
        let offset = carrier.len();
        carrier.extend_from_slice(data);
        carrier.extend_from_slice(b"new-signature");
        let original_hash = format!("{:x}", Sha256::digest(data));
        let carrier_path = resources.join("NativeRuntimeCode/bridge-test.dylib");
        fs::write(&carrier_path, &carrier).unwrap();
        let manifest = serde_json::json!({"files": {"lsteamclient.so": original_hash}});
        fs::write(source.join("bridge-manifest.json"), manifest.to_string()).unwrap();
        let delivery = serde_json::json!({
            "schema_version": 1, "runtime": {}, "bridge": {
                "lsteamclient.so": {
                    "carrier": "NativeRuntimeCode/bridge-test.dylib",
                    "carrier_sha256": format!("{:x}", Sha256::digest(&carrier)),
                    "sha256": original_hash, "size": data.len(),
                    "chunks": [{"offset": offset, "length": data.len()}]
                }
            }
        });
        fs::write(resources.join("native-delivery.json"), delivery.to_string()).unwrap();
        copy_checked_payload(&source, &target).unwrap();
        assert_eq!(
            fs::read(target.join("bridge/lsteamclient.so")).unwrap(),
            data
        );

        fs::write(&carrier_path, b"tampered carrier").unwrap();
        fs::write(target.join("bridge/lsteamclient.so"), b"existing cache").unwrap();
        assert!(copy_checked_payload(&source, &target).is_err());
        assert_eq!(
            fs::read(target.join("bridge/lsteamclient.so")).unwrap(),
            b"existing cache"
        );

        fs::write(&carrier_path, &carrier).unwrap();
        let conflicting_manifest = serde_json::json!({"files": {
            "lsteamclient.so": format!("{:x}", Sha256::digest(b"different original"))
        }});
        fs::write(
            source.join("bridge-manifest.json"),
            conflicting_manifest.to_string(),
        )
        .unwrap();
        assert!(copy_checked_payload(&source, &target).is_err());
        assert_eq!(
            fs::read(target.join("bridge/lsteamclient.so")).unwrap(),
            b"existing cache"
        );
    }

    #[test]
    #[ignore = "requires SKINOS_NATIVE_DELIVERY_FIXTURE_APP pointing to a prepared signed fixture"]
    fn signed_native_delivery_fixture_restores_all_three_pinned_bridges_offline() {
        let app = PathBuf::from(
            std::env::var_os("SKINOS_NATIVE_DELIVERY_FIXTURE_APP")
                .expect("explicit signed fixture path is required"),
        );
        let resources = app.join("Contents/Resources");
        let delivery = native_delivery::Delivery::load(&resources)
            .unwrap()
            .unwrap();
        let names = [
            "aarch64-unix/lsteamclient.so",
            "lsteamclient.so",
            "x86_64-unix/lsteamclient.so",
        ];
        assert_eq!(delivery.bridge.len(), names.len());
        let target = tempfile::tempdir().unwrap();
        copy_checked_payload(&resources.join("notproton"), target.path()).unwrap();
        let pinned =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../third_party/notproton/bridge");
        for name in names {
            assert!(delivery.bridge.contains_key(name));
            assert!(
                !resources
                    .join("notproton/bridge")
                    .join(format!("{name}.gz"))
                    .exists()
            );
            let packed = fs::read(pinned.join(format!("{name}.gz"))).unwrap();
            let mut original = Vec::new();
            flate2::read::GzDecoder::new(packed.as_slice())
                .read_to_end(&mut original)
                .unwrap();
            assert_eq!(
                fs::read(target.path().join("bridge").join(name)).unwrap(),
                original
            );
        }
    }

    #[test]
    fn existing_loader_plist_gains_usage_descriptions_without_losing_identity() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("Info.plist");
        let mut dictionary = plist::Dictionary::new();
        dictionary.insert(
            "CFBundleIdentifier".into(),
            plist::Value::String("app.skinos.existing-wine".into()),
        );
        dictionary.insert(
            "CustomValue".into(),
            plist::Value::String("retained".into()),
        );
        plist::Value::Dictionary(dictionary)
            .to_file_xml(&path)
            .unwrap();
        migrate_loader_plist(&path).unwrap();
        let updated = plist::Value::from_file(&path)
            .unwrap()
            .into_dictionary()
            .unwrap();
        assert_eq!(
            updated["CFBundleIdentifier"].as_string(),
            Some("app.skinos.existing-wine")
        );
        assert_eq!(updated["CustomValue"].as_string(), Some("retained"));
        for key in [
            "NSMicrophoneUsageDescription",
            "NSCameraUsageDescription",
            "NSLocalNetworkUsageDescription",
        ] {
            assert!(
                updated[key]
                    .as_string()
                    .is_some_and(|value| !value.is_empty())
            );
        }
        let unchanged = fs::read(&path).unwrap();
        migrate_loader_plist(&path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), unchanged);
    }
    #[test]
    fn sdk_preflight_uses_client_bootstrap_even_before_any_game_has_run() {
        // A new prefix has no Valve/Steam registry keys. runinprefix starts raw
        // Wine, so classic SteamAPI cannot discover steamclient. The foreground
        // game route starts the bundled shim, seeds the real client paths/PID,
        // and preserves the probe's stdout and exit code for verification.
        for executable in ["sdk-probe.exe", "sdk-probe32.exe"] {
            let probe = Path::new("/bundle with spaces").join(executable);
            let args = sdk_launch_args(Path::new("/owned NotProton"), &probe);
            assert_eq!(
                args,
                [
                    "/owned NotProton/run".to_owned(),
                    "waitforexitandrun".to_owned(),
                    probe.to_string_lossy().into_owned(),
                ]
            );
        }
    }
    #[test]
    fn arguments_are_data_not_shell() {
        assert_eq!(
            parse_arguments("--name 'a b' \"\" $(touch /tmp/pwned); `id`").unwrap(),
            ["--name", "a b", "", "$(touch", "/tmp/pwned);", "`id`"]
        );
        assert!(parse_arguments("'unfinished").is_err());
    }
    #[test]
    fn steam_input_setting_preserves_old_settings_and_rejects_wrong_types() {
        let old: GameSettings = serde_json::from_str(r#"{"graphics":"dxvk"}"#).unwrap();
        assert!(!old.steam_input);
        let mut enabled = old;
        enabled.steam_input = true;
        let restored: GameSettings =
            serde_json::from_value(serde_json::to_value(enabled).unwrap()).unwrap();
        assert!(restored.steam_input);
        assert_eq!(restored.graphics, "dxvk");
        assert!(serde_json::from_str::<GameSettings>(r#"{"steam_input":"true"}"#).is_err());
    }
    #[test]
    fn unsupported_settings_rejected() {
        let settings = GameSettings {
            sync: "esync".into(),
            ..GameSettings::default()
        };
        assert!(settings.validate().is_err());
        for graphics in ["auto", "dxmt", "d3dmetal", "dxvk", "wined3d"] {
            assert!(
                GameSettings {
                    graphics: graphics.into(),
                    ..GameSettings::default()
                }
                .validate()
                .is_ok()
            );
        }
    }
}
