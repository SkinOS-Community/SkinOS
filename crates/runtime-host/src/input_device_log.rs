//! Correlate live SDL IDs with complete fresh Valve discovery records.
//! Raw HID paths and serial-number lines are never retained or returned.
#![allow(dead_code)] // Wired by the ordinary-session owner separately.

use crate::input_devices::Device;
use serde::Serialize;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::macos::fs::MetadataExt as _;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_BYTES: u64 = 512 * 1024;
const MAX_LINE: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Mapping {
    pub sdl_id: u32,
    pub valve_index: u32,
}

struct ObservedFile {
    identity: (u64, u64),
    length: u64,
}
pub struct Cursor {
    path: PathBuf,
    offset: u64,
    anchor: Vec<u8>,
    discard_partial: bool,
    captured_ns: u128,
    observed: RefCell<Option<ObservedFile>>,
}

impl Cursor {
    /// Call before loading Valve so devices already connected at startup are
    /// included. Earlier log contents can never authorize a controller index.
    pub fn capture(data_root: &Path) -> Result<Self, String> {
        let captured_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let path = data_root.join("logs/controller.txt");
        let file = open_log(&path)?;
        let (offset, anchor, observed) = match file {
            Some(mut file) => {
                let metadata = file.metadata().map_err(|e| e.to_string())?;
                let offset = metadata.len();
                let mut anchor = vec![0; offset.min(4096) as usize];
                file.seek(SeekFrom::Start(offset - anchor.len() as u64))
                    .map_err(|e| e.to_string())?;
                file.read_exact(&mut anchor).map_err(|e| e.to_string())?;
                (
                    offset,
                    anchor,
                    Some(ObservedFile {
                        identity: identity(&metadata),
                        length: offset,
                    }),
                )
            }
            None => (0, Vec::new(), None),
        };
        let discard_partial = anchor.last().is_some_and(|last| *last != b'\n');
        Ok(Self {
            path,
            offset,
            anchor,
            discard_partial,
            captured_ns,
            observed: RefCell::new(observed),
        })
    }

    /// `live` must be a new inventory snapshot, not an earlier cached result.
    /// Empty mappings mean that no supported, unambiguous SDL route is proven.
    pub fn mappings(&self, live: &[Device]) -> Result<Vec<Mapping>, String> {
        let Some(mut file) = open_log(&self.path)? else {
            return if self.observed.borrow().is_some() {
                Err("Controller log disappeared".into())
            } else {
                Ok(Vec::new())
            };
        };
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        {
            let mut observed = self.observed.borrow_mut();
            if let Some(previous) = observed.as_ref() {
                if previous.identity != identity(&metadata) || metadata.len() < previous.length {
                    return Err(
                        "Controller log was replaced or truncated; freshness is unknown".into(),
                    );
                }
            } else {
                // A pre-existing file moved into place retains its older birth
                // time. No fallback to mtime or timestamps inside log text.
                let birth = u128::try_from(metadata.st_birthtime())
                    .ok()
                    .and_then(|seconds| seconds.checked_mul(1_000_000_000))
                    .and_then(|ns| {
                        u128::try_from(metadata.st_birthtime_nsec())
                            .ok()
                            .and_then(|fraction| ns.checked_add(fraction))
                    });
                if birth.is_none_or(|birth| birth < self.captured_ns) {
                    return Err("New controller log does not prove post-capture creation".into());
                }
            }
            *observed = Some(ObservedFile {
                identity: identity(&metadata),
                length: metadata.len(),
            });
        }
        if metadata.len() < self.offset || metadata.len() - self.offset > MAX_BYTES {
            return Err("Fresh controller log exceeds the supported bound".into());
        }
        file.seek(SeekFrom::Start(self.offset - self.anchor.len() as u64))
            .map_err(|e| e.to_string())?;
        let mut anchor = vec![0; self.anchor.len()];
        file.read_exact(&mut anchor).map_err(|e| e.to_string())?;
        if anchor != self.anchor {
            return Err("Controller log history was rewritten".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("Fresh controller log exceeds the supported bound".into());
        }
        let after = fs::symlink_metadata(&self.path).map_err(|e| e.to_string())?;
        if !after.is_file()
            || identity(&after) != identity(&metadata)
            || after.len() < metadata.len()
        {
            return Err("Controller log changed identity while being read".into());
        }
        let bytes = if self.discard_partial {
            match bytes.iter().position(|byte| *byte == b'\n') {
                Some(end) => &bytes[end + 1..],
                None => &[],
            }
        } else {
            &bytes[..]
        };
        let text = std::str::from_utf8(bytes).map_err(|_| "Fresh controller log is not UTF-8")?;
        parse_mappings(text, live)
    }
}

fn identity(metadata: &Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}

fn open_log(path: &Path) -> Result<Option<File>, String> {
    let parent = path.parent().ok_or("Invalid controller log path")?;
    match fs::symlink_metadata(parent) {
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            return Err("Controller log directory is not an owned regular directory".into());
        }
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.to_string()),
        _ => {}
    }
    let file = match OpenOptions::new().read(true).custom_flags(0x100).open(path) {
        // Darwin O_NOFOLLOW
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    let path_metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file()
        || !path_metadata.is_file()
        || identity(&metadata) != identity(&path_metadata)
    {
        return Err("Controller log must be a stable regular file".into());
    }
    Ok(Some(file))
}

