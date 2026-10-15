//! Approved, revalidated removal of an installed game's local files.
//! No installation mutation occurs while preparing a plan.
use super::{manifest_path, parse, read_appmanifest, text};
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UninstallPlan {
    pub appid: u32,
    pub install_dir: PathBuf,
    pub additional_install_dirs: Vec<PathBuf>,
    pub game_bytes: u64,
    pub partial_bytes: u64,
    pub remove_bytes: u64,
    pub shared_appids: Vec<u32>,
    pub partial_dirs: Vec<PathBuf>,
    #[serde(skip)]
    approval: Approval,
}
#[derive(Debug, Clone, Serialize)]
pub struct UninstallOutcome {
    pub appid: u32,
    pub install_dir: PathBuf,
    pub shared_directory_retained: bool,
    pub removed_bytes: u64,
    pub removed_partial_dirs: usize,
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct EntryStamp {
    identity: Identity,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
    kind: u8,
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Tree {
    path: PathBuf,
    identity: Identity,
    bytes: u64,
    entries: BTreeMap<PathBuf, EntryStamp>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Root {
    path: PathBuf,
    identity: Identity,
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Manifest {
    path: PathBuf,
    identity: Identity,
    bytes: Vec<u8>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    tree: Tree,
    remove: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Approval {
    steamapps_argument: PathBuf,
    legacy_argument: Option<PathBuf>,
    roots: Vec<Root>,
    targets: Vec<Target>,
    partials: Vec<Tree>,
    manifest: Option<Manifest>,
}

fn identity(metadata: &fs::Metadata) -> Identity {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Identity {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
    #[cfg(not(unix))]
    compile_error!("Safe Steam uninstall requires Unix filesystem identities");
}
fn stamp(metadata: &fs::Metadata) -> Result<EntryStamp> {
    use std::os::unix::fs::MetadataExt;
    let kind = metadata.file_type();
    ensure!(
        kind.is_file() || kind.is_dir() || kind.is_symlink(),
        "Unsupported special file in game directory"
    );
    Ok(EntryStamp {
        identity: identity(metadata),
        length: metadata.len(),
        modified: (metadata.mtime(), metadata.mtime_nsec()),
        changed: (metadata.ctime(), metadata.ctime_nsec()),
        kind: if kind.is_file() {
            1
        } else if kind.is_dir() {
            2
        } else {
            3
        },
    })
}
fn root_if_present(path: &Path) -> Result<Option<Root>> {
    ensure!(
        !path.is_symlink(),
        "Library root cannot be a symlink: {}",
        path.display()
    );
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        metadata.is_dir(),
        "Library root is not a directory: {}",
        path.display()
    );
    Ok(Some(Root {
        path: path.canonicalize()?,
        identity: identity(&metadata),
    }))
}
fn tree(path: &Path, parent: &Path) -> Result<Tree> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "Game/cache directory is missing or linked: {}",
        path.display()
    );
    let canonical = path.canonicalize()?;
    ensure!(
        canonical.parent() == Some(parent),
        "Game/cache directory escapes its library"
    );
    let mut result = Tree {
        path: canonical,
        identity: identity(&metadata),
        bytes: 0,
        entries: BTreeMap::new(),
    };
    fn walk(base: &Path, directory: &Path, result: &mut Tree) -> Result<()> {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            let metadata = fs::symlink_metadata(&path)?;
            let entry_stamp = stamp(&metadata)?;
            // Symlinks are counted as directory entries, never traversed.
            if !metadata.is_dir() {
                result.bytes = result
                    .bytes
                    .checked_add(metadata.len())
                    .context("Uninstall size overflow")?;
            }
            result
                .entries
                .insert(path.strip_prefix(base)?.to_owned(), entry_stamp);
            if metadata.is_dir() {
                walk(base, &path, result)?;
            }
        }
        Ok(())
    }
    let base = result.path.clone();
    walk(&base, &base, &mut result)?;
    Ok(result)
}
fn read_manifest(path: &Path) -> Result<Manifest> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "Appmanifest must be a regular nonlinked file"
    );
    let bytes = fs::read(path)?;
    parse(&bytes)?;
    Ok(Manifest {
        path: path.to_owned(),
        identity: identity(&metadata),
        bytes,
    })
}
fn shared_apps(steamapps: &Path, selected: u32, selected_dir: &Path) -> Result<Vec<u32>> {
    let common = steamapps.join("common");
    let mut shared = Vec::new();
    for entry in fs::read_dir(steamapps)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(appid) = name
            .strip_prefix("appmanifest_")
            .and_then(|s| s.strip_suffix(".acf"))
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if appid == selected {
            continue;
        }
        let manifest = read_manifest(&entry.path())
            .with_context(|| format!("Cannot safely inspect {name}"))?;
        let parsed = parse(&manifest.bytes)?;
        let object = parsed.value.get_obj().context("Missing app state")?;
        ensure!(
            text(object, "appid").and_then(|s| s.parse::<u32>().ok()) == Some(appid),
            "Other appmanifest identity mismatch"
        );
        let dirname = super::install_name(
            text(object, "installdir").context("Other appmanifest has no install directory")?,
        )?;
        let candidate = common.join(dirname);
        ensure!(
            !candidate.is_symlink(),
            "Another appmanifest uses a linked game directory"
        );
        // During the final post-rename check this directory may already be in
        // its tombstone; a newly created sharing manifest still protects it.
        let candidate = if candidate.exists() {
            candidate.canonicalize()?
        } else {
            candidate
        };
        if candidate == selected_dir {
            shared.push(appid);
        }
    }
    shared.sort_unstable();
    shared.dedup();
    Ok(shared)
}
fn partials(root: &Root, appid: u32) -> Result<Vec<Tree>> {
    let prefix = format!(".skinos-{appid}-");
    let mut result = Vec::new();
    for entry in fs::read_dir(&root.path)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(build) = name
            .to_str()
            .and_then(|s| s.strip_prefix(&prefix))
            .and_then(|s| s.strip_suffix(".partial"))
        else {
            continue;
        };
        if build.is_empty() || !build.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        result.push(tree(&entry.path(), &root.path)?);
    }
    result.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(result)
}
fn absolute(path: &Path) -> Result<PathBuf> {
    Ok(if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    })
}
fn build_plan(
    steamapps: &Path,
    appid: u32,
    legacy_games_root: Option<&Path>,
) -> Result<UninstallPlan> {
    ensure!(appid != 0, "Invalid AppID");
    let steamapps_argument = absolute(steamapps)?;
    let legacy_argument = legacy_games_root.map(absolute).transpose()?;
    let steam_root = root_if_present(&steamapps_argument)?;
    let common = steam_root
        .as_ref()
        .map(|r| root_if_present(&r.path.join("common")))
        .transpose()?
        .flatten();
    if let (Some(root), Some(common)) = (&steam_root, &common) {
        ensure!(
            common.path.parent() == Some(root.path.as_path()),
            "Steam common directory escapes its library"
        );
    }
    let legacy = legacy_argument
        .as_deref()
        .map(root_if_present)
        .transpose()?
        .flatten();
    let mut roots = Vec::new();
    if let Some(root) = &steam_root {
        roots.push(root.clone());
    }
    if let Some(root) = &common {
        roots.push(root.clone());
    }
    if let Some(root) = &legacy
        && !roots.iter().any(|r: &Root| r.path == root.path)
    {
        roots.push(root.clone());
    }
    let mut targets = Vec::new();
    let mut manifest = None;
    let mut shared_appids = Vec::new();
    if let Some(steamapps) = &steam_root {
        let path = manifest_path(&steamapps.path, appid);
        if path.exists() || path.is_symlink() {
            let captured = read_manifest(&path)?;
            let record = read_appmanifest(&steamapps.path, appid)?;
            let common = common
                .as_ref()
                .context("Steam common directory is missing")?;
            let install = tree(&record.install_dir, &common.path)?;
            shared_appids = shared_apps(&steamapps.path, appid, &install.path)?;
            targets.push(Target {
                tree: install,
                remove: shared_appids.is_empty(),
            });
            manifest = Some(captured);
        }
    }
    if let Some(root) = &legacy {
        let directory = root.path.join(appid.to_string());
        if directory.exists() || directory.is_symlink() {
            let install = tree(&directory, &root.path)?;
            let record_path = directory.join(".skinos-install.json");
            let record_meta = fs::symlink_metadata(&record_path)?;
            ensure!(
                record_meta.is_file() && !record_meta.file_type().is_symlink(),
                "Legacy install record cannot be linked"
            );
            let record = crate::read_install(&directory)?;
            ensure!(
                record.appid == appid && record.install_dir.canonicalize()? == install.path,
                "Legacy install identity mismatch"
            );
            if !targets
                .iter()
                .any(|target| target.tree.path == install.path)
            {
                targets.push(Target {
                    tree: install,
                    remove: true,
                });
            }
        }
    }
    ensure!(
        !targets.is_empty(),
        "No complete installation found for AppID {appid}"
    );
    let mut partial_trees = Vec::new();
    for root in [&common, &legacy].into_iter().flatten() {
        for partial in partials(root, appid)? {
            if !partial_trees.iter().any(|p: &Tree| p.path == partial.path) {
                partial_trees.push(partial);
            }
        }
    }
    partial_trees.sort_by(|a, b| a.path.cmp(&b.path));
    let game_bytes = targets.iter().try_fold(0u64, |size, target| {
        size.checked_add(target.tree.bytes)
            .context("Uninstall size overflow")
    })?;
    let partial_bytes = partial_trees.iter().try_fold(0u64, |size, target| {
        size.checked_add(target.bytes)
            .context("Uninstall size overflow")
    })?;
    let remove_bytes =
        targets
            .iter()
            .filter(|t| t.remove)
            .try_fold(partial_bytes, |size, target| {
                size.checked_add(target.tree.bytes)
                    .context("Uninstall size overflow")
            })?;
    Ok(UninstallPlan {
        appid,
        install_dir: targets[0].tree.path.clone(),
        additional_install_dirs: targets
            .iter()
            .skip(1)
            .map(|t| t.tree.path.clone())
            .collect(),
        game_bytes,
        partial_bytes,
        remove_bytes,
        shared_appids,
        partial_dirs: partial_trees.iter().map(|t| t.path.clone()).collect(),
        approval: Approval {
            steamapps_argument,
            legacy_argument,
            roots,
            targets,
            partials: partial_trees,
            manifest,
        },
    })
}
fn acquire_locks(plan: &UninstallPlan) -> Result<Vec<fs::File>> {
    let mut destinations = plan
        .approval
        .targets
        .iter()
        .map(|target| target.tree.path.clone())
        .collect::<Vec<_>>();
    destinations.sort();
    destinations.dedup();
    let mut locks = Vec::new();
    for directory in destinations {
        locks.push(super::installation_lock(&directory)?);
    }
    let mut parents = plan
        .approval
        .targets
        .iter()
        .map(|t| t.tree.path.parent().unwrap().to_owned())
        .chain(
            plan.approval
                .partials
                .iter()
                .map(|p| p.path.parent().unwrap().to_owned()),
        )
        .collect::<Vec<_>>();
    parents.sort();
    parents.dedup();
    for parent in parents {
        locks.push(super::operation_lock(&parent, plan.appid)?);
    }
    Ok(locks)
}
/// Returns exactly the game directories and private partial caches to display in
/// the confirmation. External saves, Wine prefixes and previous backups are out
/// of scope. Legacy copies must be shown explicitly so fallback cannot revive it.
pub fn uninstall_plan(
    steamapps: &Path,
    appid: u32,
    legacy_games_root: Option<&Path>,
) -> Result<UninstallPlan> {
    let plan = build_plan(steamapps, appid, legacy_games_root)?;
    let _locks = acquire_locks(&plan)?;
    ensure!(
        build_plan(steamapps, appid, legacy_games_root)? == plan,
        "Installation changed while preparing removal; refresh the plan"
    );
    Ok(plan)
}
struct Moved {
    from: PathBuf,
    to: PathBuf,
    identity: Identity,
}
fn rollback(moved: &[Moved]) -> Vec<String> {
    let mut failures = Vec::new();
    for item in moved.iter().rev() {
        if item.from.exists() || item.from.is_symlink() {
            failures.push(format!(
                "{} already exists; retained {}",
                item.from.display(),
                item.to.display()
            ));
        } else if let Err(error) = fs::rename(&item.to, &item.from) {
            failures.push(format!("{}: {error}", item.to.display()));
        }
    }
    failures
}
fn private_directory(parent: &Path, appid: u32, nonce: u128) -> Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let path = parent.join(format!(".skinos-uninstall-{appid}-{nonce}"));
    fs::DirBuilder::new().mode(0o700).create(&path)?;
    Ok(path)
}
/// Executes a previously displayed plan. Call on a blocking thread. An error
/// after file deletion begins names retained tombstones; it is never success.
pub fn uninstall(plan: &UninstallPlan) -> Result<UninstallOutcome> {
    let _locks = acquire_locks(plan)?;
    let current = build_plan(
        &plan.approval.steamapps_argument,
        plan.appid,
        plan.approval.legacy_argument.as_deref(),
    )?;
    ensure!(
        &current == plan,
        "Installation or shared apps changed since confirmation; refresh the removal plan"
    );
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let mut tombstones = BTreeMap::<PathBuf, PathBuf>::new();
    let mut moved = Vec::<Moved>::new();
    let transaction = (|| -> Result<()> {
        for directory in plan
            .approval
            .targets
            .iter()
            .filter(|t| t.remove)
            .map(|t| &t.tree)
            .chain(plan.approval.partials.iter())
        {
            let parent = directory.path.parent().context("Missing removal parent")?;
            let tombstone = match tombstones.get(parent) {
                Some(path) => path.clone(),
                None => {
                    let path = private_directory(parent, plan.appid, nonce)?;
                    tombstones.insert(parent.to_owned(), path.clone());
                    path
                }
            };
            let to = tombstone.join(format!("directory-{}", moved.len()));
            fs::rename(&directory.path, &to)?;
            moved.push(Moved {
                from: directory.path.clone(),
                to,
                identity: directory.identity.clone(),
            });
        }
        if let Some(manifest) = &plan.approval.manifest {
            let parent = manifest.path.parent().unwrap();
            let tombstone = match tombstones.get(parent) {
                Some(path) => path.clone(),
                None => {
                    let path = private_directory(parent, plan.appid, nonce)?;
                    tombstones.insert(parent.to_owned(), path.clone());
                    path
                }
            };
            let to = tombstone.join(manifest.path.file_name().unwrap());
            fs::rename(&manifest.path, &to)?;
            moved.push(Moved {
                from: manifest.path.clone(),
                to: to.clone(),
                identity: manifest.identity.clone(),
            });
            ensure!(
                fs::read(&to)? == manifest.bytes,
                "Appmanifest changed during removal"
            );
            ensure!(
                shared_apps(parent, plan.appid, &plan.install_dir)? == plan.shared_appids,
                "Shared installation changed during removal"
            );
        }
        for item in &moved {
            ensure!(
                identity(&fs::symlink_metadata(&item.to)?) == item.identity,
                "Removal target identity changed"
            );
        }
        Ok(())
    })();
    if let Err(error) = transaction {
        let failures = rollback(&moved);
        for tombstone in tombstones.values() {
            let _ = fs::remove_dir(tombstone);
        }
        if failures.is_empty() {
            return Err(error.context("Removal was rolled back before deleting game files"));
        }
        bail!(
            "Removal could not be completed or fully rolled back: {error}; {}",
            failures.join("; ")
        );
    }
    // Rust's remove_dir_all unlinks symlinks instead of traversing them. Each
    // tree is now behind a private, same-filesystem directory owned by this run.
    for tombstone in tombstones.values() {
        fs::remove_dir_all(tombstone).with_context(|| format!("Removal is incomplete; remaining files are in {} (game manifest may already be removed)", tombstone.display()))?;
    }
    Ok(UninstallOutcome {
        appid: plan.appid,
        install_dir: plan.install_dir.clone(),
        shared_directory_retained: !plan.shared_appids.is_empty(),
        removed_bytes: plan.remove_bytes,
        removed_partial_dirs: plan.partial_dirs.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn steam_fixture(base: &Path, appid: u32, dirname: &str) -> PathBuf {
        let root = base.join("steamapps");
        fs::create_dir_all(root.join("common").join(dirname)).unwrap();
        put_manifest(&root, appid, dirname);
        root
    }
    fn put_manifest(root: &Path, appid: u32, dirname: &str) {
        fs::write(manifest_path(root, appid), format!(r#""AppState" {{ "appid" "{appid}" "StateFlags" "4" "installdir" "{dirname}" "buildid" "1" }}"#)).unwrap();
    }
    fn legacy_fixture(base: &Path, appid: u32) -> PathBuf {
        let root = base.join("legacy-games");
        let directory = root.join(appid.to_string());
        fs::create_dir_all(&directory).unwrap();
        let record = crate::InstalledGame {
            appid,
            steamid: "1".into(),
            install_dir: directory.canonicalize().unwrap(),
            build_id: "1".into(),
            depots: vec![],
        };
        fs::write(
            directory.join(".skinos-install.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        fs::write(directory.join("game"), b"legacy game").unwrap();
        root
    }
    #[test]
    fn removes_game_manifest_and_own_partial_but_keeps_external_saves_prefix_and_backups() {
        let base = tempfile::tempdir().unwrap();
        let root = steam_fixture(base.path(), 42, "Game");
        let game = root.join("common/Game");
        fs::create_dir(game.join("mods")).unwrap();
        fs::write(
            game.join("mods/custom"),
            b"user mod included in confirmed removal",
        )
        .unwrap();
        fs::write(
            game.join("save"),
            b"in-game save included in confirmed removal",
        )
        .unwrap();
        let outside = base.path().join("external-saves");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), b"save").unwrap();
        std::os::unix::fs::symlink(&outside, game.join("external-link")).unwrap();
        fs::create_dir_all(root.join("compatdata/42")).unwrap();
        fs::write(root.join("compatdata/42/keep"), b"prefix").unwrap();
        for name in [
            ".skinos-42-9.partial",
            ".skinos-420-9.partial",
            ".skinos-42-9.previous",
        ] {
            fs::create_dir(root.join("common").join(name)).unwrap();
            fs::write(root.join("common").join(name).join("keep"), b"cache").unwrap();
        }
        let plan = uninstall_plan(&root, 42, None).unwrap();
        assert_eq!(plan.partial_dirs.len(), 1);
        assert!(plan.remove_bytes >= 5);
        let serialized = serde_json::to_value(&plan).unwrap();
        assert!(serialized.get("approval").is_none());
        let result = uninstall(&plan).unwrap();
        assert_eq!(result.removed_partial_dirs, 1);
        assert!(!result.shared_directory_retained);
        assert!(!game.exists());
        assert!(!manifest_path(&root, 42).exists());
        assert!(!root.join("common/.skinos-42-9.partial").exists());
        assert!(root.join("common/.skinos-420-9.partial/keep").is_file());
        assert!(root.join("common/.skinos-42-9.previous/keep").is_file());
        assert_eq!(fs::read(outside.join("keep")).unwrap(), b"save");
        assert_eq!(
            fs::read(root.join("compatdata/42/keep")).unwrap(),
            b"prefix"
        );
    }
    #[test]
    fn shared_counter_strike_half_life_directory_is_retained() {
        let base = tempfile::tempdir().unwrap();
        let root = steam_fixture(base.path(), 10, "Half-Life");
        put_manifest(&root, 70, "Half-Life");
        fs::write(root.join("common/Half-Life/hl.exe"), b"shared game").unwrap();
        fs::write(
            root.join("common/Half-Life/.skinos-install.json"),
            br#"{"appid":10}"#,
        )
        .unwrap();
        let plan = uninstall_plan(&root, 10, None).unwrap();
        assert_eq!(plan.shared_appids, [70]);
        assert_eq!(plan.remove_bytes, 0);
        assert!(uninstall(&plan).unwrap().shared_directory_retained);
        assert!(!manifest_path(&root, 10).exists());
        assert!(manifest_path(&root, 70).exists());
        assert_eq!(
            fs::read(root.join("common/Half-Life/hl.exe")).unwrap(),
            b"shared game"
        );
        assert!(root.join("common/Half-Life/.skinos-install.json").exists());
    }
    #[test]
    fn steam_and_legacy_copies_are_both_included_and_removed() {
        let base = tempfile::tempdir().unwrap();
        let root = steam_fixture(base.path(), 42, "Game");
        let legacy = legacy_fixture(base.path(), 42);
        fs::write(root.join("common/Game/game"), b"steam copy").unwrap();
        let plan = uninstall_plan(&root, 42, Some(&legacy)).unwrap();
        assert_eq!(
            plan.additional_install_dirs,
            [legacy.join("42").canonicalize().unwrap()]
        );
        uninstall(&plan).unwrap();
        assert!(!legacy.join("42").exists());
        assert!(!root.join("common/Game").exists());
    }
    #[test]
    fn legacy_only_install_is_supported_without_creating_steam_library() {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("absent-steamapps");
        let legacy = legacy_fixture(base.path(), 42);
        let plan = uninstall_plan(&root, 42, Some(&legacy)).unwrap();
        assert!(plan.additional_install_dirs.is_empty());
        assert!(plan.approval.manifest.is_none());
        uninstall(&plan).unwrap();
        assert!(!legacy.join("42").exists());
        assert!(!root.exists());
    }
    #[test]
    fn shared_steam_folder_stays_while_approved_legacy_copy_is_removed() {
        let base = tempfile::tempdir().unwrap();
        let root = steam_fixture(base.path(), 10, "Half-Life");
        put_manifest(&root, 70, "Half-Life");
        let legacy = legacy_fixture(base.path(), 10);
        let plan = uninstall_plan(&root, 10, Some(&legacy)).unwrap();
        assert_eq!(plan.shared_appids, [70]);
        assert!(plan.remove_bytes > 0);
        uninstall(&plan).unwrap();
        assert!(root.join("common/Half-Life").is_dir());
        assert!(!legacy.join("10").exists());
        assert!(manifest_path(&root, 70).exists());
    }
    #[test]
    fn changed_manifest_or_directory_identity_rejects_approved_plan_without_deleting() {
        let base = tempfile::tempdir().unwrap();
        let root = steam_fixture(base.path(), 42, "Game");
        let plan = uninstall_plan(&root, 42, None).unwrap();
        fs::write(
            manifest_path(&root, 42),
            fs::read_to_string(manifest_path(&root, 42))
                .unwrap()
                .replace("\"1\"", "\"2\""),
        )
        .unwrap();
        assert!(uninstall(&plan).is_err());
        assert!(root.join("common/Game").exists());
        let plan = uninstall_plan(&root, 42, None).unwrap();
        fs::rename(root.join("common/Game"), root.join("common/old-game")).unwrap();
        fs::create_dir(root.join("common/Game")).unwrap();
        assert!(uninstall(&plan).is_err());
        assert!(root.join("common/Game").exists());
        assert!(manifest_path(&root, 42).exists());
    }
    #[test]
    fn newly_shared_app_or_changed_content_requires_new_confirmation() {
        let base = tempfile::tempdir().unwrap();
        let root = steam_fixture(base.path(), 10, "Half-Life");
        let plan = uninstall_plan(&root, 10, None).unwrap();
        put_manifest(&root, 70, "Half-Life");
        assert!(uninstall(&plan).is_err());
        assert!(root.join("common/Half-Life").exists());
        let plan = uninstall_plan(&root, 10, None).unwrap();
        fs::write(
            root.join("common/Half-Life/new-save"),
            b"not in previous confirmation",
        )
        .unwrap();
        assert!(uninstall(&plan).is_err());
        assert!(manifest_path(&root, 10).exists());
    }
    #[test]
    fn linked_manifest_game_or_partial_is_rejected() {
        for kind in ["manifest", "game", "partial", "common", "legacy-root"] {
            let base = tempfile::tempdir().unwrap();
            let root = steam_fixture(base.path(), 42, "Game");
            let outside = base.path().join("outside");
            fs::create_dir(&outside).unwrap();
            fs::write(outside.join("keep"), b"safe").unwrap();
            let legacy = legacy_fixture(base.path(), 42);
            match kind {
                "manifest" => {
                    let p = manifest_path(&root, 42);
                    fs::rename(&p, outside.join("manifest")).unwrap();
                    std::os::unix::fs::symlink(outside.join("manifest"), p).unwrap();
                }
                "game" => {
                    fs::remove_dir(root.join("common/Game")).unwrap();
                    std::os::unix::fs::symlink(&outside, root.join("common/Game")).unwrap();
                }
                "partial" => {
                    std::os::unix::fs::symlink(&outside, root.join("common/.skinos-42-1.partial"))
                        .unwrap();
                }
                "common" => {
                    fs::rename(root.join("common"), base.path().join("moved-common")).unwrap();
                    std::os::unix::fs::symlink(
                        base.path().join("moved-common"),
                        root.join("common"),
                    )
                    .unwrap();
                }
                "legacy-root" => {
                    fs::rename(&legacy, base.path().join("moved-legacy")).unwrap();
                    std::os::unix::fs::symlink(base.path().join("moved-legacy"), &legacy).unwrap();
                }
                _ => unreachable!(),
            }
            assert!(uninstall_plan(&root, 42, Some(&legacy)).is_err(), "{kind}");
            assert_eq!(fs::read(outside.join("keep")).unwrap(), b"safe");
        }
    }
    #[test]
    fn installer_destination_and_partial_locks_exclude_removal() {
        let base = tempfile::tempdir().unwrap();
        let root = steam_fixture(base.path(), 42, "Game");
        let game = root.join("common/Game");
        let lock = super::super::installation_lock(&game).unwrap();
        assert!(uninstall_plan(&root, 42, None).is_err());
        drop(lock);
        let plan = uninstall_plan(&root, 42, None).unwrap();
        let lock = super::super::operation_lock(&root.join("common"), 42).unwrap();
        assert!(uninstall(&plan).is_err());
        drop(lock);
        assert!(game.exists());
        assert!(manifest_path(&root, 42).exists());
    }
    #[test]
    fn incomplete_or_mismatched_manifest_is_not_an_uninstall_target() {
        let base = tempfile::tempdir().unwrap();
        let root = steam_fixture(base.path(), 42, "Game");
        assert!(uninstall_plan(&root, 0, None).is_err());
        let valid = fs::read_to_string(manifest_path(&root, 42)).unwrap();
        for invalid in [
            valid.replace("\"StateFlags\" \"4\"", "\"StateFlags\" \"0\""),
            valid.replace("\"appid\" \"42\"", "\"appid\" \"99\""),
        ] {
            fs::write(manifest_path(&root, 42), invalid).unwrap();
            assert!(uninstall_plan(&root, 42, None).is_err());
            assert!(root.join("common/Game").exists());
        }
    }
    #[test]
    fn staging_failure_rolls_game_and_partial_back_before_any_deletion() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let base = tempfile::tempdir().unwrap();
        let root = steam_fixture(base.path(), 42, "Game");
        fs::write(root.join("common/Game/game"), b"untouched").unwrap();
        let partial = root.join("common/.skinos-42-1.partial");
        fs::create_dir(&partial).unwrap();
        fs::write(partial.join("chunk"), b"verified partial").unwrap();
        let plan = uninstall_plan(&root, 42, None).unwrap();
        // Moving game/cache inside writable common succeeds, then creation of
        // the manifest tombstone fails. The earlier moves must be rolled back.
        fs::set_permissions(&root, fs::Permissions::from_mode(0o500)).unwrap();
        let result = uninstall(&plan);
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.unwrap_err().to_string().contains("rolled back"));
        assert_eq!(
            fs::read(root.join("common/Game/game")).unwrap(),
            b"untouched"
        );
        assert_eq!(
            fs::read(partial.join("chunk")).unwrap(),
            b"verified partial"
        );
        assert!(manifest_path(&root, 42).exists());
    }
    #[test]
    fn deletion_failure_reports_retained_tombstone_instead_of_success() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let base = tempfile::tempdir().unwrap();
        let root = steam_fixture(base.path(), 42, "Game");
        let readonly = root.join("common/Game/readonly");
        fs::create_dir(&readonly).unwrap();
        fs::write(readonly.join("remaining"), b"remaining").unwrap();
        fs::set_permissions(&readonly, fs::Permissions::from_mode(0o500)).unwrap();
        let plan = uninstall_plan(&root, 42, None).unwrap();
        let error = uninstall(&plan).unwrap_err();
        let tombstone = fs::read_dir(root.join("common"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".skinos-uninstall-42-")
            })
            .unwrap();
        let retained = tombstone.join("directory-0/readonly");
        fs::set_permissions(&retained, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(error.to_string().contains("Removal is incomplete"));
        assert!(
            error
                .to_string()
                .contains(&tombstone.to_string_lossy().to_string())
        );
        assert_eq!(fs::read(retained.join("remaining")).unwrap(), b"remaining");
    }
}
