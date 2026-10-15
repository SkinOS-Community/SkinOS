//! Owned datagram -> Wine feeder transport. IOCTL acknowledgements are not game
//! input evidence. No frame payloads or user input history are persisted.
#![allow(dead_code)] // The caller wires this only for the opt-in Wine output backend.
use crate::wine_input_output::{FRAME_BYTES, Frame, Kind};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    ffi::c_int,
    fs::{self, OpenOptions},
    io::{Read, Write},
    net::Shutdown,
    os::{
        fd::AsRawFd,
        unix::{fs::OpenOptionsExt, net::UnixDatagram, process::ExitStatusExt},
    },
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const RUNNING: u8 = 0;
const DRAINING: u8 = 1;
const ABORTING: u8 = 2;
const MAX_PENDING: usize = 256;
const MAX_LINE: usize = 4096;
const MAX_STATUS_BYTES: u64 = 8 * 1024 * 1024;
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const ACK_TIMEOUT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(3);
const RECEIVE_TIMEOUT: Duration = Duration::from_millis(25);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
const FEEDBACK_POLL: Duration = Duration::from_millis(25);
const FEEDBACK_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_FEEDBACK_EVENTS: usize = 128;
const MAX_FEEDBACK_BATCH: usize = 8;
const MAX_FEEDBACK_BURST_QUERIES: usize = 4;
const FEEDBACK_BYTE_WINDOW: Duration = Duration::from_secs(1);
const MAX_FEEDBACK_WINDOW_BYTES: u64 = 256 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FeedbackSelection {
    #[default]
    Disabled,
    Experimental {
        epoch: u64,
    },
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub struct FeedbackBinding {
    pub epoch: u64,
    pub control_session: u32,
    pub slot: u32,
    pub generation: u32,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub struct FeedbackRumble {
    pub binding: FeedbackBinding,
    /// Actual driver sequence, distinct from any downstream SDK command ordinal.
    pub sequence: u64,
    pub left: u16,
    pub right: u16,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub enum FeedbackEvent {
    Bound(FeedbackBinding),
    Rumble(FeedbackRumble),
    Retired(FeedbackBinding),
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct FeedbackReport {
    pub selected: bool,
    pub enabled: bool,
    pub epoch: Option<u64>,
    pub stop_required: bool,
    pub error: Option<String>,
    pub queries_forwarded: u64,
    pub replies_received: u64,
    pub outputs_received: u64,
    pub events_taken: u64,
    pub stale_outputs_discarded: u64,
    pub pending_events: usize,
    pub pending_query: bool,
    pub active_bindings: usize,
    pub helper_control_bytes: u64,
    pub status_bytes: u64,
    pub driver_loss_latched: bool,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct FeedbackBatch {
    pub events: Vec<FeedbackEvent>,
    pub report: FeedbackReport,
}
#[derive(Default)]
struct FeedbackState {
    report: FeedbackReport,
    bindings: [Option<FeedbackBinding>; 16],
    last_generation: [u32; 16],
    control_session: Option<u32>,
    last_sequence: u64,
    events: VecDeque<FeedbackEvent>,
    query: Option<Instant>,
    last_query: Option<Instant>,
    backlog: bool,
    burst_queries: usize,
    byte_window_start: Option<Instant>,
    byte_window_bytes: u64,
}
impl FeedbackState {
    fn new(selection: FeedbackSelection) -> Result<Self, String> {
        let mut result = Self::default();
        if let FeedbackSelection::Experimental { epoch } = selection {
            if epoch == 0 {
                return Err("Experimental feedback requires a nonzero connection epoch".into());
            }
            result.report.selected = true;
            result.report.enabled = true;
            result.report.epoch = Some(epoch);
        }
        Ok(result)
    }
    fn disable(&mut self, reason: impl Into<String>) {
        if !self.report.selected {
            return;
        }
        self.report.enabled = false;
        self.report.stop_required = true;
        if self.report.error.is_none() {
            self.report.error = Some(reason.into().chars().take(300).collect());
        }
        self.events.clear(); // Never deliver queued nonzero motors after loss.
        self.query = None;
        self.backlog = false;
    }
    fn snapshot(&self) -> FeedbackReport {
        let mut result = self.report.clone();
        result.pending_events = self.events.len();
        result.pending_query = self.query.is_some();
        result.active_bindings = self.bindings.iter().filter(|b| b.is_some()).count();
        result
    }
    fn enqueue(&mut self, event: FeedbackEvent) {
        if !self.report.enabled {
            return;
        }
        if self.events.len() >= MAX_FEEDBACK_EVENTS {
            self.disable("Rumble owner-thread event queue overflowed");
        } else {
            self.events.push_back(event);
        }
    }
    fn bind(&mut self, value: &Value, slot: u32) {
        if !self.report.enabled {
            return;
        }
        let parse = || -> Result<FeedbackBinding, String> {
            let control_session = feedback_u32(value, "control_session")?;
            let generation = feedback_u32(value, "generation")?;
            if value["driver_binding_verified"] != true
                || control_session == 0
                || generation == 0
                || slot >= 16
            {
                return Err("Create ACK lacks an authoritative rumble binding".into());
            }
            Ok(FeedbackBinding {
                epoch: self.report.epoch.expect("selected feedback"),
                control_session,
                slot,
                generation,
            })
        };
        let binding = match parse() {
            Ok(binding) => binding,
            Err(error) => {
                self.disable(error);
                return;
            }
        };
        if self.bindings[slot as usize].is_some()
            || binding.generation <= self.last_generation[slot as usize]
            || self
                .control_session
                .is_some_and(|session| session != binding.control_session)
        {
            self.disable("Create ACK reused a generation/session or preceded retirement");
            return;
        }
        self.control_session = Some(binding.control_session);
        self.last_generation[slot as usize] = binding.generation;
        self.bindings[slot as usize] = Some(binding);
        self.enqueue(FeedbackEvent::Bound(binding));
    }
    fn retire(&mut self, slot: u32) {
        if let Some(binding) = self.bindings[slot as usize].take() {
            // Preserve Bound -> Retired -> next Bound order, but stale motor
            // requests must not survive until an owner-thread drain after unplug.
            let before = self.events.len();
            self.events.retain(
                |event| !matches!(event, FeedbackEvent::Rumble(value) if value.binding == binding),
            );
            self.report.stale_outputs_discarded += (before - self.events.len()) as u64;
            self.enqueue(FeedbackEvent::Retired(binding));
        }
    }
    fn check_timeout(&mut self) {
        if self
            .query
            .is_some_and(|started| started.elapsed() > FEEDBACK_TIMEOUT)
        {
            self.disable("Wine feedback query timed out; input transport remains active");
        }
    }
    fn reserve_query(&mut self) -> bool {
        self.check_timeout();
        if !self.report.enabled
            || self.query.is_some()
            || !self.bindings.iter().any(Option::is_some)
            || (!(self.backlog && self.burst_queries < MAX_FEEDBACK_BURST_QUERIES)
                && self
                    .last_query
                    .is_some_and(|last| last.elapsed() < FEEDBACK_POLL))
        {
            return false;
        }
        let now = Instant::now();
        if !self.backlog || self.burst_queries >= MAX_FEEDBACK_BURST_QUERIES {
            self.burst_queries = 0;
        }
        self.burst_queries += 1;
        self.query = Some(now);
        self.last_query = Some(now);
        true
    }
    fn receive_timeout(&self) -> Duration {
        if self.report.enabled
            && (self.query.is_some()
                || (self.backlog && self.burst_queries < MAX_FEEDBACK_BURST_QUERIES))
        {
            POLL
        } else {
            RECEIVE_TIMEOUT
        }
    }
    fn charge_status_bytes(&mut self, bytes: usize, now: Instant) {
        // Total volume remains telemetry, never a session-duration limit.
        self.report.status_bytes = self.report.status_bytes.saturating_add(bytes as u64);
        if self
            .byte_window_start
            .is_none_or(|start| now.saturating_duration_since(start) >= FEEDBACK_BYTE_WINDOW)
        {
            self.byte_window_start = Some(now);
            self.byte_window_bytes = 0;
        }
        self.byte_window_bytes = self.byte_window_bytes.saturating_add(bytes as u64);
        if self.byte_window_bytes > MAX_FEEDBACK_WINDOW_BYTES {
            self.disable("Wine feedback exceeded its bounded byte-rate window");
        }
    }
    fn reply(&mut self, value: &Value) {
        if !self.report.enabled {
            return;
        }
        if self.query.take().is_none() {
            self.disable("Unsolicited or duplicate Wine feedback reply");
            return;
        }
        self.report.replies_received += 1;
        if value["ok"] != true || value["win32_error"].as_u64() != Some(0) {
            if value["loss_latched"] == true && value["win32_error"].as_u64() == Some(23) {
                self.report.driver_loss_latched = true;
                self.disable(
                    "Wine driver latched lost feedback; stop motors until a fresh owner session",
                );
                return;
            }
            self.disable(format!(
                "Wine feedback query failed (Win32 {})",
                value["win32_error"].as_u64().unwrap_or(u64::MAX)
            ));
            return;
        }
        let parsed = (|| -> Result<(Vec<FeedbackRumble>, bool), String> {
            let available = value["available"]
                .as_bool()
                .ok_or("Wine feedback availability is missing")?;
            let (rows, backlog): (Vec<&Value>, bool) =
                if value.get("records").is_some() || value.get("batch_version").is_some() {
                    let records = value["records"]
                        .as_array()
                        .ok_or("Wine feedback batch records missing")?;
                    if value["batch_version"] != 1
                        || records.len() > MAX_FEEDBACK_BATCH
                        || available == records.is_empty()
                        || value["drained"].as_bool() != Some(records.len() < MAX_FEEDBACK_BATCH)
                    {
                        return Err("Malformed bounded Wine feedback batch".into());
                    }
                    (
                        records.iter().collect(),
                        records.len() == MAX_FEEDBACK_BATCH,
                    )
                } else if available {
                    // Old already-verified single-record helper remains readable;
                    // all newly built helpers use one bounded batch envelope.
                    (vec![value], true)
                } else {
                    (Vec::new(), false)
                };
            let mut result = Vec::with_capacity(rows.len());
            let mut sequence = self.last_sequence;
            for row in rows {
                let output = self.parse_record(row, sequence)?;
                sequence = output.sequence;
                result.push(output);
            }
            if self.events.len() + result.len() > MAX_FEEDBACK_EVENTS {
                return Err("Rumble owner-thread event queue overflowed".into());
            }
            Ok((result, backlog))
        })();
        match parsed {
            Ok((outputs, backlog)) => {
                // Commit only after every row, binding, sequence and queue
                // bound passes. No prefix of a corrupt batch can reach SDK.
                self.backlog = backlog;
                if !backlog {
                    self.burst_queries = 0;
                }
                if let Some(last) = outputs.last() {
                    self.last_sequence = last.sequence;
                }
                self.report.outputs_received += outputs.len() as u64;
                self.events
                    .extend(outputs.into_iter().map(FeedbackEvent::Rumble));
            }
            Err(error) => self.disable(error),
        }
    }
    fn parse_record(&self, value: &Value, after_sequence: u64) -> Result<FeedbackRumble, String> {
        let slot = feedback_u32(value, "slot")?;
        let session = feedback_u32(value, "control_session")?;
        let generation = feedback_u32(value, "generation")?;
        let low = feedback_u32(value, "sequence_low")?;
        let high = feedback_u32(value, "sequence_high")?;
        let left = u16::try_from(feedback_u32(value, "left")?)
            .map_err(|_| "Rumble left motor exceeds u16")?;
        let right = u16::try_from(feedback_u32(value, "right")?)
            .map_err(|_| "Rumble right motor exceeds u16")?;
        let sequence = u64::from(low) | (u64::from(high) << 32);
        if slot >= 16
            || session == 0
            || generation == 0
            || sequence <= after_sequence
            || value["kind"].as_u64() != Some(1)
            || value["byte_size"].as_u64() != Some(48)
            || value["report_id"].as_u64() != Some(1)
            || value["hid_output_dequeued"] != true
            || value["sdk_dispatched"] != false
            || value["physical_device_verified"] != false
        {
            return Err("Malformed, replayed or unproven Wine feedback record".into());
        }
        let binding =
            self.bindings[slot as usize].ok_or("Feedback arrived for an unbound/retired slot")?;
        if binding.control_session != session || binding.generation != generation {
            return Err("Feedback belongs to a stale generation or control session".into());
        }
        Ok(FeedbackRumble {
            binding,
            sequence,
            left,
            right,
        })
    }
    fn drain(&mut self, limit: usize) -> FeedbackBatch {
        self.check_timeout();
        let count = limit.min(MAX_FEEDBACK_EVENTS).min(self.events.len());
        let events: Vec<_> = self.events.drain(..count).collect();
        self.report.events_taken += events.len() as u64;
        FeedbackBatch {
            events,
            report: self.snapshot(),
        }
    }
}
fn feedback_u32(value: &Value, key: &str) -> Result<u32, String> {
    value[key]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| format!("Invalid feedback {key}"))
}

/// Helper control bytes, deliberately NOT a producer Frame and never hashed.
fn feedback_query_bytes() -> [u8; FRAME_BYTES] {
    let mut bytes = [0; FRAME_BYTES];
    for (index, value) in [0x53494e50_u32, 1, 4].iter().enumerate() {
        bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes
}

unsafe extern "C" {
    fn fcntl(fd: c_int, command: c_int, ...) -> c_int;
}
fn protect_fd(fd: c_int, nonblocking: bool) -> Result<(), String> {
    unsafe {
        let flags = fcntl(fd, 1, 0);
        if flags < 0 || fcntl(fd, 2, flags | 1) < 0 {
            return Err("Cannot set relay descriptor close-on-exec".into());
        }
        let applied = fcntl(fd, 1);
        if applied < 0 || applied & 1 == 0 {
            return Err("Relay descriptor close-on-exec did not take effect".into());
        }
        if nonblocking {
            #[cfg(target_os = "linux")]
            const NONBLOCK: c_int = 0x800;
            #[cfg(not(target_os = "linux"))]
            const NONBLOCK: c_int = 0x4;
            let flags = fcntl(fd, 3, 0);
            if flags < 0 || fcntl(fd, 4, flags | NONBLOCK) < 0 {
                return Err("Cannot make relay pipe nonblocking".into());
            }
            let applied = fcntl(fd, 3);
            if applied < 0 || applied & NONBLOCK == 0 {
                return Err("Relay pipe nonblocking mode did not take effect".into());
            }
        }
    }
    Ok(())
}
fn kind_number(kind: Kind) -> u32 {
    match kind {
        Kind::Create => 1,
        Kind::Report => 2,
        Kind::Destroy => 3,
    }
}

struct Expected {
    kind: u32,
    slot: u32,
    hash: [u8; 32],
    queued: Instant,
}
#[derive(Default)]
struct State {
    ready: bool,
    root_readiness_seen: bool,
    closed: bool,
    command_complete: bool,
    stdin_closed: bool,
    reader_done: bool,
    writer_done: bool,
    received: u64,
    forwarded: u64,
    acknowledged: u64,
    acknowledged_creates: u64,
    acknowledged_destroys: u64,
    cleanup_acknowledged: u64,
    status_bytes: u64,
    pending: VecDeque<Expected>,
    intended_slots: u16,
    driver_slots: u16,
    received_hash: Sha256,
    forwarded_hash: Sha256,
    acknowledged_hash: Sha256,
    error: Option<String>,
    feedback: FeedbackState,
}
struct Shared {
    state: Mutex<State>,
    mode: AtomicU8,
    disconnect: UnixDatagram,
    child: Mutex<Child>,
}
impl Shared {
    fn fail(&self, message: impl Into<String>) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.error.is_none() {
            state.error = Some(message.into().chars().take(300).collect());
        }
        let reason = state
            .error
            .clone()
            .unwrap_or_else(|| "Input transport failed".into());
        state
            .feedback
            .disable(format!("Input transport unavailable: {reason}"));
        drop(state);
        self.mode.store(ABORTING, Ordering::SeqCst);
        let _ = self.disconnect.shutdown(Shutdown::Both);
        // Child::kill is safe after try_wait has reaped the same Child object.
        let _ = self
            .child
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .kill();
    }
    fn error(&self) -> Option<String> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .error
            .clone()
    }
    fn check_ack_timeout(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.feedback.check_timeout();
        let expired = state
            .pending
            .front()
            .is_some_and(|entry| entry.queued.elapsed() > ACK_TIMEOUT);
        drop(state);
        if expired {
            self.fail("Wine feeder acknowledgement timed out");
        }
    }
    fn reserve(&self, frame: &Frame) -> Result<[u8; 32], String> {
        let mut state = self.state.lock().map_err(|_| "Relay state lock poisoned")?;
        if !state.ready || state.closed {
            return Err("Frame arrived outside the ready feeder session".into());
        }
        if state.pending.len() >= MAX_PENDING {
            return Err("Wine feeder acknowledgement queue is full".into());
        }
        let mask = 1_u16 << frame.slot;
        let active = state.intended_slots & mask != 0;
        if matches!(frame.kind, Kind::Create) == active {
            return Err("Input frame violates create/report/destroy order".into());
        }
        match frame.kind {
            Kind::Create => state.intended_slots |= mask,
            Kind::Destroy => state.intended_slots &= !mask,
            Kind::Report => {}
        }
        let hash: [u8; 32] = Sha256::digest(frame.as_bytes()).into();
        state.received += 1;
        state.received_hash.update(hash);
        state.pending.push_back(Expected {
            kind: kind_number(frame.kind),
            slot: frame.slot,
            hash,
            queued: Instant::now(),
        });
        Ok(hash)
    }
    fn accept_status(&self, value: Value) -> Result<(), String> {
        let event = value["event"]
            .as_str()
            .ok_or("Feeder status event is missing")?;
        if event == "wine_input_feedback" {
            let mut state = self.state.lock().map_err(|_| "Relay state lock poisoned")?;
            if !state.feedback.report.selected {
                return Err("Feedback status arrived without experimental selection".into());
            }
            state.feedback.reply(&value);
            return Ok(());
        }
        if value["ok"].as_bool() != Some(true) || value["win32_error"].as_u64() != Some(0) {
            return Err(format!(
                "Wine feeder reported failure (Win32 code {})",
                value["win32_error"].as_u64().unwrap_or(u64::MAX)
            ));
        }
        let mut state = self.state.lock().map_err(|_| "Relay state lock poisoned")?;
        match event {
            "wine_input_root_readiness" => {
                if state.ready
                    || state.root_readiness_seen
                    || value["state_read"] != true
                    || value["root_present"] != 1
                    || value["root_started"] != 1
                    || value["root_removed"] != 0
                {
                    return Err("Invalid Wine input root readiness status".into());
                }
                state.root_readiness_seen = true;
            }
            "wine_input_ready" => {
                if !state.root_readiness_seen || state.ready || state.closed {
                    return Err("Wine feeder readiness requires verified own root readiness".into());
                }
                state.ready = true;
            }
            "wine_input_ack" => {
                if !state.ready || state.closed {
                    return Err("Wine feeder acknowledgement outside active session".into());
                }
                let expected = state
                    .pending
                    .front()
                    .ok_or("Wine feeder acknowledged an unqueued frame")?;
                if value["kind"].as_u64() != Some(u64::from(expected.kind))
                    || value["slot"].as_u64() != Some(u64::from(expected.slot))
                {
                    return Err("Wine feeder acknowledgement kind/slot/order mismatch".into());
                }
                let expected = state.pending.pop_front().expect("validated queue front");
                let mask = 1_u16 << expected.slot;
                match expected.kind {
                    1 => {
                        state.driver_slots |= mask;
                        state.acknowledged_creates += 1;
                        state.feedback.bind(&value, expected.slot);
                    }
                    3 => {
                        state.driver_slots &= !mask;
                        state.acknowledged_destroys += 1;
                        state.feedback.retire(expected.slot);
                    }
                    _ => {}
                }
                state.acknowledged += 1;
                state.acknowledged_hash.update(expected.hash);
            }
            "wine_input_cleanup" => {
                if self.mode.load(Ordering::SeqCst) != DRAINING
                    || !state.stdin_closed
                    || !state.pending.is_empty()
                    || state.closed
                    || state.driver_slots == 0
                {
                    return Err("Unexpected feeder cleanup acknowledgement".into());
                }
                let slot = state.driver_slots.trailing_zeros();
                if value["kind"].as_u64() != Some(3)
                    || value["slot"].as_u64() != Some(u64::from(slot))
                {
                    return Err("Wine feeder cleanup slot/order mismatch".into());
                }
                state.driver_slots &= !(1_u16 << slot);
                state.cleanup_acknowledged += 1;
                state.feedback.retire(slot);
            }
            "wine_input_closed" => {
                if self.mode.load(Ordering::SeqCst) != DRAINING
                    || !state.stdin_closed
                    || !state.pending.is_empty()
                    || state.driver_slots != 0
                    || !state.ready
                    || state.closed
                {
                    return Err("Wine feeder closed before ordered input drain completed".into());
                }
                state.closed = true;
                if state.feedback.report.selected {
                    state.feedback.report.enabled = false;
                    state.feedback.report.stop_required = true;
                }
            }
            "wine_input_command_complete" => {
                if !state.closed || state.command_complete {
                    return Err("Unexpected feeder command completion".into());
                }
                state.command_complete = true;
            }
            _ => return Err("Unknown Wine input feeder status".into()),
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct RelayReport {
    pub schema_version: u32,
    pub ok: bool,
    pub ready: bool,
    pub root_readiness_verified: bool,
    pub closed: bool,
    pub command_complete: bool,
    pub stdin_closed: bool,
    pub frames_received: u64,
    pub frames_forwarded: u64,
    pub frames_acknowledged: u64,
    pub cleanup_destroy_acknowledgements: u64,
    pub pending_acknowledgements: usize,
    pub bytes_forwarded: u64,
    pub received_hash_chain_sha256: String,
    pub forwarded_hash_chain_sha256: String,
    pub acknowledged_hash_chain_sha256: String,
    pub producer_active_slots: u16,
    pub driver_active_slots: u16,
    pub threads_joined: bool,
    pub child_reaped: bool,
    pub exit_code: Option<i32>,
    pub termination_signal: Option<i32>,
    pub acknowledgement_means: &'static str,
    pub game_input_verified: bool,
    pub error: Option<String>,
    pub feedback: FeedbackReport,
}

/// Read-only acknowledgement progress. Cleanup on EOF is deliberately excluded
/// from `destroy_acknowledgements`: reconnect needs an actual producer Destroy.
#[derive(Clone, Copy, Debug, Default)]
pub struct RelayProgress {
    pub received: u64,
    pub forwarded: u64,
    pub acknowledged: u64,
    pub create_acknowledgements: u64,
    pub destroy_acknowledgements: u64,
    pub pending: usize,
    pub intended_slots: u16,
    pub driver_slots: u16,
}

pub struct Relay {
    shared: Arc<Shared>,
    writer: Option<JoinHandle<()>>,
    reader: Option<JoinHandle<()>>,
    owned_state: PathBuf,
    result: Option<RelayReport>,
}
impl Relay {
    /// The caller configures the exact NotProton executable/environment and
    /// closes the profile lock in pre_exec. No profile descriptor is retained here.
    /// The receiver must be the single receiver transferred from Channel.
    pub fn start(
        command: Command,
        receiver: UnixDatagram,
        state: &Path,
        timeout: Duration,
        pump: impl FnMut(),
    ) -> Result<Self, String> {
        Self::start_with_feedback(
            command,
            receiver,
            state,
            timeout,
            FeedbackSelection::Disabled,
            pump,
        )
    }
    /// Explicit QA-only feedback selection. No input frame or hash accounting
    /// changes; all SDK calls remain the caller/owner thread's responsibility.
    pub fn start_with_feedback(
        mut command: Command,
        receiver: UnixDatagram,
        state: &Path,
        timeout: Duration,
        selection: FeedbackSelection,
        mut pump: impl FnMut(),
    ) -> Result<Self, String> {
        let feedback = FeedbackState::new(selection)?;
        if !fs::symlink_metadata(state)
            .map_err(|error| error.to_string())?
            .is_dir()
        {
            return Err("Relay state must be a regular owned directory".into());
        }
        receiver
            .set_read_timeout(Some(RECEIVE_TIMEOUT))
            .map_err(|error| error.to_string())?;
        let disconnect = receiver.try_clone().map_err(|error| error.to_string())?;
        protect_fd(receiver.as_raw_fd(), false)?;
        protect_fd(disconnect.as_raw_fd(), false)?;
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|error| format!("Cannot start Wine input feeder: {error}"))?;
        drop(command);
        let input = child.stdin.take().expect("requested piped stdin");
        let output = child.stdout.take().expect("requested piped stdout");
        if let Err(error) =
            protect_fd(input.as_raw_fd(), true).and_then(|_| protect_fd(output.as_raw_fd(), true))
        {
            drop(input);
            drop(output);
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                feedback,
                ..State::default()
            }),
            mode: AtomicU8::new(RUNNING),
            disconnect,
            child: Mutex::new(child),
        });
        let mut relay = Self {
            shared: shared.clone(),
            writer: None,
            reader: None,
            owned_state: state.into(),
            result: None,
        };
        let sink = shared.clone();
        relay.reader = Some(
            thread::Builder::new()
                .name("wine-input-status".into())
                .spawn(move || read_status(output, sink))
                .map_err(|error| error.to_string())?,
        );
        let sink = shared.clone();
        relay.writer = Some(
            thread::Builder::new()
                .name("wine-input-forward".into())
                .spawn(move || forward_frames(receiver, input, sink))
                .map_err(|error| error.to_string())?,
        );
        let deadline = Instant::now() + timeout;
        loop {
            relay.check()?;
            if relay.shared.state.lock().unwrap().ready {
                return Ok(relay);
            }
            if Instant::now() >= deadline {
                relay.shared.fail("Wine input feeder readiness timed out");
                return Err("Wine input feeder readiness timed out".into());
            }
            pump();
            thread::sleep(POLL);
        }
    }
    pub fn current_error(&self) -> Option<String> {
        self.shared.check_ack_timeout();
        let status = self
            .shared
            .child
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .try_wait();
        match status {
            Ok(Some(_)) if self.shared.mode.load(Ordering::SeqCst) == RUNNING => {
                self.shared.fail("Wine input feeder exited unexpectedly")
            }
            Err(_) => self.shared.fail("Cannot inspect Wine input feeder process"),
            _ => {}
        }
        self.shared.error()
    }
    pub fn check(&self) -> Result<(), String> {
        self.current_error().map_or(Ok(()), Err)
    }
    /// Actual immutable completion evidence, present only after finish/abort.
    /// Inspect its resource/health fields; presence alone does not mean success.
    pub fn finished_report(&self) -> Option<RelayReport> {
        self.result.clone()
    }
    pub fn progress(&self) -> Result<RelayProgress, String> {
        self.check()?;
        let state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Ok(RelayProgress {
            received: state.received,
            forwarded: state.forwarded,
            acknowledged: state.acknowledged,
            create_acknowledgements: state.acknowledged_creates,
            destroy_acknowledgements: state.acknowledged_destroys,
            pending: state.pending.len(),
            intended_slots: state.intended_slots,
            driver_slots: state.driver_slots,
        })
    }
    /// Drain on the SDK owner thread. This never blocks on child IO or invokes
    /// Valve. A stop_required report must disable/zero the SDK sink before any
    /// new motor command; report.error is intentionally separate from input.
    pub fn take_feedback(&self, max_events: usize) -> FeedbackBatch {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .feedback
            .drain(max_events)
    }
    /// Strict owner-thread variant: None means the tiny state lock is busy;
    /// keep pumping the SDK and retry next turn, without inventing a status.
    pub fn try_take_feedback(&self, max_events: usize) -> Option<FeedbackBatch> {
        match self.shared.state.try_lock() {
            Ok(mut state) => Some(state.feedback.drain(max_events)),
            Err(std::sync::TryLockError::WouldBlock) => None,
            Err(std::sync::TryLockError::Poisoned(error)) => {
                let mut state = error.into_inner();
                state.feedback.disable("Rumble state lock poisoned");
                Some(state.feedback.drain(max_events))
            }
        }
    }
    pub fn disable_feedback(&self, reason: &str) {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .feedback
            .disable(reason);
    }
    pub fn feedback_report(&self) -> FeedbackReport {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.feedback.check_timeout();
        state.feedback.snapshot()
    }
    pub fn abort(&mut self, reason: &str) -> RelayReport {
        if let Some(report) = &self.result {
            return report.clone();
        }
        self.shared.fail(reason);
        self.complete(CLEANUP_TIMEOUT)
    }
    /// Call only AFTER Valve callbacks and engine teardown have finished. It
    /// drains remaining datagrams, closes stdin, validates cleanup/closed, waits
    /// for exit0 and joins both threads. This method never calls back into Valve.
    pub fn finish(&mut self, timeout: Duration) -> RelayReport {
        if let Some(report) = &self.result {
            return report.clone();
        }
        if self.shared.error().is_none() {
            self.shared.mode.store(DRAINING, Ordering::SeqCst);
        }
        self.complete(timeout)
    }
    fn complete(&mut self, timeout: Duration) -> RelayReport {
        let deadline = Instant::now() + timeout;
        loop {
            self.shared.check_ack_timeout();
            let done = self
                .shared
                .state
                .lock()
                .map(|s| s.writer_done && s.reader_done)
                .unwrap_or(false);
            let status = self
                .shared
                .child
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .try_wait();
            if done && matches!(status, Ok(Some(_))) {
                break;
            }
            if Instant::now() >= deadline {
                self.shared.fail("Wine input relay shutdown timed out");
                break;
            }
            thread::sleep(POLL);
        }
        if self.shared.mode.load(Ordering::SeqCst) == ABORTING {
            let _ = self
                .shared
                .child
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .kill();
        }
        let join_deadline = Instant::now() + CLEANUP_TIMEOUT;
        while self
            .writer
            .as_ref()
            .is_some_and(|thread| !thread.is_finished())
            || self
                .reader
                .as_ref()
                .is_some_and(|thread| !thread.is_finished())
        {
            if Instant::now() >= join_deadline {
                self.shared.fail("Wine input relay worker did not stop");
                break;
            }
            thread::sleep(POLL);
        }
        // Both threads use nonblocking pipes and bounded socket waits. They must
        // observe ABORTING; no thread can remain blocked in child pipe I/O.
        if self
            .writer
            .as_ref()
            .is_some_and(|thread| !thread.is_finished())
            || self
                .reader
                .as_ref()
                .is_some_and(|thread| !thread.is_finished())
        {
            self.shared
                .fail("Wine input relay worker exceeded its cancellation bound");
        }
        let mut joined = true;
        for handle in [&mut self.writer, &mut self.reader] {
            if let Some(thread) = handle.take() {
                if thread.join().is_err() {
                    joined = false;
                    self.shared.fail("Wine input relay worker panicked");
                }
            }
        }
        let status = {
            let mut child = self.shared.child.lock().unwrap_or_else(|e| e.into_inner());
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            child.wait()
        };
        let _ = self.shared.disconnect.shutdown(Shutdown::Both);
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let exit = status.as_ref().ok();
        let ok = state.error.is_none()
            && state.ready
            && state.root_readiness_seen
            && state.closed
            && state.command_complete
            && state.stdin_closed
            && state.pending.is_empty()
            && state.driver_slots == 0
            && state.received == state.forwarded
            && state.forwarded == state.acknowledged
            && joined
            && exit.is_some_and(|status| status.success());
        let mut report = RelayReport {
            schema_version: 1,
            ok,
            ready: state.ready,
            root_readiness_verified: state.root_readiness_seen,
            closed: state.closed,
            command_complete: state.command_complete,
            stdin_closed: state.stdin_closed,
            frames_received: state.received,
            frames_forwarded: state.forwarded,
            frames_acknowledged: state.acknowledged,
            cleanup_destroy_acknowledgements: state.cleanup_acknowledged,
            pending_acknowledgements: state.pending.len(),
            bytes_forwarded: state.forwarded * FRAME_BYTES as u64,
            received_hash_chain_sha256: format!("{:x}", state.received_hash.clone().finalize()),
            forwarded_hash_chain_sha256: format!("{:x}", state.forwarded_hash.clone().finalize()),
            acknowledged_hash_chain_sha256: format!(
                "{:x}",
                state.acknowledged_hash.clone().finalize()
            ),
            producer_active_slots: state.intended_slots,
            driver_active_slots: state.driver_slots,
            threads_joined: joined,
            child_reaped: status.is_ok(),
            exit_code: exit.and_then(|status| status.code()),
            termination_signal: exit.and_then(|status| status.signal()),
            acknowledgement_means: "Wine driver accepted the original frame; not game input receipt",
            game_input_verified: false,
            error: state.error.clone(),
            feedback: state.feedback.snapshot(),
        };
        drop(state);
        if !report.ok && report.error.is_none() {
            report.error = Some("Wine input relay did not finish cleanly".into());
        }
        if let Err(error) = self.persist(&report) {
            report.ok = false;
            report.error = Some(format!("Cannot save relay proof: {error}"));
        }
        self.result = Some(report.clone());
        report
    }
    fn persist(&self, report: &RelayReport) -> Result<(), String> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.owned_state.join("wine-input-relay-proof.json"))
            .map_err(|e| e.to_string())?;
        serde_json::to_writer_pretty(&mut file, report).map_err(|e| e.to_string())?;
        file.write_all(b"\n")
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())
    }
}
impl Drop for Relay {
    fn drop(&mut self) {
        if self.result.is_none() {
            let _ = self.abort("Wine input relay owner dropped without normal finish");
        }
    }
}

