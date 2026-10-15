//! Remove obsolete SteamPipe staging only for an explicitly completed game.
//! Queue ownership/lifecycle is checked by the caller; filesystem locks and
//! committed installation evidence are checked again here before any removal.
use crate::{library, read_install};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{fs, path::Path};

#[derive(Debug, Default, Serialize)]
pub struct CacheCleanup {
    pub appid: u32,
    pub removed_directories: usize,
    pub removed_bytes: u64,
    pub retained_directories: usize,
}

fn numeric_build(value: &str) -> Option<u64> {
    (!value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
        .filter(|build| *build > 0)
}

/// The caller must hold its queue's completed-state reservation. Active,
/// paused, cancelled and failed download intents must never call this API.
pub fn cleanup_completed_download_cache(steamapps: &Path, appid: u32) -> Result<CacheCleanup> {
    ensure!(appid > 0, "Invalid AppID for download cache cleanup");
    ensure!(!steamapps.is_symlink(), "Library root cannot be a symlink");
    let common = steamapps.join("common");
    ensure!(
        !common.is_symlink(),
        "Library common directory cannot be a symlink"
    );
    let initial = library::read_appmanifest(steamapps, appid)?;
    let _installation = library::installation_lock(&initial.install_dir)?;
    let _operation = library::operation_lock(&common, appid)?;
    // Re-read after acquiring both locks; another installer may have committed
    // between discovery and lock acquisition.
    let committed = library::read_appmanifest(steamapps, appid)?;
    ensure!(
        committed.install_dir == initial.install_dir,
        "Installation directory changed"
    );
    ensure!(
        !committed
            .install_dir
            .join(".skinos-install.json")
            .is_symlink(),
        "Installation record cannot be a symlink"
    );
    let owned = read_install(&committed.install_dir)?;
    ensure!(
        owned.appid == appid && owned.build_id == committed.build_id,
        "Download cache cleanup requires a committed SkinOS installation"
    );
    let completed_build = numeric_build(&committed.build_id)
        .context("Invalid completed build for download cache cleanup")?;
    let root = common.canonicalize()?;
    let prefix = format!(".skinos-{appid}-");
    let mut outcome = CacheCleanup {
        appid,
        ..Default::default()
    };
    for entry in fs::read_dir(&common)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(build) = name
            .to_str()
            .and_then(|name| name.strip_prefix(&prefix))
            .and_then(|name| name.strip_suffix(".partial"))
            .and_then(numeric_build)
        else {
            continue;
        };
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        // Keep same/newer builds: those can still belong to a resumable attempt.
        // A linked top-level cache is not an owned staging directory.
        if build >= completed_build || !metadata.is_dir() || metadata.file_type().is_symlink() {
            outcome.retained_directories += 1;
            continue;
        }
        ensure!(
            path.canonicalize()?.parent() == Some(root.as_path()),
            "Staging directory escapes library"
        );
        let bytes = regular_bytes(&path)?;
        // Never traverse links when estimating or removing. Rust's remove_dir_all
        // removes nested symlinks themselves and leaves their targets untouched.
        let current = fs::symlink_metadata(&path)?;
        use std::os::unix::fs::MetadataExt;
        ensure!(
            current.is_dir()
                && !current.file_type().is_symlink()
                && current.dev() == metadata.dev()
                && current.ino() == metadata.ino(),
            "Staging directory changed before cleanup"
        );
        fs::remove_dir_all(&path)?;
        outcome.removed_directories += 1;
        outcome.removed_bytes = outcome.removed_bytes.saturating_add(bytes);
    }
    Ok(outcome)
}

fn regular_bytes(root: &Path) -> Result<u64> {
    let mut bytes = 0u64;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        bytes = bytes.saturating_add(if metadata.is_dir() {
            regular_bytes(&entry.path())?
        } else if metadata.is_file() {
            metadata.len()
        } else {
            0
        });
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("common/Game");
        fs::create_dir_all(&game).unwrap();
        fs::write(game.join("game"), b"installed").unwrap();
        fs::write(
            library::manifest_path(root.path(), 42),
            br#""AppState" { "appid" "42" "StateFlags" "4" "installdir" "Game" "buildid" "20" }"#,
        )
        .unwrap();
        fs::write(
            game.join(".skinos-install.json"),
            serde_json::to_vec(&crate::InstalledGame {
                appid: 42,
                steamid: "1".into(),
                install_dir: game.canonicalize().unwrap(),
                build_id: "20".into(),
                platform: None,
                depots: vec![],
            })
            .unwrap(),
        )
        .unwrap();
        root
    }
    fn cache(root: &Path, name: &str) -> std::path::PathBuf {
        let path = root.join("common").join(name);
        fs::create_dir(&path).unwrap();
        fs::write(path.join("content"), b"cache").unwrap();
        path
    }
    #[test]
    fn removes_only_older_owned_partial_after_successful_commit() {
        let root = fixture();
        let old = cache(root.path(), ".skinos-42-19.partial");
        let preserved = [
            ".skinos-42-20.partial",
            ".skinos-42-21.partial",
            ".skinos-420-19.partial",
            ".skinos-42-19.previous",
            ".skinos-42-other.partial",
        ]
        .map(|name| cache(root.path(), name));
        let outcome = cleanup_completed_download_cache(root.path(), 42).unwrap();
        assert_eq!(outcome.removed_directories, 1);
        assert_eq!(outcome.removed_bytes, 5);
        assert!(!old.exists());
        for path in preserved {
            assert!(path.join("content").exists());
        }
        assert_eq!(
            fs::read(root.path().join("common/Game/game")).unwrap(),
            b"installed"
        );
    }
    #[test]
    fn missing_or_incomplete_manifest_and_active_writer_preserve_cache() {
        let root = fixture();
        let old = cache(root.path(), ".skinos-42-19.partial");
        let lock = library::operation_lock(&root.path().join("common"), 42).unwrap();
        assert!(cleanup_completed_download_cache(root.path(), 42).is_err());
        drop(lock);
        let manifest = library::manifest_path(root.path(), 42);
        fs::write(
            &manifest,
            br#""AppState" { "appid" "42" "StateFlags" "2" "installdir" "Game" "buildid" "20" }"#,
        )
        .unwrap();
        assert!(cleanup_completed_download_cache(root.path(), 42).is_err());
        fs::remove_file(manifest).unwrap();
        assert!(cleanup_completed_download_cache(root.path(), 42).is_err());
        assert!(old.join("content").exists());
    }
    #[test]
    fn symlinks_never_follow_targets_and_uncommitted_record_preserves_cache() {
        let root = fixture();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("keep"), b"outside").unwrap();
        let link = root.path().join("common/.skinos-42-18.partial");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        let old = cache(root.path(), ".skinos-42-19.partial");
        std::os::unix::fs::symlink(outside.path(), old.join("outside")).unwrap();
        cleanup_completed_download_cache(root.path(), 42).unwrap();
        assert!(link.is_symlink());
        assert_eq!(fs::read(outside.path().join("keep")).unwrap(), b"outside");
        let old = cache(root.path(), ".skinos-42-19.partial");
        fs::remove_file(root.path().join("common/Game/.skinos-install.json")).unwrap();
        assert!(cleanup_completed_download_cache(root.path(), 42).is_err());
        assert!(old.join("content").exists());
    }
}
