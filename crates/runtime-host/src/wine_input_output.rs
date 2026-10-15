//! Explicit Wine-only output selection and owned transport. No feeder spawning,
//! OS HID creation, Steam API emulation, input synthesis, or original-source edits.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::time::Duration;

pub const FRAME_BYTES: usize = 288;
pub const ORIGINAL_SHA256: &str =
    "7b09276160dfc42d1b8bb950a70b2f6a6802fa322728fe958f5ae4761b6eb585";
pub const DESCRIPTOR_SHA256: &str =
    "086699843817c3d3554b4de8d239d809407b328eafa4a82aae93284f669524a7";
const MAGIC: u32 = 0x5349_4e50;
const FD_ENV: &str = "SKINOS_INPUT_OUTPUT_FD";
const MAX_LIBRARY_BYTES: u64 = 16 * 1024 * 1024;
const PROVENANCE: &str = "steam-input-output-provenance.json";

unsafe extern "C" {
    // Darwin declares fcntl variadic. On arm64 a fixed third argument has a
    // different ABI and silently fails to set FD_CLOEXEC despite returning 0.
    fn fcntl(fd: i32, command: i32, ...) -> i32;
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
    fn dladdr(address: *const c_void, info: *mut DlInfo) -> c_int;
}

// Darwin dlfcn.h. In particular, dli_fname identifies the image containing the
// resolved symbol; dlsym on a handle alone would also search its dependencies.
#[repr(C)]
struct DlInfo {
    filename: *const c_char,
    base: *mut c_void,
    symbol: *const c_char,
    symbol_address: *mut c_void,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputSelection {
    Disabled,
    Diagnostic,
    ProductDiagnostic,
    Game,
}
impl OutputSelection {
    pub fn enabled(self) -> bool {
        self != Self::Disabled
    }
    pub fn is_diagnostic(self) -> bool {
        matches!(self, Self::Diagnostic | Self::ProductDiagnostic)
    }
    pub fn is_game(self) -> bool {
        self == Self::Game
    }
    pub fn uses_production_preparation(self) -> bool {
        matches!(self, Self::Game | Self::ProductDiagnostic)
    }
    pub fn is_product_diagnostic(self) -> bool {
        self == Self::ProductDiagnostic
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Diagnostic => "diagnostic",
            Self::ProductDiagnostic => "product_diagnostic",
            Self::Game => "game",
        }
    }
    /// Selection comes from this launch request. An inherited backend flag can
    /// opt into a diagnostic, but cannot silently enable ordinary game input.
    pub fn for_request(
        request: Option<&serde_json::Value>,
        backend: Option<&str>,
    ) -> Result<Self, String> {
        let explicit = match backend {
            None | Some("") => false,
            Some("wine") => true,
            Some(_) => return Err("Unknown Steam Input output backend".into()),
        };
        let requested = request
            .map(|request| optional_bool(request, "steam_input"))
            .transpose()?
            .unwrap_or(false);
        let diagnostic = request
            .map(|request| optional_bool(request, "input_diagnostic"))
            .transpose()?
            .unwrap_or(false);
        let selected = if diagnostic && explicit {
            Self::Diagnostic
        } else if diagnostic && requested {
            Self::ProductDiagnostic
        } else if requested {
            Self::Game
        } else if explicit {
            return Err(
                "Wine input output requires an explicit Windows input diagnostic or game setting"
                    .into(),
            );
        } else {
            Self::Disabled
        };
        if let Some(request) = request {
            selected.validate_request(request)?;
        }
        Ok(selected)
    }
    /// The caller keeps product wiring closed until game binding/installation
    /// is ready. Existing explicitly selected diagnostics keep their behavior.
    pub fn require_ready(
        self,
        explicit_diagnostic: bool,
        product_wiring_ready: bool,
    ) -> Result<(), String> {
        if !self.enabled()
            || product_wiring_ready
            || (self == Self::Diagnostic && explicit_diagnostic)
        {
            Ok(())
        } else {
            Err("Game Steam Input output is not connected to the launch lifecycle yet".into())
        }
    }
    pub fn validate_request(self, request: &serde_json::Value) -> Result<(), String> {
        let requested = optional_bool(request, "steam_input")?;
        let diagnostic = optional_bool(request, "input_diagnostic")?;
        if self.enabled()
            && (request["command"] != "launch" || !request["compatibility"].is_object())
        {
            return Err("Wine input output requires a Windows launch request".into());
        }
        match self {
            Self::Disabled if !requested => Ok(()),
            Self::Diagnostic if diagnostic => Ok(()),
            Self::ProductDiagnostic if diagnostic && requested => Ok(()),
            Self::Game if requested && !diagnostic => Ok(()),
            _ => Err("Steam Input output mode does not match its launch request".into()),
        }
    }
    pub fn from_worker_environment(live: bool) -> Result<Self, String> {
        Self::parse_worker_handoff(
            std::env::var("SKINOS_INPUT_OUTPUT_MODE").ok().as_deref(),
            std::env::var("SKINOS_INPUT_OUTPUT_BACKEND").ok().as_deref(),
            live,
        )
    }
    fn parse_worker_handoff(
        mode: Option<&str>,
        backend: Option<&str>,
        live: bool,
    ) -> Result<Self, String> {
        let selected = match mode {
            None | Some("disabled") => Self::Disabled,
            Some("diagnostic") => Self::Diagnostic,
            Some("product_diagnostic") => Self::ProductDiagnostic,
            Some("game") => Self::Game,
            _ => return Err("Unknown owned Steam Input output mode".into()),
        };
        if selected.enabled() {
            if !live || backend != Some("wine") {
                return Err(
                    "Enabled Steam Input mode lacks its owned live Wine backend handoff".into(),
                );
            }
        } else if !matches!(backend, None | Some("")) {
            return Err(
                "Disabled Steam Input mode cannot adopt an inherited output backend".into(),
            );
        }
        Ok(selected)
    }
}
fn optional_bool(request: &serde_json::Value, key: &str) -> Result<bool, String> {
    match request.get(key) {
        None | Some(serde_json::Value::Null) => Ok(false),
        Some(serde_json::Value::Bool(value)) => Ok(*value),
        _ => Err(format!("Steam Input launch field {key} must be boolean")),
    }
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Component {
    pub file: String,
    pub sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Provenance {
    pub schema_version: u32,
    pub backend: String,
    pub original: Component,
    pub selected: Component,
    pub backend_source: PathBuf,
    pub original_preserved: bool,
    pub callback_success_means: String,
    pub os_hid_created: bool,
}

/// Operates only on the caller's already-created private run state. Never edits
/// runtime-source or the installed Steam bundle. Missing selection is a no-op.
pub fn stage(
    state: &Path,
    backend_path: &Path,
    selection: OutputSelection,
) -> Result<Option<Provenance>, String> {
    if !selection.enabled() {
        return Ok(None);
    }
    stage_files(state, backend_path, ORIGINAL_SHA256).map(Some)
}

fn regular_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let meta = fs::symlink_metadata(path)
        .map_err(|e| format!("Cannot inspect {}: {e}", path.display()))?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_LIBRARY_BYTES {
        return Err(format!(
            "Output component must be a bounded regular non-symlink file: {}",
            path.display()
        ));
    }
    // O_NOFOLLOW prevents replacement of the final component with a symlink
    // between inspection and open. Check the opened object too.
    use std::io::Read;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(0x100 /* O_NOFOLLOW on macOS */)
        .open(path)
        .map_err(|e| e.to_string())?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > MAX_LIBRARY_BYTES {
        return Err("Output component changed while opening".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_LIBRARY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_LIBRARY_BYTES {
        return Err("Output component size changed".into());
    }
    Ok(bytes)
}
fn regular_directory(path: &Path) -> Result<(), String> {
    if !fs::symlink_metadata(path)
        .map_err(|e| e.to_string())?
        .is_dir()
    {
        return Err(format!(
            "Output staging directory is not a regular non-symlink directory: {}",
            path.display()
        ));
    }
    Ok(())
}
fn macho(bytes: &[u8]) -> bool {
    bytes.len() >= 32
        && matches!(
            &bytes[..4],
            [0xcf, 0xfa, 0xed, 0xfe]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xca, 0xfe, 0xba, 0xbe]
                | [0xbe, 0xba, 0xfe, 0xca]
                | [0xca, 0xfe, 0xba, 0xbf]
                | [0xbf, 0xba, 0xfe, 0xca]
        )
}
fn new_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())
}
fn stage_files(
    state: &Path,
    backend_path: &Path,
    original_sha: &str,
) -> Result<Provenance, String> {
    regular_directory(state)?;
    let runtime = state.join("runtime");
    regular_directory(&runtime)?;
    // Resolve only after rejecting final-component links. The caller selects the
    // owned backend file explicitly; it is never searched from a system install.
    let backend = regular_bytes(backend_path)?;
    if !macho(&backend) {
        return Err("Selected Wine output component is not a 64-bit Mach-O library".into());
    }
    let backend_source = backend_path.canonicalize().map_err(|e| e.to_string())?;
    let selected = runtime.join("libsteaminput.dylib");
    let original = regular_bytes(&selected)?;
    if digest(&original) != original_sha {
        return Err("Staged Valve input library differs from the audited original".into());
    }
    let backup = runtime.join("libsteaminput-valve.dylib");
    let temporary = runtime.join(".libsteaminput-wine.next");
    let manifest = state.join(PROVENANCE);
    let next_manifest = state.join(".steam-input-output-provenance.next");
    for path in [&backup, &temporary, &manifest, &next_manifest] {
        if fs::symlink_metadata(path).is_ok() {
            return Err(format!(
                "Refusing to replace existing output staging artifact {}",
                path.display()
            ));
        }
    }
    let provenance = Provenance {
        schema_version: 1,
        backend: "wine".into(),
        original: Component {
            file: "runtime/libsteaminput-valve.dylib".into(),
            sha256: original_sha.into(),
        },
        selected: Component {
            file: "runtime/libsteaminput.dylib".into(),
            sha256: digest(&backend),
        },
        backend_source,
        original_preserved: true,
        callback_success_means: "owned_datagram_queued_only".into(),
        os_hid_created: false,
    };
    // Preserve the exact original before switching the selected component. All
    // files are inside this one disposable state, never the source directory.
    new_file(&backup, &original)?;
    new_file(&temporary, &backend)?;
    new_file(
        &next_manifest,
        &serde_json::to_vec_pretty(&provenance).map_err(|e| e.to_string())?,
    )?;
    // Detect changes to the original during preparation before replacing it.
    if digest(&regular_bytes(&selected)?) != original_sha {
        return Err("Original changed during output staging".into());
    }
    fs::rename(&temporary, &selected).map_err(|e| e.to_string())?;
    if let Err(error) = fs::rename(&next_manifest, &manifest) {
        // Restore from bytes already verified in this transaction; retain the
        // preserved original/provenance temporary for diagnosis on failure.
        let restored = new_file(&temporary, &original)
            .and_then(|_| fs::rename(&temporary, &selected).map_err(|e| e.to_string()));
        return Err(format!(
            "Could not publish output provenance: {error}; rollback={restored:?}"
        ));
    }
    Ok(provenance)
}

