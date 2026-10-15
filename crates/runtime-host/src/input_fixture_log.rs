//! Correlate the owned SDL fixture with a fresh Valve controller-log record.
//! Older runs and other devices cannot provide readiness for this run.
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub struct Cursor {
    path: PathBuf,
    offset: u64,
    inode: Option<u64>,
}

impl Cursor {
    pub fn capture(data_root: &Path) -> Result<Self, String> {
        let path = data_root.join("logs/controller.txt");
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                Some(metadata)
            }
            Ok(_) => return Err("Controller fixture log must be a regular owned file".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.to_string()),
        };
        Ok(Self {
            path,
            offset: metadata.as_ref().map_or(0, |m| m.len()),
            inode: metadata.map(|m| m.ino()),
        })
    }

    pub fn controller_index(&self, joystick_id: u32) -> Result<Option<u32>, String> {
        let metadata = fs::symlink_metadata(&self.path).map_err(|e| e.to_string())?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || self.inode.is_some_and(|inode| inode != metadata.ino())
            || metadata.len() < self.offset
        {
            return Err("Controller fixture log was replaced or truncated".into());
        }
        let mut file = fs::File::open(&self.path).map_err(|e| e.to_string())?;
        file.seek(SeekFrom::Start(self.offset))
            .map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        file.take(64 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 64 * 1024 {
            return Err("Controller fixture log exceeded diagnostic bound".into());
        }
        let text =
            std::str::from_utf8(&bytes).map_err(|_| "Controller fixture log is not UTF-8")?;
        Ok(parse_index(text, joystick_id))
    }
}

fn parse_index(text: &str, joystick_id: u32) -> Option<u32> {
    let expected_path = format!("path: sdl://{joystick_id}");
    let mut path_matches = false;
    let mut name_matches = false;
    let mut found = None;
    for line in text.lines() {
        if line.contains("Local Device Found") {
            path_matches = false;
            name_matches = false;
        }
        if line.trim_start().starts_with("path:") {
            path_matches = line.trim() == expected_path;
        }
        if let Some((_, name)) = line.split_once("Product:") {
            name_matches = name.trim() == "SkinOS neutral Steam Input fixture (software)";
        }
        if let Some((_, value)) = line.split_once("!! Steam controller device opened for index ") {
            if path_matches && name_matches {
                let index = value.trim().strip_suffix('.')?.parse::<u32>().ok()?;
                if index >= 16 || found.is_some() {
                    return None;
                }
                found = Some(index);
            }
        }
        if line.contains("Controller device closed") || line.contains("Exiting workitem thread") {
            // Conservative: any closure in this short fresh span invalidates it.
            found = None;
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    const RECORD: &str = "Local Device Found\n path: sdl://1\n Product: SkinOS neutral Steam Input fixture (software)\n !! Steam controller device opened for index 0.\n";
    #[test]
    fn requires_matching_actual_sdl_id_and_name() {
        assert_eq!(parse_index(RECORD, 1), Some(0));
        assert_eq!(parse_index(RECORD, 2), None);
        assert_eq!(
            parse_index(
                &RECORD.replace(
                    "SkinOS neutral Steam Input fixture (software)",
                    "Other gamepad"
                ),
                1
            ),
            None
        );
        assert_eq!(
            parse_index("!! Steam controller device opened for index 0.", 1),
            None
        );
    }
    #[test]
    fn closed_or_ambiguous_device_cannot_authorize_configuration() {
        assert_eq!(
            parse_index(
                &format!("{RECORD}Controller device closed after hid_read failure\n"),
                1
            ),
            None
        );
        assert_eq!(parse_index(&format!("{RECORD}{RECORD}"), 1), None);
    }
}
