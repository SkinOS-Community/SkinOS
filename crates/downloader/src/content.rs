use crate::{
    InstallControl, checkpoint,
    network::{self, PreparedDepot},
    progress::{TransferProgress, VerificationProgress},
    proto::*,
    safe_output, safe_relative,
};
use aes::{
    Aes256,
    cipher::{
        BlockDecrypt, BlockDecryptMut, KeyInit, KeyIvInit, block_padding::Pkcs7,
        generic_array::GenericArray,
    },
};
use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use futures_util::{StreamExt, stream};
use prost::Message;
use sha1::{Digest, Sha1};
use std::{
    io::{Cursor, Read, Seek, Write},
    path::{Component, Path},
    sync::{Arc, Mutex},
};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
type SharedProgress = Arc<Mutex<TransferProgress>>;
const CHUNK_CONCURRENCY: usize = 4;

const MAX_CHUNK: usize = 16 * 1024 * 1024;
const MAX_MANIFEST: usize = 64 * 1024 * 1024;

fn decrypt(data: &[u8], key: &[u8; 32]) -> Result<Vec<u8>> {
    ensure!(
        data.len() >= 32 && data.len().is_multiple_of(16),
        "Invalid encrypted Steam content"
    );
    let mut iv = GenericArray::clone_from_slice(&data[..16]);
    Aes256::new(GenericArray::from_slice(key)).decrypt_block(&mut iv);
    let mut buffer = data[16..].to_vec();
    cbc::Decryptor::<Aes256>::new(GenericArray::from_slice(key), &iv)
        .decrypt_padded_mut::<Pkcs7>(&mut buffer)
        .map(|v| v.to_vec())
        .map_err(|_| anyhow::anyhow!("Content decryption failed"))
}

fn unpack_zip(data: &[u8], limit: usize) -> Result<Vec<u8>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(data))?;
    ensure!(
        archive.len() == 1,
        "Steam content archive must contain exactly one entry"
    );
    let file = archive.by_index(0)?;
    ensure!(
        file.size() <= limit as u64,
        "Uncompressed content exceeds limit"
    );
    let mut result = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut result)?;
    ensure!(result.len() <= limit, "Uncompressed content exceeds limit");
    Ok(result)
}

fn decoded_name(value: &str, key: &[u8; 32]) -> Result<String> {
    ensure!(value.is_ascii(), "Invalid encrypted filename encoding");
    let compact: String = value.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    let raw = base64::engine::general_purpose::STANDARD.decode(compact)?;
    let name = String::from_utf8(decrypt(&raw, key)?)?;
    Ok(name.trim_end_matches('\0').to_owned())
}

pub(crate) fn decode_manifest(
    data: &[u8],
    depot_id: u32,
    manifest_id: u64,
    key: &[u8; 32],
) -> Result<(ManifestMetadata, Vec<FileMapping>)> {
    let raw = if data.starts_with(b"PK") {
        unpack_zip(data, MAX_MANIFEST)?
    } else {
        data.to_vec()
    };
    ensure!(raw.len() <= MAX_MANIFEST, "Manifest exceeds size limit");
    let mut offset = 0usize;
    let mut payload = None;
    let mut metadata = None;
    let mut ended = false;
    while offset + 4 <= raw.len() {
        let magic = u32::from_le_bytes(raw[offset..offset + 4].try_into()?);
        offset += 4;
        if magic == 0x32c415ab {
            ended = true;
            break;
        }
        ensure!(offset + 4 <= raw.len(), "Truncated manifest section");
        let size = u32::from_le_bytes(raw[offset..offset + 4].try_into()?) as usize;
        offset += 4;
        ensure!(
            size <= raw.len().saturating_sub(offset),
            "Truncated manifest payload"
        );
        let section = &raw[offset..offset + size];
        match magic {
            0x71f617d0 => {
                ensure!(payload.is_none(), "Duplicate manifest payload");
                payload = Some(ManifestPayload::decode(section)?);
            }
            0x1f4812be => {
                ensure!(metadata.is_none(), "Duplicate manifest metadata");
                metadata = Some(ManifestMetadata::decode(section)?);
            }
            0x1b81b817 => {}
            _ => bail!("Unknown manifest section"),
        }
        offset += size;
    }
    ensure!(
        ended && offset == raw.len(),
        "Missing end marker or trailing manifest data"
    );
    let metadata = metadata.context("Missing manifest metadata")?;
    ensure!(
        metadata.depot_id == Some(depot_id) && metadata.manifest_id == Some(manifest_id),
        "Manifest identity mismatch"
    );
    let mut files = payload.context("Missing manifest payload")?.mappings;
    ensure!(!files.is_empty(), "Manifest contains no files");
    for file in &mut files {
        if metadata.filenames_encrypted == Some(true) {
            file.filename = Some(decoded_name(
                file.filename.as_deref().context("Missing filename")?,
                key,
            )?);
            if let Some(link) = file.linktarget.as_deref().filter(|s| !s.is_empty()) {
                file.linktarget = Some(decoded_name(link, key)?)
            }
        }
        safe_relative(file.filename.as_deref().context("Missing filename")?)?;
        let file_size = file.size.unwrap_or(0);
        let mut chunks: Vec<_> = file.chunks.iter().collect();
        chunks.sort_by_key(|chunk| chunk.offset.unwrap_or(0));
        let mut covered = 0u64;
        for chunk in chunks {
            let size = chunk.original_size.context("Missing chunk size")? as u64;
            let offset = chunk.offset.context("Missing chunk offset")?;
            ensure!(
                size <= MAX_CHUNK as u64
                    && offset.checked_add(size).is_some_and(|end| end <= file_size),
                "Chunk outside file bounds"
            );
            ensure!(
                size > 0 && offset == covered,
                "Manifest file has overlapping or missing chunks"
            );
            covered = offset + size;
            ensure!(
                chunk.sha.as_ref().is_some_and(|s| s.len() == 20),
                "Invalid chunk digest"
            );
        }
        let external_link = file
            .linktarget
            .as_ref()
            .is_some_and(|link| !link.is_empty());
        if file.flags.unwrap_or(0) & 64 == 0 && !external_link {
            ensure!(covered == file_size, "Manifest file content is incomplete");
        }
        if let Some(hash) = &file.sha_content {
            ensure!(hash.len() == 20, "Invalid file digest");
        }
    }
    Ok((metadata, files))
}

