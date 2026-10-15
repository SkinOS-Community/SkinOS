//! Resolve Steam Auto-Cloud locations, not a game's complete write policy.
//!
//! Root meanings and user placeholders are documented by Valve at
//! https://partner.steamgames.com/doc/features/cloud#steam_auto-cloud .
//! Metadata is untrusted: only narrow descendants of supported save containers
//! are accepted. There is no AppID/title exception and no fallback to all HOME.

use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

const MAX_RULES: usize = 128;
const MAX_PATH_BYTES: usize = 2048;

/// Read-only resolution. Missing Auto-Cloud settings means no additional roots;
/// games using the Cloud API do not necessarily have Auto-Cloud settings.
pub fn native_save_roots(request: &Value) -> Result<Vec<PathBuf>, String> {
    if !is_native_launch(request) || request["cloud_config"].is_null() {
        return Ok(Vec::new());
    }
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("User home unavailable")?);
    resolve(request, &home)
}

fn is_native_launch(request: &Value) -> bool {
    request["command"] == "launch" && request["compatibility"].is_null()
}

fn resolve(request: &Value, home: &Path) -> Result<Vec<PathBuf>, String> {
    if !is_native_launch(request) || request["cloud_config"].is_null() {
        return Ok(Vec::new());
    }
    let config = &request["cloud_config"];
    if !config.is_object() {
        return Err("Steam Auto-Cloud configuration is not an object".into());
    }
    let home = home
        .canonicalize()
        .map_err(|_| "Cannot resolve the native save home directory")?;
    let files = numbered_entries(&config["savefiles"], "savefiles")?;
    let overrides = numbered_entries(&config["rootoverrides"], "rootoverrides")?;
    let mut roots = BTreeSet::new();
    for file in files {
        if !applies_to_mac(file)? {
            continue;
        }
        let mut root = required_string(file, "root")?.to_owned();
        let mut path = file["path"].as_str().unwrap_or("").to_owned();
        let matching: Vec<_> = overrides
            .iter()
            .copied()
            .filter(|entry| {
                entry["root"]
                    .as_str()
                    .is_some_and(|s| s.eq_ignore_ascii_case(&root))
            })
            .filter(|entry| entry["os"].as_str().is_some_and(is_mac))
            .collect();
        if matching.len() > 1 {
            return Err("Steam Auto-Cloud has ambiguous macOS root overrides".into());
        }
        if let Some(over) = matching.first() {
            if over["oscompare"].as_str().is_some_and(|s| s != "=") {
                return Err("Unsupported Steam Auto-Cloud OS comparison".into());
            }
            root = required_string(over, "useinstead")?.to_owned();
            path = override_path(&path, over)?;
        }
        let Some(base) = root_base(&root, request, &home)? else {
            continue;
        };
        let relative = relative_path(&expand_user_path(&path, request)?)?;
        let target = base.join(relative);
        require_narrow_save_path(&target, &base, &home)?;
        if !matches!(
            root.to_ascii_lowercase().as_str(),
            "gameinstall" | "appinstalldir"
        ) {
            require_game_identity(&target, &home, request)?;
        }
        check_existing_components(&target)?;
        roots.insert(target);
    }
    Ok(roots.into_iter().collect())
}

fn numbered_entries<'a>(value: &'a Value, field: &str) -> Result<Vec<&'a Value>, String> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    let entries = match value {
        Value::Object(object) => {
            if object.keys().any(|key| key.parse::<u32>().is_err()) {
                return Err(format!("Unsupported Steam Auto-Cloud {field} entries"));
            }
            let mut entries: Vec<_> = object.iter().collect();
            entries.sort_by_key(|(key, _)| key.parse::<u32>().unwrap_or(u32::MAX));
            entries
                .into_iter()
                .map(|(_, value)| value)
                .collect::<Vec<_>>()
        }
        Value::Array(array) => array.iter().collect(),
        _ => return Err(format!("Malformed Steam Auto-Cloud {field}")),
    };
    if entries.len() > MAX_RULES || entries.iter().any(|entry| !entry.is_object()) {
        return Err(format!(
            "Unsupported Steam Auto-Cloud {field} size or shape"
        ));
    }
    Ok(entries)
}

