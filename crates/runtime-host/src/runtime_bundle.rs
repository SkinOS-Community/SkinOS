//! Account-free runtime provisioning. This module never executes Valve code.
//! The app carries one gzip stream of file bytes in a compiled-in manifest order.
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const PIN: &[u8] = include_bytes!("../resources/runtime-payload-pin.json");
const BOOTSTRAP: &[u8] = include_bytes!("../resources/runtime-bootstrap.json");
const MAX_FILE: u64 = 128 * 1024 * 1024;
const MAX_TOTAL: u64 = 512 * 1024 * 1024;
type Result<T> = std::result::Result<T, String>;
unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    encoding: String,
    runtime_sha256: String,
    account_data_copied: bool,
    files: BTreeMap<String, Entry>,
    symlinks: BTreeMap<String, String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    sha256: String,
    size: u64,
    executable: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Bootstrap {
    schema_version: u32,
    publisher: String,
    client_manifest_version: String,
    archives: Vec<SourceArchive>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceArchive {
    id: String,
    archive: skinos_dependency_assets::ArchiveSpec,
    members: Vec<String>,
}

fn bootstrap(pin: &Manifest) -> Result<Bootstrap> {
    let sources: Bootstrap = serde_json::from_slice(BOOTSTRAP).map_err(|e| e.to_string())?;
    if sources.schema_version != 1
        || sources.publisher != "Valve"
        || sources.client_manifest_version != "1788652215"
        || sources.archives.len() != 7
    {
        return Err("Invalid runtime bootstrap pin".into());
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut packages = std::collections::BTreeSet::new();
    for source in &sources.archives {
        if source.members.is_empty() || !packages.insert(&source.id) {
            return Err("Invalid runtime archive inventory".into());
        }
        for member in &source.members {
            safe_name(member)?;
            if member == "steam-input-source.json"
                || !pin.files.contains_key(member)
                || !seen.insert(member.as_str())
            {
                return Err("Runtime bootstrap member is duplicated or outside the pin".into());
            }
        }
    }
    if seen.len() + 1 != pin.files.len() {
        return Err("Runtime bootstrap does not cover the full pin".into());
    }
    check_bytes(
        "steam-input-source.json",
        &generated_input_metadata(pin)?,
        &pin.files["steam-input-source.json"],
    )?;
    Ok(sources)
}

// Recreate the immutable cohort's legacy descriptor exactly. This describes
// the original audited cohort; current.json records its actual delivery source.
fn generated_input_metadata(pin: &Manifest) -> Result<Vec<u8>> {
    let mut names = vec!["libSDL3.dylib", "libsteaminput.dylib"];
    names.extend(
        pin.files
            .keys()
            .filter(|name| name.starts_with("controller_base/"))
            .map(String::as_str),
    );
    let lines: Result<Vec<_>> = names
        .into_iter()
        .map(|name| {
            let entry = pin
                .files
                .get(name)
                .ok_or("Missing pinned input component")?;
            Ok(format!(
                "    {}: \"{}\"",
                serde_json::to_string(name).map_err(|e| e.to_string())?,
                entry.sha256
            ))
        })
        .collect();
    Ok(format!("{{\n  \"schema_version\": 1,\n  \"runtime_sha256\": \"{}\",\n  \"source_kind\": \"user_installed_valve_binaries\",\n  \"account_data_copied\": false,\n  \"files\": {{\n{}\n  }}\n}}\n", pin.runtime_sha256, lines?.join(",\n")).into_bytes())
}

fn write_member(destination: &Path, name: &str, bytes: &[u8], entry: &Entry) -> Result<()> {
    safe_name(name)?;
    check_bytes(name, bytes, entry)?;
    let target = destination.join(name);
    fs::create_dir_all(target.parent().ok_or("Missing runtime parent")?)
        .map_err(|e| e.to_string())?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(if entry.executable { 0o700 } else { 0o600 })
        .open(target)
        .map_err(|e| e.to_string())?;
    file.write_all(bytes).map_err(|e| e.to_string())
}

fn link_framework(destination: &Path, pin: &Manifest) -> Result<()> {
    for (name, target) in &pin.symlinks {
        let link = destination.join(name);
        fs::create_dir_all(link.parent().ok_or("Missing framework parent")?)
            .map_err(|e| e.to_string())?;
        symlink(target, link).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn download(destination: &Path, data: &Path, pin: &Manifest) -> Result<()> {
    let sources = bootstrap(pin)?;
    let cache = data.join("dependency-archives");
    private_directory(&cache)?;
    for source in sources.archives {
        let archive = skinos_dependency_assets::ensure_archive(&cache, &source.archive)?;
        let mut archive = skinos_dependency_assets::PinnedZip::open(&archive)?;
        for name in source.members {
            let entry = &pin.files[&name];
            let bytes = archive.read_member(&name, &entry.sha256, Some(entry.size))?;
            write_member(destination, &name, &bytes, entry)?;
        }
    }
    write_member(
        destination,
        "steam-input-source.json",
        &generated_input_metadata(pin)?,
        &pin.files["steam-input-source.json"],
    )?;
    link_framework(destination, pin)?;
    verify_tree(destination, pin)
}

pub fn prepare() -> Result<PathBuf> {
    let data = match std::env::var_os("SKINOS_DATA_DIR") {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(std::env::var_os("HOME").ok_or("HOME unavailable")?)
            .join("Library/Application Support/SkinOs"),
    };
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let payload = executable
        .parent()
        .ok_or("Missing executable directory")?
        .join("../Resources/runtime-payload");
    prepare_at(&payload, &data)
}

fn safe_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.contains(['\\', '\0'])
        || name
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
        || Path::new(name)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err("Unsafe bundled runtime relative path".into());
    }
    Ok(())
}

fn manifest() -> Result<Manifest> {
    let value: Manifest = serde_json::from_slice(PIN).map_err(|e| e.to_string())?;
    if value.schema_version != 1
        || value.encoding != "ordered-file-concat-gzip"
        || value.runtime_sha256 != crate::AUDITED_RUNTIME
        || value.account_data_copied
        || !(1..=5000).contains(&value.files.len())
    {
        return Err("Invalid compiled runtime pin".into());
    }
    let mut total = 0_u64;
    for (name, entry) in &value.files {
        safe_name(name)?;
        total = total
            .checked_add(entry.size)
            .ok_or("Runtime payload exceeds size limit")?;
        if entry.size > MAX_FILE || total > MAX_TOTAL {
            return Err("Runtime payload exceeds size limit".into());
        }
    }
    for (name, target) in &value.symlinks {
        safe_name(name)?;
        safe_name(target)?;
    }
    Ok(value)
}

fn check_components(root: &Path, relative: &Path) -> Result<()> {
    let mut path = root.to_path_buf();
    for component in relative.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err("Runtime path escapes the owned source".into());
        }
        path.push(component);
        if fs::symlink_metadata(&path)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink()
        {
            return Err("Unexpected symlink in owned runtime".into());
        }
    }
    Ok(())
}

