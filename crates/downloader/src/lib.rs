//! SteamPipe downloads over an existing authenticated Steam CM connection.
mod content;
mod control;
pub use control::{InstallControl, InstallInterrupted};
pub mod library;
mod macos;
mod network;
mod progress;
mod proto;
mod regions;
pub use regions::{DownloadRegion, download_regions, validate_download_region};
mod update;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use steam_cm_protocol::connection::Connection;
use steam_cm_protocol::kv::KVValue;
use tokio::sync::Mutex;

pub type SharedConnection = Arc<Mutex<Connection>>;
pub type ProgressCallback = Arc<dyn Fn(InstallProgress) + Send + Sync>;

#[derive(Debug, Clone)]
pub struct InstallRequest {
    pub appid: u32,
    pub steamid: String,
    pub destination: PathBuf,
    pub platform: InstallPlatform,
    pub steamapps_dir: Option<PathBuf>,
    /// Steam download cell, or automatic selection when absent.
    pub download_region_id: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InstallPlatform {
    #[default]
    Macos,
    Windows,
}
impl InstallPlatform {
    pub fn os(self) -> &'static str {
        match self {
            Self::Macos => "macos",
            Self::Windows => "windows",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct InstallProgress {
    pub appid: u32,
    pub phase: &'static str,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub progress: Option<f64>,
    pub message: String,
    pub bytes_per_second: f64,
    pub eta_seconds: Option<u64>,
    pub reused_bytes: u64,
    pub processed_bytes: u64,
    pub install_total_bytes: u64,
    pub files_completed: u64,
    pub total_files: u64,
    pub chunks_completed: u64,
    pub total_chunks: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledGame {
    pub appid: u32,
    pub steamid: String,
    pub install_dir: PathBuf,
    pub build_id: String,
    /// Absent on imports/older installations; callers may infer from files.
    #[serde(default)]
    pub platform: Option<InstallPlatform>,
    pub depots: Vec<InstalledDepot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledDepot {
    pub depot_id: u32,
    pub manifest_id: String,
}

#[derive(Debug, Clone)]
struct DepotSelection {
    depot_id: u32,
    manifest_id: u64,
    source_appid: u32,
}

fn at<'a>(node: &'a KVValue, keys: &[&str]) -> Option<&'a KVValue> {
    keys.iter().try_fold(node, |value, key| value.get(key))
}
fn string<'a>(node: &'a KVValue, keys: &[&str]) -> Option<&'a str> {
    at(node, keys).and_then(KVValue::as_str)
}

/// Never treat Windows/Linux-only depots as macOS content. Neutral depots are
/// shared and must be retained. Optional DLC and low-violence replacements are
/// not part of a default base-game install.
type DepotPlan = (Vec<DepotSelection>, Vec<(u32, u32)>);

fn select_depots(appid: u32, root: &KVValue, platform: InstallPlatform) -> Result<DepotPlan> {
    let depots = root
        .get("depots")
        .and_then(KVValue::as_nested)
        .context("App has no depot metadata")?;
    let mut selected = Vec::new();
    let mut shared = Vec::new();
    for (key, value) in depots {
        let Ok(depot_id) = key.parse::<u32>() else {
            continue;
        };
        // Low-violence depots are separately entitled replacement content, not
        // neutral base content. The default install does not opt into them.
        if at(value, &["config", "lowviolence"])
            .and_then(KVValue::as_u32)
            .is_some_and(|enabled| enabled != 0)
        {
            continue;
        }
        if string(value, &["config", "oslist"])
            .is_some_and(|os| !os.is_empty() && !os.split(',').any(|v| v.trim() == platform.os()))
        {
            continue;
        }
        if platform == InstallPlatform::Macos && string(value, &["config", "osarch"]) == Some("32")
        {
            continue;
        }
        if string(value, &["config", "language"])
            .is_some_and(|language| !language.is_empty() && language != "english")
        {
            continue;
        }
        if value
            .get("dlcappid")
            .and_then(KVValue::as_u32)
            .is_some_and(|id| id != appid && id != 0)
        {
            continue;
        }
        if let Some(source) = value
            .get("depotfromapp")
            .and_then(KVValue::as_u32)
            .filter(|id| *id != appid)
        {
            shared.push((depot_id, source));
            continue;
        }
        let manifest = string(value, &["manifests", "public", "gid"])
            .or_else(|| string(value, &["manifests", "public"]));
        if let Some(id) = manifest.and_then(|v| v.parse().ok()) {
            selected.push(DepotSelection {
                depot_id,
                manifest_id: id,
                source_appid: appid,
            });
        }
    }
    Ok((selected, shared))
}

fn checkpoint(control: &InstallControl) -> Result<()> {
    control.checkpoint()?;
    Ok(())
}

/// Fetch the current public build without downloading any game content.
pub async fn latest_build(
    connection: &SharedConnection,
    appid: u32,
    platform: InstallPlatform,
) -> Result<String> {
    let metadata = network::app_info(connection, appid).await?;
    let (selected, shared) = select_depots(appid, &metadata, platform)?;
    ensure!(
        !selected.is_empty() || !shared.is_empty(),
        "No depots for selected platform"
    );
    at(&metadata, &["depots", "branches", "public", "buildid"])
        .and_then(KVValue::as_u32)
        .map(|v| v.to_string())
        .context("Steam returned no public build")
}

pub async fn install(
    connection: SharedConnection,
    request: InstallRequest,
    on_progress: ProgressCallback,
    control: InstallControl,
) -> Result<InstalledGame> {
    ensure!(request.appid != 0, "Invalid AppID");
    ensure!(request.steamid.parse::<u64>().is_ok(), "Invalid SteamID");
    validate_download_region(request.download_region_id)?;
    let state = control
        .run(async { Ok(connection.lock().await.state_snapshot().await) })
        .await?;
    ensure!(
        state.steamid.map(|id| id.to_string()).as_deref() == Some(request.steamid.as_str()),
        "Account session changed"
    );
    checkpoint(&control)?;
    on_progress(InstallProgress {
        appid: request.appid,
        phase: "preparing",
        downloaded_bytes: 0,
        total_bytes: 0,
        progress: None,
        message: "Получение файлов игры".into(),
        bytes_per_second: 0.0,
        eta_seconds: None,
        reused_bytes: 0,
        processed_bytes: 0,
        install_total_bytes: 0,
        files_completed: 0,
        total_files: 0,
        chunks_completed: 0,
        total_chunks: 0,
    });
    let metadata = control
        .run(network::app_info(&connection, request.appid))
        .await?;
    let os = string(&metadata, &["common", "oslist"]).unwrap_or("");
    ensure!(
        os.split(',').any(|s| s.trim() == request.platform.os()),
        "У игры нет версии для выбранной платформы"
    );
    let build_id = at(&metadata, &["depots", "branches", "public", "buildid"])
        .and_then(KVValue::as_u32)
        .unwrap_or(0)
        .to_string();
    let (mut selections, shared) = select_depots(request.appid, &metadata, request.platform)?;
    for (depot_id, source_appid) in shared {
        let source = control
            .run(network::app_info(&connection, source_appid))
            .await?;
        let key = depot_id.to_string();
        let id = string(&source, &["depots", &key, "manifests", "public", "gid"])
            .or_else(|| string(&source, &["depots", &key, "manifests", "public"]))
            .context("Shared depot has no public manifest")?
            .parse()?;
        selections.push(DepotSelection {
            depot_id,
            manifest_id: id,
            source_appid,
        });
    }
    selections.sort_by_key(|d| d.depot_id);
    selections.dedup_by_key(|d| d.depot_id);
    ensure!(
        !selections.is_empty(),
        "Не найдены доступные файлы игры для выбранной платформы"
    );

    let parent = request
        .destination
        .parent()
        .context("Installation requires a parent directory")?;
    tokio::fs::create_dir_all(parent).await?;
    let parent = parent.canonicalize()?;
    let destination = parent.join(
        request
            .destination
            .file_name()
            .context("Missing installation name")?,
    );
    ensure!(
        !destination.is_symlink(),
        "Install destination cannot be a symlink"
    );
    let _install_lock = library::installation_lock(&destination)?;
    let _operation_lock = library::operation_lock(&parent, request.appid)?;
    let original_manifest = request
        .steamapps_dir
        .as_ref()
        .map(|root| library::snapshot(root, request.appid))
        .transpose()?
        .flatten();
    if let Some(steamapps) = &request.steamapps_dir {
        ensure!(
            destination.parent() == Some(steamapps.join("common").canonicalize()?.as_path()),
            "Destination is outside the selected Steam library"
        );
    }
    if destination.exists() {
        let record = if let Some(root) = &request.steamapps_dir {
            if original_manifest.is_some() {
                library::read_appmanifest(root, request.appid)?
            } else {
                read_install(&destination)?
            }
        } else {
            read_install(&destination)?
        };
        ensure!(
            record.appid == request.appid
                && record.install_dir.canonicalize()? == destination.canonicalize()?,
            "Destination belongs to another game"
        );
        ensure!(
            record
                .platform
                .is_none_or(|installed| installed == request.platform),
            "Установлена другая версия игры. Сначала удалите её с устройства"
        );
    }

    // Keep the stable cache name so upgrades resume existing partial content.
    // Every reused chunk is verified against the selected platform's manifest.
    let staging = parent.join(format!(".skinos-{}-{}.partial", request.appid, build_id));
    ensure!(
        !staging.is_symlink(),
        "Staging destination cannot be a symlink"
    );
    tokio::fs::create_dir_all(&staging).await?;
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(90))
        .https_only(true)
        .build()?;
    let servers = control
        .run(network::servers(
            &connection,
            request.appid,
            request.download_region_id,
        ))
        .await?;
    let mut depots = Vec::new();
    for selection in &selections {
        checkpoint(&control)?;
        let depot = control
            .run(network::prepare_depot(
                &connection,
                &client,
                &servers,
                selection,
            ))
            .await?;
        depots.push(depot);
    }
    content::resolve_depot_overlays(&mut depots)?;
    let total = depots
        .iter()
        .flat_map(|depot| &depot.files)
        .try_fold(0u64, |total, file| {
            total
                .checked_add(file.size.unwrap_or(0))
                .context("Download size overflow")
        })?;
    let depots = Arc::new(depots);
    let total_files = depots.iter().map(|d| d.files.len() as u64).sum();
    let total_chunks = depots
        .iter()
        .flat_map(|d| &d.files)
        .map(|f| f.chunks.len() as u64)
        .sum();
    let mut verifying = progress::VerificationProgress::new(
        request.appid,
        total,
        total_files,
        total_chunks,
        on_progress.clone(),
    );
    verifying.emit("Проверка существующих файлов", true);
    let plan_root = staging.clone();
    let plan_installed = destination.clone();
    let plan_depots = depots.clone();
    let plan_control = control.clone();
    let plan = control
        .blocking(move || {
            prune_staging(&plan_root, &plan_depots, &plan_control)?;
            content::seed_and_plan(
                &plan_depots,
                &plan_root,
                &plan_installed,
                &plan_control,
                &mut verifying,
            )
        })
        .await?;
    let mut meter = progress::TransferProgress::new(
        request.appid,
        plan.transfer,
        plan.reused,
        total,
        on_progress.clone(),
    );
    meter.total_files = total_files;
    meter.total_chunks = total_chunks;
    meter.emit("downloading", "Загрузка файлов", false);
    let progress = Arc::new(std::sync::Mutex::new(meter));
    for symlinks_only in [false, true] {
        for (index, depot) in depots.iter().enumerate() {
            checkpoint(&control)?;
            content::write_depot(
                &client,
                depot,
                &staging,
                &control,
                &progress,
                symlinks_only,
                &plan.depots[index],
            )
            .await?;
        }
    }
    checkpoint(&control)?;
    progress
        .lock()
        .unwrap()
        .emit("installing", "Установка обновлённых файлов", false);
    let finish_root = staging.clone();
    let finish_installed = destination.clone();
    let finish_depots = depots.clone();
    let finish_control = control.clone();
    let platform = request.platform;
    let appid = request.appid;
    control
        .blocking(move || {
            if platform == InstallPlatform::Macos {
                macos::restore_bundle_executables(&finish_root)?;
            }
            finish_control.checkpoint()?;
            update::preserve_extras(&finish_installed, &finish_root, appid, &finish_control)?;
            update::write_inventory(&finish_root, &finish_depots)?;
            finish_control.checkpoint()?;
            Ok(())
        })
        .await?;
    let record = InstalledGame {
        appid: request.appid,
        steamid: request.steamid,
        install_dir: destination.clone(),
        build_id,
        platform: Some(request.platform),
        depots: selections
            .iter()
            .map(|d| InstalledDepot {
                depot_id: d.depot_id,
                manifest_id: d.manifest_id.to_string(),
            })
            .collect(),
    };
    tokio::fs::write(
        staging.join(".skinos-install.json"),
        serde_json::to_vec_pretty(&record)?,
    )
    .await?;
    let had_previous = destination.exists();
    control.commit(|| {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let backup = parent.join(format!(".skinos-{}-{nonce}.previous", request.appid));
        ensure!(!backup.exists(), "Installation recovery directory exists");

        if had_previous {
            std::fs::rename(&destination, &backup)?;
        }
        // No await in this transaction: cancellation must not strand an install.
        if let Err(error) = std::fs::rename(&staging, &destination) {
            if had_previous {
                let _ = std::fs::rename(&backup, &destination);
            }
            return Err(error.into());
        }
        if let Some(steamapps) = &request.steamapps_dir {
            let sizes = depots
                .iter()
                .map(|d| {
                    (
                        d.depot_id,
                        d.metadata
                            .original_size
                            .unwrap_or_else(|| d.files.iter().map(|f| f.size.unwrap_or(0)).sum()),
                    )
                })
                .collect::<Vec<_>>();
            if let Err(error) = library::write_appmanifest(
                steamapps,
                &record,
                string(&metadata, &["common", "name"]).unwrap_or("Steam game"),
                original_manifest.as_deref(),
                total,
                progress.lock().unwrap().downloaded,
                &sizes,
            ) {
                let _ = std::fs::rename(&destination, &staging);
                if had_previous {
                    let _ = std::fs::rename(&backup, &destination);
                }
                return Err(error);
            }
        }
        Ok(())
    })?;
    // The previous tree includes user mods and modified files, and is deliberately
    // retained for recovery instead of silently deleting user content.
    progress.lock().unwrap().emit(
        "complete",
        if had_previous {
            "Игра обновлена"
        } else {
            "Игра установлена"
        },
        true,
    );
    Ok(record)
}

