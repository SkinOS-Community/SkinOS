use std::{
    fs::{self, File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::fs::{OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};
unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

pub struct Profile {
    pub root: PathBuf,
    pub data: PathBuf,
    pub library: PathBuf,
    lock: File,
}
impl Profile {
    pub fn open(state_root: &Path, steamid: &str) -> Result<Self, String> {
        if steamid.len() != 17
            || !steamid.bytes().all(|b| b.is_ascii_digit())
            || steamid.starts_with('0')
        {
            return Err("Invalid profile SteamID".into());
        }
        let accounts = state_root
            .parent()
            .ok_or("State root has no parent")?
            .join("accounts");
        fs::create_dir_all(&accounts).map_err(|e| e.to_string())?;
        let accounts = accounts.canonicalize().map_err(|e| e.to_string())?;
        let root = accounts.join(steamid).join("runtime");
        fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        if !root.starts_with(&accounts) {
            return Err("Profile escapes the owned accounts directory".into());
        }
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(root.join("runtime.lock"))
            .map_err(|e| e.to_string())?;
        if unsafe { flock(lock.as_raw_fd(), 2 | 4) } != 0 {
            return Err("This account already has an active game runtime".into());
        }
        let data = root.join("data");
        let library = root.join("library");
        fs::create_dir_all(&data).map_err(|e| e.to_string())?;
        fs::create_dir_all(&library).map_err(|e| e.to_string())?;
        Ok(Self {
            root,
            data,
            library,
            lock,
        })
    }
    pub fn lock_fd(&self) -> i32 {
        self.lock.as_raw_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profile_keeps_saves_and_rejects_concurrent_runtime() {
        let base = std::env::temp_dir().join(format!("skinos-profile-test-{}", std::process::id()));
        fs::create_dir_all(base.join("runtime")).unwrap();
        let first = Profile::open(&base.join("runtime"), "76561198000000001").unwrap();
        fs::write(first.data.join("save.fixture"), b"saved progress").unwrap();
        assert!(Profile::open(&base.join("runtime"), "76561198000000001").is_err());
        drop(first);
        // Parallel native-spawn tests can briefly inherit this CLOEXEC FD
        // between fork and exec. Allow only that bounded handoff window; a
        // persistent child leak must still fail instead of waiting indefinitely.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let restored = loop {
            match Profile::open(&base.join("runtime"), "76561198000000001") {
                Ok(profile) => break profile,
                Err(error) if error == "This account already has an active game runtime" => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "profile lock remained held after its owner closed: {error}"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => panic!("could not reopen profile: {error}"),
            }
        };
        assert_eq!(
            fs::read(restored.data.join("save.fixture")).unwrap(),
            b"saved progress"
        );
        assert!(Profile::open(&base.join("runtime"), "../other-account").is_err());
        drop(restored);
        fs::remove_dir_all(base).unwrap();
    }
}
