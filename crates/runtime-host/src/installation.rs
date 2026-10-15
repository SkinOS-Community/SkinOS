use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

mod save_roots;
pub use save_roots::native_save_roots;

/// Create only paths accepted by the read-only resolver, then re-resolve them
/// before the caller constructs its normal sandbox permissions.
pub fn prepare_native_save_roots(request: &Value) -> Result<Vec<PathBuf>, String> {
    let roots = native_save_roots(request)?;
    for root in &roots {
        save_roots::create_checked_directory(root)?;
    }
    if native_save_roots(request)? != roots {
        return Err("Native save paths changed while preparing the launch".into());
    }
    Ok(roots)
}

/// Register only a completed, caller-verified SkinOs installation in a private
/// Steam library. These manifests do not grant ownership; the runtime rechecks it.
pub fn stage(request: &Value, state: &Path) -> Result<Option<PathBuf>, String> {
    if request["command"] != "launch" {
        return Ok(None);
    }
    let appid = request["appid"]
        .as_u64()
        .filter(|id| *id > 0 && *id <= u32::MAX as u64)
        .ok_or("Invalid AppID")?;
    let install = fs::canonicalize(
        request["install_dir"]
            .as_str()
            .ok_or("Missing install directory")?,
    )
    .map_err(|e| format!("Installation unavailable: {e}"))?;
    if !install.is_dir() {
        return Err("Installation must be a directory".into());
    }
    let executable = fs::canonicalize(request["executable"].as_str().ok_or("Missing executable")?)
        .map_err(|e| format!("Executable unavailable: {e}"))?;
    if !executable.starts_with(&install) {
        return Err("Executable escapes the selected installation".into());
    }
    let build = request["build_id"]
        .as_str()
        .filter(|x| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit()))
        .ok_or("Missing verified build ID")?;
    let sid = request["steamid"]
        .as_str()
        .filter(|x| x.bytes().all(|b| b.is_ascii_digit()))
        .ok_or("Invalid SteamID")?;
    let depots = request["depots"]
        .as_array()
        .filter(|x| !x.is_empty())
        .ok_or("Missing verified depot manifests")?;
    let mut installed = String::new();
    for depot in depots {
        let id = depot["depot_id"].as_u64().ok_or("Invalid depot ID")?;
        let manifest = depot["manifest_id"]
            .as_str()
            .filter(|x| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit()))
            .ok_or("Invalid manifest ID")?;
        installed.push_str(&format!("\t\t\"{id}\" {{ \"manifest\" \"{manifest}\" }}\n"));
    }
    let library = state.join("library");
    let common = library.join("steamapps/common");
    fs::create_dir_all(&common).map_err(|e| e.to_string())?;
    let link = common.join(appid.to_string());
    if let Ok(metadata) = fs::symlink_metadata(&link) {
        if !metadata.file_type().is_symlink() {
            return Err("Private library entry is not a managed symlink".into());
        }
        if link.canonicalize().ok().as_ref() != Some(&install) {
            fs::remove_file(&link).map_err(|e| e.to_string())?;
        }
    }
    if fs::symlink_metadata(&link).is_err() {
        std::os::unix::fs::symlink(&install, &link).map_err(|e| e.to_string())?;
    }
    let name = request["name"]
        .as_str()
        .unwrap_or("SkinOs game")
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace(['\r', '\n'], " ");
    let acf = format!(
        "\"AppState\"\n{{\n\t\"appid\" \"{appid}\"\n\t\"Universe\" \"1\"\n\t\"name\" \"{name}\"\n\t\"StateFlags\" \"4\"\n\t\"installdir\" \"{appid}\"\n\t\"buildid\" \"{build}\"\n\t\"LastOwner\" \"{sid}\"\n\t\"AutoUpdateBehavior\" \"1\"\n\t\"InstalledDepots\"\n\t{{\n{installed}\t}}\n}}\n"
    );
    fs::write(
        library.join(format!("steamapps/appmanifest_{appid}.acf")),
        acf,
    )
    .map_err(|e| e.to_string())?;
    Ok(Some(install))
}

#[cfg(test)]
mod save_policy_regression_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unknown_native_game_without_cloud_metadata_has_no_extra_save_roots() {
        let request = json!({"command": "launch", "appid": 424242, "name": "Fixture"});
        assert!(native_save_roots(&request).unwrap().is_empty());
    }
}
