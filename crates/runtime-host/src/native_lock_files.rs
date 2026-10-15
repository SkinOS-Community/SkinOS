//! Source 2 uses a numeric single-instance lock in the POSIX temporary root.
//! Permit that file family without allowing arbitrary temporary files or
//! changing the engine's kernel locking semantics. No existing lock is removed.
pub const SOURCE2_LOCK_FILES: &str = r#"(regex #"^(/private)?/tmp/source_engine_[0-9]+[.]lock$")"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn real_sandbox_allows_only_engine_locks_and_preserves_exclusion() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("skinos-lock-policy-test-{nonce}"));
        let state = root.join("state");
        fs::create_dir_all(&state).unwrap();
        let state = state.canonicalize().unwrap();
        let canary = root.join("private-canary");
        fs::write(&canary, b"owned test data").unwrap();
        let lock = std::path::PathBuf::from(format!("/private/tmp/source_engine_{nonce}.lock"));
        let unrelated =
            std::path::PathBuf::from(format!("/private/tmp/source_engine_{nonce}Xlock"));
        assert!(!lock.exists() && !unrelated.exists());
        let source = state.join("fixture.c");
        let executable = state.join("fixture");
        fs::write(
            &source,
            r#"
#include <errno.h>
#include <fcntl.h>
#include <stdlib.h>
#include <sys/file.h>
#include <unistd.h>
int main(int argc, char **argv) {
    if (argc != 5) return 10;
    int expected = atoi(argv[4]);
    int fd = open(argv[1], O_RDWR | O_CREAT | O_EXCL, 0600);
    if (!expected) {
        if (fd >= 0) { close(fd); return 11; }
        return (errno == EPERM || errno == EACCES) ? 0 : 12;
    }
    if (fd < 0 || flock(fd, LOCK_EX | LOCK_NB)) return 13;
    int second = open(argv[1], O_RDWR);
    if (second < 0) return 14;
    if (flock(second, LOCK_EX | LOCK_NB) == 0 || errno != EWOULDBLOCK) return 15;
    close(second);
    int other = open(argv[2], O_RDWR | O_CREAT | O_EXCL, 0600);
    if (other >= 0) { close(other); return 16; }
    if (errno != EPERM && errno != EACCES) return 17;
    other = open(argv[3], O_RDONLY);
    if (other >= 0) { close(other); return 18; }
    if (errno != EPERM && errno != EACCES) return 19;
    if (flock(fd, LOCK_UN)) return 20;
    close(fd);
    return 0;
}
"#,
        )
        .unwrap();
        assert!(
            Command::new("/usr/bin/clang")
                .args(["-Wall", "-Wextra", "-Werror"])
                .arg(&source)
                .arg("-o")
                .arg(&executable)
                .status()
                .unwrap()
                .success()
        );
        for allowed in [false, true] {
            let rule = if allowed { SOURCE2_LOCK_FILES } else { "" };
            let profile = crate::PROFILE
                .replace("; SKINOS_EXTRA_READ", rule)
                .replace("; SKINOS_EXTRA_WRITE", rule)
                .replace("; SKINOS_EXTRA_EXEC", "");
            let file = state.join("profile.sb");
            fs::write(&file, profile).unwrap();
            let status = Command::new("/usr/bin/sandbox-exec")
                .arg("-D")
                .arg(format!("STATE_ROOT={}", state.display()))
                .arg("-D")
                .arg(format!("WORKER={}", executable.display()))
                .arg("-f")
                .arg(file)
                .arg(&executable)
                .arg(&lock)
                .arg(&unrelated)
                .arg(&canary)
                .arg(if allowed { "1" } else { "0" })
                .status()
                .unwrap();
            assert!(status.success(), "lock policy fixture failed: {status}");
        }
        assert!(!unrelated.exists());
        // Only this test's freshly created, unique fixture is removed. The
        // application never removes a game's real lock file.
        fs::remove_file(lock).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
