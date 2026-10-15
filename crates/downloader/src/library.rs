//! Interoperable Steam library discovery and atomic appmanifest updates.
mod uninstall;
use crate::{InstalledDepot, InstalledGame, safe_relative};
use anyhow::{Context, Result, ensure};
use keyvalues_parser::{Obj, Value, Vdf};
use std::{
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
pub use uninstall::{UninstallOutcome, UninstallPlan, uninstall, uninstall_plan};

fn lock_file(path: &Path) -> Result<std::fs::File> {
    ensure!(!path.is_symlink(), "Install lock cannot be a symlink");
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "Install lock is not a regular file"
    );
    fs2::FileExt::try_lock_exclusive(&file)
        .context("An installation or removal is already using this destination")?;
    Ok(file)
}
pub(crate) fn installation_lock(destination: &Path) -> Result<std::fs::File> {
    let parent = destination
        .parent()
        .context("Installation requires a parent directory")?;
    let name = destination
        .file_name()
        .context("Missing installation name")?;
    lock_file(&parent.join(format!(".{}.skinos-lock", name.to_string_lossy())))
}
pub(crate) fn operation_lock(parent: &Path, appid: u32) -> Result<std::fs::File> {
    lock_file(&parent.join(format!(".skinos-{appid}.operation-lock")))
}

pub fn default_steamapps_dir() -> Result<PathBuf> {
    Ok(
        PathBuf::from(std::env::var_os("HOME").context("HOME is unavailable")?)
            .join("Library/Application Support/Steam/steamapps"),
    )
}

pub fn manifest_path(steamapps: &Path, appid: u32) -> PathBuf {
    steamapps.join(format!("appmanifest_{appid}.acf"))
}

fn parse(bytes: &[u8]) -> Result<Vdf<'static>> {
    let parsed = keyvalues_parser::parse(std::str::from_utf8(bytes)?)?;
    ensure!(
        parsed.bases.is_empty() && parsed.key.eq_ignore_ascii_case("AppState"),
        "Invalid Steam appmanifest"
    );
    ensure!(parsed.value.is_obj(), "Invalid appmanifest state");
    Ok(parsed.into_vdf().into_owned())
}
fn value<'a>(obj: &'a Obj<'_>, key: &str) -> Option<&'a Value<'a>> {
    obj.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .and_then(|(_, v)| v.first())
}
fn text<'a>(obj: &'a Obj<'_>, key: &str) -> Option<&'a str> {
    value(obj, key).and_then(Value::get_str)
}
fn set(obj: &mut Obj<'static>, key: &str, value: Value<'static>) {
    obj.retain(|k, _| !k.eq_ignore_ascii_case(key));
    obj.insert(key.to_owned().into(), vec![value]);
}
fn set_text(obj: &mut Obj<'static>, key: &str, value: impl ToString) {
    set(obj, key, Value::Str(value.to_string().into()));
}
fn install_name(value: &str) -> Result<&str> {
    let path = safe_relative(value)?;
    ensure!(
        path.components().count() == 1
            && matches!(path.components().next(), Some(Component::Normal(_)))
            && !value.contains(['/', '\\']),
        "Unsafe Steam install directory"
    );
    Ok(value)
}

/// Reuse an existing manifest's exact directory; otherwise use PICS installdir.
pub fn installation_directory(steamapps: &Path, appid: u32, installdir: &str) -> Result<PathBuf> {
    if manifest_path(steamapps, appid).exists() {
        return Ok(read_appmanifest(steamapps, appid)?.install_dir);
    }
    let dir = steamapps.join("common").join(install_name(installdir)?);
    ensure!(!dir.is_symlink(), "Game directory cannot be a symlink");
    Ok(dir)
}

pub fn read_appmanifest(steamapps: &Path, appid: u32) -> Result<InstalledGame> {
    let path = manifest_path(steamapps, appid);
    ensure!(!path.is_symlink(), "Appmanifest cannot be a symlink");
    let parsed = parse(&std::fs::read(path)?)?;
    let obj = parsed.value.get_obj().context("Missing app state")?;
    ensure!(
        text(obj, "appid").and_then(|s| s.parse().ok()) == Some(appid),
        "Appmanifest identity mismatch"
    );
    let flags = text(obj, "StateFlags")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    ensure!(flags & 4 != 0, "Steam installation is not complete");
    let install_dir = steamapps.join("common").join(install_name(
        text(obj, "installdir").context("Missing install directory")?,
    )?);
    ensure!(
        install_dir.is_dir() && !install_dir.is_symlink(),
        "Installed game directory is missing or linked"
    );
    let common = steamapps.join("common").canonicalize()?;
    ensure!(
        install_dir.canonicalize()?.parent() == Some(common.as_path()),
        "Installation escapes library"
    );
    let mut depots = Vec::new();
    if let Some(obj) = value(obj, "InstalledDepots").and_then(Value::get_obj) {
        for (id, values) in obj.iter() {
            if let (Ok(depot_id), Some(depot)) =
                (id.parse::<u32>(), values.first().and_then(Value::get_obj))
                && let Some(manifest_id) =
                    text(depot, "manifest").filter(|v| v.parse::<u64>().is_ok())
            {
                depots.push(InstalledDepot {
                    depot_id,
                    manifest_id: manifest_id.into(),
                });
            }
        }
    }
    Ok(InstalledGame {
        appid,
        steamid: text(obj, "LastOwner").unwrap_or_default().into(),
        install_dir,
        build_id: text(obj, "buildid").unwrap_or("0").into(),
        depots,
    })
}

