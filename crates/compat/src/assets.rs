//! Verify and stage the revision-pinned non-code NotProton payload.
use crate::{NotProtonInfo, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_ASSET_BYTES: u64 = 64 * 1024 * 1024;
const RECEIPT: &str = ".skinos-fonts.json";

#[derive(Deserialize, Serialize)]
struct FontManifest {
    schema_version: u32,
    revision: String,
    version: String,
    files: BTreeMap<String, String>,
}

pub(crate) fn stage_fonts(payload: &Path, support: &Path, pin: &NotProtonInfo) -> Result<()> {
    let encoded = fs::read(payload.join("fonts-manifest.json"))
        .map_err(|e| format!("Missing bundled NotProton font manifest: {e}"))?;
    let manifest: FontManifest = serde_json::from_slice(&encoded)
        .map_err(|e| format!("Invalid bundled NotProton font manifest: {e}"))?;
    if manifest.schema_version != 1
        || manifest.revision != pin.revision
        || manifest.version != pin.version
    {
        return Err("NotProton fonts do not match the bundled source revision".into());
    }
    if manifest.files.is_empty()
        || !manifest
            .files
            .keys()
            .any(|name| name.ends_with(".ttf") || name.ends_with(".ttc"))
    {
        return Err("NotProton font payload is empty".into());
    }
    let source = payload.join("fonts");
    let destination = support.join("fonts");
    let mut verified = BTreeMap::new();
    // Validate the complete incoming set before changing a working installation.
    for (name, expected) in &manifest.files {
        check_name(name)?;
        if expected.len() != 64
            || !expected
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return Err(format!("Invalid NotProton font checksum: {name}"));
        }
        reject_links(&source, name)?;
        let path = source.join(name);
        let metadata = fs::metadata(&path)
            .map_err(|e| format!("Missing bundled NotProton font {name}: {e}"))?;
        if !metadata.is_file() || metadata.len() > MAX_ASSET_BYTES {
            return Err(format!("Invalid bundled NotProton font file: {name}"));
        }
        let data = fs::read(&path).map_err(|e| e.to_string())?;
        if format!("{:x}", Sha256::digest(&data)) != *expected {
            return Err(format!("Bundled NotProton font checksum mismatch: {name}"));
        }
        reject_links(&destination, name)?;
        verified.insert(name, data);
    }
    reject_links(&destination, RECEIPT)?;
    let previous = fs::read(destination.join(RECEIPT))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<FontManifest>(&bytes).ok());
    for (name, data) in verified {
        let path = destination.join(name);
        if fs::read(&path).ok().as_deref() != Some(data.as_slice()) {
            atomic_write(&path, &data)?;
        }
    }
    // Only prune paths recorded as owned by our last successful staging. Do not
    // delete unrelated fonts somebody placed in this directory themselves.
    if let Some(previous) = previous {
        for name in previous
            .files
            .keys()
            .filter(|name| !manifest.files.contains_key(*name))
        {
            check_name(name)?;
            reject_links(&destination, name)?;
            let path = destination.join(name);
            if path.is_file() {
                fs::remove_file(path).map_err(|e| e.to_string())?;
            }
        }
    }
    atomic_write(&destination.join(RECEIPT), &encoded)
}

fn check_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.contains('\\')
        || name.contains('\0')
        || name
            .split('/')
            .any(|piece| piece.is_empty() || piece == "." || piece == "..")
        || Path::new(name).is_absolute()
        || name == RECEIPT
    {
        return Err(format!("Unsafe NotProton font path: {name}"));
    }
    Ok(())
}

fn reject_links(root: &Path, name: &str) -> Result<()> {
    let mut path = root.to_owned();
    for part in std::iter::once(None).chain(Path::new(name).components().map(Some)) {
        if let Some(part) = part {
            path.push(part);
        }
        if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(format!(
                "Refusing a symlink in NotProton font payload: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

pub(crate) fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let directory = path.parent().ok_or("Missing payload parent directory")?;
    fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let temporary: PathBuf =
        directory.join(format!(".skinos-asset-{}-{unique}", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        file.write_all(data).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&temporary, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(root: &Path, files: &[(&str, &[u8])]) -> NotProtonInfo {
        let pin = NotProtonInfo {
            revision: "revision-117".into(),
            version: "1.1.7".into(),
        };
        let mut hashes = BTreeMap::new();
        for (name, data) in files {
            let path = root.join("fonts").join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, data).unwrap();
            hashes.insert(name.to_string(), format!("{:x}", Sha256::digest(data)));
        }
        fs::write(
            root.join("fonts-manifest.json"),
            serde_json::to_vec(&FontManifest {
                schema_version: 1,
                revision: pin.revision.clone(),
                version: pin.version.clone(),
                files: hashes,
            })
            .unwrap(),
        )
        .unwrap();
        pin
    }
    #[test]
    fn stages_alt_fonts_and_licenses_and_prunes_only_previously_owned_files() {
        let temp = tempfile::tempdir().unwrap();
        let payload = temp.path().join("payload");
        let support = temp.path().join("support");
        let pin = fixture(
            &payload,
            &[
                ("arial.ttf", b"arial"),
                ("alt/arial.ttf", b"alt"),
                ("LICENSE.fonts", b"license"),
            ],
        );
        stage_fonts(&payload, &support, &pin).unwrap();
        assert_eq!(
            fs::read(support.join("fonts/alt/arial.ttf")).unwrap(),
            b"alt"
        );
        assert_eq!(
            fs::read(support.join("fonts/LICENSE.fonts")).unwrap(),
            b"license"
        );
        fs::write(support.join("fonts/personal.ttf"), b"leave me").unwrap();
        fixture(&payload, &[("arial.ttf", b"updated")]);
        stage_fonts(&payload, &support, &pin).unwrap();
        assert!(!support.join("fonts/alt/arial.ttf").exists());
        assert_eq!(
            fs::read(support.join("fonts/personal.ttf")).unwrap(),
            b"leave me"
        );
    }
    #[test]
    fn bad_hash_or_revision_leaves_existing_payload_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let payload = temp.path().join("payload");
        let support = temp.path().join("support");
        let pin = fixture(&payload, &[("arial.ttf", b"old")]);
        stage_fonts(&payload, &support, &pin).unwrap();
        fixture(
            &payload,
            &[("arial.ttf", b"new"), ("broken.ttf", b"expected")],
        );
        fs::write(payload.join("fonts/broken.ttf"), b"corrupt").unwrap();
        assert!(stage_fonts(&payload, &support, &pin).is_err());
        assert_eq!(fs::read(support.join("fonts/arial.ttf")).unwrap(), b"old");
        let wrong = NotProtonInfo {
            revision: "other".into(),
            version: pin.version,
        };
        assert!(stage_fonts(&payload, &support, &wrong).is_err());
    }
    #[test]
    fn does_not_follow_destination_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let payload = temp.path().join("payload");
        let support = temp.path().join("support");
        let pin = fixture(&payload, &[("alt/arial.ttf", b"new")]);
        let external = temp.path().join("external");
        fs::create_dir(&external).unwrap();
        fs::write(external.join("arial.ttf"), b"user font").unwrap();
        fs::create_dir_all(support.join("fonts")).unwrap();
        std::os::unix::fs::symlink(&external, support.join("fonts/alt")).unwrap();
        assert!(stage_fonts(&payload, &support, &pin).is_err());
        assert_eq!(fs::read(external.join("arial.ttf")).unwrap(), b"user font");
    }
}