fn read_regular(root: &Path, name: &str, limit: u64) -> Result<Vec<u8>> {
    safe_name(name)?;
    check_components(root, Path::new(name))?;
    let path = root.join(name);
    let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(format!("Invalid runtime file: {name}"));
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("Runtime file exceeds size limit".into());
    }
    Ok(bytes)
}

fn check_bytes(name: &str, bytes: &[u8], entry: &Entry) -> Result<()> {
    if bytes.len() as u64 != entry.size || format!("{:x}", Sha256::digest(bytes)) != entry.sha256 {
        return Err(format!("Pinned runtime checksum mismatch: {name}"));
    }
    Ok(())
}

fn verify_tree(directory: &Path, pin: &Manifest) -> Result<()> {
    if fs::symlink_metadata(directory)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("Owned runtime directory must not be a symlink".into());
    }
    for (name, entry) in &pin.files {
        check_bytes(name, &read_regular(directory, name, entry.size)?, entry)?;
    }
    for (name, target) in &pin.symlinks {
        check_components(
            directory,
            Path::new(name).parent().ok_or("Invalid framework link")?,
        )?;
        if fs::read_link(directory.join(name)).map_err(|e| e.to_string())? != Path::new(target) {
            return Err("Unexpected framework symlink target".into());
        }
    }
    Ok(())
}

fn existing(root: &Path, pin: &Manifest) -> Result<Option<PathBuf>> {
    let current = root.join("current.json");
    match fs::symlink_metadata(&current) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
        Ok(_) => {}
    }
    let value: serde_json::Value =
        serde_json::from_slice(&read_regular(root, "current.json", 65536)?)
            .map_err(|_| "Invalid owned runtime manifest")?;
    if value["runtime_sha256"] != crate::AUDITED_RUNTIME {
        return Err("Owned runtime version is not audited".into());
    }
    let directory = PathBuf::from(
        value["runtime_directory"]
            .as_str()
            .ok_or("Owned runtime directory missing")?,
    );
    let directory = if directory.is_absolute() {
        directory
    } else {
        root.join(directory)
    };
    let relative = directory
        .strip_prefix(root)
        .map_err(|_| "Owned runtime directory escapes source root")?;
    if relative.as_os_str().is_empty() {
        return Err("Owned runtime directory must be a versioned child".into());
    }
    check_components(root, relative)?;
    verify_tree(&directory, pin)?;
    Ok(Some(directory.join("steamclient.dylib")))
}

