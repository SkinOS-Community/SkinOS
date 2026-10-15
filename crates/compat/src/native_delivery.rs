//! Restore compatibility bytes from visible signed Mach-O carriers.
//! Every result must match the caller's pre-existing complete SHA-256 pin.
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path};

const MAX_MANIFEST: u64 = 24 * 1024 * 1024;
const MAX_CARRIER: u64 = 160 * 1024 * 1024;
const MAX_ORIGINAL: u64 = 128 * 1024 * 1024;
const MAX_LITERALS: usize = 16 * 1024 * 1024;
type Result<T> = std::result::Result<T, String>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delivery {
    schema_version: u32,
    pub runtime: BTreeMap<String, Record>,
    pub bridge: BTreeMap<String, Record>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    carrier: String,
    carrier_sha256: String,
    sha256: String,
    size: u64,
    chunks: Vec<Chunk>,
}

#[derive(Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum Chunk {
    Copy { offset: u64, length: u64 },
    Literal { data: String },
}

impl Delivery {
    pub fn load(resources: &Path) -> Result<Option<Self>> {
        match fs::symlink_metadata(resources.join("native-delivery.json")) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
            Ok(_) => {}
        }
        let bytes = read_regular(resources, "native-delivery.json", MAX_MANIFEST)?;
        let delivery: Self = serde_json::from_slice(&bytes)
            .map_err(|e| format!("Malformed native delivery manifest: {e}"))?;
        if delivery.schema_version != 1 || delivery.runtime.len() > 32 || delivery.bridge.len() > 32
        {
            return Err("Unsupported native delivery manifest".into());
        }
        for (name, record) in delivery.runtime.iter().chain(&delivery.bridge) {
            safe_relative(name)?;
            record.validate()?;
        }
        Ok(Some(delivery))
    }
}

impl Record {
    fn validate(&self) -> Result<()> {
        safe_relative(&self.carrier)?;
        let parts: Vec<_> = Path::new(&self.carrier).components().collect();
        if parts.len() != 2
            || parts[0].as_os_str() != "NativeRuntimeCode"
            || self.size == 0
            || self.size > MAX_ORIGINAL
            || self.chunks.is_empty()
            || self.chunks.len() > 2048
            || !valid_hash(&self.carrier_sha256)
            || !valid_hash(&self.sha256)
        {
            return Err("Invalid native carrier record".into());
        }
        Ok(())
    }

    pub fn restore(
        &self,
        resources: &Path,
        expected_sha: &str,
        expected_size: Option<u64>,
    ) -> Result<Vec<u8>> {
        self.validate()?;
        if self.sha256 != expected_sha || expected_size.is_some_and(|size| self.size != size) {
            return Err("Native delivery record differs from the pinned original".into());
        }
        let carrier = read_regular(resources, &self.carrier, MAX_CARRIER)?;
        if format!("{:x}", Sha256::digest(&carrier)) != self.carrier_sha256 {
            return Err("Signed native carrier checksum mismatch".into());
        }
        let size = usize::try_from(self.size).map_err(|_| "Native output size overflow")?;
        let mut result = Vec::with_capacity(size);
        let mut literal_bytes = 0_usize;
        for chunk in &self.chunks {
            match chunk {
                Chunk::Copy { offset, length } => {
                    let end = offset
                        .checked_add(*length)
                        .ok_or("Native copy range overflow")?;
                    if *length == 0
                        || end > carrier.len() as u64
                        || *length > (size - result.len()) as u64
                    {
                        return Err("Native copy range exceeds carrier or output bounds".into());
                    }
                    result.extend_from_slice(&carrier[*offset as usize..end as usize]);
                }
                Chunk::Literal { data } => {
                    if data.len() > (MAX_LITERALS - literal_bytes).saturating_add(2) / 3 * 4 + 4 {
                        return Err("Native signing metadata exceeds size limit".into());
                    }
                    let bytes = decode_base64(data)?;
                    literal_bytes = literal_bytes
                        .checked_add(bytes.len())
                        .ok_or("Native literal size overflow")?;
                    if literal_bytes > MAX_LITERALS
                        || bytes.is_empty()
                        || bytes.len() > size - result.len()
                    {
                        return Err("Native signing metadata exceeds output bounds".into());
                    }
                    result.extend_from_slice(&bytes);
                }
            }
        }
        if result.len() != size || format!("{:x}", Sha256::digest(&result)) != expected_sha {
            return Err("Restored native component does not match its original SHA-256 pin".into());
        }
        Ok(result)
    }
}