fn required_string<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value[key]
        .as_str()
        .ok_or_else(|| format!("Steam Auto-Cloud rule has no {key}"))
}

fn is_mac(os: &str) -> bool {
    matches!(
        os.trim().to_ascii_lowercase().as_str(),
        "macos" | "mac" | "osx"
    )
}

fn applies_to_mac(rule: &Value) -> Result<bool, String> {
    let field = if !rule["platforms"].is_null() {
        &rule["platforms"]
    } else if !rule["oslist"].is_null() {
        &rule["oslist"]
    } else {
        &rule["os"]
    };
    let values: Vec<&str> = match field {
        Value::Null => return Ok(true),
        Value::String(value) => value.split([',', ';']).collect(),
        Value::Object(object) => object
            .values()
            .map(|value| {
                value
                    .as_str()
                    .ok_or("Malformed Auto-Cloud platform restriction")
            })
            .collect::<Result<_, _>>()?,
        Value::Array(array) => array
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .ok_or("Malformed Auto-Cloud platform restriction")
            })
            .collect::<Result<_, _>>()?,
        _ => return Err("Unsupported Auto-Cloud platform restriction".into()),
    };
    Ok(values.is_empty()
        || values.iter().any(|value| {
            is_mac(value)
                || matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "" | "all" | "cross-platform"
                )
        }))
}

fn override_path(original: &str, over: &Value) -> Result<String, String> {
    // Validate every supplied path, including replaced portions, so an override
    // cannot hide traversal in otherwise discarded input.
    relative_path(original)?;
    let add = over["addpath"].as_str().unwrap_or("");
    relative_path(add)?;
    let replace = match &over["replacepath"] {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::String(value) if value == "1" => true,
        Value::String(value) if value == "0" || value.is_empty() => false,
        _ => return Err("Unsupported Auto-Cloud replace-path flag".into()),
    };
    let mut path = if replace {
        add.to_owned()
    } else {
        [add, original]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("/")
    };
    for transform in numbered_entries(&over["pathtransforms"], "pathtransforms")? {
        let find = required_string(transform, "find")?.replace('\\', "/");
        let replace = required_string(transform, "replace")?.replace('\\', "/");
        let find_path = relative_path(&find)?;
        relative_path(&replace)?;
        if find_path.as_os_str().is_empty() {
            return Err("Empty Auto-Cloud path transform is unsupported".into());
        }
        path = path.replace('\\', "/");
        // A transform may replace a directory prefix, never an arbitrary piece
        // of a component. Reject ambiguous substitutions instead of guessing.
        let original_path = relative_path(&path)?;
        if let Ok(suffix) = original_path.strip_prefix(&find_path) {
            path = relative_path(&replace)?
                .join(suffix)
                .to_string_lossy()
                .into_owned();
        } else if path.starts_with(&find) {
            return Err("Auto-Cloud transform does not end at a directory boundary".into());
        }
    }
    Ok(path)
}

fn expand_user_path(path: &str, request: &Value) -> Result<String, String> {
    let mut expanded = path.to_owned();
    if path.contains("{64BitSteamID}") || path.contains("{Steam3AccountID}") {
        let id = request["steamid"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|id| *id != 0)
            .ok_or("Auto-Cloud user placeholder requires a valid SteamID")?;
        expanded = expanded
            .replace("{64BitSteamID}", &id.to_string())
            .replace("{Steam3AccountID}", &(id as u32).to_string());
    }
    if expanded.contains(['{', '}', '%', '$', '~']) {
        return Err("Unsupported or unresolved Steam Auto-Cloud path placeholder".into());
    }
    Ok(expanded)
}

fn relative_path(value: &str) -> Result<PathBuf, String> {
    if value.len() > MAX_PATH_BYTES
        || value.chars().any(|c| c.is_control())
        || value.contains([':', '*', '?', '[', ']'])
    {
        return Err("Unsafe Steam Auto-Cloud directory path".into());
    }
    let normalized = value.replace('\\', "/");
    let path = Path::new(&normalized);
    if path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
        return Err("Steam Auto-Cloud directory escapes its root".into());
    }
    Ok(path
        .components()
        .filter(|c| *c != Component::CurDir)
        .collect())
}