struct LimitedBuffer {
    bytes: Vec<u8>,
    limit: usize,
    control: Option<InstallControl>,
    checked_at: usize,
}
impl Write for LimitedBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.bytes.len().saturating_sub(self.checked_at) >= 128 * 1024 {
            if let Some(control) = &self.control {
                control.checkpoint().map_err(std::io::Error::other)?;
            }
            self.checked_at = self.bytes.len();
        }
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("Decompressed chunk exceeds limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
fn decompress(data: &[u8], expected: usize) -> Result<Vec<u8>> {
    decompress_controlled(data, expected, None)
}
fn decompress_controlled(
    data: &[u8],
    expected: usize,
    control: Option<InstallControl>,
) -> Result<Vec<u8>> {
    ensure!(expected <= MAX_CHUNK, "Chunk exceeds size limit");
    let mut output = LimitedBuffer {
        bytes: Vec::new(),
        limit: expected,
        control,
        checked_at: 0,
    };
    if data.starts_with(b"VZa") {
        ensure!(
            data.len() >= 22 && data.ends_with(b"zv"),
            "Invalid VZ container"
        );
        let footer = data.len() - 10;
        let crc = u32::from_le_bytes(data[footer..footer + 4].try_into()?);
        let size = u32::from_le_bytes(data[footer + 4..footer + 8].try_into()?) as usize;
        ensure!(size == expected, "VZ length mismatch");
        let dictionary = u32::from_le_bytes(data[8..12].try_into()?);
        ensure!(
            dictionary <= 64 * 1024 * 1024,
            "LZMA dictionary exceeds limit"
        );
        let mut alone = Vec::with_capacity(data.len() + 8);
        alone.extend_from_slice(&data[7..12]);
        alone.extend_from_slice(&(expected as u64).to_le_bytes());
        alone.extend_from_slice(&data[12..footer]);
        lzma_rs::lzma_decompress(&mut Cursor::new(alone), &mut output)?;
        ensure!(
            crc32fast::hash(&output.bytes) == crc,
            "VZ checksum mismatch"
        );
    } else if data.starts_with(b"VSZa") {
        ensure!(
            data.len() >= 23 && data.ends_with(b"zsv"),
            "Invalid VSZ container"
        );
        // SteamKit VZstdUtil: CRC + length + four reserved bytes + magic = 15.
        let footer = data.len() - 15;
        let crc = u32::from_le_bytes(data[footer..footer + 4].try_into()?);
        let size = u32::from_le_bytes(data[footer + 4..footer + 8].try_into()?) as usize;
        ensure!(size == expected, "VSZ length mismatch");
        ensure!(
            data[4..8] == data[footer..footer + 4],
            "VSZ header/footer checksum mismatch"
        );
        let mut decoder = ruzstd::StreamingDecoder::new(Cursor::new(&data[8..footer]))
            .map_err(|_| anyhow::anyhow!("Invalid zstd content"))?;
        std::io::copy(&mut decoder, &mut output)?;
        ensure!(
            crc32fast::hash(&output.bytes) == crc,
            "VSZ checksum mismatch"
        );
    } else {
        output.bytes = unpack_zip(data, expected)?;
    }
    ensure!(
        output.bytes.len() == expected,
        "Downloaded chunk length mismatch"
    );
    Ok(output.bytes)
}

async fn chunk_bytes(
    client: &reqwest::Client,
    depot: &PreparedDepot,
    chunk: &Chunk,
    control: &InstallControl,
    progress: &SharedProgress,
    message: &str,
) -> Result<Vec<u8>> {
    let sha = chunk.sha.as_ref().context("Missing chunk SHA1")?.clone();
    let path = format!("depot/{}/chunk/{}", depot.depot_id, hex::encode(&sha));
    let mut received = 0u64;
    let encrypted = control
        .run(network::fetch_with_progress(
            client,
            &depot.servers,
            &path,
            MAX_CHUNK + 1024 * 1024,
            &mut |count| {
                received += count as u64;
                progress.lock().unwrap().receive(count, message);
            },
        ))
        .await?;
    progress
        .lock()
        .unwrap()
        .complete_chunk(wire_size(chunk), received);
    let key = depot.key;
    let expected = chunk.original_size.context("Missing chunk size")? as usize;
    let worker_control = control.clone();
    control
        .blocking(move || {
            worker_control.checkpoint()?;
            let data = decompress_controlled(
                &decrypt(&encrypted, &key)?,
                expected,
                Some(worker_control.clone()),
            )?;
            worker_control.checkpoint()?;
            ensure!(
                digest_bytes(&data, &worker_control)? == sha,
                "Downloaded chunk SHA1 mismatch"
            );
            Ok(data)
        })
        .await
}

fn digest_bytes(bytes: &[u8], control: &InstallControl) -> Result<Vec<u8>> {
    let mut hash = Sha1::new();
    for block in bytes.chunks(128 * 1024) {
        control.checkpoint()?;
        hash.update(block);
    }
    Ok(hash.finalize().to_vec())
}

fn valid_symlink(relative: &Path, target: &str) -> Result<()> {
    ensure!(
        !target.is_empty() && !target.contains('\0'),
        "Invalid symlink target"
    );
    let target = Path::new(target);
    let mut depth = relative
        .parent()
        .map(|p| p.components().count())
        .unwrap_or(0);
    for part in target.components() {
        match part {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                ensure!(depth > 0, "Symlink escapes installation");
                depth -= 1;
            }
            _ => bail!("Symlink target must be relative to installation"),
        }
    }
    Ok(())
}