pub fn restore(
    resources: &Path,
    group: &str,
    name: &str,
    expected_sha: &str,
    expected_size: Option<u64>,
) -> Result<Option<Vec<u8>>> {
    let Some(delivery) = Delivery::load(resources)? else {
        return Ok(None);
    };
    let records = match group {
        "runtime" => &delivery.runtime,
        "bridge" => &delivery.bridge,
        _ => return Err("Unknown native delivery group".into()),
    };
    records
        .get(name)
        .map(|record| record.restore(resources, expected_sha, expected_size))
        .transpose()
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn safe_relative(value: &str) -> Result<()> {
    if value.is_empty()
        || value.contains(['\\', '\0'])
        || value
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || Path::new(value)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("Unsafe native delivery relative path".into());
    }
    Ok(())
}

fn read_regular(root: &Path, name: &str, limit: u64) -> Result<Vec<u8>> {
    safe_relative(name)?;
    let mut path = root.to_path_buf();
    if fs::symlink_metadata(root)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("Native resources root must not be a symlink".into());
    }
    for part in Path::new(name).components() {
        path.push(part);
        if fs::symlink_metadata(&path)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink()
        {
            return Err("Native delivery symlink is not allowed".into());
        }
    }
    let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err("Native delivery file exceeds size/type limit".into());
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("Native delivery file grew past size limit".into());
    }
    Ok(bytes)
}

fn decode_base64(text: &str) -> Result<Vec<u8>> {
    fn digit(byte: u8) -> Result<u8> {
        match byte {
            b'A'..=b'Z' => Ok(byte - b'A'),
            b'a'..=b'z' => Ok(byte - b'a' + 26),
            b'0'..=b'9' => Ok(byte - b'0' + 52),
            b'+' => Ok(62),
            b'/' => Ok(63),
            _ => Err("Invalid native metadata base64".into()),
        }
    }
    let input = text.as_bytes();
    if input.len() % 4 != 0 {
        return Err("Invalid native metadata base64 length".into());
    }
    let mut output = Vec::with_capacity(input.len() / 4 * 3);
    for (index, part) in input.chunks_exact(4).enumerate() {
        let first = digit(part[0])?;
        let second = digit(part[1])?;
        output.push((first << 2) | (second >> 4));
        if part[2] == b'=' {
            if part[3] != b'=' || second & 15 != 0 || (index + 1) * 4 != input.len() {
                return Err("Invalid native metadata base64 padding".into());
            }
        } else {
            let third = digit(part[2])?;
            output.push((second << 4) | (third >> 2));
            if part[3] == b'=' {
                if third & 3 != 0 || (index + 1) * 4 != input.len() {
                    return Err("Invalid native metadata base64 padding".into());
                }
            } else {
                output.push((third << 6) | digit(part[3])?);
            }
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn base64_and_paths_are_strict() {
        assert_eq!(decode_base64("YWJj").unwrap(), b"abc");
        assert_eq!(decode_base64("YQ==").unwrap(), b"a");
        assert_eq!(decode_base64("YWI=").unwrap(), b"ab");
        for bad in ["YQ=", "YR==", "YWJ=", "YQ==YQ==", "YW Jj", "!!!!"] {
            assert!(decode_base64(bad).is_err());
        }
        for bad in ["../carrier", "/carrier", "a//b", "a/./b", "a\\b"] {
            assert!(safe_relative(bad).is_err());
        }
    }
    #[test]
    fn records_reject_ranges_literals_wrong_hashes_and_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("NativeRuntimeCode")).unwrap();
        let path = dir.path().join("NativeRuntimeCode/carrier.tool");
        fs::write(&path, b"payload").unwrap();
        let digest = format!("{:x}", Sha256::digest(b"payload"));
        let record = |chunks| Record {
            carrier: "NativeRuntimeCode/carrier.tool".into(),
            carrier_sha256: digest.clone(),
            sha256: digest.clone(),
            size: 7,
            chunks,
        };
        assert_eq!(
            record(vec![Chunk::Copy {
                offset: 0,
                length: 7
            }])
            .restore(dir.path(), &digest, Some(7))
            .unwrap(),
            b"payload"
        );
        assert!(
            record(vec![Chunk::Copy {
                offset: u64::MAX,
                length: 2
            }])
            .restore(dir.path(), &digest, Some(7))
            .is_err()
        );
        assert!(
            record(vec![Chunk::Copy {
                offset: 6,
                length: 2
            }])
            .restore(dir.path(), &digest, Some(7))
            .is_err()
        );
        assert!(
            record(vec![Chunk::Literal {
                data: "cGF5bG9hZAA=".into()
            }])
            .restore(dir.path(), &digest, Some(7))
            .is_err()
        );
        assert!(
            record(vec![Chunk::Copy {
                offset: 0,
                length: 7
            }])
            .restore(dir.path(), &"0".repeat(64), Some(7))
            .is_err()
        );
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("/dev/null", &path).unwrap();
        assert!(
            record(vec![Chunk::Copy {
                offset: 0,
                length: 7
            }])
            .restore(dir.path(), &digest, Some(7))
            .is_err()
        );
    }
}