fn private_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err("Runtime directory is redirected or is not a directory".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|e| e.to_string())?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())
        }
        Err(error) => Err(error.to_string()),
    }
}

fn has_bundled_payload(payload: &Path) -> Result<bool> {
    match fs::symlink_metadata(payload) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err("Bundled runtime payload must be a regular directory".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    }
    if read_regular(payload, "manifest.json", PIN.len() as u64)? != PIN {
        return Err("Bundled runtime manifest differs from the compiled pin".into());
    }
    // Public builds ship the same inspection manifest but no proprietary bytes.
    // A present malformed/redirected archive remains an error, never a fallback.
    match fs::symlink_metadata(payload.join("payload.gz")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.to_string()),
        Ok(metadata)
            if metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.len() <= MAX_TOTAL =>
        {
            Ok(true)
        }
        Ok(_) => Err("Invalid compressed runtime payload".into()),
    }
}

fn prepare_at(payload: &Path, data: &Path) -> Result<PathBuf> {
    let pin = manifest()?;
    if let Ok(metadata) = fs::symlink_metadata(data) {
        if metadata.file_type().is_symlink() {
            return Err("Data directory must not be a symlink".into());
        }
    }
    fs::create_dir_all(data).map_err(|e| e.to_string())?;
    let data = data.canonicalize().map_err(|e| e.to_string())?;
    let root = data.join("runtime-source");
    private_directory(&root)?;
    let lock_path = root.join(".prepare.lock");
    if fs::symlink_metadata(&lock_path).is_ok_and(|m| !m.is_file() || m.file_type().is_symlink()) {
        return Err("Unsafe runtime preparation lock".into());
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(lock_path)
        .map_err(|e| e.to_string())?;
    if unsafe { flock(lock.as_raw_fd(), 2) } != 0 {
        return Err("Cannot lock runtime preparation".into());
    }
    if let Some(runtime) = existing(&root, &pin)? {
        return Ok(runtime);
    }
    let destination = root.join(crate::AUDITED_RUNTIME);
    let delivery_source;
    if fs::symlink_metadata(&destination).is_ok() {
        verify_tree(&destination, &pin)?;
        delivery_source = "verified_existing_cache";
    } else {
        let bundled = has_bundled_payload(payload)?;
        delivery_source = if bundled {
            "bundled_pinned_binaries"
        } else {
            "publisher_pinned_downloads"
        };
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let stage = root.join(format!(".prepare-{}-{stamp}", std::process::id()));
        private_directory(&stage)?;
        let result = (if bundled {
            extract(payload, &stage, &pin)
        } else {
            download(&stage, &data, &pin)
        })
        .and_then(|()| fs::rename(&stage, &destination).map_err(|e| e.to_string()));
        if result.is_err() {
            let _ = fs::remove_dir_all(&stage);
        }
        result?;
    }
    let value = serde_json::json!({"schema_version":1, "runtime_sha256":crate::AUDITED_RUNTIME,
        "runtime_directory":crate::AUDITED_RUNTIME, "source_kind":delivery_source, "account_data_copied":false});
    let temporary = root.join(format!(".current-{}.json", std::process::id()));
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|e| e.to_string())?;
    output
        .write_all(&serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    output.sync_all().map_err(|e| e.to_string())?;
    fs::rename(temporary, root.join("current.json")).map_err(|e| e.to_string())?;
    Ok(destination.join("steamclient.dylib"))
}

fn extract(payload: &Path, destination: &Path, pin: &Manifest) -> Result<()> {
    let resources = payload.parent().ok_or("Missing runtime resource parent")?;
    let delivery = skinos_compat::native_delivery::Delivery::load(resources)?;
    if let Some(delivery) = &delivery {
        for name in delivery.runtime.keys() {
            if !pin.files.contains_key(name) {
                return Err("Native delivery runtime entry is outside the compiled pin".into());
            }
        }
    }
    let mut child = Command::new("/usr/bin/gzip")
        .arg("-dc")
        .arg(payload.join("payload.gz"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Cannot decompress bundled runtime: {e}"))?;
    let result: Result<()> = (|| {
        let mut input = child.stdout.take().ok_or("Missing decompression output")?;
        for (name, entry) in &pin.files {
            let bytes = if let Some(record) = delivery
                .as_ref()
                .and_then(|manifest| manifest.runtime.get(name))
            {
                record.restore(resources, &entry.sha256, Some(entry.size))?
            } else {
                let mut bytes = vec![0; entry.size as usize];
                input
                    .read_exact(&mut bytes)
                    .map_err(|_| "Bundled runtime payload is truncated")?;
                bytes
            };
            write_member(destination, name, &bytes, entry)?;
        }
        let mut extra = [0];
        if input.read(&mut extra).map_err(|e| e.to_string())? != 0 {
            return Err("Unexpected extra runtime payload bytes".into());
        }
        link_framework(destination, pin)
    })();
    if result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    result?;
    if !status.success() {
        return Err("Bundled runtime gzip integrity check failed".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_only_public_bundle_selects_download_without_ignoring_corruption() {
        let root = std::env::temp_dir().join(format!(
            "skinos-bootstrap-source-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert!(!has_bundled_payload(&root).unwrap());
        fs::create_dir(&root).unwrap();
        fs::write(root.join("manifest.json"), PIN).unwrap();
        assert!(!has_bundled_payload(&root).unwrap());
        fs::write(root.join("payload.gz"), b"present-but-invalid").unwrap();
        assert!(has_bundled_payload(&root).unwrap());
        fs::remove_file(root.join("payload.gz")).unwrap();
        symlink("/dev/null", root.join("payload.gz")).unwrap();
        assert!(has_bundled_payload(&root).is_err());
        fs::remove_file(root.join("payload.gz")).unwrap();
        fs::write(root.join("manifest.json"), b"{}").unwrap();
        assert!(has_bundled_payload(&root).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn official_archive_inventory_covers_every_original_pinned_byte() {
        let pin = manifest().unwrap();
        let sources = bootstrap(&pin).unwrap();
        assert_eq!(
            sources
                .archives
                .iter()
                .map(|a| a.members.len())
                .sum::<usize>(),
            3812
        );
        let bytes = generated_input_metadata(&pin).unwrap();
        assert_eq!(bytes.len(), 492622);
        check_bytes(
            "steam-input-source.json",
            &bytes,
            &pin.files["steam-input-source.json"],
        )
        .unwrap();
    }
    #[test]
    fn pin_has_only_public_components_and_safe_framework_links() {
        let pin = manifest().unwrap();
        assert_eq!(
            pin.files["steamclient.dylib"].sha256,
            crate::AUDITED_RUNTIME
        );
        assert_eq!(pin.symlinks.len(), 4);
        for bad in ["../accounts.json", "/secret", "a/../../b", "a//b", "a\\b"] {
            assert!(safe_name(bad).is_err());
        }
    }

    #[test]
    fn malformed_current_and_redirected_cache_are_rejected() {
        let data = std::env::temp_dir().join(format!(
            "skinos-bundle-unsafe-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(data.join("runtime-source")).unwrap();
        let current = data.join("runtime-source/current.json");
        fs::write(&current, b"not JSON").unwrap();
        assert!(prepare_at(Path::new("/missing-payload"), &data).is_err());
        let unsafe_current = serde_json::json!({"runtime_sha256":crate::AUDITED_RUNTIME,
            "runtime_directory":"../outside"});
        fs::write(&current, serde_json::to_vec(&unsafe_current).unwrap()).unwrap();
        assert!(prepare_at(Path::new("/missing-payload"), &data).is_err());
        fs::remove_file(&current).unwrap();
        symlink("/dev/null", &current).unwrap();
        assert!(prepare_at(Path::new("/missing-payload"), &data).is_err());
        fs::remove_dir_all(data.join("runtime-source")).unwrap();
        symlink("/tmp", data.join("runtime-source")).unwrap();
        assert!(prepare_at(Path::new("/missing-payload"), &data).is_err());
        fs::remove_dir_all(data).unwrap();
    }

    #[test]
    fn fresh_payload_roundtrip_and_cache_validation() {
        let Some(payload) = std::env::var_os("SKINOS_TEST_RUNTIME_PAYLOAD") else {
            return;
        };
        let root = std::env::temp_dir().join(format!(
            "skinos-bundle-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let runtime = prepare_at(Path::new(&payload), &root).unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(fs::read(&runtime).unwrap())),
            crate::AUDITED_RUNTIME
        );
        let current = root.join("runtime-source/current.json");
        let before = fs::read(&current).unwrap();
        let modified = fs::metadata(&current).unwrap().modified().unwrap();
        assert_eq!(
            prepare_at(Path::new("/missing-payload"), &root).unwrap(),
            runtime
        );
        assert_eq!(fs::read(&current).unwrap(), before);
        assert_eq!(
            fs::metadata(&current).unwrap().modified().unwrap(),
            modified
        );
        let component = runtime.with_file_name("ipcserver");
        fs::write(&component, b"corrupted").unwrap();
        assert!(prepare_at(Path::new(&payload), &root).is_err());
        fs::remove_file(&component).unwrap();
        symlink("/dev/null", &component).unwrap();
        assert!(prepare_at(Path::new(&payload), &root).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