fn wire_size(chunk: &Chunk) -> u64 {
    u64::from(chunk.compressed_size.or(chunk.original_size).unwrap_or(0))
}

/// Depot files form an ordered overlay: the last selected depot owns a path.
/// Resolve that once before seeding so a later write cannot invalidate an
/// earlier file's verified bitmap (Soundpad ships overlapping 32/64-bit depots).
pub(crate) fn resolve_depot_overlays(depots: &mut [PreparedDepot]) -> Result<()> {
    use std::collections::{HashMap, HashSet};
    let mut owners = HashMap::<std::path::PathBuf, (usize, usize, bool)>::new();
    for (depot_index, depot) in depots.iter().enumerate() {
        for (file_index, file) in depot.files.iter().enumerate() {
            let relative = safe_relative(file.filename.as_deref().context("Missing filename")?)?;
            let key = relative
                .components()
                .filter(|part| matches!(part, Component::Normal(_)))
                .collect();
            let directory = file.flags.unwrap_or(0) & 64 != 0;
            if let Some((_, _, previous_directory)) = owners.get(&key) {
                ensure!(
                    *previous_directory == directory,
                    "Depot file/directory collision: {}",
                    relative.display()
                );
            }
            owners.insert(key, (depot_index, file_index, directory));
        }
    }
    let keep = owners
        .into_values()
        .map(|(depot, file, _)| (depot, file))
        .collect::<HashSet<_>>();
    for (depot_index, depot) in depots.iter_mut().enumerate() {
        let mut index = 0;
        depot.files.retain(|_| {
            let retain = keep.contains(&(depot_index, index));
            index += 1;
            retain
        });
    }
    Ok(())
}

/// Verified only for this attempt; every resumed attempt rebuilds the plan from
/// disk hashes. Never persist a boolean that could outlive changed file bytes.
#[derive(Default)]
pub(crate) struct FilePlan {
    verified: Vec<bool>,
}
pub(crate) struct DownloadPlan {
    pub transfer: u64,
    pub reused: u64,
    pub depots: Vec<Vec<FilePlan>>,
}

fn contains_stored_bytes(file: &std::fs::File, offset: u64, size: u64) -> Result<bool> {
    let metadata = file.metadata()?;
    if offset
        .checked_add(size)
        .is_none_or(|end| end > metadata.len())
    {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // Old SkinOS preallocated whole files with set_len before hashing their
        // holes. This also fixes resume of those existing multi-gigabyte files.
        if metadata.blocks() == 0 {
            return Ok(false);
        }
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        use std::os::fd::AsRawFd;
        let data = unsafe { libc::lseek(file.as_raw_fd(), offset as libc::off_t, libc::SEEK_DATA) };
        if data >= 0 {
            return Ok((data as u64) < offset.saturating_add(size));
        }
        if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENXIO) {
            return Ok(false);
        }
        // Filesystems without SEEK_DATA support still get full hash validation.
    }
    Ok(true)
}
fn read_chunk(
    file: &mut std::fs::File,
    offset: u64,
    size: usize,
    control: &InstallControl,
) -> Result<Vec<u8>> {
    control.checkpoint()?;
    file.seek(std::io::SeekFrom::Start(offset))?;
    let mut bytes = vec![0; size];
    for block in bytes.chunks_mut(128 * 1024) {
        control.checkpoint()?;
        file.read_exact(block)?;
    }
    Ok(bytes)
}
fn write_chunk(
    file: &mut std::fs::File,
    offset: u64,
    bytes: &[u8],
    control: &InstallControl,
) -> Result<()> {
    file.seek(std::io::SeekFrom::Start(offset))?;
    for block in bytes.chunks(128 * 1024) {
        control.checkpoint()?;
        file.write_all(block)?;
    }
    Ok(())
}

