//! Retrieve pinned compatibility dependencies directly from their publishers.
//!
//! Public builds contain the owned launcher, not the Valve DLLs, upstream
//! bridge or fonts. These generic dependencies are fetched only when needed;
//! account state and installed applications are never used as a source.
use crate::{Result, assets};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use skinos_dependency_assets::{ArchiveSpec, PinnedZip, ensure_archive};
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

const PIN: &str = include_str!("../../../third_party/notproton/assets.json");
const MAX_FILE: u64 = 128 * 1024 * 1024;

#[derive(Deserialize)]
struct Manifest {
    schema_version: u32,
    revision: String,
    version: String,
    archives: BTreeMap<String, ArchiveSpec>,
    files: BTreeMap<String, FileSpec>,
}

#[derive(Deserialize)]
struct FileSpec {
    archive: String,
    member: String,
    sha256: String,
    size: u64,
}

impl Manifest {
    fn pinned() -> Result<Self> {
        let manifest: Self = serde_json::from_str(PIN).map_err(|e| e.to_string())?;
        let info = crate::bundled_info();
        if manifest.schema_version != 1
            || manifest.revision != info.revision
            || manifest.version != info.version
            || manifest.files.is_empty()
        {
            return Err("NotProton dependency pin does not match the compiled source".into());
        }
        for (name, file) in &manifest.files {
            safe_name(name)?;
            if file.size == 0
                || file.size > MAX_FILE
                || !manifest.archives.contains_key(&file.archive)
            {
                return Err(format!("Invalid NotProton dependency pin: {name}"));
            }
        }
        Ok(manifest)
    }

    fn bridge_manifest(&self) -> serde_json::Value {
        serde_json::json!({
            "source": "direct-upstream-pinned-download",
            "revision": self.revision,
            "version": self.version,
            "encoding": "gzip",
            "files": self.files.iter().filter(|(name, _)| !name.starts_with("fonts/"))
                .map(|(name, file)| (name.clone(), file.sha256.clone())).collect::<BTreeMap<_, _>>()
        })
    }

    fn fonts_manifest(&self) -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "revision": self.revision,
            "version": self.version,
            "files": self.files.iter().filter_map(|(name, file)|
                name.strip_prefix("fonts/").map(|name| (name.to_owned(), file.sha256.clone())))
                .collect::<BTreeMap<_, _>>()
        })
    }
}

/// Select existing complete bundled assets, or provision the public build's
/// dependencies into its own data directory. Corrupt bundled files are still
/// rejected by staging; they are never silently replaced by another version.
pub(crate) fn dependency_payload(payload: &Path, data_dir: &Path) -> Result<PathBuf> {
    let has_bundled_bridge = fs::symlink_metadata(payload.join("bridge")).is_ok()
        || payload
            .parent()
            .is_some_and(|parent| parent.join("native-delivery.json").is_file());
    if has_bundled_bridge {
        return Ok(payload.to_path_buf());
    }
    prepare_dependencies(data_dir)
}

/// Prepare only generic, hash-pinned upstream dependencies. This is also an
/// explicit diagnostic entry point for isolated bootstrap verification; it
/// never signs in, opens CrossOver, or starts a game.
pub fn prepare_dependencies(data_dir: &Path) -> Result<PathBuf> {
    prepare_with(&Manifest::pinned()?, data_dir, |root, spec| {
        ensure_archive(root, spec)
    })
}

fn prepare_with(
    manifest: &Manifest,
    data_dir: &Path,
    mut fetch: impl FnMut(&Path, &ArchiveSpec) -> Result<PathBuf>,
) -> Result<PathBuf> {
    let cache = checked_path(data_dir, "dependency-cache/notproton")?;
    let destination = checked_path(&cache, &manifest.revision)?;
    let archives = checked_path(&cache, "archives")?;
    let mut opened = BTreeMap::new();
    for (name, spec) in &manifest.files {
        let (relative, compressed) = payload_path(name)?;
        let target = checked_path(&destination, &relative)?;
        if is_verified(&target, spec, compressed)? {
            continue;
        }
        if !opened.contains_key(&spec.archive) {
            let archive_spec = manifest
                .archives
                .get(&spec.archive)
                .ok_or("Unknown dependency archive")?;
            let path = fetch(&archives, archive_spec)
                .map_err(|e| format!("Could not download NotProton dependencies: {e}"))?;
            opened.insert(spec.archive.clone(), PinnedZip::open(&path)?);
        }
        let data = opened
            .get_mut(&spec.archive)
            .ok_or("Missing archive")?
            .read_member(&spec.member, &spec.sha256, Some(spec.size))?;
        write_asset(&target, &data, compressed)?;
    }
    // Publish the metadata last; staging starts only after every selected member
    // has been verified. Concurrent preparations write the same pinned bytes via
    // distinct temporary files, and never observe partially written files.
    for (name, value) in [
        ("bridge-manifest.json", manifest.bridge_manifest()),
        ("fonts-manifest.json", manifest.fonts_manifest()),
    ] {
        let path = checked_path(&destination, name)?;
        let encoded = serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?;
        if fs::read(&path).ok().as_deref() != Some(encoded.as_slice()) {
            assets::atomic_write(&path, &encoded)?;
        }
    }
    Ok(destination)
}

fn payload_path(name: &str) -> Result<(String, bool)> {
    safe_name(name)?;
    Ok(if name.starts_with("fonts/") {
        (name.to_owned(), false)
    } else {
        (format!("bridge/{name}.gz"), true)
    })
}

