//! Stable legacy Steam DLL alias for games which search above their install.
//! Create it before the sandboxed runner; never replace a user's existing entry.
use crate::Result;
use std::{fs, io::ErrorKind, path::Path};

pub(super) fn stage_shared_client(
    support: &Path,
    install: &Path,
    steamapps: &Path,
    data_dir: &Path,
) -> Result<()> {
    let install = install.canonicalize().map_err(|e| e.to_string())?;
    let parent = install.parent().ok_or("Missing game installation parent")?;
    let allowed = [steamapps.join("common"), data_dir.join("games")]
        .iter()
        .any(|root| root.canonicalize().is_ok_and(|root| root == parent));
    if !allowed {
        return Err("Legacy Steam DLL destination is outside the selected library".into());
    }
    let destination = parent.join("Steam.dll");
    match fs::symlink_metadata(&destination) {
        // Includes dangling/foreign symlinks. We do not own those entries and
        // must not emulate upstream's unconditional ln -sfn replacement.
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(format!("Cannot inspect legacy Steam DLL: {error}")),
    }
    let source = support.join("bridge/legacycompat/Steam.dll");
    if source.is_symlink() || !source.is_file() {
        return Err("Missing checked legacy Steam DLL in the NotProton payload".into());
    }
    let owned = support.canonicalize().map_err(|e| e.to_string())?;
    let source = source.canonicalize().map_err(|e| e.to_string())?;
    if !source.starts_with(owned) {
        return Err("Legacy Steam DLL escapes owned NotProton data".into());
    }
    // The bytes were hash-checked by copy_checked_payload. This stable path
    // survives runtime log cleanup and is atomically refreshed on later updates.
    match std::os::unix::fs::symlink(source, destination) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(format!("Cannot prepare legacy Steam DLL: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_uses_stable_owned_bridge_and_tracks_updated_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let steamapps = temp.path().join("steamapps");
        let data = temp.path().join("skinos");
        let support = data.join("notproton");
        let source = support.join("bridge/legacycompat/Steam.dll");
        let install = steamapps.join("common/Game");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::create_dir_all(&install).unwrap();
        fs::write(&source, b"old fixture DLL").unwrap();
        stage_shared_client(&support, &install, &steamapps, &data).unwrap();
        let alias = steamapps.join("common/Steam.dll");
        assert_eq!(
            fs::read_link(&alias).unwrap(),
            source.canonicalize().unwrap()
        );
        let next = source.with_extension("new");
        fs::write(&next, b"new fixture DLL").unwrap();
        fs::rename(next, &source).unwrap();
        assert_eq!(fs::read(&alias).unwrap(), b"new fixture DLL");
        stage_shared_client(&support, &install, &steamapps, &data).unwrap();
    }

    #[test]
    fn preserves_regular_files_and_even_dangling_foreign_links() {
        let temp = tempfile::tempdir().unwrap();
        let steamapps = temp.path().join("steamapps");
        let data = temp.path().join("skinos");
        let install = steamapps.join("common/Game");
        fs::create_dir_all(&install).unwrap();
        let alias = steamapps.join("common/Steam.dll");
        fs::write(&alias, b"user file").unwrap();
        stage_shared_client(&data.join("notproton"), &install, &steamapps, &data).unwrap();
        assert_eq!(fs::read(&alias).unwrap(), b"user file");
        fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink("missing-user-owned-dll", &alias).unwrap();
        stage_shared_client(&data.join("notproton"), &install, &steamapps, &data).unwrap();
        assert_eq!(
            fs::read_link(&alias).unwrap(),
            Path::new("missing-user-owned-dll")
        );
    }

    #[test]
    fn refuses_unknown_parent_and_escaped_bridge_without_writing_an_alias() {
        let temp = tempfile::tempdir().unwrap();
        let steamapps = temp.path().join("steamapps");
        let data = temp.path().join("skinos");
        let support = data.join("notproton");
        let unknown = temp.path().join("unrelated/Game");
        fs::create_dir_all(&unknown).unwrap();
        assert!(stage_shared_client(&support, &unknown, &steamapps, &data).is_err());
        assert!(!unknown.parent().unwrap().join("Steam.dll").exists());
        let install = steamapps.join("common/Game");
        fs::create_dir_all(&install).unwrap();
        fs::create_dir_all(support.join("bridge")).unwrap();
        let external = temp.path().join("external");
        fs::create_dir_all(&external).unwrap();
        fs::write(external.join("Steam.dll"), b"external fixture").unwrap();
        std::os::unix::fs::symlink(&external, support.join("bridge/legacycompat")).unwrap();
        assert!(stage_shared_client(&support, &install, &steamapps, &data).is_err());
        assert!(fs::symlink_metadata(steamapps.join("common/Steam.dll")).is_err());
    }
}
