use std::fs::{self, File};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "skinos-runtime-probe-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn cli_inspects_without_path_or_an_available_developer_directory() {
    let temp = TempDirectory::new();
    let path = temp.0.join("runtime with spaces.dylib");
    let bytes: Vec<u8> = [0xfeedfacfu32, 0x0100_000c, 0, 6, 0, 0, 0, 0]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    fs::write(&path, bytes).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_skinos-runtime-probe"))
        .args(["inspect", "--runtime"])
        .arg(&path)
        .arg("--json")
        .env("PATH", "")
        .env("DEVELOPER_DIR", temp.0.join("no-xcode-here"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["sha256"],
        "995e430ee5f995404e3e36bbeb88bd97161e0cc898d6f954ce25950780d9cc5f"
    );
    assert_eq!(report["file_size_bytes"], 32);
    assert_eq!(report["runtime_initialized"], false);
    assert_eq!(report["architectures"][0]["name"], "arm64");
    assert_eq!(report["architectures"][0]["exported_symbol_count"], 0);
    assert_eq!(
        report["architectures"][0]["all_target_exports_present"],
        false
    );
}

#[test]
fn rejects_nonfiles_nonmacho_and_oversized_files() {
    let temp = TempDirectory::new();
    assert!(
        skinos_runtime_probe::inspect(&temp.0)
            .unwrap_err()
            .contains("regular file")
    );
    let path = temp.0.join("invalid.dylib");
    fs::write(&path, b"not a Mach-O file").unwrap();
    assert!(
        skinos_runtime_probe::inspect(&path)
            .unwrap_err()
            .contains("Mach-O")
    );
    let oversized = temp.0.join("oversized.dylib");
    File::create(&oversized)
        .unwrap()
        .set_len(512 * 1024 * 1024 + 1)
        .unwrap();
    assert!(
        skinos_runtime_probe::inspect(&oversized)
            .unwrap_err()
            .contains("512 MiB")
    );
}