#[derive(Default)]
struct Block {
    sdl_id: Option<u32>,
    path_seen: bool,
    name: Option<String>,
    lines: usize,
}

fn message(line: &str) -> &str {
    let line = line.trim();
    if line.starts_with('[')
        && let Some((stamp, rest)) = line.split_once(']')
        && stamp.len() == 20
        && stamp.as_bytes()[1..]
            .iter()
            .enumerate()
            .all(|(index, byte)| match index {
                4 | 7 => *byte == b'-',
                10 => *byte == b' ',
                13 | 16 => *byte == b':',
                _ => byte.is_ascii_digit(),
            })
    {
        return rest.trim();
    }
    line
}

fn parse_mappings(text: &str, live: &[Device]) -> Result<Vec<Mapping>, String> {
    if text.len() as u64 > MAX_BYTES || live.len() > 64 {
        return Err("Controller correlation exceeds supported bounds".into());
    }
    let mut inventory = BTreeMap::new();
    for device in live {
        if device.sdl_id == 0 || inventory.insert(device.sdl_id, device).is_some() {
            return Err("Live SDL inventory has ambiguous IDs".into());
        }
    }
    let mut pending: Option<Block> = None;
    let mut ids = BTreeSet::new();
    let mut indices = BTreeSet::new();
    let mut mappings = BTreeMap::new();
    // The last unterminated line may still be in-flight; never interpret it.
    for raw in text
        .split_inclusive('\n')
        .filter(|line| line.ends_with('\n'))
    {
        if raw.len() > MAX_LINE {
            return Err("Controller log line exceeds supported bound".into());
        }
        let line = message(raw);
        if line.contains("Controller device closed") || line.contains("Exiting workitem thread") {
            // These messages have no stable SDL identity. All previous routes
            // become unproven until new complete discovery/open records arrive.
            pending = None;
            ids.clear();
            indices.clear();
            mappings.clear();
            continue;
        }
        if line == "Local Device Found" {
            pending = Some(Block::default());
            continue;
        }
        if let Some(block) = &mut pending {
            block.lines += 1;
            if block.lines > 32 {
                pending = None;
                continue;
            }
            if let Some(path) = line.strip_prefix("path:") {
                if block.path_seen {
                    return Err("Ambiguous controller discovery path".into());
                }
                block.path_seen = true;
                if let Some(id) = path.trim().strip_prefix("sdl://") {
                    block.sdl_id = Some(
                        decimal(id)
                            .filter(|id| *id != 0)
                            .ok_or("Invalid SDL discovery identity")?,
                    );
                }
            }
            if let Some(name) = line.strip_prefix("Product:") {
                if block.name.is_some() {
                    return Err("Ambiguous controller discovery name".into());
                }
                block.name = Some(name.trim().into());
            }
        }
        if let Some(value) = line.strip_prefix("!! Steam controller device opened for index ") {
            let index = value
                .strip_suffix('.')
                .and_then(decimal)
                .filter(|index| *index < 16)
                .ok_or("Invalid Valve controller index")?;
            let Some(block) = pending.take() else {
                return Err(
                    "Valve opened a device without a complete fresh discovery block".into(),
                );
            };
            if !block.path_seen || block.name.is_none() || !indices.insert(index) {
                return Err("Ambiguous or incomplete Valve controller discovery".into());
            }
            if let Some(id) = block.sdl_id {
                if !ids.insert(id) {
                    return Err("SDL device has multiple fresh Valve index records".into());
                }
                if let Some(device) = inventory.get(&id)
                    && device.is_gamepad
                    && block.name.as_deref() == Some(device.name.as_str())
                {
                    mappings.insert(
                        id,
                        Mapping {
                            sdl_id: id,
                            valve_index: index,
                        },
                    );
                }
            }
        }
    }
    Ok(mappings.into_values().collect())
}