fn root_base(root: &str, request: &Value, home: &Path) -> Result<Option<PathBuf>, String> {
    Ok(match root.to_ascii_lowercase().as_str() {
        "machome" => Some(home.to_path_buf()),
        "macappsupport" => Some(home.join("Library/Application Support")),
        "macdocuments" => Some(home.join("Documents")),
        "gameinstall" | "appinstalldir" => {
            let install = request["install_dir"]
                .as_str()
                .ok_or("Missing verified installation")?;
            let install = Path::new(install)
                .canonicalize()
                .map_err(|_| "Cannot resolve the verified installation for Auto-Cloud")?;
            if !install.is_dir() {
                return Err("Auto-Cloud installation root is not a directory".into());
            }
            Some(install)
        }
        "winmydocuments" | "winappdatalocal" | "winappdatalocallow" | "winappdataroaming"
        | "winsavedgames" | "windowshome" | "linuxhome" | "linuxxdgdatahome" => None,
        "steamclouddocuments" => {
            return Err(
                "SteamCloudDocuments has no verified macOS mapping; no extra access was granted"
                    .into(),
            );
        }
        _ => return Err("Unsupported Steam Auto-Cloud root; no extra access was granted".into()),
    })
}

fn require_narrow_save_path(target: &Path, base: &Path, home: &Path) -> Result<(), String> {
    if target == base {
        return Err("Auto-Cloud requires a specific save subdirectory, not a whole root".into());
    }
    if base == home {
        let support = home.join("Library/Application Support");
        let documents = home.join("Documents");
        let container = [&support, &documents]
            .into_iter()
            .find(|container| target.starts_with(container))
            .ok_or("MacHome Auto-Cloud path is outside supported save containers")?;
        return require_narrow_save_path(target, container, home);
    }
    if base == home.join("Library/Application Support") {
        let first = target.strip_prefix(base).ok().and_then(|p| p.iter().next());
        let first = first
            .and_then(|p| p.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(
            first.as_str(),
            "steam"
                | "skinos"
                | "codex"
                | "google"
                | "mozilla"
                | "firefox"
                | "chromium"
                | "bravesoftware"
                | "1password"
                | "keychains"
        ) || first.starts_with("com.apple.")
            || first.starts_with("dev.skinos")
        {
            return Err("Auto-Cloud path names a protected application data directory".into());
        }
    }
    if target.strip_prefix(base).ok().is_none_or(|p| {
        p.components().any(|component| {
            component
                .as_os_str()
                .to_str()
                .is_none_or(|s| s.starts_with('.'))
        })
    }) {
        return Err("Auto-Cloud path is not a supported visible save subdirectory".into());
    }
    Ok(())
}

fn identity_component(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// UFS may propose a directory, but its spelling alone is not a grant. Match a
/// complete path component to identity from the catalog/verified installation,
/// never to a value taken from the UFS rule itself or to a substring.
fn require_game_identity(target: &Path, home: &Path, request: &Value) -> Result<(), String> {
    let containers = [
        home.join("Library/Application Support"),
        home.join("Documents"),
    ];
    let relative = containers
        .iter()
        .find_map(|base| target.strip_prefix(base).ok())
        .ok_or("Native save path is outside supported external containers")?;
    let mut identities = BTreeSet::new();
    if let Some(name) = request["name"].as_str() {
        identities.insert(identity_component(name));
    }
    if let Some(install) = request["install_dir"].as_str() {
        let install = Path::new(install)
            .canonicalize()
            .map_err(|_| "Cannot verify installation identity for native saves")?;
        if !install.is_dir() {
            return Err("Native save identity requires an installed directory".into());
        }
        if let Some(name) = install.file_name().and_then(|name| name.to_str()) {
            identities.insert(identity_component(name));
        }
        if let Some(executable) = request["executable"].as_str() {
            let executable = Path::new(executable)
                .canonicalize()
                .map_err(|_| "Cannot verify executable identity for native saves")?;
            if executable == install || !executable.starts_with(&install) {
                return Err("Native save identity executable escapes its installation".into());
            }
            if let Some(name) = executable.file_stem().and_then(|name| name.to_str()) {
                identities.insert(identity_component(name));
            }
            for bundle in executable
                .ancestors()
                .take_while(|path| path.starts_with(&install) && *path != install)
            {
                if bundle
                    .extension()
                    .is_some_and(|extension| extension == "app")
                    && bundle.is_dir()
                {
                    add_bundle_identities(bundle, &mut identities)?;
                    break;
                }
            }
        }
    }
    identities.remove("");
    if relative
        .iter()
        .filter_map(|part| part.to_str())
        .any(|part| identities.contains(&identity_component(part)))
    {
        return Ok(());
    }
    Err(format!(
        "Steam Auto-Cloud directory '{}' does not match the selected game's identity; no extra save access was granted",
        relative.display()
    ))
}

fn add_bundle_identities(bundle: &Path, identities: &mut BTreeSet<String>) -> Result<(), String> {
    let plist = bundle.join("Contents/Info.plist");
    if !plist.exists() {
        return Ok(());
    }
    let plist = plist
        .canonicalize()
        .map_err(|_| "Cannot verify native bundle save identity")?;
    if !plist.starts_with(bundle)
        || !plist.is_file()
        || fs::metadata(&plist)
            .map_err(|_| "Cannot inspect native bundle save identity")?
            .len()
            > 1024 * 1024
    {
        return Err(
            "Native bundle save identity metadata is outside the bundle or too large".into(),
        );
    }
    for key in ["CFBundleIdentifier", "CFBundleName"] {
        let output = std::process::Command::new("/usr/bin/plutil")
            .args(["-extract", key, "raw", "-o", "-", "--"])
            .arg(&plist)
            .output()
            .map_err(|_| "Cannot inspect native bundle save identity")?;
        if output.status.success() && output.stdout.len() <= 4096 {
            if let Ok(value) = std::str::from_utf8(&output.stdout) {
                identities.insert(identity_component(value.trim()));
            }
        }
    }
    Ok(())
}

fn check_existing_components(path: &Path) -> Result<(), String> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err("Native save directory is redirected by a symlink".into());
            }
            Ok(meta) if !meta.is_dir() => {
                return Err("Native save path contains a non-directory".into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(_) => return Err("Cannot inspect native save directory safely".into()),
        }
    }
    Ok(())
}

