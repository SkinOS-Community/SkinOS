//! Transaction preparation for updates. A new tree is fully verified before the
//! existing install is renamed; previous content remains available for recovery.
use crate::{InstallControl, network::PreparedDepot, safe_relative};
use anyhow::{Context, Result, ensure};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

const INVENTORY: &str = ".skinos-files.json";

pub(crate) fn write_inventory(root: &Path, depots: &[PreparedDepot]) -> Result<()> {
    let files = depots
        .iter()
        .flat_map(|d| d.files.iter())
        .filter(|f| f.flags.unwrap_or(0) & 64 == 0)
        .map(|f| safe_relative(f.filename.as_deref().context("Missing filename")?))
        .collect::<Result<Vec<_>>>()?;
    std::fs::write(root.join(INVENTORY), serde_json::to_vec_pretty(&files)?)?;
    Ok(())
}

/// Keep mods, saves, and unknown user files. Only files in our previous verified
/// inventory may be removed when absent from a new build of the same AppID.
/// Steam apps can share an install directory (CS 1.6 / Half-Life); a different
/// or unknown inventory owner must never authorize deleting their content.
pub(crate) fn preserve_extras(
    installed: &Path,
    staging: &Path,
    appid: u32,
    control: &InstallControl,
) -> Result<()> {
    if !installed.exists() {
        return Ok(());
    }
    let previous = installed.join(INVENTORY);
    ensure!(
        !previous.is_symlink(),
        "Install inventory cannot be a symlink"
    );
    let owner_path = installed.join(".skinos-install.json");
    ensure!(
        !owner_path.is_symlink(),
        "Install record cannot be a symlink"
    );
    #[derive(serde::Deserialize)]
    struct Owner {
        appid: u32,
    }
    let same_owner = match std::fs::read(&owner_path) {
        Ok(bytes) => {
            serde_json::from_slice::<Owner>(&bytes).is_ok_and(|owner| owner.appid == appid)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    let managed: HashSet<PathBuf> = if same_owner && previous.exists() {
        let paths: Vec<PathBuf> = serde_json::from_slice(&std::fs::read(previous)?)?;
        paths
            .into_iter()
            .map(|p| safe_relative(p.to_str().context("Invalid inventory filename")?))
            .collect::<Result<_>>()?
    } else {
        HashSet::new()
    };
    fn walk(
        base: &Path,
        dir: &Path,
        staging: &Path,
        managed: &HashSet<PathBuf>,
        control: &InstallControl,
    ) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            control.checkpoint()?;
            let entry = entry?;
            let from = entry.path();
            let relative = from.strip_prefix(base)?;
            if matches!(relative.to_str(), Some(".skinos-install.json" | INVENTORY)) {
                continue;
            }
            let to = staging.join(relative);
            let kind = entry.file_type()?;
            if kind.is_dir() {
                ensure!(
                    !to.is_symlink() && (!to.exists() || to.is_dir()),
                    "Update conflicts with a user directory: {}",
                    relative.display()
                );
                std::fs::create_dir_all(&to)?;
                walk(base, &from, staging, managed, control)?;
            } else if !to.exists() && !to.is_symlink() && !managed.contains(relative) {
                if kind.is_symlink() {
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(std::fs::read_link(&from)?, to)?;
                } else if kind.is_file() {
                    use std::io::{Read, Write};
                    let mut source = std::fs::File::open(&from)?;
                    let mut target = std::fs::File::create(&to)?;
                    let mut bytes = vec![0u8; 128 * 1024];
                    loop {
                        control.checkpoint()?;
                        let count = source.read(&mut bytes)?;
                        if count == 0 {
                            break;
                        }
                        target.write_all(&bytes[..count])?;
                    }
                    target.set_permissions(source.metadata()?.permissions())?;
                } else {
                    ensure!(
                        false,
                        "Unsupported user file in installation: {}",
                        relative.display()
                    );
                }
            }
        }
        Ok(())
    }
    walk(installed, installed, staging, &managed, control)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn update_preserves_mods_and_saves_but_removes_known_obsolete_content() {
        let old = tempfile::tempdir().unwrap();
        let new = tempfile::tempdir().unwrap();
        std::fs::create_dir(old.path().join("mods")).unwrap();
        std::fs::write(old.path().join("mods/custom"), b"mod").unwrap();
        std::fs::write(old.path().join("save"), b"save").unwrap();
        std::fs::write(old.path().join("old-binary"), b"stale").unwrap();
        std::fs::write(old.path().join("game"), b"old").unwrap();
        std::fs::write(new.path().join("game"), b"new").unwrap();
        std::fs::write(old.path().join(INVENTORY), br#"["game","old-binary"]"#).unwrap();
        std::fs::write(old.path().join(".skinos-install.json"), br#"{"appid":42}"#).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/tmp", old.path().join("user-link")).unwrap();
        preserve_extras(old.path(), new.path(), 42, &InstallControl::new()).unwrap();
        assert_eq!(std::fs::read(new.path().join("game")).unwrap(), b"new");
        assert_eq!(
            std::fs::read(new.path().join("mods/custom")).unwrap(),
            b"mod"
        );
        assert_eq!(std::fs::read(new.path().join("save")).unwrap(), b"save");
        assert!(!new.path().join("old-binary").exists());
        assert_eq!(std::fs::read(old.path().join("game")).unwrap(), b"old");
        #[cfg(unix)]
        assert_eq!(
            std::fs::read_link(new.path().join("user-link")).unwrap(),
            Path::new("/tmp")
        );
    }

    #[test]
    fn shared_install_keeps_other_app_content_in_previous_inventory() {
        let old = tempfile::tempdir().unwrap();
        let new = tempfile::tempdir().unwrap();
        std::fs::create_dir(old.path().join("cstrike")).unwrap();
        std::fs::write(
            old.path().join("cstrike/game.dll"),
            b"Counter-Strike content",
        )
        .unwrap();
        std::fs::write(old.path().join(INVENTORY), br#"["cstrike/game.dll"]"#).unwrap();
        std::fs::write(old.path().join(".skinos-install.json"), br#"{"appid":10}"#).unwrap();
        std::fs::write(new.path().join("hl.exe"), b"Half-Life update").unwrap();
        preserve_extras(old.path(), new.path(), 70, &InstallControl::new()).unwrap();
        assert_eq!(
            std::fs::read(new.path().join("cstrike/game.dll")).unwrap(),
            b"Counter-Strike content"
        );
        assert_eq!(
            std::fs::read(old.path().join("cstrike/game.dll")).unwrap(),
            b"Counter-Strike content"
        );
    }

    #[test]
    fn absent_or_malformed_owner_never_authorizes_obsolete_file_removal() {
        for owner in [None, Some(&b"{}"[..]), Some(&b"invalid"[..])] {
            let old = tempfile::tempdir().unwrap();
            let new = tempfile::tempdir().unwrap();
            std::fs::write(old.path().join("unknown-game"), b"keep").unwrap();
            std::fs::write(old.path().join(INVENTORY), br#"["unknown-game"]"#).unwrap();
            if let Some(bytes) = owner {
                std::fs::write(old.path().join(".skinos-install.json"), bytes).unwrap();
            }
            preserve_extras(old.path(), new.path(), 70, &InstallControl::new()).unwrap();
            assert_eq!(
                std::fs::read(new.path().join("unknown-game")).unwrap(),
                b"keep"
            );
        }
    }
}