/// Parent/worker-owned connected pair. Keep this object alive until Valve's
/// output component has finished. Receiver ownership can be transferred once.
pub struct Channel {
    writer: UnixDatagram,
    receiver: Option<UnixDatagram>,
}
impl Channel {
    /// # Safety
    /// Call in the isolated runtime worker before loading Valve or starting any
    /// threads: configuring process environment after threads exist is unsafe.
    /// No existing FD is imported/reused; pre-existing FD_ENV is rejected.
    pub unsafe fn create(selection: OutputSelection) -> Result<Option<Self>, String> {
        if !selection.enabled() {
            return Ok(None);
        }
        if std::env::var("SKINOS_COMPONENT_ROLE").ok().as_deref() != Some("runtime") {
            return Err("Input output channel belongs only to the owned runtime worker".into());
        }
        if std::env::var_os(FD_ENV).is_some() {
            return Err("Refusing to adopt or overwrite an inherited input output FD".into());
        }
        let channel = Self::pair()?;
        unsafe { std::env::set_var(FD_ENV, channel.writer.as_raw_fd().to_string()) };
        Ok(Some(channel))
    }
    fn pair() -> Result<Self, String> {
        let (writer, receiver) = UnixDatagram::pair().map_err(|e| e.to_string())?;
        for fd in [writer.as_raw_fd(), receiver.as_raw_fd()] {
            let flags = unsafe {
                fcntl(fd, 1 /* F_GETFD */, 0)
            };
            if flags < 0
                || unsafe {
                    fcntl(fd, 2 /* F_SETFD */, flags | 1 /* FD_CLOEXEC */)
                } < 0
            {
                return Err("Could not protect input channel descriptors from game exec".into());
            }
        }
        writer.set_nonblocking(true).map_err(|e| e.to_string())?;
        receiver
            .set_read_timeout(Some(Duration::from_millis(250)))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            writer,
            receiver: Some(receiver),
        })
    }
    #[cfg(test)]
    pub fn writer_fd(&self) -> i32 {
        self.writer.as_raw_fd()
    }
    pub fn take_receiver(&mut self) -> Option<UnixDatagram> {
        self.receiver.take()
    }
    // Drop closes only our descriptors. It deliberately does not mutate the
    // process-global environment after threads may have started. The worker
    // lifecycle ends after runtime teardown; FD_CLOEXEC excludes game children.
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Create,
    Report,
    Destroy,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    raw: [u8; FRAME_BYTES],
    pub kind: Kind,
    pub slot: u32,
    payload_len: usize,
}
impl Frame {
    /// Read into one extra byte so an oversized datagram cannot masquerade as a
    /// valid frame after UnixDatagram's truncation. Timeout means no event yet.
    pub fn receive(receiver: &UnixDatagram) -> Result<Option<Self>, String> {
        let mut bytes = [0_u8; FRAME_BYTES + 1];
        match receiver.recv(&mut bytes) {
            Ok(size) => Self::parse(&bytes[..size]).map(Some),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(format!("Input output channel receive failed: {error}")),
        }
    }
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() != FRAME_BYTES {
            return Err("Input frame length is not 288".into());
        }
        let word = |n: usize| u32::from_le_bytes(bytes[n * 4..n * 4 + 4].try_into().unwrap());
        if word(0) != MAGIC || word(1) != 1 {
            return Err("Input frame magic/version mismatch".into());
        }
        let (kind, expected) = match word(2) {
            1 => (Kind::Create, 188),
            2 => (Kind::Report, 20),
            3 => (Kind::Destroy, 0),
            _ => return Err("Unknown input frame kind".into()),
        };
        let slot = word(3);
        let payload_len = word(4) as usize;
        if slot >= 16
            || payload_len != expected
            || word(5) != 0x045e
            || word(6) != 0x028e
            || word(7) != 0
        {
            return Err("Input frame slot/length/device cohort mismatch".into());
        }
        let payload = &bytes[32..32 + payload_len];
        if kind == Kind::Create && digest(payload) != DESCRIPTOR_SHA256 {
            return Err("Input Create descriptor mismatch".into());
        }
        if bytes[32 + payload_len..].iter().any(|v| *v != 0) {
            return Err("Input frame unused payload is not zero".into());
        }
        Ok(Self {
            raw: bytes.try_into().unwrap(),
            kind,
            slot,
            payload_len,
        })
    }
    #[cfg(test)]
    pub fn payload(&self) -> &[u8] {
        &self.raw[32..32 + self.payload_len]
    }
    pub fn as_bytes(&self) -> &[u8; FRAME_BYTES] {
        &self.raw
    }
}