/// Runs on the blocking pool. New files and sparse holes are marked missing
/// without reading, allocating, or hashing gigabytes of zero-filled content.
pub(crate) fn seed_and_plan(
    depots: &[PreparedDepot],
    root: &Path,
    installed: &Path,
    control: &InstallControl,
    progress: &mut VerificationProgress,
) -> Result<DownloadPlan> {
    let mut plan = DownloadPlan {
        transfer: 0,
        reused: 0,
        depots: Vec::new(),
    };
    for depot in depots {
        let mut files = Vec::new();
        for file in &depot.files {
            checkpoint(control)?;
            let relative = safe_relative(file.filename.as_deref().context("Missing filename")?)?;
            let flags = file.flags.unwrap_or(0);
            let mut planned = FilePlan {
                verified: vec![false; file.chunks.len()],
            };
            if flags & 64 == 0 {
                let is_link =
                    flags & 512 != 0 || file.linktarget.as_ref().is_some_and(|v| !v.is_empty());
                if !is_link {
                    let path = safe_output(root, &relative)?;
                    let mut source = match safe_output(installed, &relative) {
                        Ok(path) if path.is_file() => Some(std::fs::File::open(path)?),
                        _ => None,
                    };
                    let mut output = if path.is_file() {
                        Some(
                            std::fs::OpenOptions::new()
                                .read(true)
                                .write(true)
                                .open(&path)?,
                        )
                    } else {
                        None
                    };
                    for (index, chunk) in file.chunks.iter().enumerate() {
                        checkpoint(control)?;
                        let offset = chunk.offset.context("Missing chunk offset")?;
                        let size = chunk.original_size.context("Missing chunk size")? as usize;
                        let mut valid = false;
                        if let Some(output) = output.as_mut()
                            && contains_stored_bytes(output, offset, size as u64)?
                        {
                            let bytes = read_chunk(output, offset, size, control)?;
                            valid = Some(digest_bytes(&bytes, control)?.as_slice())
                                == chunk.sha.as_deref();
                        }
                        if !valid
                            && let Some(source) = source.as_mut()
                            && contains_stored_bytes(source, offset, size as u64)?
                        {
                            let bytes = read_chunk(source, offset, size, control)?;
                            valid = Some(digest_bytes(&bytes, control)?.as_slice())
                                == chunk.sha.as_deref();
                            if valid {
                                if output.is_none() {
                                    if let Some(parent) = path.parent() {
                                        std::fs::create_dir_all(parent)?;
                                    }
                                    output = Some(
                                        std::fs::OpenOptions::new()
                                            .read(true)
                                            .write(true)
                                            .create(true)
                                            .truncate(false)
                                            .open(&path)?,
                                    );
                                }
                                write_chunk(output.as_mut().unwrap(), offset, &bytes, control)?;
                            }
                        }
                        planned.verified[index] = valid;
                        if valid {
                            plan.reused = plan
                                .reused
                                .checked_add(size as u64)
                                .context("Reuse size overflow")?;
                        } else {
                            plan.transfer = plan
                                .transfer
                                .checked_add(wire_size(chunk))
                                .context("Transfer size overflow")?;
                        }
                        progress.processed += size as u64;
                        progress.chunks += 1;
                        progress.reused = plan.reused;
                        progress.emit(
                            &format!(
                                "Проверка: {} · файл {} из {}",
                                relative.display(),
                                progress.files + 1,
                                progress.total_files
                            ),
                            false,
                        );
                    }
                    if let Some(output) = output.as_mut() {
                        output.flush()?;
                    }
                } else if file.linktarget.as_ref().is_none_or(|v| v.is_empty()) {
                    for chunk in &file.chunks {
                        plan.transfer = plan
                            .transfer
                            .checked_add(wire_size(chunk))
                            .context("Transfer size overflow")?;
                    }
                    progress.processed += file.size.unwrap_or(0);
                    progress.chunks += file.chunks.len() as u64;
                }
            }
            progress.files += 1;
            progress.emit(
                &format!(
                    "Проверено файлов: {} из {}",
                    progress.files, progress.total_files
                ),
                false,
            );
            files.push(planned);
        }
        plan.depots.push(files);
    }
    progress.emit("Проверка существующих файлов завершена", true);
    Ok(plan)
}

fn verify_file(
    path: &Path,
    file: &FileMapping,
    control: &InstallControl,
    progress: &SharedProgress,
) -> Result<()> {
    let Some(expected) = file.sha_content.as_deref().filter(|s| s.len() == 20) else {
        return Ok(());
    };
    let mut output = std::fs::File::open(path)?;
    let size = file.size.unwrap_or(0);
    ensure!(
        output.metadata()?.len() == size,
        "Downloaded file size mismatch"
    );
    let empty_sentinel =
        size == 0 && file.chunks.is_empty() && expected.iter().all(|byte| *byte == 0);
    let mut hash = Sha1::new();
    let mut buffer = vec![0; 128 * 1024];
    let mut checked = 0u64;
    let mut last = std::time::Instant::now();
    loop {
        control.checkpoint()?;
        let count = output.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
        checked += count as u64;
        if last.elapsed() >= std::time::Duration::from_millis(150) {
            progress.lock().unwrap().emit_verification(
                checked,
                size,
                &format!(
                    "Проверка SHA1: {} · {} / {} МБ",
                    file.filename.as_deref().unwrap_or("файл"),
                    checked / 1048576,
                    size / 1048576
                ),
            );
            last = std::time::Instant::now();
        }
    }
    ensure!(
        hash.finalize().as_slice() == expected || empty_sentinel,
        "Downloaded file SHA1 mismatch: {}",
        path.display()
    );
    Ok(())
}

