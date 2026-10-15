//! Steam depots do not always set Executable on CFBundleExecutable entries.
//! Restore the declared bundle executable's mode without changing file content,
//! signatures, quarantine or any OS security setting.
use anyhow::{Context, Result, ensure};
use std::path::{Component, Path};

pub(crate) fn restore_bundle_executables(root: &Path) -> Result<()> {
    fn walk(root: &Path, dir: &Path) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if !kind.is_dir() {
                continue;
            }
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "app") {
                let plist_path = path.join("Contents/Info.plist");
                ensure!(
                    !plist_path.is_symlink(),
                    "Bundle Info.plist cannot be a symlink"
                );
                let plist = plist::Value::from_file(&plist_path)
                    .context("Cannot read downloaded macOS bundle metadata")?;
                let name = plist
                    .as_dictionary()
                    .and_then(|d| d.get("CFBundleExecutable"))
                    .and_then(plist::Value::as_string)
                    .context("Bundle has no CFBundleExecutable")?;
                ensure!(
                    Path::new(name).components().count() == 1
                        && matches!(
                            Path::new(name).components().next(),
                            Some(Component::Normal(_))
                        ),
                    "Invalid bundle executable name"
                );
                let executable = path.join("Contents/MacOS").join(name);
                ensure!(
                    executable.canonicalize()?.starts_with(root.canonicalize()?),
                    "Bundle executable escapes installation"
                );
                ensure!(executable.is_file(), "Bundle executable is missing");
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mut permissions = executable.metadata()?.permissions();
                    permissions.set_mode(permissions.mode() | 0o111);
                    std::fs::set_permissions(executable, permissions)?;
                }
            }
            walk(root, &path)?;
        }
        Ok(())
    }
    walk(root, root)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restores_declared_executable_without_modifying_its_bytes() {
        let root = tempfile::tempdir().unwrap();
        let bundle = root.path().join("Sample.app/Contents");
        std::fs::create_dir_all(bundle.join("MacOS")).unwrap();
        let mut info = plist::Dictionary::new();
        info.insert(
            "CFBundleExecutable".into(),
            plist::Value::String("Sample".into()),
        );
        plist::Value::Dictionary(info)
            .to_file_xml(bundle.join("Info.plist"))
            .unwrap();
        let executable = bundle.join("MacOS/Sample");
        let content = b"unchanged-test-program";
        std::fs::write(&executable, content).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        restore_bundle_executables(root.path()).unwrap();
        assert_eq!(std::fs::read(&executable).unwrap(), content);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                executable.metadata().unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
    }
}