/// Custom backend stats export. Any loss_generation increase requires relay
/// disconnect/failure so failed report queuing cannot leave held input behind.
/// Poll while the backend is alive and once more before normal teardown.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OutputStats {
    pub version: u32,
    pub bytes: u32,
    pub queued_frames: u64,
    pub failed_frames: u64,
    pub loss_generation: u64,
    pub rejected_active_creates: u64,
    pub active_slot_mask: u32,
    pub reserved: u32,
}
type GetStatsFn = unsafe extern "C" fn(*mut OutputStats, u32) -> bool;
const STATS_SYMBOL: &CStr = c"SkinosInputOutput_GetStats";
const STATS_BYTES: u32 = 48;
const _: () = assert!(std::mem::size_of::<OutputStats>() == STATS_BYTES as usize);
impl OutputStats {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1
            || self.bytes != STATS_BYTES
            || self.reserved != 0
            || self.active_slot_mask & !0xffff != 0
        {
            return Err("Invalid own output backend stats ABI".into());
        }
        Ok(())
    }
    pub fn had_loss(&self) -> bool {
        self.loss_generation != 0 || self.failed_frames != 0 || self.rejected_active_creates != 0
    }
}

#[derive(Default)]
struct StatsMonitor {
    previous: Option<OutputStats>,
    failure: Option<String>,
}
impl StatsMonitor {
    fn observe(&mut self, result: Result<OutputStats, String>) -> Result<OutputStats, String> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        let checked = result.and_then(|stats| {
            stats.validate()?;
            if self.previous.is_some_and(|previous| {
                stats.queued_frames < previous.queued_frames
                    || stats.failed_frames < previous.failed_frames
                    || stats.loss_generation < previous.loss_generation
                    || stats.rejected_active_creates < previous.rejected_active_creates
            }) {
                return Err(
                    "Own output backend counters regressed during one loaded lifetime".into(),
                );
            }
            Ok(stats)
        });
        match checked {
            Ok(stats) => {
                self.previous = Some(stats);
                Ok(stats)
            }
            Err(error) => {
                self.failure = Some(error.clone());
                Err(error)
            }
        }
    }
}

