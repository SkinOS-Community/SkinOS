//! Read-only file inspection. This crate never loads or initializes Valve code.
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

mod macho;

// Far above the current universal Steam runtime; bound allocations for untrusted input.
const MAX_RUNTIME_BYTES: u64 = 512 * 1024 * 1024;

pub type Result<T> = std::result::Result<T, String>;

/// A research target set, not a published ABI or a compatibility guarantee.
pub const TARGET_EXPORTS: &[&str] = &[
    "CreateInterface",
    "Steam_CreateSteamPipe",
    "Steam_ConnectToGlobalUser",
    "Steam_CreateGlobalUser",
    "Steam_CreateLocalUser",
    "Steam_ReleaseUser",
    "Steam_BReleaseSteamPipe",
    "Steam_BGetCallback",
    "Steam_FreeLastCallback",
    "Steam_GetAPICallResult",
    "Steam_ReleaseThreadLocalMemory",
    "Steam_BConnected",
    "Steam_BLoggedOn",
];

#[derive(Debug, Serialize)]
pub struct Inspection {
    pub schema_version: u32,
    pub inspection_mode: &'static str,
    pub runtime_initialized: bool,
    pub compatibility: &'static str,
    pub canonical_path: PathBuf,
    pub sha256: String,
    pub file_size_bytes: u64,
    pub architectures: Vec<Architecture>,
    pub limitations: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct Architecture {
    pub name: String,
    pub install_name: Option<String>,
    pub dependencies: Vec<String>,
    pub initializer_sections: Vec<String>,
    pub initializer_section_present: bool,
    pub exported_symbol_count: usize,
    pub target_exports: Vec<Export>,
    pub all_target_exports_present: bool,
}

#[derive(Debug, Serialize)]
pub struct Export {
    pub name: &'static str,
    pub present: bool,
}

/// Only conventional binary locations are checked; account/config directories
/// are not searched or opened.
pub fn discover_runtime() -> Result<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(
            "Library/Application Support/Steam/Steam.AppBundle/Steam/Contents/MacOS/steamclient.dylib",
        ));
    }
    candidates.push(PathBuf::from(
        "/Applications/Steam.app/Contents/MacOS/steamclient.dylib",
    ));
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| {
            "No installed steamclient.dylib found. Use inspect --runtime /absolute/path/steamclient.dylib. No application was started.".into()
        })
}

fn read_runtime(path: &Path) -> Result<Vec<u8>> {
    // Reject directories/devices/FIFOs before opening (a FIFO could block).
    if !path
        .metadata()
        .map_err(|error| format!("Cannot inspect runtime file: {error}"))?
        .is_file()
    {
        return Err("The runtime path must be a regular file.".into());
    }
    let file = File::open(path).map_err(|error| format!("Cannot open runtime file: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("Cannot inspect runtime file: {error}"))?;
    if !metadata.is_file() {
        return Err("The runtime path must be a regular file.".into());
    }
    if metadata.len() > MAX_RUNTIME_BYTES {
        return Err("The runtime exceeds the 512 MiB inspection limit.".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_RUNTIME_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Cannot read runtime file: {error}"))?;
    if bytes.len() as u64 > MAX_RUNTIME_BYTES {
        return Err("The runtime exceeds the 512 MiB inspection limit.".into());
    }
    Ok(bytes)
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)
        .map_err(|error| format!("Cannot reopen runtime for verification: {error}"))?
        .take(MAX_RUNTIME_BYTES + 1);
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    let mut size = 0;
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("Cannot verify runtime file: {error}"))?;
        if count == 0 {
            break;
        }
        size += count as u64;
        if size > MAX_RUNTIME_BYTES {
            return Err("The runtime changed during inspection and exceeds the size limit.".into());
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Parses a bounded file snapshot in Rust; no developer tools or Valve code are run.
pub fn inspect(path: &Path) -> Result<Inspection> {
    let canonical_path = path
        .canonicalize()
        .map_err(|error| format!("Cannot resolve runtime {}: {error}", path.display()))?;
    let bytes = read_runtime(&canonical_path)?;
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    let file_size_bytes = bytes.len() as u64;
    let architectures = macho::inspect(&bytes)
        .map_err(|error| format!("Invalid or unsupported Mach-O runtime: {error}"))?;
    // Steam can self-update. Do not accept a snapshot replaced during inspection.
    if hash_file(&canonical_path)? != sha256 {
        return Err("The runtime changed during inspection. Run the probe again after its update completes.".into());
    }
    Ok(Inspection {
        schema_version: 1,
        inspection_mode: "static",
        runtime_initialized: false,
        compatibility: "unverified",
        canonical_path,
        sha256,
        file_size_bytes,
        architectures,
        limitations: vec![
            "Export presence does not verify function signatures, interface layouts, initialization, login or game launch.",
            "Dependencies are declared Mach-O install names; their availability and loadability are not verified.",
            "Initializer sections are inspected only in this binary; dependencies can have additional initializers.",
            "No Valve code was loaded and no Steam account, cookie or token files were read.",
        ],
    })
}