fn decimal(value: &str) -> Option<u32> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    fn device(id: u32, name: &str) -> Device {
        Device {
            sdl_id: id,
            name: name.into(),
            vendor_id: 0,
            product_id: 0,
            gamepad_type: 1,
            is_gamepad: true,
            is_virtual: false,
        }
    }
    fn block(id: u32, name: &str, index: u32) -> String {
        format!(
            "[2026-10-13 15:35:36] Local Device Found\n  path: sdl://{id}\n[2026-10-13 15:35:36]   Product:      {name}\n[2026-10-13 15:35:36] !! Steam controller device opened for index {index}.\n"
        )
    }
    struct Folder(PathBuf);
    impl Folder {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "skinos-device-log-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(path.join("logs")).unwrap();
            Self(path)
        }
        fn log(&self) -> PathBuf {
            self.0.join("logs/controller.txt")
        }
        fn append(&self, text: &str) {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.log())
                .unwrap()
                .write_all(text.as_bytes())
                .unwrap();
        }
    }
    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn multiple_devices_map_by_exact_live_id_and_name_not_order_or_index_zero() {
        let rows = block(14, "One", 5) + &block(2, "Two", 1);
        assert_eq!(
            parse_mappings(&rows, &[device(2, "Two"), device(14, "One")]).unwrap(),
            vec![
                Mapping {
                    sdl_id: 2,
                    valve_index: 1
                },
                Mapping {
                    sdl_id: 14,
                    valve_index: 5
                }
            ]
        );
        assert!(
            parse_mappings(&rows, &[device(4, "One"), device(2, "Wrong")])
                .unwrap()
                .is_empty()
        );
        assert!(
            parse_mappings(
                &block(14, "One", 5).replace("sdl://14", "IOService:/unsupported"),
                &[device(14, "One")]
            )
            .unwrap()
            .is_empty()
        );
    }
    #[test]
    fn incomplete_and_ambiguous_records_cannot_supply_routes() {
        let row = block(1, "One", 3);
        assert!(
            parse_mappings(row.trim_end_matches('\n'), &[device(1, "One")])
                .unwrap()
                .is_empty()
        );
        for text in [
            row.clone() + &row,
            row.clone() + &block(2, "Two", 3),
            "!! Steam controller device opened for index 3.\n".into(),
        ] {
            assert!(parse_mappings(&text, &[device(1, "One"), device(2, "Two")]).is_err());
        }
        assert!(
            parse_mappings(
                &(row + "Controller device closed after failure\n"),
                &[device(1, "One")]
            )
            .unwrap()
            .is_empty()
        );
    }
    #[test]
    fn old_records_are_ignored_and_devices_already_connected_at_start_are_included() {
        let folder = Folder::new();
        folder.append(&block(1, "Old", 0));
        let cursor = Cursor::capture(&folder.0).unwrap();
        assert!(cursor.mappings(&[device(1, "Old")]).unwrap().is_empty());
        folder.append(&block(9, "Connected before startup", 7));
        assert_eq!(
            cursor
                .mappings(&[device(9, "Connected before startup")])
                .unwrap(),
            vec![Mapping {
                sdl_id: 9,
                valve_index: 7
            }]
        );
    }
    #[test]
    fn first_log_created_after_capture_is_supported_but_old_file_moved_in_is_not() {
        let folder = Folder::new();
        let old = folder.0.join("old.log");
        fs::write(&old, block(1, "One", 2)).unwrap();
        let cursor = Cursor::capture(&folder.0).unwrap();
        assert!(cursor.mappings(&[device(1, "One")]).unwrap().is_empty());
        folder.append(&block(1, "One", 2));
        assert_eq!(cursor.mappings(&[device(1, "One")]).unwrap().len(), 1);
        let folder2 = Folder::new();
        let cursor2 = Cursor::capture(&folder2.0).unwrap();
        fs::rename(old, folder2.log()).unwrap();
        assert!(cursor2.mappings(&[device(1, "One")]).is_err());
    }
    #[test]
    fn replaced_truncated_or_rewritten_history_fails_closed() {
        for kind in ["replace", "truncate", "rewrite"] {
            let folder = Folder::new();
            folder.append("old history\n");
            let cursor = Cursor::capture(&folder.0).unwrap();
            match kind {
                "replace" => {
                    fs::rename(folder.log(), folder.0.join("old")).unwrap();
                    folder.append(&block(1, "One", 2));
                }
                "truncate" => fs::write(folder.log(), b"").unwrap(),
                _ => fs::write(
                    folder.log(),
                    "new history\n".to_string() + &block(1, "One", 2),
                )
                .unwrap(),
            }
            assert!(cursor.mappings(&[device(1, "One")]).is_err(), "{kind}");
        }
    }
    #[test]
    fn partial_capture_and_size_bounds_do_not_admit_stale_discovery() {
        let folder = Folder::new();
        folder.append("Local Device ");
        let cursor = Cursor::capture(&folder.0).unwrap();
        folder.append(
            "Found\npath: sdl://1\nProduct: One\n!! Steam controller device opened for index 0.\n",
        );
        assert!(cursor.mappings(&[device(1, "One")]).is_err());
        assert!(parse_mappings(&"x".repeat(MAX_BYTES as usize + 1), &[]).is_err());
        assert!(parse_mappings(&("x".repeat(MAX_LINE + 1) + "\n"), &[]).is_err());
    }
}