struct LibraryLease(NonNull<c_void>);
impl Drop for LibraryLease {
    fn drop(&mut self) {
        // A successful RTLD_NOLOAD open owns a reference too. Its only close is
        // here, after all stats calls, including on acquisition-error paths.
        unsafe { dlclose(self.0.as_ptr()) };
    }
}

/// An owned reference to the exact already-loaded, privately staged output
/// library. No handle or callable function pointer escapes this object.
///
/// Keep this lease on the runtime worker thread until Valve shutdown has
/// finished, the relay has drained/closed, and a final `check` has succeeded.
/// It intentionally is not Send/Sync: a separate poller must not outlive this
/// owner or call an export after dlclose. Dropping it does not finish the relay.
pub struct OutputLease {
    _library: LibraryLease,
    get_stats: GetStatsFn,
    monitor: StatsMonitor,
}
impl OutputLease {
    /// Return None while the explicitly selected library is not yet loaded.
    /// Never causes a load, searches a system path, or adopts the original
    /// Valve output backend. Wrong provenance, symbol, or ABI is an error.
    pub fn try_loaded(state: &Path, selection: OutputSelection) -> Result<Option<Self>, String> {
        if !selection.enabled() {
            return Ok(None);
        }
        if std::env::var("SKINOS_COMPONENT_ROLE").ok().as_deref() != Some("runtime") {
            return Err("Input output lease belongs only to the owned runtime worker".into());
        }
        let path = selected_library(state)?;
        let name = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| "Selected output path contains a NUL byte")?;
        // Darwin RTLD_NOW | RTLD_LOCAL | RTLD_NOLOAD | RTLD_FIRST. RTLD_FIRST
        // scopes lookup to this image, and dladdr below independently verifies
        // the export's origin before converting it to a callable pointer.
        let Some(handle) = NonNull::new(unsafe { dlopen(name.as_ptr(), 2 | 4 | 16 | 256) }) else {
            return Ok(None);
        };
        let library = LibraryLease(handle);
        let symbol = unsafe { dlsym(library.0.as_ptr(), STATS_SYMBOL.as_ptr()) };
        if symbol.is_null() {
            return Err("Selected output backend is missing its own stats export".into());
        }
        let mut info = DlInfo {
            filename: std::ptr::null(),
            base: std::ptr::null_mut(),
            symbol: std::ptr::null(),
            symbol_address: std::ptr::null_mut(),
        };
        if unsafe { dladdr(symbol, &mut info) } == 0 || info.filename.is_null() {
            return Err("Cannot establish own output stats export image".into());
        }
        // SAFETY: successful dladdr returns a NUL-terminated filename whose
        // image remains loaded through `library` for this entire check.
        let image = unsafe { CStr::from_ptr(info.filename) };
        let image = Path::new(std::ffi::OsStr::from_bytes(image.to_bytes()))
            .canonicalize()
            .map_err(|e| format!("Cannot resolve own output stats image: {e}"))?;
        if image != path || info.symbol_address != symbol {
            return Err(
                "Own output stats export does not originate from the selected runtime file".into(),
            );
        }
        // Revalidate the private staged selection after acquiring the image.
        if selected_library(state)? != path {
            return Err("Selected output library changed while acquiring its lease".into());
        }
        // SAFETY: this exact private backend exports the audited 48-byte C ABI;
        // no function pointer is exposed, and `library` owns its loaded lifetime.
        let get_stats = unsafe { std::mem::transmute::<*mut c_void, GetStatsFn>(symbol) };
        let mut lease = Self {
            _library: library,
            get_stats,
            monitor: StatsMonitor::default(),
        };
        lease.check()?;
        Ok(Some(lease))
    }

    /// Read the complete 48-byte ABI under the backend's own mutex. Invalid
    /// samples/counter regressions latch an error for the rest of the lease.
    /// This observes queuing only; no sample proves a Wine device or delivery.
    pub fn snapshot(&mut self) -> Result<OutputStats, String> {
        if let Some(error) = &self.monitor.failure {
            return Err(error.clone());
        }
        let mut stats = OutputStats::default();
        // SAFETY: the callable lives inside our still-owned library reference,
        // and the pointer refers to a fully initialized, exact-size C struct.
        let ok = unsafe { (self.get_stats)(&mut stats, STATS_BYTES) };
        self.monitor.observe(if ok {
            Ok(stats)
        } else {
            Err("Own output backend rejected its stats read".into())
        })
    }

    /// Fail on any observed loss, including a loss that happened before the
    /// lease was acquired. Callers must disconnect the relay on this error.
    pub fn check(&mut self) -> Result<OutputStats, String> {
        let stats = self.snapshot()?;
        if stats.had_loss() {
            return Err(format!(
                "Own output backend lost input: failed={}, generation={}, duplicate_creates={}",
                stats.failed_frames, stats.loss_generation, stats.rejected_active_creates
            ));
        }
        Ok(stats)
    }
}

