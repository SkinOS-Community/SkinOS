//! SkinOs-owned sessions only. No Steam configuration files or credential stores are imported.
use crate::model::Game;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

pub const KEYCHAIN_SERVICE: &str = "dev.skinos.steam.refresh-token";

/// Intentionally has neither Debug nor Serialize implementations.
pub struct SecretString(Zeroizing<String>);
impl SecretString {
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    pub steamid: String,
    pub account_name: String,
    pub last_used: u64,
}

pub fn data_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("SKINOS_DATA_DIR") {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME").context("HOME is unavailable")?;
    Ok(PathBuf::from(home).join("Library/Application Support/SkinOs"))
}

fn validate_id(id: &str) -> Result<()> {
    if id.len() != 17 || !id.bytes().all(|b| b.is_ascii_digit()) {
        bail!("Invalid SteamID");
    }
    Ok(())
}

pub fn list_accounts() -> Result<Vec<Account>> {
    let path = data_dir()?.join("accounts.json");
    if !path.exists() {
        return Ok(vec![]);
    }
    let bytes = std::fs::read(path)?;
    serde_json::from_slice(&bytes).context("Cannot read SkinOs account registry")
}

pub fn auto_login_account() -> Result<Option<Account>> {
    auto_login_account_at(&data_dir()?)
}
fn auto_login_account_at(root: &Path) -> Result<Option<Account>> {
    if root.join("auto-login-disabled").exists() {
        return Ok(None);
    }
    let path = root.join("accounts.json");
    if !path.exists() {
        return Ok(None);
    }
    let accounts: Vec<Account> = serde_json::from_slice(&std::fs::read(path)?)?;
    Ok(accounts.into_iter().max_by_key(|account| account.last_used))
}

pub fn set_auto_login(enabled: bool) -> Result<()> {
    let marker = data_dir()?.join("auto-login-disabled");
    if enabled {
        match std::fs::remove_file(marker) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    } else {
        atomic_json(&marker, &true)
    }
}

pub fn save_account(account: Account, token: &str) -> Result<()> {
    validate_id(&account.steamid)?;
    save_refresh_token(&account.steamid, token)?;
    let mut accounts = list_accounts()?;
    accounts.retain(|a| a.steamid != account.steamid);
    accounts.insert(0, account);
    atomic_json(&data_dir()?.join("accounts.json"), &accounts)?;
    set_auto_login(true)
}

