//! Download immutable dependencies from their original publishers. Never execute
//! downloaded code or extract an archive tree; return only hash-pinned members.

use reqwest::{Url, blocking::Client, redirect::Policy};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub type Result<T> = std::result::Result<T, String>;
const MAX_ARCHIVE: u64 = 512 * 1024 * 1024;
const MAX_MEMBER: u64 = 128 * 1024 * 1024;
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveSpec {
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn trusted_url(url: &Url, redirect: bool) -> bool {
    if url.scheme() != "https"
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || (!redirect && url.query().is_some())
    {
        return false;
    }
    match url.host_str() {
        Some("client-update.akamai.steamstatic.com") => true,
        Some("steamcdn-a.akamaihd.net" | "media.steampowered.com") => {
            url.path().starts_with("/client/")
        }
        Some("github.com") => url
            .path()
            .starts_with("/NotProtonNot/NotProton/releases/download/"),
        Some("codeload.github.com") => url.path().starts_with("/NotProtonNot/NotProton/"),
        Some("release-assets.githubusercontent.com") => redirect,
        _ => false,
    }
}

fn checked_spec(spec: &ArchiveSpec) -> Result<Url> {
    let url = Url::parse(&spec.url).map_err(|_| "Invalid dependency URL")?;
    if !trusted_url(&url, false)
        || spec.size == 0
        || spec.size > MAX_ARCHIVE
        || !valid_hash(&spec.sha256)
    {
        return Err("Invalid pinned dependency source".into());
    }
    Ok(url)
}

fn private_directory(path: &Path) -> Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err("Dependency cache must be a regular directory".into());
        }
    } else {
        fs::create_dir_all(path).map_err(|e| e.to_string())?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn hash_file(path: &Path, expected_size: u64) -> Result<String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() != expected_size {
        return Err("Dependency archive is redirected or has the wrong size".into());
    }
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let count = std::io::copy(&mut (&mut file).take(expected_size + 1), &mut hash)
        .map_err(|e| e.to_string())?;
    if count != expected_size {
        return Err("Dependency archive changed while reading".into());
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn store_archive(mut input: impl Read, destination: &Path, spec: &ArchiveSpec) -> Result<()> {
    let parent = destination
        .parent()
        .ok_or("Missing dependency cache directory")?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let temporary = parent.join(format!(
        ".download-{}-{stamp}-{}",
        std::process::id(),
        TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        let mut hash = Sha256::new();
        let mut size = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = input
                .read(&mut buffer)
                .map_err(|e| format!("Dependency download interrupted: {e}"))?;
            if count == 0 {
                break;
            }
            size = size
                .checked_add(count as u64)
                .ok_or("Dependency download size overflow")?;
            if size > spec.size {
                return Err("Dependency download exceeds pinned size".into());
            }
            hash.update(&buffer[..count]);
            file.write_all(&buffer[..count])
                .map_err(|e| e.to_string())?;
        }
        if size != spec.size || format!("{:x}", hash.finalize()) != spec.sha256 {
            return Err("Dependency archive checksum or size mismatch".into());
        }
        file.sync_all().map_err(|e| e.to_string())?;
        // Content-addressed destination; concurrent successful writers hold
        // exactly the same bytes. No user-selected filename is overwritten.
        fs::rename(&temporary, destination).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Return a verified, content-addressed archive; an existing corrupt cache is
/// reported explicitly. This blocking function must run outside an async executor.
pub fn ensure_archive(cache_root: &Path, spec: &ArchiveSpec) -> Result<PathBuf> {
    let url = checked_spec(spec)?;
    private_directory(cache_root)?;
    let destination = cache_root.join(format!("{}.zip", spec.sha256));
    match fs::symlink_metadata(&destination) {
        Ok(_) => {
            if hash_file(&destination, spec.size)? != spec.sha256 {
                return Err("Cached dependency checksum mismatch".into());
            }
            return Ok(destination);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.to_string()),
    }
    let client = Client::builder()
        .https_only(true)
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(600))
        .redirect(Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 || !trusted_url(attempt.url(), true) {
                attempt.error("Dependency redirect is outside publisher hosts")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|e| e.without_url().to_string())?;
    let response = client
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.without_url().to_string())?;
    if response
        .content_length()
        .is_some_and(|size| size != spec.size)
    {
        return Err("Dependency server size differs from the pin".into());
    }
    store_archive(response, &destination, spec)?;
    Ok(destination)
}

fn safe_member(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['\\', '\0', ':'])
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

pub struct PinnedZip {
    archive: zip::ZipArchive<File>,
}

impl PinnedZip {
    pub fn open(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > MAX_ARCHIVE
        {
            return Err("Unsafe dependency archive".into());
        }
        let archive = zip::ZipArchive::new(File::open(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        if archive.len() > 50_000 {
            return Err("Dependency archive has too many members".into());
        }
        Ok(Self { archive })
    }

    /// Only the exact requested regular file is decompressed, bounded, and
    /// verified. Unrelated archive paths are never written to the filesystem.
    pub fn read_member(
        &mut self,
        member: &str,
        sha256: &str,
        size: Option<u64>,
    ) -> Result<Vec<u8>> {
        if !safe_member(member) || !valid_hash(sha256) || size.is_some_and(|size| size > MAX_MEMBER)
        {
            return Err("Unsafe pinned ZIP member".into());
        }
        let mut file = self
            .archive
            .by_name(member)
            .map_err(|_| format!("Missing pinned ZIP member: {member}"))?;
        if file.is_dir()
            || file.is_symlink()
            || file.size() > MAX_MEMBER
            || size.is_some_and(|size| size != file.size())
        {
            return Err(format!("Invalid ZIP member type or size: {member}"));
        }
        let expected_size = size.unwrap_or(file.size());
        let mut bytes = Vec::new();
        (&mut file)
            .take(expected_size + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 != expected_size || format!("{:x}", Sha256::digest(&bytes)) != sha256
        {
            return Err(format!("Pinned ZIP member checksum mismatch: {member}"));
        }
        Ok(bytes)
    }
}

pub fn read_member(
    archive: &Path,
    member: &str,
    expected_sha256: &str,
    expected_size: Option<u64>,
) -> Result<Vec<u8>> {
    PinnedZip::open(archive)?.read_member(member, expected_sha256, expected_size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::os::unix::fs::symlink;
    fn hash(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }
    fn spec(bytes: &[u8]) -> ArchiveSpec {
        ArchiveSpec {
            url: "https://client-update.akamai.steamstatic.com/pinned.zip".into(),
            size: bytes.len() as u64,
            sha256: hash(bytes),
        }
    }
    #[test]
    fn publisher_boundaries_reject_http_credentials_and_untrusted_redirects() {
        for url in [
            "http://client-update.akamai.steamstatic.com/file",
            "https://github.com/other/repo/file",
            "https://localhost/file",
            "https://user@client-update.akamai.steamstatic.com/file",
            "https://client-update.akamai.steamstatic.com:444/file",
        ] {
            assert!(!trusted_url(&Url::parse(url).unwrap(), false));
        }
        assert!(trusted_url(
            &Url::parse("https://release-assets.githubusercontent.com/path?sig=temporary").unwrap(),
            true
        ));
        assert!(!trusted_url(
            &Url::parse("https://example.com/path").unwrap(),
            true
        ));
    }
    #[test]
    fn stream_limits_and_checksum_failures_never_publish_partial_archives() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("archive.zip");
        for bytes in [b"longer".as_slice(), b"bad", b"go"] {
            assert!(store_archive(Cursor::new(bytes), &target, &spec(b"good")).is_err());
            assert!(!target.exists());
            assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
        }
        store_archive(Cursor::new(b"good"), &target, &spec(b"good")).unwrap();
        assert_eq!(hash_file(&target, 4).unwrap(), hash(b"good"));
    }
    #[test]
    fn cache_reuse_is_offline_and_rejects_corruption_or_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let spec = spec(b"good");
        let path = root.path().join(format!("{}.zip", spec.sha256));
        fs::write(&path, b"good").unwrap();
        assert_eq!(ensure_archive(root.path(), &spec).unwrap(), path);
        fs::write(&path, b"evil").unwrap();
        assert!(ensure_archive(root.path(), &spec).is_err());
        fs::remove_file(&path).unwrap();
        symlink("/dev/null", &path).unwrap();
        assert!(ensure_archive(root.path(), &spec).is_err());
    }
    #[test]
    fn selected_zip_member_checks_hash_size_type_and_path_without_extracting() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("test.zip");
        let mut archive = zip::ZipWriter::new(File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        archive.start_file("nested/owned", options).unwrap();
        archive.write_all(b"value").unwrap();
        archive.start_file("../outside", options).unwrap();
        archive.write_all(b"outside").unwrap();
        archive
            .add_symlink("redirect", "/etc/passwd", options)
            .unwrap();
        archive.finish().unwrap();
        let mut zip = PinnedZip::open(&path).unwrap();
        assert_eq!(
            zip.read_member("nested/owned", &hash(b"value"), Some(5))
                .unwrap(),
            b"value"
        );
        for name in [
            "../outside",
            "redirect",
            "/absolute",
            "a\\b",
            "nested//owned",
        ] {
            assert!(zip.read_member(name, &hash(b"value"), None).is_err());
        }
        assert!(
            zip.read_member("nested/owned", &hash(b"wrong"), None)
                .is_err()
        );
        assert!(
            zip.read_member("nested/owned", &hash(b"value"), Some(1))
                .is_err()
        );
        assert!(!root.path().join("nested").exists());
    }
}