fn selected_library(state: &Path) -> Result<PathBuf, String> {
    regular_directory(state)?;
    regular_directory(&state.join("runtime"))?;
    let provenance: Provenance = serde_json::from_slice(&regular_bytes(&state.join(PROVENANCE))?)
        .map_err(|e| format!("Invalid own output provenance: {e}"))?;
    if provenance.schema_version != 1
        || provenance.backend != "wine"
        || provenance.original.file != "runtime/libsteaminput-valve.dylib"
        || provenance.original.sha256 != ORIGINAL_SHA256
        || provenance.selected.file != "runtime/libsteaminput.dylib"
        || !provenance.original_preserved
        || provenance.callback_success_means != "owned_datagram_queued_only"
        || provenance.os_hid_created
    {
        return Err("Runtime did not explicitly stage the own Wine output backend".into());
    }
    let selected = state.join("runtime/libsteaminput.dylib");
    let bytes = regular_bytes(&selected)?;
    if !macho(&bytes) || digest(&bytes) != provenance.selected.sha256 {
        return Err("Selected own output library differs from its staged provenance".into());
    }
    selected.canonicalize().map_err(|e| e.to_string())
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "skinos-wine-output-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn wire(kind: u32, len: u32) -> [u8; 288] {
        let mut b = [0; 288];
        for (n, v) in [MAGIC, 1, kind, 0, len, 0x045e, 0x028e, 0]
            .into_iter()
            .enumerate()
        {
            b[n * 4..n * 4 + 4].copy_from_slice(&v.to_le_bytes())
        }
        b
    }
    #[test]
    fn report_bytes_preserved_and_bad_headers_rejected() {
        let mut b = wire(2, 20);
        for (i, v) in b[32..52].iter_mut().enumerate() {
            *v = (i * 7) as u8;
        }
        let frame = Frame::parse(&b).unwrap();
        assert_eq!(frame.payload(), &b[32..52]);
        assert_eq!(frame.as_bytes(), &b);
        for word in [0, 1, 2, 3, 4, 5, 6, 7] {
            let mut bad = b;
            bad[word * 4..word * 4 + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            assert!(Frame::parse(&bad).is_err());
        }
        b[287] = 1;
        assert!(Frame::parse(&b).is_err());
        assert!(Frame::parse(&b[..287]).is_err());
        assert!(Frame::parse(&wire(1, 188)).is_err());
        assert_eq!(Frame::parse(&wire(3, 0)).unwrap().kind, Kind::Destroy);
    }
    #[test]
    fn staging_preserves_original_and_records_truthful_provenance() {
        let t = Temp::new();
        fs::create_dir(t.0.join("runtime")).unwrap();
        let original = b"labelled original test fixture";
        fs::write(t.0.join("runtime/libsteaminput.dylib"), original).unwrap();
        let backend = t.0.join("own-backend.dylib");
        let mut code = vec![0; 64];
        code[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        fs::write(&backend, &code).unwrap();
        let p = stage_files(&t.0, &backend, &digest(original)).unwrap();
        assert_eq!(
            fs::read(t.0.join("runtime/libsteaminput-valve.dylib")).unwrap(),
            original
        );
        assert_eq!(
            fs::read(t.0.join("runtime/libsteaminput.dylib")).unwrap(),
            code
        );
        assert_eq!(p.selected.sha256, digest(&code));
        assert!(!p.os_hid_created);
        assert_eq!(
            serde_json::from_slice::<Provenance>(&fs::read(t.0.join(PROVENANCE)).unwrap()).unwrap(),
            p
        );
        assert!(stage_files(&t.0, &backend, &digest(original)).is_err());
    }
    #[test]
    fn source_symlink_and_wrong_original_fail_before_selection() {
        let t = Temp::new();
        fs::create_dir(t.0.join("runtime")).unwrap();
        fs::write(t.0.join("runtime/libsteaminput.dylib"), b"wrong").unwrap();
        let actual = t.0.join("actual");
        fs::write(&actual, [0u8; 64]).unwrap();
        let linked = t.0.join("linked");
        symlink(&actual, &linked).unwrap();
        assert!(stage_files(&t.0, &linked, ORIGINAL_SHA256).is_err());
        assert!(!t.0.join("runtime/libsteaminput-valve.dylib").exists());
        let mut code = vec![0; 64];
        code[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        fs::write(&actual, code).unwrap();
        assert!(stage_files(&t.0, &actual, ORIGINAL_SHA256).is_err());
        assert_eq!(
            fs::read(t.0.join("runtime/libsteaminput.dylib")).unwrap(),
            b"wrong"
        );
        assert!(!t.0.join("runtime/libsteaminput-valve.dylib").exists());
    }
    #[test]
    fn channel_receiver_is_single_owner_and_frames_roundtrip() {
        let mut channel = Channel::pair().unwrap();
        let recv = channel.take_receiver().unwrap();
        assert!(channel.take_receiver().is_none());
        let b = wire(3, 0);
        channel.writer.send(&b).unwrap();
        let mut received = [0; 289];
        let size = recv.recv(&mut received).unwrap();
        assert_eq!(Frame::parse(&received[..size]).unwrap().as_bytes(), &b);
        for fd in [channel.writer_fd(), recv.as_raw_fd()] {
            assert_ne!(unsafe { fcntl(fd, 1, 0) } & 1, 0);
        }
        channel.writer.send(&[0; FRAME_BYTES + 16]).unwrap();
        assert!(Frame::receive(&recv).is_err());
    }
    #[test]
    fn child_fd_closed() {
        if let Ok(fd) = std::env::var("SKINOS_TEST_OUTPUT_FD") {
            let fd = fd.parse::<i32>().unwrap();
            assert_eq!(unsafe { fcntl(fd, 1, 0) }, -1);
        }
    }
    #[test]
    fn channel_writer_does_not_leak_across_exec() {
        let channel = Channel::pair().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "wine_input_output::tests::child_fd_closed",
                "--nocapture",
            ])
            .env("SKINOS_TEST_OUTPUT_FD", channel.writer_fd().to_string())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }
    #[test]
    fn default_policy_is_noop_and_diagnostic_gate_is_required() {
        assert_eq!(
            OutputSelection::for_request(None, None).unwrap(),
            OutputSelection::Disabled
        );
        assert_eq!(
            OutputSelection::for_request(None, Some("")).unwrap(),
            OutputSelection::Disabled
        );
        assert!(OutputSelection::for_request(None, Some("wine")).is_err());
        assert!(OutputSelection::for_request(None, Some("other")).is_err());
        let request =
            serde_json::json!({"command":"launch","compatibility":{},"input_diagnostic":true});
        assert_eq!(
            OutputSelection::for_request(Some(&request), Some("wine")).unwrap(),
            OutputSelection::Diagnostic
        );
        assert!(
            OutputSelection::Diagnostic
                .require_ready(true, false)
                .is_ok()
        );
        assert_eq!(
            OutputSelection::for_request(Some(&request), None).unwrap(),
            OutputSelection::Disabled
        );
    }

    #[test]
    fn product_setting_selects_typed_mode_but_cannot_skip_readiness_gate() {
        let mut request =
            serde_json::json!({"command":"launch","compatibility":{},"steam_input":true});
        let game = OutputSelection::for_request(Some(&request), None).unwrap();
        assert_eq!(game, OutputSelection::Game);
        assert!(game.require_ready(false, false).is_err());
        // An inherited diagnostic override never unlocks the game path.
        assert!(game.require_ready(true, false).is_err());
        assert!(game.require_ready(false, true).is_ok());
        request["input_diagnostic"] = serde_json::json!(true);
        let diagnostic = OutputSelection::for_request(Some(&request), None).unwrap();
        assert_eq!(diagnostic, OutputSelection::ProductDiagnostic);
        assert!(diagnostic.require_ready(false, false).is_err());
        assert!(diagnostic.require_ready(true, false).is_err());
        assert!(diagnostic.require_ready(false, true).is_ok());
        let explicit = OutputSelection::for_request(Some(&request), Some("wine")).unwrap();
        assert_eq!(explicit, OutputSelection::Diagnostic);
        assert!(explicit.require_ready(true, false).is_ok());
        assert!(!explicit.uses_production_preparation());
        assert!(diagnostic.uses_production_preparation());
    }

    #[test]
    fn mode_cannot_drift_from_its_original_windows_request() {
        let request = serde_json::json!({"command":"launch","compatibility":{},"steam_input":true});
        assert!(
            OutputSelection::Disabled
                .validate_request(&request)
                .is_err()
        );
        assert!(
            OutputSelection::Diagnostic
                .validate_request(&request)
                .is_err()
        );
        assert!(OutputSelection::Game.validate_request(&request).is_ok());
        for patch in [
            serde_json::json!({"steam_input":"true"}),
            serde_json::json!({"input_diagnostic":1}),
            serde_json::json!({"command":"authenticate"}),
            serde_json::json!({"compatibility":null}),
        ] {
            let mut invalid = request.clone();
            for (key, value) in patch.as_object().unwrap() {
                invalid[key] = value.clone();
            }
            assert!(OutputSelection::for_request(Some(&invalid), None).is_err());
        }
        let request =
            serde_json::json!({"command":"launch","compatibility":{},"input_diagnostic":true});
        assert!(OutputSelection::Game.validate_request(&request).is_err());
    }

    #[test]
    fn worker_handoff_is_explicit_and_rejects_inherited_backend_or_nonlive_mode() {
        for (name, expected) in [
            ("disabled", OutputSelection::Disabled),
            ("diagnostic", OutputSelection::Diagnostic),
            ("product_diagnostic", OutputSelection::ProductDiagnostic),
            ("game", OutputSelection::Game),
        ] {
            let backend = expected.enabled().then_some("wine");
            assert_eq!(
                OutputSelection::parse_worker_handoff(Some(name), backend, true).unwrap(),
                expected
            );
            if expected.enabled() {
                assert!(OutputSelection::parse_worker_handoff(Some(name), None, true).is_err());
                assert!(OutputSelection::parse_worker_handoff(Some(name), backend, false).is_err());
            }
        }
        assert!(OutputSelection::parse_worker_handoff(None, Some("wine"), true).is_err());
        assert!(
            OutputSelection::parse_worker_handoff(Some("disabled"), Some("wine"), true).is_err()
        );
        assert!(OutputSelection::parse_worker_handoff(Some("unknown"), None, true).is_err());
    }
    #[test]
    fn stats_loss_is_fail_closed() {
        assert_eq!(std::mem::size_of::<OutputStats>(), 48);
        let mut s = OutputStats {
            version: 1,
            bytes: 48,
            ..OutputStats::default()
        };
        assert!(s.validate().is_ok());
        assert!(!s.had_loss());
        s.loss_generation = 1;
        assert!(s.had_loss());
        s.bytes = 40;
        assert!(s.validate().is_err());
    }

    fn stats() -> OutputStats {
        OutputStats {
            version: 1,
            bytes: STATS_BYTES,
            ..OutputStats::default()
        }
    }

    #[test]
    fn stats_reject_incompatible_abi_and_all_loss_signals() {
        assert!(OutputStats::default().validate().is_err());
        for invalid in [
            OutputStats {
                version: 2,
                ..stats()
            },
            OutputStats {
                bytes: 47,
                ..stats()
            },
            OutputStats {
                bytes: 49,
                ..stats()
            },
            OutputStats {
                reserved: 1,
                ..stats()
            },
            OutputStats {
                active_slot_mask: 1 << 16,
                ..stats()
            },
        ] {
            assert!(invalid.validate().is_err());
        }
        for lossy in [
            OutputStats {
                failed_frames: 1,
                ..stats()
            },
            OutputStats {
                loss_generation: 1,
                ..stats()
            },
            OutputStats {
                rejected_active_creates: 1,
                ..stats()
            },
        ] {
            // One bad counter alone is enough to disconnect, even if a future
            // backend bug stopped incrementing the other loss indicators.
            assert!(lossy.validate().is_ok());
            assert!(lossy.had_loss());
        }
    }

    #[test]
    fn stats_monitor_latches_counter_reset_and_failed_read() {
        let initial = OutputStats {
            queued_frames: 7,
            failed_frames: 2,
            loss_generation: 2,
            rejected_active_creates: 1,
            active_slot_mask: 0xffff,
            ..stats()
        };
        for regressed in [
            OutputStats {
                queued_frames: 6,
                ..initial
            },
            OutputStats {
                failed_frames: 1,
                ..initial
            },
            OutputStats {
                loss_generation: 1,
                ..initial
            },
            OutputStats {
                rejected_active_creates: 0,
                ..initial
            },
        ] {
            let mut monitor = StatsMonitor::default();
            monitor.observe(Ok(initial)).unwrap();
            let error = monitor.observe(Ok(regressed)).unwrap_err();
            assert!(error.contains("regressed"));
            // A later apparently valid sample must not erase a dropped read
            // or reset counters from a different loaded library lifetime.
            assert_eq!(monitor.observe(Ok(initial)).unwrap_err(), error);
        }
        let mut monitor = StatsMonitor::default();
        assert_eq!(
            monitor.observe(Err("read failed".into())).unwrap_err(),
            "read failed"
        );
        assert_eq!(monitor.observe(Ok(stats())).unwrap_err(), "read failed");
    }

    #[test]
    fn stats_monitor_allows_real_device_removal_but_latches_bad_abi() {
        let mut monitor = StatsMonitor::default();
        let created = OutputStats {
            queued_frames: 1,
            active_slot_mask: 8,
            ..stats()
        };
        let removed = OutputStats {
            queued_frames: 2,
            active_slot_mask: 0,
            ..stats()
        };
        assert_eq!(monitor.observe(Ok(created)).unwrap(), created);
        assert_eq!(monitor.observe(Ok(removed)).unwrap(), removed);
        assert!(
            monitor
                .observe(Ok(OutputStats {
                    bytes: 40,
                    ..removed
                }))
                .is_err()
        );
        assert!(monitor.observe(Ok(removed)).is_err());
    }
}
