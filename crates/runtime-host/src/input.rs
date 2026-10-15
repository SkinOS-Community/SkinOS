//! Stage only the reviewed Steam Input cohort. Never discover or load an
//! original Steam install as a fallback, and never copy account configuration.
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path};

const MANIFEST: &str = "steam-input-source.json";
const SDL: &str = "56fad2994d5c1c618862459d3aaa6e7c6048a83f1a4dc3e3ef543edf2272dfed";
const INPUT: &str = "7b09276160dfc42d1b8bb950a70b2f6a6802fa322728fe958f5ae4761b6eb585";
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    runtime_sha256: String,
    source_kind: String,
    account_data_copied: bool,
    files: BTreeMap<String, String>,
}

pub fn stage(source: &Path, destination: &Path) -> Result<(), String> {
    let manifest_path = source.join(MANIFEST);
    let metadata = fs::symlink_metadata(&manifest_path).map_err(|e| {
        format!("Steam Input cohort is not staged: {e}. Run scripts/stage-runtime-input.py.")
    })?;
    if !metadata.is_file() || metadata.len() > 1024 * 1024 {
        return Err("Invalid Steam Input source manifest".into());
    }
    let bytes = fs::read(&manifest_path).map_err(|e| e.to_string())?;
    let manifest: Manifest = serde_json::from_slice(&bytes)
        .map_err(|e| format!("Invalid Steam Input source manifest: {e}"))?;
    validate_manifest(&manifest)?;

    // Verify the entire allowlist before writing any component. Re-read and
    // re-check each file when copying to detect replacement during staging.
    let mut total = 0_u64;
    for (name, digest) in &manifest.files {
        let content = read_component(source, name)?;
        total = total
            .checked_add(content.len() as u64)
            .ok_or("Input cohort too large")?;
        if total > MAX_TOTAL_BYTES {
            return Err("Steam Input cohort exceeds size limit".into());
        }
        verify_hash(name, &content, digest)?;
    }
    for (name, digest) in &manifest.files {
        let content = read_component(source, name)?;
        verify_hash(name, &content, digest)?;
        let target = destination.join(name);
        fs::create_dir_all(target.parent().ok_or("Invalid input target")?)
            .map_err(|e| e.to_string())?;
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&target)
            .map_err(|e| format!("Could not stage {name}: {e}"))?;
        output.write_all(&content).map_err(|e| e.to_string())?;
    }
    fs::write(destination.join(MANIFEST), bytes).map_err(|e| e.to_string())?;
    Ok(())
}

fn validate_manifest(manifest: &Manifest) -> Result<(), String> {
    if manifest.schema_version != 1
        || manifest.runtime_sha256 != crate::AUDITED_RUNTIME
        || manifest.source_kind != "user_installed_valve_binaries"
        || manifest.account_data_copied
        || !(3..=4000).contains(&manifest.files.len())
        || manifest.files.get("libSDL3.dylib").map(String::as_str) != Some(SDL)
        || manifest
            .files
            .get("libsteaminput.dylib")
            .map(String::as_str)
            != Some(INPUT)
    {
        return Err("Steam Input manifest does not match the audited Valve runtime cohort".into());
    }
    for (name, digest) in &manifest.files {
        validate_name(name)?;
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(format!("Invalid SHA256 for input component {name}"));
        }
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.contains('\\')
        || name.contains('\0')
        || name
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || Path::new(name)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        || !(name == "libSDL3.dylib"
            || name == "libsteaminput.dylib"
            || name.starts_with("controller_base/"))
    {
        return Err(format!(
            "Input component is outside the public allowlist: {name}"
        ));
    }
    Ok(())
}

fn read_component(source: &Path, name: &str) -> Result<Vec<u8>, String> {
    validate_name(name)?;
    let mut path = source.to_path_buf();
    let components: Vec<_> = Path::new(name).components().collect();
    for (index, component) in components.iter().enumerate() {
        path.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&path)
            .map_err(|e| format!("Missing input component {name}: {e}"))?;
        if metadata.file_type().is_symlink()
            || (index + 1 < components.len() && !metadata.is_dir())
            || (index + 1 == components.len()
                && (!metadata.is_file() || metadata.len() > MAX_FILE_BYTES))
        {
            return Err(format!(
                "Input component must be a regular file under the owned source: {name}"
            ));
        }
    }
    fs::read(path).map_err(|e| format!("Cannot read input component {name}: {e}"))
}

fn verify_hash(name: &str, bytes: &[u8], expected: &str) -> Result<(), String> {
    if format!("{:x}", Sha256::digest(bytes)) != expected {
        return Err(format!("Steam Input component SHA256 mismatch: {name}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_allowlist_rejects_account_paths_and_traversal() {
        for name in [
            "../config/loginusers.vdf",
            "controller_base/../config",
            "/controller_base/a",
            "controller_base//a",
            "controller_base/./a",
            "controller_base/a\\b",
            "userdata/a",
            "steamclient.dylib",
        ] {
            assert!(validate_name(name).is_err(), "accepted {name}");
        }
        assert!(validate_name("controller_base/templates/controller_generic.vdf").is_ok());
    }

    #[test]
    fn changed_component_is_rejected() {
        let expected = format!("{:x}", Sha256::digest(b"reviewed content"));
        assert!(verify_hash("controller_base/test.vdf", b"reviewed content", &expected).is_ok());
        assert!(verify_hash("controller_base/test.vdf", b"changed content", &expected).is_err());
    }

    #[test]
    fn manifest_requires_matching_engine_and_no_account_data() {
        let mut manifest = Manifest {
            schema_version: 1,
            runtime_sha256: crate::AUDITED_RUNTIME.into(),
            source_kind: "user_installed_valve_binaries".into(),
            account_data_copied: false,
            files: [
                ("libSDL3.dylib".into(), SDL.into()),
                ("libsteaminput.dylib".into(), INPUT.into()),
                ("controller_base/test.vdf".into(), "0".repeat(64)),
            ]
            .into(),
        };
        assert!(validate_manifest(&manifest).is_ok());
        manifest.account_data_copied = true;
        assert!(validate_manifest(&manifest).is_err());
        manifest.account_data_copied = false;
        manifest.runtime_sha256 = "0".repeat(64);
        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn symlinked_directory_cannot_escape_source() {
        let root = std::env::temp_dir().join(format!(
            "skinos-input-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink("/tmp", root.join("controller_base")).unwrap();
        assert!(read_component(&root, "controller_base/test.vdf").is_err());
        fs::remove_file(root.join("controller_base")).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