pub(crate) fn snapshot(steamapps: &Path, appid: u32) -> Result<Option<Vec<u8>>> {
    let path = manifest_path(steamapps, appid);
    ensure!(!path.is_symlink(), "Appmanifest cannot be a symlink");
    match std::fs::read(path) {
        Ok(data) => {
            parse(&data)?;
            Ok(Some(data))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Preserve unrecognized Steam fields and archive the previous manifest before
/// replacing it. Concurrent Steam changes are rejected rather than overwritten.
pub(crate) fn write_appmanifest(
    steamapps: &Path,
    record: &InstalledGame,
    name: &str,
    original: Option<&[u8]>,
    size: u64,
    transferred: u64,
    depot_sizes: &[(u32, u64)],
) -> Result<()> {
    ensure!(
        snapshot(steamapps, record.appid)?.as_deref() == original,
        "Steam changed this appmanifest during the download; retry after closing Steam"
    );
    let mut vdf = original
        .map(parse)
        .transpose()?
        .unwrap_or_else(|| Vdf::new("AppState".into(), Value::Obj(Obj::new())));
    let obj = vdf.value.get_mut_obj().context("Missing app state")?;
    let dirname = record
        .install_dir
        .file_name()
        .and_then(|s| s.to_str())
        .context("Invalid install name")?;
    install_name(dirname)?;
    for (key, val) in [
        ("appid", record.appid.to_string()),
        ("name", name.into()),
        ("StateFlags", "4".into()),
        ("Universe", "1".into()),
        ("installdir", dirname.into()),
        ("buildid", record.build_id.clone()),
        ("LastOwner", record.steamid.clone()),
        ("SizeOnDisk", size.to_string()),
        ("StagingSize", "0".into()),
        (
            "lastupdated",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)?
                .as_secs()
                .to_string(),
        ),
        ("UpdateResult", "0".into()),
        ("TargetBuildID", record.build_id.clone()),
        ("BytesToDownload", transferred.to_string()),
        ("BytesDownloaded", transferred.to_string()),
        ("BytesToStage", size.to_string()),
        ("BytesStaged", size.to_string()),
    ] {
        set_text(obj, key, val);
    }
    let mut depots = Obj::new();
    for depot in &record.depots {
        let mut entry = Obj::new();
        set_text(&mut entry, "manifest", &depot.manifest_id);
        if let Some((_, size)) = depot_sizes.iter().find(|(id, _)| *id == depot.depot_id) {
            set_text(&mut entry, "size", size);
        }
        set(&mut depots, &depot.depot_id.to_string(), Value::Obj(entry));
    }
    set(obj, "InstalledDepots", Value::Obj(depots));
    let path = manifest_path(steamapps, record.appid);
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    if let Some(original) = original {
        let dir = steamapps.join("skinos-backups");
        std::fs::create_dir_all(&dir)?;
        ensure!(!dir.is_symlink(), "Backup directory cannot be a symlink");
        std::fs::write(
            dir.join(format!("appmanifest_{}.{nonce}.acf", record.appid)),
            original,
        )?;
    }
    let tmp = steamapps.join(format!(".skinos-manifest-{}-{nonce}.tmp", record.appid));
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)?;
    file.write_all(vdf.to_string().as_bytes())?;
    file.sync_all()?;
    // Check again after archive creation and serialization.
    ensure!(
        snapshot(steamapps, record.appid)?.as_deref() == original,
        "Steam changed appmanifest before commit"
    );
    std::fs::rename(tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn imported_manifest_preserves_user_fields_and_rejects_concurrent_changes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("common/Game")).unwrap();
        let old = br#""AppState" { "appid" "42" "StateFlags" "4" "installdir" "Game" "buildid" "1" "UserConfig" { "language" "russian" } "InstalledDepots" { "43" { "manifest" "11" } } }"#;
        std::fs::write(manifest_path(dir.path(), 42), old).unwrap();
        let mut record = read_appmanifest(dir.path(), 42).unwrap();
        assert_eq!(record.depots[0].manifest_id, "11");
        record.build_id = "2".into();
        write_appmanifest(dir.path(), &record, "Game", Some(old), 10, 4, &[(43, 10)]).unwrap();
        let updated = std::fs::read_to_string(manifest_path(dir.path(), 42)).unwrap();
        assert!(updated.contains("russian"));
        assert_eq!(read_appmanifest(dir.path(), 42).unwrap().build_id, "2");
        assert!(write_appmanifest(dir.path(), &record, "Game", Some(old), 10, 4, &[]).is_err());
        assert_eq!(
            std::fs::read_dir(dir.path().join("skinos-backups"))
                .unwrap()
                .count(),
            1
        );
    }
    #[test]
    fn rejects_escaping_install_dirs() {
        for path in ["..", "../other", "/tmp/foo", "foo/bar", "foo\\bar", "."] {
            assert!(install_name(path).is_err());
        }
    }
}
