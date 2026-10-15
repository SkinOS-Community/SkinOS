use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

/// Verified from Brotato's shipped Godot project config and Steam UFS metadata.
/// Never derive a permission grant from arbitrary title text or allow all HOME.
pub fn native_save_root(request: &Value) -> Result<Option<PathBuf>, String> {
    if request["command"] != "launch" || !request["compatibility"].is_null() {
        return Ok(None);
    }
    if request["appid"] != 1942280 || request["name"] != "Brotato" {
        return Err("This game's native save-directory policy has not been verified yet".into());
    }
    let executable = Path::new(request["executable"].as_str().ok_or("Executable missing")?);
    if executable.file_name() != Some(std::ffi::OsStr::new("Brotato.app")) {
        return Err("Save policy does not match the native bundle".into());
    }
    let base = PathBuf::from(std::env::var_os("HOME").ok_or("User home unavailable")?)
        .join("Library/Application Support");
    let saves = base.join("Brotato");
    fs::create_dir_all(&saves).map_err(|e| e.to_string())?;
    let canonical = saves.canonicalize().map_err(|e| e.to_string())?;
    if canonical
        != base
            .canonicalize()
            .map_err(|e| e.to_string())?
            .join("Brotato")
    {
        return Err(
            "Native save root is unexpectedly redirected; no broad access was granted".into(),
        );
    }
    Ok(Some(canonical))
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