/// Check every existing component for redirection before and after creation.
/// Errors must never be converted to a grant on a broader parent directory.
pub(super) fn create_checked_directory(path: &Path) -> Result<(), String> {
    check_existing_components(path)?;
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        if let Err(error) = fs::create_dir(&current) {
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err("Cannot create native save directory".into());
            }
        }
        check_existing_components(&current)?;
    }
    if fs::canonicalize(path).ok().as_deref() != Some(path) {
        return Err("Native save directory changed while being created".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "skinos-save-policy-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(path.join("Library/Application Support")).unwrap();
            fs::create_dir(path.join("Documents")).unwrap();
            let bundle = path.join("Installed Fixture/Game.app/Contents");
            fs::create_dir_all(&bundle).unwrap();
            fs::write(bundle.join("Info.plist"), br#"<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>com.studio.game</string><key>CFBundleName</key><string>MacGame</string></dict></plist>"#).unwrap();
            Self(path.canonicalize().unwrap())
        }
        fn resolve(&self, cloud: Value) -> Result<Vec<PathBuf>, String> {
            self.resolve_as(cloud, "Game")
        }
        fn resolve_as(&self, cloud: Value, name: &str) -> Result<Vec<PathBuf>, String> {
            resolve(
                &json!({"command":"launch", "appid":424242, "name":name,"install_dir":self.0.join("Installed Fixture"),"executable":self.0.join("Installed Fixture/Game.app"), "steamid":"76561197960265770", "cloud_config":cloud}),
                &self.0,
            )
        }
        fn rule(&self, root: &str, path: &str) -> Result<Vec<PathBuf>, String> {
            self.resolve(json!({"savefiles":{"0":{"root":root,"path":path,"pattern":"*"}}}))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn mac_saves_follow_metadata_without_an_appid_allowlist() {
        let f = Fixture::new();
        let roots = f
            .resolve_as(
                json!({"savefiles":{"0":{"root":"MacAppSupport","path":"Brotato"}}}),
                "Brotato",
            )
            .unwrap();
        assert_eq!(roots, vec![f.0.join("Library/Application Support/Brotato")]);
        assert!(
            !roots[0].exists(),
            "read-only resolution must not create directories"
        );
    }

    #[test]
    fn multiple_save_locations_are_deduplicated() {
        let f = Fixture::new();
        let roots = f
            .resolve(json!({"savefiles":{
                "0":{"root":"MacAppSupport","path":"Studio/Game","pattern":"*.sav"},
                "1":{"root":"MacAppSupport","path":"Studio/Game","pattern":"*.cfg"},
                "2":{"root":"MacDocuments","path":"My Games/Game"}
            }}))
            .unwrap();
        assert_eq!(roots.len(), 2);
        assert!(roots.contains(&f.0.join("Library/Application Support/Studio/Game")));
        assert!(roots.contains(&f.0.join("Documents/My Games/Game")));
    }

    #[test]
    fn unrelated_documents_are_not_authorized_by_cloud_metadata() {
        let f = Fixture::new();
        assert!(f.rule("MacDocuments", "Taxes2026").is_err());
        assert!(f.rule("MacAppSupport", "AnotherGame/Backups").is_err());
        assert!(
            f.rule("MacDocuments", "GameSaves").is_err(),
            "substring identity is not proof"
        );
        assert!(f.rule("MacDocuments", "SomeGame").is_err());
        assert!(f.rule("MacDocuments", "GAME/Saves").is_ok());
    }

    #[test]
    fn external_paths_match_catalog_install_executable_or_bundle_identity() {
        let f = Fixture::new();
        for path in [
            "Game/Saves",
            "Installed Fixture/Saves",
            "com.studio.game/Saves",
            "MacGame/Saves",
        ] {
            assert!(
                f.rule("MacDocuments", path).is_ok(),
                "identity not recognized: {path}"
            );
        }
        assert!(
            f.resolve_as(
                json!({"savefiles":{"0":{"root":"MacAppSupport","path":"Space Adventure/Saves"}}}),
                "Space: Adventure"
            )
            .is_ok()
        );
    }

    #[test]
    fn external_paths_do_not_take_identity_from_an_escaping_executable() {
        let f = Fixture::new();
        let outside = Fixture::new();
        let request = json!({"command":"launch","name":"Other","install_dir":f.0.join("Installed Fixture"),"executable":outside.0.join("Installed Fixture/Game.app"),
            "cloud_config":{"savefiles":{"0":{"root":"MacDocuments","path":"Game"}}}});
        assert!(resolve(&request, &f.0).is_err());
    }

    #[test]
    fn user_placeholders_use_the_selected_account() {
        let f = Fixture::new();
        assert_eq!(
            f.rule("MacAppSupport", "Game/{64BitSteamID}/{Steam3AccountID}")
                .unwrap(),
            vec![f.0.join("Library/Application Support/Game/76561197960265770/42")]
        );
    }

    #[test]
    fn windows_paths_require_a_mac_override() {
        let f = Fixture::new();
        assert!(
            f.rule("WinAppDataLocalLow", "Studio/Game")
                .unwrap()
                .is_empty()
        );
        let roots = f.resolve(json!({
            "savefiles":{"0":{"root":"WinAppDataLocalLow","path":"Studio\\Game"}},
            "rootoverrides":{"0":{"root":"WinAppDataLocalLow","os":"MacOS","oscompare":"=","useinstead":"MacAppSupport","pathtransforms":{"0":{"find":"Studio/Game","replace":"com.studio.game"}}}}
        })).unwrap();
        assert_eq!(
            roots,
            vec![f.0.join("Library/Application Support/com.studio.game")]
        );
    }

    #[test]
    fn override_add_and_replace_paths_are_applied() {
        let f = Fixture::new();
        for (replace, expected) in [("0", "Studio/Game/Saves"), ("1", "Studio/Game")] {
            let roots = f.resolve(json!({
                "savefiles":{"0":{"root":"WinMyDocuments","path":"Saves"}},
                "rootoverrides":{"0":{"root":"WinMyDocuments","os":"MacOS","useinstead":"MacDocuments","addpath":"Studio/Game","replacepath":replace}}
            })).unwrap();
            assert_eq!(roots, vec![f.0.join("Documents").join(expected)]);
        }
    }

    #[test]
    fn non_mac_platform_restrictions_are_respected() {
        let f = Fixture::new();
        let roots = f
            .resolve(json!({"savefiles":{
                "0":{"root":"MacAppSupport","path":"WindowsOnly","platforms":{"1":"Windows"}},
                "1":{"root":"MacAppSupport","path":"MacGame","platforms":{"1":"MacOS","2":"Linux"}},
                "2":{"root":"MacAppSupport","path":"Other","oslist":"windows,linux"}
            }}))
            .unwrap();
        assert_eq!(roots, vec![f.0.join("Library/Application Support/MacGame")]);
    }

    #[test]
    fn mac_home_only_maps_into_narrow_supported_save_containers() {
        let f = Fixture::new();
        assert_eq!(
            f.rule("MacHome", "Library/Application Support/Studio/Game")
                .unwrap(),
            vec![f.0.join("Library/Application Support/Studio/Game")]
        );
        assert_eq!(
            f.rule("MacHome", "Documents/Game").unwrap(),
            vec![f.0.join("Documents/Game")]
        );
        for path in [
            "",
            ".",
            "Library",
            "Documents",
            "Library/Application Support",
            ".ssh",
            "Desktop",
            "Library/Keychains",
        ] {
            assert!(f.rule("MacHome", path).is_err(), "accepted {path}");
        }
    }

    #[test]
    fn broad_and_injected_paths_are_rejected() {
        let f = Fixture::new();
        for root in ["MacAppSupport", "MacDocuments"] {
            for path in [
                "",
                ".",
                "../escape",
                "Game/../../escape",
                "/tmp/escape",
                "C:\\escape",
                "\\escape",
                "Game/*",
                "Game\nOther",
                "~",
                "Game/{Unknown}",
                "$HOME/Game",
                "%USERPROFILE%/Game",
            ] {
                assert!(f.rule(root, path).is_err(), "accepted {root} {path}");
            }
        }
    }

    #[test]
    fn sensitive_application_roots_are_not_granted_by_cloud_metadata() {
        let f = Fixture::new();
        for path in [
            "Steam",
            "Steam/config",
            "SkinOS/accounts",
            "Google/Chrome",
            "com.apple.TCC",
            ".secrets",
        ] {
            assert!(f.rule("MacAppSupport", path).is_err(), "accepted {path}");
        }
    }

    #[test]
    fn directory_symlinks_do_not_expand_the_save_grant() {
        let f = Fixture::new();
        let outside = Fixture::new();
        std::os::unix::fs::symlink(&outside.0, f.0.join("Documents/Game")).unwrap();
        assert!(
            f.rule("MacDocuments", "Game/Saves")
                .unwrap_err()
                .contains("symlink")
        );
        assert!(!outside.0.join("Saves").exists());
    }

    #[test]
    fn a_file_cannot_be_used_as_a_save_directory() {
        let f = Fixture::new();
        fs::write(f.0.join("Documents/Game"), b"fixture").unwrap();
        assert!(
            f.rule("MacDocuments", "Game")
                .unwrap_err()
                .contains("non-directory")
        );
    }

    #[test]
    fn unsupported_roots_and_ambiguous_overrides_have_diagnostics() {
        let f = Fixture::new();
        assert!(
            f.rule("SteamCloudDocuments", "Game")
                .unwrap_err()
                .contains("verified macOS mapping")
        );
        assert!(
            f.rule("UnexpectedRoot", "Game")
                .unwrap_err()
                .contains("Unsupported")
        );
        assert!(
            f.resolve(json!({
                "savefiles":{"0":{"root":"WinMyDocuments","path":"Game"}},
                "rootoverrides":{
                    "0":{"root":"WinMyDocuments","os":"MacOS","useinstead":"MacDocuments"},
                    "1":{"root":"WinMyDocuments","os":"MacOS","useinstead":"MacAppSupport"}
                }
            }))
            .unwrap_err()
            .contains("ambiguous")
        );
    }

    #[test]
    fn malformed_metadata_does_not_silently_expand_access() {
        let f = Fixture::new();
        for cloud in [
            json!("bad"),
            json!({"savefiles":true}),
            json!({"savefiles":{"bad":{}}}),
            json!({"savefiles":{"0":{"path":"Game"}}}),
        ] {
            assert!(f.resolve(cloud).is_err());
        }
        let files: serde_json::Map<String, Value> = (0..=MAX_RULES)
            .map(|i| (i.to_string(), json!({"root":"MacDocuments","path":"Game"})))
            .collect();
        assert!(f.resolve(json!({"savefiles":files})).is_err());
    }

    #[test]
    fn invalid_path_transforms_are_rejected() {
        let f = Fixture::new();
        for (find, replacement) in [
            ("", "Game"),
            (".", "Game"),
            ("Studio", "../escape"),
            ("Stu", "Game"),
        ] {
            assert!(f.resolve(json!({
                "savefiles":{"0":{"root":"WinMyDocuments","path":"Studio/Game"}},
                "rootoverrides":{"0":{"root":"WinMyDocuments","os":"MacOS","useinstead":"MacDocuments","pathtransforms":{"0":{"find":find,"replace":replacement}}}}
            })).is_err());
        }
    }

    #[test]
    fn path_transforms_preserve_directory_boundaries_and_subdirectories() {
        let f = Fixture::new();
        let roots = f.resolve(json!({
            "savefiles":{"0":{"root":"WinMyDocuments","path":"Studio/Game/Saves"}},
            "rootoverrides":{"0":{"root":"WinMyDocuments","os":"MacOS","useinstead":"MacDocuments","pathtransforms":{"0":{"find":"Studio/Game/","replace":"com.studio.game"}}}}
        })).unwrap();
        assert_eq!(roots, vec![f.0.join("Documents/com.studio.game/Saves")]);
    }

    #[test]
    fn game_install_rules_are_confined_to_specific_children() {
        let f = Fixture::new();
        let install = f.0.join("Installed Game");
        fs::create_dir(&install).unwrap();
        let request = |path| {
            json!({
                "command":"launch","install_dir":install,
                "cloud_config":{"savefiles":{"0":{"root":"gameinstall","path":path}}}
            })
        };
        assert_eq!(
            resolve(&request("game/cfg"), &f.0).unwrap(),
            vec![install.join("game/cfg")]
        );
        for path in [".", "../Documents", "/tmp/Game"] {
            assert!(resolve(&request(path), &f.0).is_err());
        }
        std::os::unix::fs::symlink(f.0.join("Documents"), install.join("game")).unwrap();
        assert!(resolve(&request("game/cfg"), &f.0).is_err());
    }

    #[test]
    fn preparation_creates_only_fixture_subdirectories_and_rechecks_redirects() {
        let f = Fixture::new();
        let roots = f.rule("MacDocuments", "Studio/Game").unwrap();
        create_checked_directory(&roots[0]).unwrap();
        assert!(roots[0].is_dir());
        assert_eq!(f.rule("MacDocuments", "Studio/Game").unwrap(), roots);
        fs::remove_dir(&roots[0]).unwrap();
        let outside = Fixture::new();
        std::os::unix::fs::symlink(&outside.0, &roots[0]).unwrap();
        assert!(create_checked_directory(&roots[0]).is_err());
    }

    #[test]
    fn cloud_absence_and_non_native_commands_need_no_home_or_appid_rules() {
        for request in [
            json!({"command":"launch"}),
            json!({"command":"probe","cloud_config":true}),
            json!({"command":"launch","compatibility":{},"cloud_config":true}),
        ] {
            assert!(
                resolve(&request, Path::new("/fixture-does-not-exist"))
                    .unwrap()
                    .is_empty()
            );
        }
    }
}