fn safe_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.contains(['\\', '\0'])
        || name
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || Path::new(name)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("Unsafe NotProton dependency path".into());
    }
    Ok(())
}

fn checked_path(root: &Path, name: &str) -> Result<PathBuf> {
    safe_name(name)?;
    let mut path = root.to_owned();
    for part in std::iter::once(None).chain(Path::new(name).components().map(Some)) {
        if let Some(part) = part {
            path.push(part);
        }
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("Refusing linked NotProton dependency cache path".into());
            }
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(error.to_string());
            }
            _ => {}
        }
    }
    Ok(path)
}

fn is_verified(path: &Path, spec: &FileSpec, compressed: bool) -> Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > MAX_FILE {
        return Err("Invalid NotProton dependency cache file".into());
    }
    let input = fs::File::open(path).map_err(|e| e.to_string())?;
    let reader: Box<dyn Read> = if compressed {
        Box::new(GzDecoder::new(input))
    } else {
        Box::new(input)
    };
    let mut data = Vec::new();
    if reader.take(spec.size + 1).read_to_end(&mut data).is_err() {
        return Ok(false);
    }
    Ok(data.len() as u64 == spec.size && format!("{:x}", Sha256::digest(&data)) == spec.sha256)
}

fn write_asset(path: &Path, data: &[u8], compressed: bool) -> Result<()> {
    if compressed {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).map_err(|e| e.to_string())?;
        assets::atomic_write(path, &encoder.finish().map_err(|e| e.to_string())?)
    } else {
        assets::atomic_write(path, data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Manifest {
        serde_json::from_value(serde_json::json!({
            "schema_version": 1, "revision": "test-revision", "version": "test",
            "archives": {},
            "files": {
                "x86_64-unix/lsteamclient.so": {"archive":"never", "member":"unused", "size":6,
                    "sha256":format!("{:x}", Sha256::digest(b"bridge"))},
                "fonts/alt/arial.ttf": {"archive":"never", "member":"unused", "size":4,
                    "sha256":format!("{:x}", Sha256::digest(b"font"))}
            }
        }))
        .unwrap()
    }

    #[test]
    fn compiled_pin_matches_all_existing_bridge_and_font_hashes() {
        let manifest = Manifest::pinned().unwrap();
        let existing: serde_json::Value = serde_json::from_str(include_str!(
            "../../../third_party/notproton/bridge/manifest.json"
        ))
        .unwrap();
        assert_eq!(
            manifest.files.len(),
            existing["files"].as_object().unwrap().len()
        );
        for (name, spec) in manifest.files {
            assert_eq!(spec.sha256, existing["files"][name].as_str().unwrap());
        }
    }

    #[test]
    fn verified_cache_reuses_without_network_and_stages_exact_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = fixture();
        let root = temp.path().join("dependency-cache/notproton/test-revision");
        write_asset(
            &root.join("bridge/x86_64-unix/lsteamclient.so.gz"),
            b"bridge",
            true,
        )
        .unwrap();
        write_asset(&root.join("fonts/alt/arial.ttf"), b"font", false).unwrap();
        let result =
            prepare_with(&manifest, temp.path(), |_, _| panic!("unexpected network")).unwrap();
        let staged = temp.path().join("staged");
        crate::copy_checked_payload(&result, &staged).unwrap();
        assets::stage_fonts(
            &result,
            &staged,
            &crate::NotProtonInfo {
                revision: manifest.revision,
                version: manifest.version,
            },
        )
        .unwrap();
        assert_eq!(
            fs::read(staged.join("bridge/x86_64-unix/lsteamclient.so")).unwrap(),
            b"bridge"
        );
        assert_eq!(
            fs::read(staged.join("fonts/alt/arial.ttf")).unwrap(),
            b"font"
        );
    }

    #[test]
    fn refuses_traversal_and_linked_cache_without_changing_external_file() {
        for path in ["../escape", "/absolute", "fonts/../escape", "x\\y", "x//y"] {
            assert!(payload_path(path).is_err());
        }
        let temp = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        fs::write(external.path().join("keep"), b"untouched").unwrap();
        std::os::unix::fs::symlink(external.path(), temp.path().join("dependency-cache")).unwrap();
        assert!(
            prepare_with(&fixture(), temp.path(), |_, _| panic!("unexpected network")).is_err()
        );
        assert_eq!(
            fs::read(external.path().join("keep")).unwrap(),
            b"untouched"
        );
    }

    #[test]
    fn corruption_is_not_accepted_as_verified_dependency() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = fixture();
        let spec = &manifest.files["x86_64-unix/lsteamclient.so"];
        let file = temp.path().join("bridge.gz");
        write_asset(&file, b"wrong!", true).unwrap();
        assert!(!is_verified(&file, spec, true).unwrap());
        fs::write(&file, b"broken gzip").unwrap();
        assert!(!is_verified(&file, spec, true).unwrap());
    }

    #[test]
    fn existing_bundled_payload_does_not_fetch_dependencies() {
        let temp = tempfile::tempdir().unwrap();
        let payload = temp.path().join("bundle");
        fs::create_dir_all(payload.join("bridge")).unwrap();
        assert_eq!(
            dependency_payload(&payload, Path::new("/unused-data")).unwrap(),
            payload
        );
    }
}