pub fn remove_account(steamid: &str) -> Result<()> {
    validate_id(steamid)?;
    delete_refresh_token(steamid)?;
    let mut accounts = list_accounts()?;
    accounts.retain(|a| a.steamid != steamid);
    atomic_json(&data_dir()?.join("accounts.json"), &accounts)?;
    let cache = data_dir()?.join("library").join(format!("{steamid}.json"));
    if cache.exists() {
        std::fs::remove_file(cache)?;
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CredentialStore {
    File,
    Keychain,
}
fn selected_store(value: Option<&str>, debug: bool) -> Result<CredentialStore> {
    match value {
        Some("file") => Ok(CredentialStore::File),
        Some("keychain") => Ok(CredentialStore::Keychain),
        None => Ok(if debug {
            CredentialStore::File
        } else {
            CredentialStore::Keychain
        }),
        _ => bail!("SKINOS_CREDENTIAL_STORE must be file or keychain"),
    }
}
fn credential_store() -> Result<CredentialStore> {
    selected_store(
        std::env::var("SKINOS_CREDENTIAL_STORE").ok().as_deref(),
        cfg!(debug_assertions),
    )
}

pub fn load_refresh_token(steamid: &str) -> Result<SecretString> {
    validate_id(steamid)?;
    match credential_store()? {
        CredentialStore::Keychain => keychain_load(steamid),
        CredentialStore::File => FileCredentials::open(&data_dir()?)?
            .load_with_migration(steamid, || keychain_load(steamid)),
    }
}
fn save_refresh_token(steamid: &str, token: &str) -> Result<()> {
    validate_id(steamid)?;
    match credential_store()? {
        CredentialStore::Keychain => keychain_save(steamid, token),
        CredentialStore::File => FileCredentials::open(&data_dir()?)?.save(steamid, token),
    }
}
fn delete_refresh_token(steamid: &str) -> Result<()> {
    validate_id(steamid)?;
    match credential_store()? {
        CredentialStore::Keychain => keychain_delete(steamid),
        CredentialStore::File => FileCredentials::open(&data_dir()?)?.forget(steamid),
    }
}

/// Development-only local storage by default. Tokens remain opaque and are
/// never serialized into events, account metadata, diagnostics, or Debug output.
struct FileCredentials {
    root: PathBuf,
}
impl FileCredentials {
    fn open(data: &Path) -> Result<Self> {
        ensure!(
            !data.is_symlink(),
            "Session data directory cannot be a symlink"
        );
        std::fs::create_dir_all(data)?;
        let root = data.join("credentials");
        ensure!(
            !root.is_symlink(),
            "Credential directory cannot be a symlink"
        );
        let mut create = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            create.mode(0o700);
        }
        match create.create(&root) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e).context("Cannot create credential directory"),
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            let dir = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
                .open(&root)?;
            dir.set_permissions(std::fs::Permissions::from_mode(0o700))?;
        }
        ensure!(root.is_dir(), "Credential directory is unavailable");
        Ok(Self { root })
    }
    fn path(&self, id: &str, extension: &str) -> Result<PathBuf> {
        validate_id(id)?;
        ensure!(
            !self.root.is_symlink(),
            "Credential directory cannot be a symlink"
        );
        Ok(self.root.join(format!("{id}.{extension}")))
    }
    fn read(&self, id: &str) -> Result<Option<SecretString>> {
        use std::io::Read;
        let path = self.path(id, "token")?;
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = match options.open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).context("Cannot open local SkinOs session"),
        };
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file() && metadata.len() > 0 && metadata.len() <= 65536,
            "Invalid local session file"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                metadata.permissions().mode() & 0o777 == 0o600,
                "Local session permissions must be 0600"
            );
        }
        let mut bytes = Zeroizing::new(Vec::new());
        file.read_to_end(&mut bytes)?;
        let token = std::str::from_utf8(&bytes).context("Invalid session encoding")?;
        ensure!(!token.contains('\0'), "Invalid session encoding");
        Ok(Some(SecretString(Zeroizing::new(token.to_owned()))))
    }
    fn migration_marker(&self, id: &str) -> Result<PathBuf> {
        self.path(id, "keychain-migration-attempted")
    }
    fn mark_migration_attempted(&self, id: &str) -> Result<()> {
        self.atomic_write(&self.migration_marker(id)?, b"1\n")
    }
    fn claim_migration(&self, id: &str) -> Result<()> {
        use std::io::Write;
        let path = self.migration_marker(id)?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut marker = options
            .open(path)
            .context("Local session migration already attempted; sign in again")?;
        marker.write_all(b"1\n")?;
        marker.sync_all()?;
        std::fs::File::open(&self.root)?.sync_all()?;
        Ok(())
    }
    fn load_with_migration(
        &self,
        id: &str,
        migrate: impl FnOnce() -> Result<SecretString>,
    ) -> Result<SecretString> {
        if let Some(secret) = self.read(id)? {
            return Ok(secret);
        }
        let marker = self.migration_marker(id)?;
        ensure!(
            !marker.is_symlink(),
            "Session migration marker cannot be a symlink"
        );
        ensure!(
            !marker.exists(),
            "Local SkinOs session unavailable; sign in again"
        );
        // Mark before invoking Keychain, including when the user cancels. A
        // missing/forgotten token can never create a loop of authorization prompts.
        self.claim_migration(id)?;
        let secret = migrate()?;
        self.save(id, secret.expose_secret())?;
        Ok(secret)
    }
    fn save(&self, id: &str, token: &str) -> Result<()> {
        ensure!(
            !token.is_empty() && token.len() <= 65536 && !token.contains('\0'),
            "Invalid session token"
        );
        self.mark_migration_attempted(id)?;
        self.atomic_write(&self.path(id, "token")?, token.as_bytes())
    }
    fn forget(&self, id: &str) -> Result<()> {
        self.mark_migration_attempted(id)?;
        let path = self.path(id, "token")?;
        ensure!(!path.is_symlink(), "Local session file cannot be a symlink");
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        std::fs::File::open(&self.root)?.sync_all()?;
        Ok(())
    }
    fn atomic_write(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        use std::io::Write;
        ensure!(
            !self.root.is_symlink() && !path.is_symlink(),
            "Local session path cannot be a symlink"
        );
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let tmp = path.with_extension(format!("{}-{nonce}.tmp", std::process::id()));
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let result = (|| -> Result<()> {
            let mut file = options.open(&tmp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            ensure!(!path.is_symlink(), "Local session path cannot be a symlink");
            std::fs::rename(&tmp, path)?;
            std::fs::File::open(&self.root)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(tmp);
        }
        result
    }
}

#[cfg(target_os = "macos")]
fn keychain_load(steamid: &str) -> Result<SecretString> {
    let bytes = Zeroizing::new(
        security_framework::passwords::generic_password(
            security_framework::passwords::PasswordOptions::new_generic_password(
                KEYCHAIN_SERVICE,
                steamid,
            ),
        )
        .context("SkinOs session unavailable in Keychain; sign in again")?,
    );
    let token = std::str::from_utf8(&bytes).context("Invalid session encoding")?;
    Ok(SecretString(Zeroizing::new(token.to_owned())))
}
#[cfg(target_os = "macos")]
fn keychain_save(steamid: &str, token: &str) -> Result<()> {
    security_framework::passwords::set_generic_password(KEYCHAIN_SERVICE, steamid, token.as_bytes())
        .context("Cannot save SkinOs session in Keychain")
}
#[cfg(target_os = "macos")]
fn keychain_delete(steamid: &str) -> Result<()> {
    match security_framework::passwords::delete_generic_password(KEYCHAIN_SERVICE, steamid) {
        Ok(()) => Ok(()),
        Err(error) if error.code() == -25300 => Ok(()),
        Err(error) => Err(error).context("Cannot remove SkinOs session from Keychain"),
    }
}
#[cfg(not(target_os = "macos"))]
fn keychain_load(_: &str) -> Result<SecretString> {
    bail!("Keychain sessions require macOS")
}
#[cfg(not(target_os = "macos"))]
fn keychain_save(_: &str, _: &str) -> Result<()> {
    bail!("Keychain sessions require macOS")
}
#[cfg(not(target_os = "macos"))]
fn keychain_delete(_: &str) -> Result<()> {
    bail!("Keychain sessions require macOS")
}

pub fn cache_library(steamid: &str, games: &[Game]) -> Result<()> {
    validate_id(steamid)?;
    atomic_json(
        &data_dir()?.join("library").join(format!("{steamid}.json")),
        games,
    )
}

pub fn load_library(steamid: &str) -> Result<Option<Vec<Game>>> {
    validate_id(steamid)?;
    let path = data_dir()?.join("library").join(format!("{steamid}.json"));
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&std::fs::read(path)?)?))
}