fn bounded_chunks<T, F, Fut>(
    items: impl IntoIterator<Item = T>,
    work: F,
) -> impl futures_util::Stream<Item = Fut::Output>
where
    F: FnMut(T) -> Fut,
    Fut: std::future::Future,
{
    stream::iter(items)
        .map(work)
        .buffer_unordered(CHUNK_CONCURRENCY)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn write_depot(
    client: &reqwest::Client,
    depot: &PreparedDepot,
    root: &Path,
    control: &InstallControl,
    progress: &SharedProgress,
    symlinks_only: bool,
    plans: &[FilePlan],
) -> Result<()> {
    let mut symlinks = Vec::new();
    for (file_index, file) in depot.files.iter().enumerate() {
        checkpoint(control)?;
        let relative = safe_relative(file.filename.as_deref().context("Missing filename")?)?;
        let flags = file.flags.unwrap_or(0);
        if flags & 512 != 0 || file.linktarget.as_ref().is_some_and(|s| !s.is_empty()) {
            symlinks.push((relative, file));
            continue;
        }
        if symlinks_only {
            continue;
        }
        let path = safe_output(root, &relative)?;
        if flags & 64 != 0 {
            tokio::fs::create_dir_all(&path).await?;
            progress.lock().unwrap().files_completed += 1;
            continue;
        }
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut output = tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .await?;
        output.set_len(file.size.unwrap_or(0)).await?;
        let plan = plans
            .get(file_index)
            .context("Missing verified chunk plan")?;
        ensure!(
            plan.verified.len() == file.chunks.len(),
            "Chunk plan changed"
        );
        let mut missing = Vec::new();
        for (index, chunk) in file.chunks.iter().enumerate() {
            if plan.verified[index] {
                let mut p = progress.lock().unwrap();
                p.processed += u64::from(chunk.original_size.unwrap_or(0));
                p.chunks_completed += 1;
            } else {
                missing.push(chunk.clone());
            }
        }
        let message = format!("Загрузка: {}", relative.display());
        // At most four chunk buffers/decryptions in flight per game. Disk writes
        // stay serialized, and dropping HTTP futures cannot mutate staging.
        let mut downloads = bounded_chunks(missing, |chunk| {
            let message = &message;
            async move {
                chunk_bytes(client, depot, &chunk, control, progress, message)
                    .await
                    .map(|bytes| (chunk, bytes))
            }
        });
        while let Some(result) = downloads.next().await {
            let (chunk, bytes) = result?;
            checkpoint(control)?;
            output
                .seek(std::io::SeekFrom::Start(
                    chunk.offset.context("Missing chunk offset")?,
                ))
                .await?;
            // Await every write; a paused slot is never released with pending
            // Tokio disk writes that could race the subsequent resumed attempt.
            output.write_all(&bytes).await?;
            output.flush().await?;
            checkpoint(control)?;
            let mut p = progress.lock().unwrap();
            p.processed += bytes.len() as u64;
            p.chunks_completed += 1;
            p.emit("downloading", &message, false);
        }
        output.flush().await?;
        let verify_path = path.clone();
        let verify_mapping = file.clone();
        let verify_control = control.clone();
        let verify_progress = progress.clone();
        control
            .blocking(move || {
                verify_file(
                    &verify_path,
                    &verify_mapping,
                    &verify_control,
                    &verify_progress,
                )
            })
            .await?;
        #[cfg(unix)]
        if flags & 32 != 0 {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).await?;
        }
        let mut p = progress.lock().unwrap();
        p.files_completed += 1;
        p.emit("downloading", &message, false);
    }
    if !symlinks_only {
        return Ok(());
    }
    // Commit links after all regular content, so no manifest write follows one.
    for (relative, file) in symlinks {
        checkpoint(control)?;
        let mut target = file.linktarget.clone().unwrap_or_default();
        if target.is_empty() {
            ensure!(
                file.size.unwrap_or(0) <= 4096,
                "Symlink target is too large"
            );
            let mut bytes = Vec::new();
            for chunk in &file.chunks {
                bytes.extend_from_slice(
                    &chunk_bytes(client, depot, chunk, control, progress, "Загрузка ссылок")
                        .await?,
                );
            }
            target = String::from_utf8(bytes)?.trim_end_matches('\0').to_owned();
        }
        let target = target.replace('\\', "/");
        valid_symlink(&relative, &target)?;
        let parent = safe_output(root, relative.parent().context("Symlink missing parent")?)?;
        tokio::fs::create_dir_all(&parent).await?;
        let path = parent.join(relative.file_name().context("Missing symlink name")?);
        if path.exists() || path.is_symlink() {
            tokio::fs::remove_file(&path).await?;
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &path)?;
        #[cfg(not(unix))]
        bail!("Depot symlinks require Unix");
        let mut p = progress.lock().unwrap();
        p.files_completed += 1;
        p.chunks_completed += file.chunks.len() as u64;
        p.processed += file.size.unwrap_or(0);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn test_plan(depot: &PreparedDepot, root: &Path) -> Vec<FilePlan> {
        let mut progress =
            VerificationProgress::new(42, 0, depot.files.len() as u64, 0, Arc::new(|_| {}));
        seed_and_plan(
            std::slice::from_ref(depot),
            root,
            &root.join("not-installed"),
            &InstallControl::new(),
            &mut progress,
        )
        .unwrap()
        .depots
        .remove(0)
    }

    #[tokio::test]
    async fn update_reuses_only_verified_chunks_and_keeps_source_unchanged() {
        let installed = tempfile::tempdir().unwrap();
        let staging = tempfile::tempdir().unwrap();
        std::fs::write(installed.path().join("game"), b"keepOLD!").unwrap();
        let chunk = |bytes: &[u8], offset, compressed| Chunk {
            sha: Some(Sha1::digest(bytes).to_vec()),
            offset: Some(offset),
            original_size: Some(bytes.len() as u32),
            compressed_size: Some(compressed),
            ..Default::default()
        };
        let depot = PreparedDepot {
            depot_id: 1,
            key: [0; 32],
            servers: vec![],
            metadata: Default::default(),
            files: vec![FileMapping {
                filename: Some("game".into()),
                size: Some(8),
                chunks: vec![chunk(b"keep", 0, 32), chunk(b"NEW!", 4, 48)],
                ..Default::default()
            }],
        };
        let mut progress = VerificationProgress::new(42, 8, 1, 2, Arc::new(|_| {}));
        let plan = seed_and_plan(
            &[depot],
            staging.path(),
            installed.path(),
            &InstallControl::new(),
            &mut progress,
        )
        .unwrap();
        assert_eq!((plan.transfer, plan.reused), (48, 4));
        assert_eq!(
            std::fs::read(installed.path().join("game")).unwrap(),
            b"keepOLD!"
        );
        assert_eq!(
            &std::fs::read(staging.path().join("game")).unwrap()[..4],
            b"keep"
        );
    }
    fn fixture_depot(file: FileMapping) -> PreparedDepot {
        PreparedDepot {
            depot_id: 1,
            key: [0; 32],
            servers: vec![],
            metadata: Default::default(),
            files: vec![file],
        }
    }
    #[test]
    fn fresh_large_file_is_planned_without_preallocation_or_hashing() {
        let staging = tempfile::tempdir().unwrap();
        let installed = tempfile::tempdir().unwrap();
        let size = 8u64 * 1024 * 1024 * 1024;
        let chunks = (0..8192)
            .map(|i| Chunk {
                offset: Some(i * 1048576),
                original_size: Some(1048576),
                compressed_size: Some(42),
                sha: Some(vec![1; 20]),
                ..Default::default()
            })
            .collect();
        let depot = fixture_depot(FileMapping {
            filename: Some("large.pak".into()),
            size: Some(size),
            chunks,
            ..Default::default()
        });
        let mut progress = VerificationProgress::new(42, size, 1, 8192, Arc::new(|_| {}));
        let now = std::time::Instant::now();
        let plan = seed_and_plan(
            &[depot],
            staging.path(),
            installed.path(),
            &InstallControl::new(),
            &mut progress,
        )
        .unwrap();
        assert!(
            !staging.path().join("large.pak").exists(),
            "fresh planning must not create/hash a multi-GB zero file"
        );
        assert_eq!(
            (
                plan.transfer,
                plan.reused,
                progress.processed,
                progress.chunks
            ),
            (8192 * 42, 0, size, 8192)
        );
        assert!(now.elapsed() < std::time::Duration::from_secs(3));
    }
    #[test]
    #[cfg(unix)]
    fn old_fully_sparse_partial_is_skipped_without_scanning_8gb_of_zeros() {
        use std::os::unix::fs::MetadataExt;
        let staging = tempfile::tempdir().unwrap();
        let installed = tempfile::tempdir().unwrap();
        let size = 8u64 * 1024 * 1024 * 1024;
        let file = std::fs::File::create(staging.path().join("large.pak")).unwrap();
        file.set_len(size).unwrap();
        assert_eq!(file.metadata().unwrap().blocks(), 0);
        let chunks = (0..8192)
            .map(|i| Chunk {
                offset: Some(i * 1048576),
                original_size: Some(1048576),
                compressed_size: Some(42),
                sha: Some(vec![1; 20]),
                ..Default::default()
            })
            .collect();
        let depot = fixture_depot(FileMapping {
            filename: Some("large.pak".into()),
            size: Some(size),
            chunks,
            ..Default::default()
        });
        let mut progress = VerificationProgress::new(42, size, 1, 8192, Arc::new(|_| {}));
        let now = std::time::Instant::now();
        let plan = seed_and_plan(
            &[depot],
            staging.path(),
            installed.path(),
            &InstallControl::new(),
            &mut progress,
        )
        .unwrap();
        assert_eq!(
            (plan.transfer, plan.reused, progress.processed),
            (8192 * 42, 0, size)
        );
        assert_eq!(file.metadata().unwrap().blocks(), 0);
        assert!(
            now.elapsed() < std::time::Duration::from_secs(3),
            "sparse planning must scale with chunk count, not logical file bytes"
        );
    }
    #[test]
    fn resume_revalidates_partial_chunks_and_retains_unchanged_verified_bytes() {
        let staging = tempfile::tempdir().unwrap();
        let installed = tempfile::tempdir().unwrap();
        std::fs::write(staging.path().join("game"), b"goodBAD!").unwrap();
        let chunks = [b"good", b"new!"]
            .into_iter()
            .enumerate()
            .map(|(i, b)| Chunk {
                offset: Some(i as u64 * 4),
                original_size: Some(4),
                compressed_size: Some(16),
                sha: Some(Sha1::digest(b).to_vec()),
                ..Default::default()
            })
            .collect();
        let depot = fixture_depot(FileMapping {
            filename: Some("game".into()),
            size: Some(8),
            chunks,
            ..Default::default()
        });
        let mut progress = VerificationProgress::new(42, 8, 1, 2, Arc::new(|_| {}));
        let plan = seed_and_plan(
            std::slice::from_ref(&depot),
            staging.path(),
            installed.path(),
            &InstallControl::new(),
            &mut progress,
        )
        .unwrap();
        assert_eq!((plan.transfer, plan.reused), (16, 4));
        assert_eq!(plan.depots[0][0].verified, [true, false]);
        assert_eq!(
            std::fs::read(staging.path().join("game")).unwrap(),
            b"goodBAD!"
        );
        // A new attempt cannot trust a previous in-memory verification result.
        std::fs::write(staging.path().join("game"), b"xxxxnew!").unwrap();
        let plan = seed_and_plan(
            &[depot],
            staging.path(),
            installed.path(),
            &InstallControl::new(),
            &mut progress,
        )
        .unwrap();
        assert_eq!(plan.depots[0][0].verified, [false, true]);
    }
    #[tokio::test]
    async fn per_game_chunk_pipeline_is_parallel_and_never_exceeds_four() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let outputs = bounded_chunks(0..12, |index| {
            let active = active.clone();
            let peak = peak.clone();
            async move {
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(current, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                active.fetch_sub(1, Ordering::SeqCst);
                index
            }
        })
        .collect::<Vec<_>>()
        .await;
        assert_eq!(outputs.len(), 12);
        assert_eq!(peak.load(Ordering::SeqCst), 4);
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn overlapping_depots_are_resolved_before_verified_chunk_plans_can_go_stale() {
        let installed = tempfile::tempdir().unwrap();
        let staging = tempfile::tempdir().unwrap();
        let earlier = b"new first depot content";
        let later = b"old final DLL";
        // Same pattern as Soundpad's 629521/629522 UniteFx-ARM.dll: distinct
        // content and lengths at one output path, plus reusable installed bytes.
        std::fs::write(installed.path().join("shared.dll"), later).unwrap();
        std::fs::write(staging.path().join("shared.dll"), earlier).unwrap();
        let file = |bytes: &[u8], name: &str| FileMapping {
            filename: Some(name.into()),
            size: Some(bytes.len() as u64),
            sha_content: Some(Sha1::digest(bytes).to_vec()),
            chunks: vec![Chunk {
                offset: Some(0),
                original_size: Some(bytes.len() as u32),
                compressed_size: Some(16),
                sha: Some(Sha1::digest(bytes).to_vec()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut depots = vec![
            fixture_depot(file(earlier, "./shared.dll")),
            fixture_depot(file(later, "shared.dll")),
        ];
        depots[1].depot_id = 2;
        resolve_depot_overlays(&mut depots).unwrap();
        let mut verifying =
            VerificationProgress::new(42, later.len() as u64, 1, 1, Arc::new(|_| {}));
        let plan = seed_and_plan(
            &depots,
            staging.path(),
            installed.path(),
            &InstallControl::new(),
            &mut verifying,
        )
        .unwrap();
        let progress = Arc::new(Mutex::new(TransferProgress::new(
            42,
            plan.transfer,
            plan.reused,
            later.len() as u64,
            Arc::new(|_| {}),
        )));
        for (index, depot) in depots.iter().enumerate() {
            write_depot(
                &reqwest::Client::new(),
                depot,
                staging.path(),
                &InstallControl::new(),
                &progress,
                false,
                &plan.depots[index],
            )
            .await
            .unwrap();
        }
        assert_eq!(
            std::fs::read(staging.path().join("shared.dll")).unwrap(),
            later
        );
        assert_eq!(
            std::fs::read(installed.path().join("shared.dll")).unwrap(),
            later
        );
        assert_eq!((plan.transfer, plan.reused), (0, later.len() as u64));
        assert!(depots[0].files.is_empty());
    }
    #[test]
    fn overlay_normalizes_manifest_separators_and_rejects_file_directory_collisions() {
        let mapping = |name: &str, flags| FileMapping {
            filename: Some(name.into()),
            flags: Some(flags),
            ..Default::default()
        };
        let mut depots = vec![
            fixture_depot(mapping("folder\\shared.dll", 0)),
            fixture_depot(mapping("folder/shared.dll", 0)),
        ];
        resolve_depot_overlays(&mut depots).unwrap();
        assert!(depots[0].files.is_empty());
        assert_eq!(depots[1].files.len(), 1);
        let mut conflict = vec![
            fixture_depot(mapping("folder", 64)),
            fixture_depot(mapping("folder", 0)),
        ];
        assert!(
            resolve_depot_overlays(&mut conflict)
                .unwrap_err()
                .to_string()
                .contains("file/directory collision")
        );
    }
    #[test]
    fn malformed_manifest_never_becomes_an_empty_success() {
        for input in [
            &b""[..],
            &b"garbage"[..],
            &0x71f617d0u32.to_le_bytes()[..],
            &[0u8; 16][..],
        ] {
            assert!(decode_manifest(input, 1, 2, &[0; 32]).is_err());
        }
    }
    #[test]
    fn preserves_in_bundle_framework_links_but_rejects_escape() {
        assert!(
            valid_symlink(
                Path::new("Game.app/Contents/Frameworks/Foo.framework/Versions/Current"),
                "A"
            )
            .is_ok()
        );
        assert!(valid_symlink(Path::new("Game.app/Contents/Foo"), "../../../outside").is_err());
        assert!(valid_symlink(Path::new("link"), "/etc/passwd").is_err());
    }
    #[test]
    fn truncated_encrypted_chunk_returns_error() {
        for len in [0, 1, 15, 16, 31, 33] {
            assert!(decrypt(&vec![0; len], &[0; 32]).is_err());
        }
    }
    fn manifest_bytes(file: FileMapping, encrypted: bool) -> Vec<u8> {
        let mut raw = Vec::new();
        for (magic, body) in [
            (
                0x71f617d0u32,
                ManifestPayload {
                    mappings: vec![file],
                }
                .encode_to_vec(),
            ),
            (
                0x1f4812beu32,
                ManifestMetadata {
                    depot_id: Some(1),
                    manifest_id: Some(2),
                    filenames_encrypted: Some(encrypted),
                    original_size: None,
                }
                .encode_to_vec(),
            ),
        ] {
            raw.extend(magic.to_le_bytes());
            raw.extend((body.len() as u32).to_le_bytes());
            raw.extend(body);
        }
        raw.extend(0x32c415abu32.to_le_bytes());
        raw
    }
    #[test]
    fn rejects_chunk_gaps_and_overlaps_before_writing() {
        let chunk = |offset| Chunk {
            sha: Some(vec![1; 20]),
            offset: Some(offset),
            original_size: Some(2),
            ..Default::default()
        };
        for offset in [1, 3] {
            let f = FileMapping {
                filename: Some("file".into()),
                size: Some(4),
                chunks: vec![chunk(0), chunk(offset)],
                ..Default::default()
            };
            assert!(decode_manifest(&manifest_bytes(f, false), 1, 2, &[0; 32]).is_err());
        }
    }
    #[test]
    fn decodes_encrypted_names_with_steam_base64_whitespace() {
        use aes::cipher::{BlockEncrypt, BlockEncryptMut};
        let key = [7u8; 32];
        let iv = [9u8; 16];
        let plain = b"Game.app/Contents/MacOS/Game\0";
        let mut data = vec![0; plain.len() + 16];
        data[..plain.len()].copy_from_slice(plain);
        let encrypted = cbc::Encryptor::<Aes256>::new_from_slices(&key, &iv)
            .unwrap()
            .encrypt_padded_mut::<Pkcs7>(&mut data, plain.len())
            .unwrap();
        let mut eiv = GenericArray::clone_from_slice(&iv);
        Aes256::new(GenericArray::from_slice(&key)).encrypt_block(&mut eiv);
        let bytes = [eiv.as_slice(), encrypted].concat();
        let name = format!(
            "{}\n",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        );
        let f = FileMapping {
            filename: Some(name),
            size: Some(0),
            ..Default::default()
        };
        let (_, files) = decode_manifest(&manifest_bytes(f, true), 1, 2, &key).unwrap();
        assert_eq!(
            files[0].filename.as_deref(),
            Some("Game.app/Contents/MacOS/Game")
        );
    }
    #[test]
    fn vsz_uses_fifteen_byte_footer_and_checks_crc() {
        let data = b"abc";
        // A Zstandard single-segment frame with one uncompressed last block.
        let frame = [
            0x28, 0xb5, 0x2f, 0xfd, 0x20, 3, 0x19, 0, 0, b'a', b'b', b'c',
        ];
        let crc = crc32fast::hash(data).to_le_bytes();
        let mut container = b"VSZa".to_vec();
        container.extend(crc);
        container.extend(frame);
        container.extend(crc);
        container.extend(3u32.to_le_bytes());
        container.extend([0; 4]);
        container.extend(b"zsv");
        assert_eq!(decompress(&container, 3).unwrap(), data);
        container[4] ^= 1;
        assert!(decompress(&container, 3).is_err());
    }
    #[tokio::test]
    async fn empty_file_zero_hash_sentinel_does_not_fail_a_download() {
        let root = tempfile::tempdir().unwrap();
        let depot = PreparedDepot {
            depot_id: 1942283,
            key: [0; 32],
            servers: vec![],
            metadata: Default::default(),
            files: vec![FileMapping {
                filename: Some("Game.app/Contents/Resources/fr.lproj/InfoPlist.strings".into()),
                size: Some(0),
                sha_content: Some(vec![0; 20]),
                ..Default::default()
            }],
        };
        let progress = Arc::new(Mutex::new(TransferProgress::new(
            42,
            0,
            0,
            0,
            std::sync::Arc::new(|_| {}),
        )));
        let plan = test_plan(&depot, root.path());
        let result = write_depot(
            &reqwest::Client::new(),
            &depot,
            root.path(),
            &InstallControl::new(),
            &progress,
            false,
            &plan,
        )
        .await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(
            std::fs::metadata(
                root.path()
                    .join("Game.app/Contents/Resources/fr.lproj/InfoPlist.strings")
            )
            .unwrap()
            .len(),
            0
        );
    }

    #[tokio::test]
    async fn zero_sentinel_does_not_disable_nonempty_file_hash_checks() {
        let root = tempfile::tempdir().unwrap();
        let bytes = b"abc";
        std::fs::write(root.path().join("file"), bytes).unwrap();
        let depot = PreparedDepot {
            depot_id: 1,
            key: [0; 32],
            servers: vec![],
            metadata: Default::default(),
            files: vec![FileMapping {
                filename: Some("file".into()),
                size: Some(3),
                sha_content: Some(vec![0; 20]),
                chunks: vec![Chunk {
                    offset: Some(0),
                    original_size: Some(3),
                    sha: Some(Sha1::digest(bytes).to_vec()),
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };
        let progress = Arc::new(Mutex::new(TransferProgress::new(
            42,
            0,
            0,
            0,
            std::sync::Arc::new(|_| {}),
        )));
        let plan = test_plan(&depot, root.path());
        let error = write_depot(
            &reqwest::Client::new(),
            &depot,
            root.path(),
            &InstallControl::new(),
            &progress,
            false,
            &plan,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("Downloaded file SHA1 mismatch"));
    }
    #[tokio::test]
    async fn empty_file_with_nonzero_wrong_hash_still_fails() {
        let root = tempfile::tempdir().unwrap();
        let depot = PreparedDepot {
            depot_id: 1,
            key: [0; 32],
            servers: vec![],
            metadata: Default::default(),
            files: vec![FileMapping {
                filename: Some("empty".into()),
                size: Some(0),
                sha_content: Some(vec![1; 20]),
                ..Default::default()
            }],
        };
        let progress = Arc::new(Mutex::new(TransferProgress::new(
            42,
            0,
            0,
            0,
            std::sync::Arc::new(|_| {}),
        )));
        let plan = test_plan(&depot, root.path());
        let error = write_depot(
            &reqwest::Client::new(),
            &depot,
            root.path(),
            &InstallControl::new(),
            &progress,
            false,
            &plan,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("Downloaded file SHA1 mismatch"));
    }
}