fn forward_frames(receiver: UnixDatagram, mut input: ChildStdin, shared: Arc<Shared>) {
    let mut receive_timeout = RECEIVE_TIMEOUT;
    let mut timeout_adjustment_failed = false;
    loop {
        if shared.mode.load(Ordering::SeqCst) == ABORTING {
            break;
        }
        shared.check_ack_timeout();
        if !shared.state.lock().unwrap_or_else(|e| e.into_inner()).ready {
            thread::sleep(POLL);
            continue;
        }
        let wanted = shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .feedback
            .receive_timeout();
        if wanted != receive_timeout && !timeout_adjustment_failed {
            match receiver.set_read_timeout(Some(wanted)) {
                Ok(()) => receive_timeout = wanted,
                Err(_) => {
                    timeout_adjustment_failed = true;
                    shared
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .feedback
                        .disable("Cannot select bounded feedback polling cadence");
                }
            }
        }
        // An empty receive that started while RUNNING cannot attest final
        // drain: the producer may have queued its last frames just before the
        // caller switched to DRAINING. Observe a fresh empty wait afterward.
        let draining_before_receive = shared.mode.load(Ordering::SeqCst) == DRAINING;
        match Frame::receive(&receiver) {
            Ok(Some(frame)) => {
                // At most one bounded helper query can precede this original
                // frame. Queries never enter reserve() or its SHA/count chain.
                if !forward_feedback_query(&mut input, &shared) {
                    break;
                }
                let hash = match shared.reserve(&frame) {
                    Ok(hash) => hash,
                    Err(error) => {
                        shared.fail(error);
                        break;
                    }
                };
                let deadline = Instant::now() + WRITE_TIMEOUT;
                let mut offset = 0;
                while offset < FRAME_BYTES && shared.mode.load(Ordering::SeqCst) != ABORTING {
                    match input.write(&frame.as_bytes()[offset..]) {
                        Ok(0) => {
                            shared.fail("Wine input feeder stdin closed during frame write");
                            break;
                        }
                        Ok(written) => offset += written,
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if Instant::now() >= deadline {
                                shared.fail("Wine feeder stdin backpressure timed out");
                                break;
                            }
                            thread::sleep(POLL);
                        }
                        Err(_) => {
                            shared.fail("Cannot forward frame to Wine feeder stdin");
                            break;
                        }
                    }
                }
                if offset == FRAME_BYTES {
                    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.forwarded += 1;
                    state.forwarded_hash.update(hash);
                }
            }
            Ok(None) if draining_before_receive => break,
            Ok(None) => {
                if !forward_feedback_query(&mut input, &shared) {
                    break;
                }
            }
            Err(error) => {
                if shared.mode.load(Ordering::SeqCst) != ABORTING {
                    shared.fail(error);
                }
                break;
            }
        }
    }
    shared
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .stdin_closed = true;
    drop(input);
    shared
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .writer_done = true;
}
fn forward_feedback_query(input: &mut ChildStdin, shared: &Shared) -> bool {
    if shared.mode.load(Ordering::SeqCst) != RUNNING {
        return true;
    }
    let send = {
        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        // One writer preserves stream order; an input ACK need not block a
        // separate query. Avoid spending query budget on heavy input backlog.
        state.ready
            && !state.closed
            && state.pending.len() < MAX_PENDING / 2
            && state.feedback.reserve_query()
    };
    if !send {
        return true;
    }
    let bytes = feedback_query_bytes();
    let mut offset = 0;
    let deadline = Instant::now() + WRITE_TIMEOUT;
    while offset < bytes.len() && shared.mode.load(Ordering::SeqCst) != ABORTING {
        match input.write(&bytes[offset..]) {
            Ok(0) => {
                shared.fail("Shared feeder pipe closed during helper query");
                return false;
            }
            Ok(count) => offset += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    shared.fail("Shared feeder pipe backpressure during helper query");
                    return false;
                }
                thread::sleep(POLL);
            }
            Err(_) => {
                shared.fail("Shared feeder pipe failed during helper query");
                return false;
            }
        }
    }
    if offset == bytes.len() {
        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.feedback.report.queries_forwarded += 1;
        state.feedback.report.helper_control_bytes += bytes.len() as u64;
    }
    offset == bytes.len()
}
fn read_status(mut output: ChildStdout, shared: Arc<Shared>) {
    let mut pending = Vec::new();
    let mut buffer = [0_u8; 2048];
    loop {
        if shared.mode.load(Ordering::SeqCst) == ABORTING {
            break;
        }
        match output.read(&mut buffer) {
            Ok(0) => {
                let valid = {
                    let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.closed && state.command_complete && pending.is_empty()
                };
                if !valid {
                    shared.fail("Unexpected EOF from Wine input feeder");
                }
                break;
            }
            Ok(size) => {
                pending.extend_from_slice(&buffer[..size]);
                while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
                    if newline > MAX_LINE {
                        shared.fail("Wine feeder status line limit exceeded");
                        break;
                    }
                    let event = serde_json::from_slice::<Value>(&pending[..newline]);
                    let result = event
                        .map_err(|_| "Invalid JSON from Wine input feeder".to_string())
                        .and_then(|event| {
                            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                            if event["event"] == "wine_input_feedback"
                                && state.feedback.report.selected
                            {
                                state
                                    .feedback
                                    .charge_status_bytes(newline + 1, Instant::now());
                            } else {
                                state.status_bytes += (newline + 1) as u64;
                                if state.status_bytes > MAX_STATUS_BYTES {
                                    return Err("Wine feeder status byte limit exceeded".into());
                                }
                            }
                            drop(state);
                            shared.accept_status(event)
                        });
                    pending.drain(..=newline);
                    if let Err(error) = result {
                        shared.fail(error);
                        break;
                    }
                }
                if pending.len() > MAX_LINE {
                    shared.fail("Wine feeder incomplete status line limit exceeded");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => thread::sleep(POLL),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => {
                shared.fail("Cannot read Wine input feeder status");
                break;
            }
        }
    }
    shared
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .reader_done = true;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const MOCK: &str = r#"
import hashlib,json,os,struct,sys,time
mode=os.environ.get('MOCK_MODE','normal')
def event(name,ok=True,slot=0,kind=0,code=0):
 print(json.dumps(dict(event=name,ok=ok,win32_error=code,slot=slot,kind=kind)),flush=True)
if mode=='no-ready':time.sleep(30)
if mode!='no-root':print(json.dumps(dict(event='wine_input_root_readiness',ok=True,win32_error=0,state_read=True,root_present=1,root_started=1,root_removed=0,children_mask=0)),flush=True)
event('wine_input_ready',mode!='bad-ready',code=5 if mode=='bad-ready' else 0)
if mode in ('stall','bad-ready'):time.sleep(30)
if mode=='unknown':event('unknown');time.sleep(30)
if mode=='oversize':print('x'*5000,flush=True);time.sleep(30)
owned=set();wire=hashlib.sha256();generation=0;feedback=[];rate_began=None;rate_sequence=0
while True:
 data=sys.stdin.buffer.read(288)
 if not data:break
 if mode=='eof':sys.exit(0)
 if len(data)!=288:sys.exit(3)
 magic,version,kind,slot,length,vendor,product,revision=struct.unpack('<8I',data[:32])
 if kind==4:
  if mode=='feedback-error':event('wine_input_feedback',False,code=5)
  elif mode=='feedback-timeout':pass
  elif mode.startswith('feedback-rate-'):
   rate=int(mode.rsplit('-',1)[1]);available=int((time.monotonic()-rate_began)*rate) if rate_began else rate_sequence
   if available-rate_sequence>64:
    print(json.dumps(dict(event='wine_input_feedback',ok=False,win32_error=23,loss_latched=True)),flush=True)
   else:
    records=[]
    for _ in range(min(8,available-rate_sequence)):
     rate_sequence+=1
     records.append(dict(kind=1,byte_size=48,control_session=7,slot=0,generation=generation,sequence_low=rate_sequence,sequence_high=0,left=rate_sequence if rate_sequence%4 else 0,right=0,report_id=1,hid_output_dequeued=True,sdk_dispatched=False,physical_device_verified=False))
    print(json.dumps(dict(event='wine_input_feedback',ok=True,win32_error=0,available=bool(records),batch_version=1,records=records,drained=len(records)<8)),flush=True)
  else:
   row=dict(event='wine_input_feedback',ok=True,win32_error=0,available=bool(feedback))
   if feedback:row.update(feedback.pop(0))
   print(json.dumps(row),flush=True)
  continue
 wire.update(data)
 if mode=='bad-ack':event('wine_input_ack',slot=slot+1,kind=kind);time.sleep(30)
 if mode=='failed-ack':event('wine_input_ack',False,slot,kind,5);time.sleep(30)
 if kind==1:
  owned.add(slot);generation+=1
  if mode.startswith('feedback-rate-'):rate_began=time.monotonic();rate_sequence=0
  if mode.startswith('feedback'):
   feedback.append(dict(kind=1,byte_size=48,control_session=7,slot=slot,generation=generation,sequence_low=generation,sequence_high=0,left=1234,right=0,report_id=1,hid_output_dequeued=True,sdk_dispatched=False,physical_device_verified=False))
   print(json.dumps(dict(event='wine_input_ack',ok=True,win32_error=0,slot=slot,kind=1,control_session=7,generation=generation,driver_binding_verified=True)),flush=True)
   continue
 if kind==3:owned.remove(slot);feedback.clear();rate_began=None
 event('wine_input_ack',slot=slot,kind=kind)
for slot in sorted(owned):event('wine_input_cleanup',slot=slot,kind=3)
expected=os.environ.get('MOCK_WIRE_HASH')
ok=not expected or wire.hexdigest()==expected
event('wine_input_closed',ok,code=0 if ok else 13)
event('wine_input_command_complete',ok,code=0 if ok else 13)
sys.exit(0 if ok else 1)
"#;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "skinos-relay-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn command(mode: &str) -> Command {
        let mut command = Command::new("/usr/bin/python3");
        command.args(["-u", "-c", MOCK]).env("MOCK_MODE", mode);
        command
    }
    fn frame(kind: u32, slot: u32) -> [u8; FRAME_BYTES] {
        let descriptor = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../third_party/notproton/integration/wine-input/descriptor.h"
        ));
        let payload: Vec<u8> = match kind {
            1 => descriptor
                .split("0x")
                .skip(1)
                .map(|value| u8::from_str_radix(&value[..2], 16).unwrap())
                .collect(),
            2 => (0..20).map(|value| value * 7).collect(),
            _ => vec![],
        };
        let mut bytes = [0; FRAME_BYTES];
        for (index, value) in [
            0x53494e50,
            1,
            kind,
            slot,
            payload.len() as u32,
            0x045e,
            0x028e,
            0,
        ]
        .iter()
        .enumerate()
        {
            bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes[32..32 + payload.len()].copy_from_slice(&payload);
        Frame::parse(&bytes).unwrap();
        bytes
    }
    fn start(mode: &str, state: &Path) -> (Relay, UnixDatagram) {
        let (send, receive) = UnixDatagram::pair().unwrap();
        let relay =
            Relay::start(command(mode), receive, state, Duration::from_secs(3), || {}).unwrap();
        (relay, send)
    }
    fn error(relay: &Relay) -> String {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(error) = relay.current_error() {
                return error;
            }
            assert!(Instant::now() < deadline, "missing relay failure");
            thread::sleep(POLL);
        }
    }
    fn binding_json(generation: u32) -> Value {
        json!({"driver_binding_verified":true,"control_session":7,"generation":generation})
    }
    fn feedback_json(generation: u32, sequence: u64) -> Value {
        json!({"event":"wine_input_feedback","ok":true,"win32_error":0,"available":true,
            "kind":1,"byte_size":48,"control_session":7,"slot":0,"generation":generation,
            "sequence_low":sequence as u32,"sequence_high":(sequence>>32) as u32,
            "left":1234,"right":0,"report_id":1,"hid_output_dequeued":true,
            "sdk_dispatched":false,"physical_device_verified":false})
    }
    fn batch_json(records: Vec<Value>) -> Value {
        json!({"event":"wine_input_feedback","ok":true,"win32_error":0,"available":!records.is_empty(),
            "batch_version":1,"drained":records.len()<MAX_FEEDBACK_BATCH,"records":records})
    }
    #[test]
    fn feedback_batch_is_atomic_and_burst_stops_at_idle_or_fairness_bound() {
        let mut f = FeedbackState::new(FeedbackSelection::Experimental { epoch: 77 }).unwrap();
        f.bind(&binding_json(1), 0);
        for burst in 0..MAX_FEEDBACK_BURST_QUERIES {
            assert!(f.reserve_query());
            assert_eq!(f.receive_timeout(), POLL);
            f.reply(&batch_json(
                (1..=8)
                    .map(|n| feedback_json(1, (burst * 8 + n) as u64))
                    .collect(),
            ));
        }
        assert!(f.report.enabled);
        assert_eq!(f.report.outputs_received, 32);
        assert!(!f.reserve_query());
        assert_eq!(f.receive_timeout(), RECEIVE_TIMEOUT);
        f.last_query = Some(Instant::now() - FEEDBACK_POLL);
        assert!(f.reserve_query());
        f.reply(&batch_json(vec![]));
        assert!(!f.backlog && !f.reserve_query());
        assert_eq!(f.receive_timeout(), RECEIVE_TIMEOUT);
        let mut f = FeedbackState::new(FeedbackSelection::Experimental { epoch: 77 }).unwrap();
        f.bind(&binding_json(1), 0);
        f.drain(128);
        let mut rows = (1..=8).map(|n| feedback_json(1, n)).collect::<Vec<_>>();
        rows[7]["left"] = json!(65536);
        assert!(f.reserve_query());
        f.reply(&batch_json(rows));
        assert!(f.report.stop_required);
        assert!(f.drain(128).events.is_empty());
        assert_eq!(f.last_sequence, 0);
        assert_eq!(f.report.outputs_received, 0);
        for row in [
            batch_json((1..=9).map(|n| feedback_json(1, n)).collect()),
            json!({"ok":true,"win32_error":0,"available":false,"batch_version":1,"records":[feedback_json(1,1)],"drained":true}),
        ] {
            let mut f = FeedbackState::new(FeedbackSelection::Experimental { epoch: 1 }).unwrap();
            f.bind(&binding_json(1), 0);
            assert!(f.reserve_query());
            f.reply(&row);
            assert!(f.report.stop_required && f.report.outputs_received == 0);
        }
    }
    #[test]
    fn feedback_byte_budget_allows_hours_of_valid_status_but_rejects_one_window_flood() {
        let mut f = FeedbackState::new(FeedbackSelection::Experimental { epoch: 1 }).unwrap();
        let start = Instant::now();
        for second in 0..7200 {
            f.charge_status_bytes(16 * 1024, start + Duration::from_secs(second));
        }
        assert!(f.report.enabled && !f.report.stop_required);
        assert!(f.report.status_bytes > MAX_STATUS_BYTES);
        f.charge_status_bytes(
            MAX_FEEDBACK_WINDOW_BYTES as usize + 1,
            start + Duration::from_secs(7200),
        );
        assert!(!f.report.enabled && f.report.stop_required);
        assert!(f.report.error.unwrap().contains("byte-rate"));
    }
    #[test]
    fn latched_driver_loss_clears_already_queued_motors_and_requests_stop() {
        let mut f = FeedbackState::new(FeedbackSelection::Experimental { epoch: 1 }).unwrap();
        f.bind(&binding_json(1), 0);
        assert!(f.reserve_query());
        f.reply(&batch_json(vec![feedback_json(1, 1)]));
        f.query = Some(Instant::now());
        f.reply(
            &json!({"event":"wine_input_feedback","ok":false,"win32_error":23,"loss_latched":true}),
        );
        assert!(f.report.driver_loss_latched && f.report.stop_required && !f.report.enabled);
        assert!(f.drain(128).events.is_empty());
        assert!(!f.reserve_query());
    }
    #[test]
    fn actual_pipe_bounded_batches_keep_up_with_120_and_240_hz_including_zeroes() {
        for rate in [120_u64, 240] {
            let temporary = Temp::new();
            let (send, receive) = UnixDatagram::pair().unwrap();
            let mut relay = Relay::start_with_feedback(
                command(&format!("feedback-rate-{rate}")),
                receive,
                &temporary.0,
                Duration::from_secs(3),
                FeedbackSelection::Experimental { epoch: 77 },
                || {},
            )
            .unwrap();
            send.send(&frame(1, 0)).unwrap();
            let until = Instant::now() + Duration::from_secs(1);
            let mut motors = vec![];
            while Instant::now() < until {
                if let Some(batch) = relay.try_take_feedback(128) {
                    assert!(batch.report.error.is_none(), "{:?}", batch.report);
                    motors.extend(batch.events.into_iter().filter_map(|event| match event {
                        FeedbackEvent::Rumble(row) => Some(row),
                        _ => None,
                    }));
                }
                thread::sleep(POLL);
            }
            assert!(
                motors.len() as u64 >= rate * 8 / 10,
                "only {} outputs at {rate}Hz",
                motors.len()
            );
            assert!(
                motors
                    .windows(2)
                    .all(|pair| pair[1].sequence == pair[0].sequence + 1)
            );
            assert!(
                motors
                    .iter()
                    .all(|row| (row.left == 0) == (row.sequence % 4 == 0))
            );
            send.send(&frame(2, 0)).unwrap();
            send.send(&frame(3, 0)).unwrap();
            let report = relay.finish(Duration::from_secs(3));
            assert!(report.ok, "{report:?}");
            assert!(report.feedback.error.is_none());
            assert_eq!(report.frames_acknowledged, 3);
            assert_eq!(report.bytes_forwarded, 864);
            assert_eq!(
                report.received_hash_chain_sha256,
                report.acknowledged_hash_chain_sha256
            );
        }
    }
    #[test]
    fn feedback_control_is_not_a_producer_frame_and_requires_explicit_epoch() {
        assert!(Frame::parse(&feedback_query_bytes()).is_err());
        assert_eq!(&feedback_query_bytes()[8..12], &4_u32.to_le_bytes());
        assert!(
            !FeedbackState::new(FeedbackSelection::Disabled)
                .unwrap()
                .report
                .selected
        );
        assert!(FeedbackState::new(FeedbackSelection::Experimental { epoch: 0 }).is_err());
    }
    #[test]
    fn feedback_retirement_purges_motors_but_preserves_bound_retired_rebound_order() {
        let mut f = FeedbackState::new(FeedbackSelection::Experimental { epoch: 77 }).unwrap();
        f.bind(&binding_json(1), 0);
        assert!(f.reserve_query());
        f.reply(&feedback_json(1, 1));
        f.retire(0);
        f.bind(&binding_json(2), 0);
        let batch = f.drain(128);
        let one = FeedbackBinding {
            epoch: 77,
            control_session: 7,
            slot: 0,
            generation: 1,
        };
        let two = FeedbackBinding {
            generation: 2,
            ..one
        };
        assert_eq!(
            batch.events,
            vec![
                FeedbackEvent::Bound(one),
                FeedbackEvent::Retired(one),
                FeedbackEvent::Bound(two)
            ]
        );
        assert_eq!(batch.report.stale_outputs_discarded, 1);
        assert!(batch.report.enabled && !batch.report.stop_required);
        // A late old-generation row must never be reinterpreted as generation2.
        f.query = Some(Instant::now());
        f.reply(&feedback_json(1, 2));
        assert!(f.report.stop_required && !f.report.enabled);
    }
    #[test]
    fn feedback_missing_binding_wrong_session_replay_and_claimed_sdk_delivery_fail_closed() {
        let changes = [
            json!({"control_session":0}),
            json!({"control_session":8}),
            json!({"generation":2}),
            json!({"slot":16}),
            json!({"sequence_low":0}),
            json!({"left":65536}),
            json!({"right":-1}),
            json!({"byte_size":40}),
            json!({"report_id":0}),
            json!({"hid_output_dequeued":false}),
            json!({"sdk_dispatched":true}),
            json!({"physical_device_verified":true}),
        ];
        for change in changes {
            let mut f = FeedbackState::new(FeedbackSelection::Experimental { epoch: 1 }).unwrap();
            f.bind(&binding_json(1), 0);
            let mut row = feedback_json(1, 1);
            for (key, value) in change.as_object().unwrap() {
                row[key] = value.clone();
            }
            assert!(f.reserve_query());
            f.reply(&row);
            assert!(
                !f.report.enabled && f.report.stop_required && f.report.error.is_some(),
                "{row}"
            );
            assert!(f.drain(128).events.is_empty());
        }
        let mut f = FeedbackState::new(FeedbackSelection::Experimental { epoch: 1 }).unwrap();
        f.bind(&json!({}), 0);
        assert!(f.report.stop_required);
        let mut f = FeedbackState::new(FeedbackSelection::Experimental { epoch: 1 }).unwrap();
        f.bind(&binding_json(1), 0);
        assert!(f.reserve_query());
        f.reply(&feedback_json(1, 1));
        f.query = Some(Instant::now());
        f.reply(&feedback_json(1, 1));
        assert!(f.report.stop_required);
    }
    #[test]
    fn feedback_overflow_unsolicited_reply_and_timeout_disable_only_feedback() {
        let mut f = FeedbackState::new(FeedbackSelection::Experimental { epoch: 1 }).unwrap();
        f.bind(&binding_json(1), 0);
        for n in 1..=MAX_FEEDBACK_EVENTS {
            f.query = Some(Instant::now());
            f.reply(&feedback_json(1, n as u64));
        }
        assert!(f.report.error.as_ref().unwrap().contains("overflow"));
        assert!(f.events.is_empty());
        let mut f = FeedbackState::new(FeedbackSelection::Experimental { epoch: 1 }).unwrap();
        f.bind(&binding_json(1), 0);
        f.reply(&feedback_json(1, 1));
        assert!(f.report.error.as_ref().unwrap().contains("Unsolicited"));
        let mut f = FeedbackState::new(FeedbackSelection::Experimental { epoch: 1 }).unwrap();
        f.bind(&binding_json(1), 0);
        f.query = Some(Instant::now() - FEEDBACK_TIMEOUT - Duration::from_millis(1));
        f.check_timeout();
        assert!(f.report.stop_required && f.report.error.as_ref().unwrap().contains("timed out"));
    }
    #[test]
    fn actual_pipe_feedback_queries_leave_original_frame_counts_bytes_and_hashes_unchanged() {
        let state = Temp::new();
        let (send, receive) = UnixDatagram::pair().unwrap();
        let frames = [frame(1, 0), frame(2, 0), frame(3, 0)];
        let mut hash = Sha256::new();
        for f in frames {
            hash.update(f);
        }
        let mut cmd = command("feedback");
        cmd.env("MOCK_WIRE_HASH", format!("{:x}", hash.finalize()));
        let mut relay = Relay::start_with_feedback(
            cmd,
            receive,
            &state.0,
            Duration::from_secs(3),
            FeedbackSelection::Experimental { epoch: 77 },
            || {},
        )
        .unwrap();
        send.send(&frames[0]).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut events = vec![];
        loop {
            events.extend(relay.take_feedback(128).events);
            if events.iter().any(|e| matches!(e, FeedbackEvent::Rumble(_))) {
                break;
            }
            assert!(Instant::now() < deadline, "{:?}", relay.feedback_report());
            thread::sleep(POLL);
        }
        assert!(matches!(events[0], FeedbackEvent::Bound(_)));
        let progress = relay.progress().unwrap();
        assert_eq!(
            (progress.received, progress.forwarded, progress.acknowledged),
            (1, 1, 1)
        );
        send.send(&frames[1]).unwrap();
        send.send(&frames[2]).unwrap();
        let report = relay.finish(Duration::from_secs(3));
        assert!(report.ok, "{report:?}");
        assert_eq!(
            (
                report.frames_received,
                report.frames_forwarded,
                report.frames_acknowledged,
                report.bytes_forwarded
            ),
            (3, 3, 3, 864)
        );
        assert_eq!(
            report.received_hash_chain_sha256,
            report.acknowledged_hash_chain_sha256
        );
        assert!(report.feedback.queries_forwarded > 0);
        assert_eq!(
            report.feedback.helper_control_bytes,
            report.feedback.queries_forwarded * 288
        );
        assert!(
            relay
                .take_feedback(128)
                .events
                .iter()
                .any(|e| matches!(e, FeedbackEvent::Retired(_)))
        );
    }
    #[test]
    fn actual_pipe_query_failure_or_timeout_keeps_later_input_operation_and_cleanup_valid() {
        for mode in ["feedback-error", "feedback-timeout"] {
            let state = Temp::new();
            let (send, receive) = UnixDatagram::pair().unwrap();
            let mut relay = Relay::start_with_feedback(
                command(mode),
                receive,
                &state.0,
                Duration::from_secs(3),
                FeedbackSelection::Experimental { epoch: 9 },
                || {},
            )
            .unwrap();
            send.send(&frame(1, 0)).unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while relay.feedback_report().error.is_none() {
                assert!(Instant::now() < deadline);
                thread::sleep(POLL);
            }
            assert!(relay.check().is_ok());
            assert!(relay.feedback_report().stop_required);
            send.send(&frame(2, 0)).unwrap();
            send.send(&frame(3, 0)).unwrap();
            let report = relay.finish(Duration::from_secs(3));
            assert!(report.ok, "{report:?}");
            assert_eq!(report.frames_acknowledged, 3);
            assert!(report.error.is_none());
            assert!(report.feedback.error.is_some() && !report.feedback.enabled);
        }
    }
    #[test]
    fn owner_thread_try_drain_never_waits_or_fabricates_a_report_on_contention() {
        let temporary = Temp::new();
        let (mut relay, _send) = start("normal", &temporary.0);
        {
            let _locked = relay.shared.state.lock().unwrap();
            let began = Instant::now();
            assert!(relay.try_take_feedback(128).is_none());
            assert!(began.elapsed() < Duration::from_millis(100));
        }
        let batch = relay.try_take_feedback(128).unwrap();
        assert!(batch.events.is_empty());
        assert!(!batch.report.selected);
        assert!(relay.finish(Duration::from_secs(3)).ok);
    }
    #[test]
    fn reconnect_progress_counts_only_real_acknowledged_create_and_destroy() {
        let state = Temp::new();
        let (mut relay, send) = start("normal", &state.0);
        assert_eq!(relay.progress().unwrap().create_acknowledgements, 0);
        for (kind, creates, destroys, mask) in
            [(1, 1, 0, 1), (2, 1, 0, 1), (3, 1, 1, 0), (1, 2, 1, 1)]
        {
            send.send(&frame(kind, 0)).unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                let progress = relay.progress().unwrap();
                if progress.pending == 0
                    && progress.create_acknowledgements == creates
                    && progress.destroy_acknowledgements == destroys
                    && progress.driver_slots == mask
                {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "missing acknowledged lifecycle transition"
                );
                thread::sleep(POLL);
            }
        }
        let report = relay.finish(Duration::from_secs(3));
        assert!(report.ok, "{report:?}");
        assert_eq!(report.cleanup_destroy_acknowledgements, 1);
        // EOF cleanup removes the final pad, but must not attest a producer
        // Destroy that would authorize reconnection in an active game.
        let progress = relay.progress().unwrap();
        assert_eq!(progress.destroy_acknowledgements, 1);
        assert_eq!(progress.driver_slots, 0);
    }
    #[test]
    fn real_pipe_keeps_all_288_bytes_and_order_then_reaps_and_joins() {
        let state = Temp::new();
        let (send, receive) = UnixDatagram::pair().unwrap();
        let frames = [frame(1, 0), frame(2, 0), frame(3, 0)];
        let mut hash = Sha256::new();
        for frame in &frames {
            hash.update(frame);
        }
        let mut command = command("normal");
        command.env("MOCK_WIRE_HASH", format!("{:x}", hash.finalize()));
        let mut relay =
            Relay::start(command, receive, &state.0, Duration::from_secs(3), || {}).unwrap();
        for frame in frames {
            send.send(&frame).unwrap();
        }
        let report = relay.finish(Duration::from_secs(3));
        assert!(report.ok, "{:?}", report);
        assert_eq!(report.frames_forwarded, 3);
        assert_eq!(report.frames_acknowledged, 3);
        assert_eq!(report.bytes_forwarded, 864);
        assert!(report.child_reaped && report.threads_joined);
        assert_eq!(
            report.received_hash_chain_sha256,
            report.forwarded_hash_chain_sha256
        );
        assert_eq!(
            report.forwarded_hash_chain_sha256,
            report.acknowledged_hash_chain_sha256
        );
        assert!(!report.game_input_verified);
        let proof = fs::read_to_string(state.0.join("wine-input-relay-proof.json")).unwrap();
        assert!(!proof.contains("payload"));
        assert!(report.error.is_none());
    }
    #[test]
    fn eof_cleanup_is_verified_for_remaining_owned_slot() {
        let state = Temp::new();
        let (mut relay, send) = start("normal", &state.0);
        send.send(&frame(1, 2)).unwrap();
        send.send(&frame(2, 2)).unwrap();
        let report = relay.finish(Duration::from_secs(3));
        assert!(report.ok, "{:?}", report);
        assert_eq!(report.cleanup_destroy_acknowledgements, 1);
        assert_eq!(report.driver_active_slots, 0);
    }
    #[test]
    fn invalid_frame_disconnects_transport_and_child() {
        let state = Temp::new();
        let (mut relay, send) = start("normal", &state.0);
        send.send(&[0; FRAME_BYTES]).unwrap();
        assert!(error(&relay).contains("frame"));
        assert!(send.send(&frame(1, 0)).is_err());
        let report = relay.finish(Duration::from_secs(1));
        assert!(!report.ok);
        assert!(report.child_reaped && report.threads_joined);
    }
    #[test]
    fn bad_order_unexpected_eof_and_failed_ack_are_fatal() {
        for mode in ["bad-ack", "eof", "failed-ack"] {
            let state = Temp::new();
            let (mut relay, send) = start(mode, &state.0);
            send.send(&frame(1, 0)).unwrap();
            assert!(!error(&relay).is_empty());
            let report = relay.finish(Duration::from_secs(1));
            assert!(!report.ok);
            assert!(report.child_reaped && report.threads_joined);
            assert_eq!(report.frames_acknowledged, 0);
        }
    }
    #[test]
    fn report_before_create_and_duplicate_create_never_reach_driver() {
        for frames in [vec![frame(2, 0)], vec![frame(1, 0), frame(1, 0)]] {
            let state = Temp::new();
            let (mut relay, send) = start("normal", &state.0);
            for frame in frames {
                let _ = send.send(&frame);
            }
            assert!(error(&relay).contains("order"));
            let report = relay.finish(Duration::from_secs(1));
            assert!(!report.ok);
            assert!(report.child_reaped);
        }
    }
    #[test]
    fn readiness_failure_and_oversized_or_unknown_status_clean_up() {
        for mode in ["no-ready", "no-root", "bad-ready", "oversize", "unknown"] {
            let state = Temp::new();
            let (_send, receive) = UnixDatagram::pair().unwrap();
            let started = Instant::now();
            match Relay::start(
                command(mode),
                receive,
                &state.0,
                Duration::from_millis(200),
                || {},
            ) {
                Err(_) => {}
                Ok(mut relay) => {
                    assert!(!error(&relay).is_empty());
                    assert!(!relay.finish(Duration::from_secs(1)).ok);
                }
            }
            assert!(started.elapsed() < Duration::from_secs(4));
            let report: Value = serde_json::from_slice(
                &fs::read(state.0.join("wine-input-relay-proof.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(report["child_reaped"], true);
            assert_eq!(report["threads_joined"], true);
        }
    }
    #[test]
    fn descriptor_flags_are_read_back_and_stdin_stdout_are_actually_nonblocking() {
        struct ChildGuard(Child);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let child = Command::new("/usr/bin/python3")
            .args(["-c", "import time; time.sleep(30)"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut child = ChildGuard(child);
        let mut input = child.0.stdin.take().unwrap();
        let mut output = child.0.stdout.take().unwrap();
        for fd in [input.as_raw_fd(), output.as_raw_fd()] {
            assert_eq!(unsafe { fcntl(fd, 2, 0) }, 0);
            protect_fd(fd, true).unwrap();
            assert_ne!(unsafe { fcntl(fd, 1) } & 1, 0);
        }
        let mut byte = [0];
        assert_eq!(
            output.read(&mut byte).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        let buffer = [0_u8; 4096];
        let mut would_block = false;
        for _ in 0..1024 {
            match input.write(&buffer) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    would_block = true;
                    break;
                }
                Err(error) => panic!("unexpected pipe write failure: {error}"),
            }
        }
        assert!(
            would_block,
            "an unread pipe must return backpressure without blocking"
        );
    }

    #[test]
    fn drop_cannot_block_on_a_feeder_that_never_reads_stdin() {
        let state = Temp::new();
        let (relay, send) = start("stall", &state.0);
        send.set_nonblocking(true).unwrap();
        send.send(&frame(1, 0)).unwrap();
        for _ in 0..1024 {
            if send.send(&frame(2, 0)).is_err() {
                break;
            }
        }
        thread::sleep(Duration::from_millis(50));
        let started = Instant::now();
        drop(relay);
        assert!(started.elapsed() < Duration::from_secs(3));
        let report: Value =
            serde_json::from_slice(&fs::read(state.0.join("wine-input-relay-proof.json")).unwrap())
                .unwrap();
        assert_eq!(report["ok"], false);
        assert_eq!(report["child_reaped"], true);
        assert_eq!(report["threads_joined"], true);
    }
}
