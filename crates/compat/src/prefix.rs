//! Separate prefixes for incompatible CrossOver builds, with non-destructive data import.
use super::Result;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn select(original: &Path, owned: &Path, appid: u32, build: &str) -> Result<PathBuf> {
    if build.is_empty()
        || !build
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        return Err("Unsafe CrossOver build identifier".into());
    }
    let record = fs::read_to_string(original.join("notproton-build")).ok();
    if record.as_deref().and_then(|s| s.lines().next()) == Some(build)
        || (!original.join("pfx").exists() && record.is_none())
    {
        fs::create_dir_all(original).map_err(|e| e.to_string())?;
        return Ok(original.to_owned());
    }
    let destination = owned.join("prefixes").join(appid.to_string()).join(build);
    if destination.join("skinos-prefix-import.json").is_file() {
        return Ok(destination);
    }
    if destination.exists() {
        return Err(format!(
            "Incomplete compatibility prefix at {}; the previous data remains untouched",
            destination.display()
        ));
    }
    let parent = destination.parent().ok_or("Missing prefix parent")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let staging = parent.join(format!(".{build}-import-{now}"));
    fs::create_dir(&staging).map_err(|e| e.to_string())?;
    let source_pfx = original.join("pfx");
    let stage_pfx = staging.join("pfx");
    let final_pfx = destination.join("pfx");
    let mut count = 0_u64;
    let result = (|| {
        fs::create_dir_all(stage_pfx.join("drive_c")).map_err(|e| e.to_string())?;
        for name in ["user.reg", "userdef.reg"] {
            let source = source_pfx.join(name);
            if source.exists() {
                count += copy_tree(&source, &stage_pfx.join(name), &source_pfx, &final_pfx)?;
            }
        }
        let drive = source_pfx.join("drive_c");
        if drive.is_dir() {
            for entry in fs::read_dir(&drive).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                // The fresh Wine build supplies Windows components. Carry user
                // profiles and application data, including nonstandard folders,
                // without copying the previous architecture's DLLs/registry.
                let name = entry.file_name();
                let lower = name.to_string_lossy().to_ascii_lowercase();
                if ["windows", "program files", "program files (x86)"].contains(&lower.as_str()) {
                    continue;
                }
                count += copy_tree(
                    &entry.path(),
                    &stage_pfx.join("drive_c").join(name),
                    &source_pfx,
                    &final_pfx,
                )?;
            }
        }
        let metadata = serde_json::json!({
            "schema_version":1, "appid":appid, "build":build,
            "source_prefix":source_pfx, "imported_at_unix_ns":now.to_string(),
            "copied_regular_files":count, "independent_save_snapshot":true,
        });
        fs::write(
            staging.join("skinos-prefix-import.json"),
            serde_json::to_vec_pretty(&metadata).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        fs::rename(&staging, &destination).map_err(|e| e.to_string())?;
        Ok(destination.clone())
    })();
    if result.is_err() {
        // Only the unique staging directory created in this function is removed.
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn copy_tree(source: &Path, target: &Path, old_pfx: &Path, new_pfx: &Path) -> Result<u64> {
    let meta = fs::symlink_metadata(source).map_err(|e| e.to_string())?;
    if meta.file_type().is_symlink() {
        let link = fs::read_link(source).map_err(|e| e.to_string())?;
        let link = link
            .strip_prefix(old_pfx)
            .map(|p| new_pfx.join(p))
            .unwrap_or(link);
        std::os::unix::fs::symlink(link, target).map_err(|e| e.to_string())?;
        return Ok(0);
    }
    if meta.is_dir() {
        fs::create_dir(target).map_err(|e| e.to_string())?;
        let mut count = 0;
        for entry in fs::read_dir(source).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            count += copy_tree(
                &entry.path(),
                &target.join(entry.file_name()),
                old_pfx,
                new_pfx,
            )?;
        }
        return Ok(count);
    }
    if !meta.is_file() {
        return Err(format!(
            "Cannot safely copy special file {}",
            source.display()
        ));
    }
    fs::copy(source, target).map_err(|e| e.to_string())?;
    if Sha256::digest(fs::read(source).map_err(|e| e.to_string())?)
        != Sha256::digest(fs::read(target).map_err(|e| e.to_string())?)
    {
        return Err(format!(
            "Save data verification failed for {}",
            source.display()
        ));
    }
    Ok(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn build_switch_preserves_original_and_reuses_independent_saves() {
        let root = std::env::temp_dir().join(format!(
            "skinos-prefix-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let old = root.join("original");
        let user = old.join("pfx/drive_c/users/steamuser");
        fs::create_dir_all(&user).unwrap();
        fs::create_dir_all(old.join("pfx/drive_c/windows/system32")).unwrap();
        fs::write(old.join("notproton-build"), "27-fex\n").unwrap();
        fs::write(old.join("pfx/user.reg"), "HKCU game settings").unwrap();
        fs::write(user.join("save.dat"), "original progress").unwrap();
        fs::write(
            old.join("pfx/drive_c/windows/system32/ntdll.dll"),
            "wrong architecture",
        )
        .unwrap();
        std::os::unix::fs::symlink("steamuser", old.join("pfx/drive_c/users/crossover")).unwrap();
        assert_eq!(
            select(&old, &root.join("owned"), 10, "27-fex").unwrap(),
            old
        );
        let selected = select(&old, &root.join("owned"), 10, "26-rosetta").unwrap();
        assert_eq!(
            fs::read(selected.join("pfx/drive_c/users/steamuser/save.dat")).unwrap(),
            b"original progress"
        );
        assert_eq!(
            fs::read(selected.join("pfx/user.reg")).unwrap(),
            b"HKCU game settings"
        );
        assert!(!selected.join("pfx/drive_c/windows").exists());
        assert!(selected.join("pfx/drive_c/users/crossover").is_symlink());
        fs::write(
            selected.join("pfx/drive_c/users/steamuser/save.dat"),
            "new progress",
        )
        .unwrap();
        assert_eq!(
            select(&old, &root.join("owned"), 10, "26-rosetta").unwrap(),
            selected
        );
        assert_eq!(
            fs::read(selected.join("pfx/drive_c/users/steamuser/save.dat")).unwrap(),
            b"new progress"
        );
        assert_eq!(
            fs::read(user.join("save.dat")).unwrap(),
            b"original progress"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