/// Paths in a depot are relative and may not walk out of the installation.
fn safe_relative(value: &str) -> Result<PathBuf> {
    let normalized = value.trim_end_matches('\0').replace('\\', "/");
    ensure!(
        !normalized.is_empty() && !normalized.contains('\0'),
        "Invalid depot path"
    );
    let path = PathBuf::from(normalized);
    ensure!(
        path.components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir)),
        "Unsafe depot path"
    );
    Ok(path)
}

fn safe_output(root: &Path, relative: &Path) -> Result<PathBuf> {
    let mut current = root.to_owned();
    for component in relative.components() {
        current.push(component);
        if current.is_symlink() {
            bail!("A depot output path traverses a symlink")
        }
    }
    Ok(current)
}

/// A resumed staging tree may contain files from an older manifest/depot set.
/// Remove only inside our staging tree; never follow existing symlinks.
fn prune_staging(
    root: &Path,
    depots: &[network::PreparedDepot],
    control: &InstallControl,
) -> Result<()> {
    use std::collections::HashSet;
    let mut keep = HashSet::new();
    for depot in depots {
        for file in &depot.files {
            control.checkpoint()?;
            let relative = safe_relative(file.filename.as_deref().context("Missing filename")?)?;
            keep.insert(relative.clone());
            for parent in relative
                .ancestors()
                .skip(1)
                .filter(|p| !p.as_os_str().is_empty())
            {
                keep.insert(parent.to_owned());
            }
        }
    }
    fn remove_owned_tree(path: &Path, control: &InstallControl) -> Result<()> {
        for entry in std::fs::read_dir(path)? {
            control.checkpoint()?;
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                remove_owned_tree(&entry.path(), control)?;
            } else {
                std::fs::remove_file(entry.path())?;
            }
        }
        control.checkpoint()?;
        std::fs::remove_dir(path)?;
        Ok(())
    }
    fn walk(
        root: &Path,
        dir: &Path,
        keep: &HashSet<PathBuf>,
        control: &InstallControl,
    ) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            control.checkpoint()?;
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            let relative = path.strip_prefix(root)?;
            if kind.is_symlink() || !keep.contains(relative) {
                if kind.is_dir() {
                    remove_owned_tree(&path, control)?;
                } else {
                    std::fs::remove_file(path)?;
                }
            } else if kind.is_dir() {
                walk(root, &path, keep, control)?;
            }
        }
        Ok(())
    }
    walk(root, root, &keep, control)
}