fn atomic_json(path: &Path, value: &(impl Serialize + ?Sized)) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().context("Missing data directory")?;
    std::fs::create_dir_all(parent)?;
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(&serde_json::to_vec(value)?)?;
    file.sync_all()?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const TEST_ID: &str = "76561198000000000";
    fn sample() -> SecretString {
        SecretString(Zeroizing::new("test-only-refresh-token".into()))
    }
    #[test]
    fn credential_defaults_are_development_only_and_explicitly_overridable() {
        assert!(selected_store(None, true).unwrap() == CredentialStore::File);
        assert!(selected_store(None, false).unwrap() == CredentialStore::Keychain);
        assert!(selected_store(Some("file"), false).unwrap() == CredentialStore::File);
        assert!(selected_store(Some("keychain"), true).unwrap() == CredentialStore::Keychain);
        assert!(selected_store(Some("typo"), true).is_err());
    }
    #[test]
    fn file_store_roundtrip_has_private_permissions_and_no_backend_reads() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileCredentials::open(dir.path()).unwrap();
        store.save(TEST_ID, sample().expose_secret()).unwrap();
        let result = store
            .load_with_migration(TEST_ID, || panic!("Keychain must not be consulted"))
            .unwrap();
        assert_eq!(result.expose_secret(), sample().expose_secret());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&store.root).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(store.path(TEST_ID, "token").unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        store.save(TEST_ID, "replacement-test-token").unwrap();
        assert_eq!(
            store.read(TEST_ID).unwrap().unwrap().expose_secret(),
            "replacement-test-token"
        );
    }
    #[test]
    fn lazy_migration_reads_backend_once_and_forget_prevents_resurrection() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileCredentials::open(dir.path()).unwrap();
        let reads = std::cell::Cell::new(0);
        store
            .load_with_migration(TEST_ID, || {
                reads.set(reads.get() + 1);
                Ok(sample())
            })
            .unwrap();
        store
            .load_with_migration(TEST_ID, || {
                reads.set(reads.get() + 1);
                Ok(sample())
            })
            .unwrap();
        assert_eq!(reads.get(), 1);
        store.forget(TEST_ID).unwrap();
        assert!(
            store
                .load_with_migration(TEST_ID, || panic!("Forgotten session cannot migrate again"))
                .is_err()
        );
        store.save(TEST_ID, "new-login-test-token").unwrap();
        assert!(store.read(TEST_ID).unwrap().is_some());
    }
    #[test]
    fn cancelled_migration_does_not_repeat_keychain_prompts() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileCredentials::open(dir.path()).unwrap();
        assert!(
            store
                .load_with_migration(TEST_ID, || bail!("Test user cancelled"))
                .is_err()
        );
        assert!(
            store
                .load_with_migration(TEST_ID, || panic!("No repeated Keychain reads"))
                .is_err()
        );
    }
    #[test]
    #[cfg(unix)]
    fn secret_paths_reject_symlinks_without_modifying_targets() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("untouched");
        std::fs::write(&target, b"unchanged").unwrap();
        let store = FileCredentials::open(dir.path()).unwrap();
        std::os::unix::fs::symlink(&target, store.path(TEST_ID, "token").unwrap()).unwrap();
        assert!(store.save(TEST_ID, "test-only-token").is_err());
        assert!(store.read(TEST_ID).is_err());
        assert_eq!(std::fs::read(target).unwrap(), b"unchanged");
        let linked = dir.path().join("linked");
        std::os::unix::fs::symlink(outside.path(), &linked).unwrap();
        assert!(FileCredentials::open(&linked).is_err());
        assert!(store.save("../outside", "test-token").is_err());
    }
    #[test]
    fn auto_login_selects_last_successful_account_and_honors_persisted_logout() {
        let dir = tempfile::tempdir().unwrap();
        let accounts = vec![
            Account {
                steamid: "76561198000000001".into(),
                account_name: "alphabetical-first".into(),
                last_used: 1,
            },
            Account {
                steamid: "76561198000000002".into(),
                account_name: "last-used".into(),
                last_used: 99,
            },
        ];
        atomic_json(&dir.path().join("accounts.json"), &accounts).unwrap();
        assert_eq!(
            auto_login_account_at(dir.path()).unwrap().unwrap().steamid,
            "76561198000000002"
        );
        atomic_json(&dir.path().join("auto-login-disabled"), &true).unwrap();
        assert!(auto_login_account_at(dir.path()).unwrap().is_none());
        std::fs::remove_file(dir.path().join("auto-login-disabled")).unwrap();
        assert!(auto_login_account_at(dir.path()).unwrap().is_some());
    }
    #[test]
    fn account_ids_cannot_be_paths() {
        assert!(validate_id("../token").is_err());
        assert!(validate_id("76561198000000000").is_ok());
    }
}