pub fn read_install(destination: &Path) -> Result<InstalledGame> {
    let record: InstalledGame =
        serde_json::from_slice(&std::fs::read(destination.join(".skinos-install.json"))?)?;
    ensure!(
        record.install_dir.canonicalize()? == destination.canonicalize()?,
        "Installation location changed"
    );
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn s(value: &str) -> KVValue {
        KVValue::Str(value.into())
    }
    fn obj(values: Vec<(&str, KVValue)>) -> KVValue {
        KVValue::Nested(values.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }
    fn kv_from_json(value: serde_json::Value) -> KVValue {
        match value {
            serde_json::Value::Object(entries) => KVValue::Nested(
                entries
                    .into_iter()
                    .map(|(k, v)| (k, kv_from_json(v)))
                    .collect(),
            ),
            serde_json::Value::String(value) => KVValue::Str(value),
            serde_json::Value::Number(value) => {
                let value = value.as_u64().expect("unsigned PICS fixture value");
                i32::try_from(value)
                    .map(KVValue::Int)
                    .unwrap_or(KVValue::Uint64(value))
            }
            _ => panic!("unsupported PICS fixture value"),
        }
    }
    #[test]
    fn mac_selection_retains_shared_content_and_excludes_other_os_and_dlc() {
        let depot = |os: &str| {
            obj(vec![
                ("config", obj(vec![("oslist", s(os))])),
                (
                    "manifests",
                    obj(vec![("public", obj(vec![("gid", s("123"))]))]),
                ),
            ])
        };
        let neutral = obj(vec![("manifests", obj(vec![("public", s("456"))]))]);
        let root = obj(vec![(
            "depots",
            obj(vec![
                ("1", depot("macos")),
                ("2", depot("windows")),
                ("3", neutral),
            ]),
        )]);
        let (selected, _) = select_depots(42, &root, InstallPlatform::Macos).unwrap();
        assert_eq!(
            selected.iter().map(|s| s.depot_id).collect::<Vec<_>>(),
            vec![1, 3]
        );
    }
    #[test]
    fn dota2_base_install_does_not_request_restricted_low_violence_depot() {
        // Dota 2 public app metadata: 373307 has no OS or DLC restriction,
        // only config.lowviolence=1. Requesting its key for a normal base-game
        // installation fails with Steam result 15 (AccessDenied).
        let root = obj(vec![(
            "depots",
            obj(vec![
                (
                    "373304",
                    obj(vec![
                        ("config", obj(vec![("oslist", s("macos"))])),
                        (
                            "manifests",
                            obj(vec![(
                                "public",
                                obj(vec![("gid", s("2196426338917394934"))]),
                            )]),
                        ),
                    ]),
                ),
                (
                    "373307",
                    obj(vec![
                        ("config", obj(vec![("lowviolence", s("1"))])),
                        (
                            "manifests",
                            obj(vec![(
                                "public",
                                obj(vec![("gid", s("3007099922130981321"))]),
                            )]),
                        ),
                    ]),
                ),
            ]),
        )]);
        let (selected, shared) = select_depots(570, &root, InstallPlatform::Macos).unwrap();
        let selected = selected.iter().map(|s| s.depot_id).collect::<Vec<_>>();
        assert_eq!(
            selected,
            vec![373304],
            "base-game plan must not request depot 373307 (Steam result 15)"
        );
        assert!(shared.is_empty());
    }
    #[test]
    fn dota2_cached_appinfo_plans_only_default_content_for_both_platforms() {
        let root = kv_from_json(
            serde_json::from_str(include_str!("../tests/fixtures/dota2-public-appinfo.json"))
                .unwrap(),
        );
        for (platform, expected, expected_shared) in [
            (
                InstallPlatform::Macos,
                vec![
                    373301, 373304, 373305, 381451, 381452, 381453, 381454, 381455,
                ],
                vec![],
            ),
            (
                InstallPlatform::Windows,
                vec![
                    373301, 373302, 373303, 373305, 381451, 381452, 381453, 381454, 381455,
                ],
                vec![(228983, 228980), (228989, 228980), (228990, 228980)],
            ),
        ] {
            let (selected, shared) = select_depots(570, &root, platform).unwrap();
            let selected = selected.iter().map(|s| s.depot_id).collect::<Vec<_>>();
            assert_eq!(
                selected, expected,
                "Dota 2 default content for {platform:?}"
            );
            assert_eq!(
                shared, expected_shared,
                "Dota 2 shared content for {platform:?}"
            );
        }
    }
    #[test]
    fn low_violence_opt_in_filters_direct_and_shared_depots_with_both_kv_encodings() {
        for flag in [
            None,
            Some(s("0")),
            Some(KVValue::Int(0)),
            Some(s("1")),
            Some(KVValue::Int(1)),
        ] {
            let excluded = flag.as_ref().and_then(KVValue::as_u32) == Some(1);
            let config = flag
                .clone()
                .map(|flag| obj(vec![("lowviolence", flag)]))
                .unwrap_or_else(|| obj(vec![]));
            let root = obj(vec![(
                "depots",
                obj(vec![
                    (
                        "1",
                        obj(vec![
                            ("config", config.clone()),
                            ("manifests", obj(vec![("public", s("123"))])),
                        ]),
                    ),
                    (
                        "2",
                        obj(vec![("config", config), ("depotfromapp", KVValue::Int(99))]),
                    ),
                    (
                        "3",
                        obj(vec![("manifests", obj(vec![("public", s("456"))]))]),
                    ),
                ]),
            )]);
            for platform in [InstallPlatform::Macos, InstallPlatform::Windows] {
                let (selected, shared) = select_depots(42, &root, platform).unwrap();
                let selected = selected.iter().map(|s| s.depot_id).collect::<Vec<_>>();
                assert_eq!(
                    selected,
                    if excluded { vec![3] } else { vec![1, 3] },
                    "{platform:?}, {flag:?}"
                );
                assert_eq!(
                    shared,
                    if excluded { vec![] } else { vec![(2, 99)] },
                    "{platform:?}, {flag:?}"
                );
            }
        }
    }
    #[test]
    fn rejects_escaping_paths_and_existing_symlink_parents() {
        for path in ["../save", "/etc/file", "a/../../file", "", "a\0b"] {
            assert!(safe_relative(path).is_err(), "{path:?}");
        }
        assert_eq!(
            safe_relative("Game.app\\Contents/MacOS/Game").unwrap(),
            PathBuf::from("Game.app/Contents/MacOS/Game")
        );
        let root = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/tmp", root.path().join("alias")).unwrap();
            assert!(safe_output(root.path(), Path::new("alias/file")).is_err());
        }
    }
    #[test]
    fn resumed_staging_removes_stale_files_without_following_links() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("needed")).unwrap();
        std::fs::write(root.path().join("needed/file"), b"resume").unwrap();
        std::fs::write(root.path().join("stale"), b"old").unwrap();
        std::fs::write(outside.path().join("untouched"), b"outside").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), root.path().join("old-link")).unwrap();
        let depot = network::PreparedDepot {
            depot_id: 1,
            key: [0; 32],
            servers: vec![],
            metadata: Default::default(),
            files: vec![proto::FileMapping {
                filename: Some("needed/file".into()),
                ..Default::default()
            }],
        };
        prune_staging(root.path(), &[depot], &InstallControl::new()).unwrap();
        assert_eq!(
            std::fs::read(root.path().join("needed/file")).unwrap(),
            b"resume"
        );
        assert!(!root.path().join("stale").exists());
        assert!(!root.path().join("old-link").is_symlink());
        assert_eq!(
            std::fs::read(outside.path().join("untouched")).unwrap(),
            b"outside"
        );
    }
}
