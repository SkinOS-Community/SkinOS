//! Isolated retained-SDK owner. Not registered in worker/main yet.
//! Caller supplies the verified compatibility command/receipt and the existing
//! PID/start/prefix/AppID tracker. This module never locates or stops a game.
#![allow(dead_code)]
use crate::wine_input_relay::{FeedbackBinding, FeedbackRumble};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    fs,
    io::{Read, Write},
    marker::PhantomData,
    os::{fd::AsRawFd, unix::process::ExitStatusExt},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    rc::Rc,
    time::{Duration, Instant},
};

const MAX_PENDING: usize = 32;
const MAX_LINE: usize = 4096;
const MAX_READ_PER_POLL: usize = 64 * 1024;
const ACK_TIMEOUT: Duration = Duration::from_millis(750);
const PING_INTERVAL: Duration = Duration::from_millis(400);
unsafe extern "C" {
    fn fcntl(fd: i32, command: i32, ...) -> i32;
}

pub struct VerifiedProbe {
    path: PathBuf,
    sha256: String,
}
impl VerifiedProbe {
    pub fn from_experiment(root: &Path, name: &str) -> Result<Self, String> {
        ensure(
            matches!(name, "input-probe.exe" | "input-probe32.exe"),
            "Unexpected SDK sink filename",
        )?;
        let manifest: Value =
            serde_json::from_slice(&bounded_file(&root.join("manifest.json"), 65536)?)
                .map_err(|_| "Malformed SDK sink manifest")?;
        ensure(
            manifest["schema_version"] == 1
                && manifest["experiment"] == "retained_genuine_sdk_rumble_sink",
            "Wrong SDK sink manifest kind",
        )?;
        let relative = format!("pe/{name}");
        let path = root
            .join(&relative)
            .canonicalize()
            .map_err(|_| "SDK sink probe unavailable")?;
        let expected = manifest["files"][&relative]
            .as_str()
            .ok_or("SDK sink hash missing")?;
        ensure(
            format!(
                "{:x}",
                Sha256::digest(bounded_file(&root.join(relative), 16 * 1024 * 1024)?)
            ) == expected,
            "SDK sink payload checksum mismatch",
        )?;
        Ok(Self {
            path,
            sha256: expected.into(),
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
}
/// Intentionally not Debug/Serialize: expected account stays private.
pub struct Context {
    appid: u32,
    expected_account: u64,
    epoch: u64,
}
impl Context {
    pub fn new(appid: u32, expected_account: u64, epoch: u64) -> Result<Self, String> {
        ensure(
            appid != 0 && expected_account != 0 && epoch != 0,
            "SDK sink requires a valid app, account and epoch",
        )?;
        Ok(Self {
            appid,
            expected_account,
            epoch,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_seconds: u64,
    pub start_microseconds: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Termination {
    KernelZombie,
    KernelEsrch,
    PidReused,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Exit {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}
impl Exit {
    fn from_status(status: std::process::ExitStatus) -> Self {
        Self {
            code: status.code(),
            signal: status.signal(),
        }
    }
    fn failed(self) -> bool {
        self.signal.is_some() || self.code.is_some_and(|code| code != 0)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelperObservation {
    Searching,
    Running(ProcessIdentity),
    Terminated {
        identity: ProcessIdentity,
        reason: Termination,
        exit: Option<Exit>,
    },
}
/// Adapter must use the already scoped actual helper PID, start time, vnode,
/// exact prefix/AppID and executable. Inspection failure is Searching/Err,
/// never Terminated. `terminate_owned` may signal only that verified helper.
pub trait HelperTracker {
    fn observe(&mut self) -> Result<HelperObservation, String>;
    fn terminate_owned(&mut self) -> Result<(), String>;
}
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct PollTiming {
    pub gap_ms: u64,
    pub read_before_ms: u64,
    pub tracker_ms: u64,
    pub read_after_ms: u64,
    pub write_ms: u64,
    pub total_ms: u64,
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct CommandTiming {
    pub kind: u32,
    pub sequence: u64,
    pub queued_ms: u64,
    pub sent_ms: Option<u64>,
    pub written_bytes: usize,
}
fn millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    pub ready: bool,
    pub xinput_warmup_ms: Option<u32>,
    pub xinput_warmup_calls: Option<u32>,
    pub appid: u32,
    pub epoch: u64,
    pub commands_sent: u64,
    pub acknowledgements: u64,
    pub sdk_dispatches: u64,
    pub refresh_dispatches: u64,
    pub last_driver_sequence: Option<u64>,
    pub timeout_pending: Option<CommandTiming>,
    pub timeout_poll: Option<PollTiming>,
    pub last_poll: Option<PollTiming>,
    pub last_ack_timing: Option<CommandTiming>,
    pub bind_ack_timing: Option<CommandTiming>,
    pub max_ack_sent_ms: u64,
    pub bind_route_timing: Option<Value>,
    pub bound: Option<FeedbackBinding>,
    pub suspended: bool,
    pub suspensions: u64,
    pub rejected_commands: u64,
    pub last_rejected_driver_sequence: Option<u64>,
    pub last_retired_binding: Option<FeedbackBinding>,
    pub retirements: u64,
    pub steam_input_handle: Option<u64>,
    pub stopped_message: bool,
    pub helper_stopped_reason: Option<String>,
    pub helper_probe_error: Option<String>,
    pub helper_probe_win32_error: Option<u32>,
    pub helper_transport_win32_error: Option<u32>,
    pub helper_input_shutdown: Option<bool>,
    pub helper_sdk_shutdown_dispatched: Option<bool>,
    pub helper_process: Option<ProcessIdentity>,
    pub helper_termination: Option<Termination>,
    pub helper_exit: Option<Exit>,
    pub helper_forced_cleanup: bool,
    pub closing_inspection_retries: u64,
    pub waiting_launcher_pending: bool,
    pub error: Option<String>,
    pub physical_device_verified: bool,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct LauncherReport {
    pub reaped: bool,
    pub exit: Option<Exit>,
    pub forced_cleanup: bool,
    pub error: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Bind = 1,
    Submit = 2,
    Retire = 3,
    Ping = 4,
    Shutdown = 5,
}
#[derive(Clone, Copy, Debug, Default)]
struct Desired {
    sequence: u64,
    left: u16,
    right: u16,
}
struct Pending {
    kind: Kind,
    binding: FeedbackBinding,
    sequence: u64,
    desired: Desired,
    driver_sequence: Option<u64>,
    bytes: [u8; 64],
    offset: usize,
    queued: Instant,
    sent: Option<Instant>,
}
impl Pending {
    fn timing(&self) -> CommandTiming {
        CommandTiming {
            kind: self.kind as u32,
            sequence: self.sequence,
            queued_ms: millis(self.queued.elapsed()),
            sent_ms: self.sent.map(|sent| millis(sent.elapsed())),
            written_bytes: self.offset,
        }
    }
    fn expired(&self) -> bool {
        self.sent.unwrap_or(self.queued).elapsed() >= ACK_TIMEOUT
    }
}
#[derive(Default)]
struct Protocol {
    appid: u32,
    epoch: u64,
    context_seen: bool,
    ready: bool,
    next_sequence: u64,
    last_ack: u64,
    control_session: Option<u32>,
    highest_generation: u32,
    planned: Option<FeedbackBinding>,
    acknowledged: Option<FeedbackBinding>,
    handle: u64,
    planned_desired: Desired,
    actual_desired: Desired,
    last_driver_sequence: u64,
    pending: VecDeque<Pending>,
    report: Report,
    closing: bool,
    stopped: bool,
}
impl Protocol {
    fn new(context: &Context) -> Self {
        Self {
            appid: context.appid,
            epoch: context.epoch,
            report: Report {
                appid: context.appid,
                epoch: context.epoch,
                ..Default::default()
            },
            ..Default::default()
        }
    }
    fn queue(
        &mut self,
        kind: Kind,
        binding: FeedbackBinding,
        motors: Option<FeedbackRumble>,
    ) -> Result<(), String> {
        ensure(
            self.ready && !self.closing && !self.stopped,
            "SDK sink is not ready for commands",
        )?;
        ensure(
            self.pending.len() < MAX_PENDING,
            "SDK sink command queue is full",
        )?;
        ensure(
            binding.epoch == self.epoch && binding.control_session != 0 && binding.slot == 0,
            "SDK sink binding belongs to another connection",
        )?;
        ensure(
            self.control_session
                .is_none_or(|session| session == binding.control_session),
            "SDK sink control session changed",
        )?;
        let sequence = self
            .next_sequence
            .checked_add(1)
            .filter(|n| *n != 0)
            .ok_or("SDK sink command sequence overflow")?;
        let mut desired = self.planned_desired;
        let mut driver_sequence = None;
        match kind {
            Kind::Bind => {
                ensure(
                    self.planned.is_none() && binding.generation > self.highest_generation,
                    "SDK sink binding is active or retired",
                )?;
                desired = Desired::default();
            }
            Kind::Submit => {
                ensure(
                    self.planned == Some(binding),
                    "SDK sink output targets a retired binding",
                )?;
                let event = motors.ok_or("SDK sink motors missing")?;
                ensure(
                    event.binding == binding && event.sequence > self.last_driver_sequence,
                    "SDK sink feedback was replayed",
                )?;
                desired = Desired {
                    sequence,
                    left: event.left,
                    right: event.right,
                };
                driver_sequence = Some(event.sequence);
            }
            Kind::Retire => {
                ensure(
                    self.planned == Some(binding),
                    "SDK sink retirement does not match active binding",
                )?;
                desired = Desired::default();
            }
            Kind::Ping | Kind::Shutdown => {
                ensure(
                    self.control_session.is_some() && binding == self.current_wire_binding(),
                    "SDK sink control command has wrong binding",
                )?;
            }
        }
        let (left, right) = if kind == Kind::Submit {
            (desired.left, desired.right)
        } else {
            (0, 0)
        };
        let words = [
            0x53495253,
            1,
            64,
            kind as u32,
            self.epoch as u32,
            (self.epoch >> 32) as u32,
            binding.control_session,
            binding.slot,
            binding.generation,
            sequence as u32,
            (sequence >> 32) as u32,
            u32::from(left),
            u32::from(right),
            0,
            0,
            0,
        ];
        let mut bytes = [0; 64];
        for (chunk, word) in bytes.chunks_exact_mut(4).zip(words) {
            chunk.copy_from_slice(&word.to_le_bytes());
        }
        self.pending.push_back(Pending {
            kind,
            binding,
            sequence,
            desired,
            driver_sequence,
            bytes,
            offset: 0,
            queued: Instant::now(),
            sent: None,
        });
        self.next_sequence = sequence;
        self.control_session = Some(binding.control_session);
        self.planned_desired = desired;
        match kind {
            Kind::Bind => {
                self.planned = Some(binding);
                self.highest_generation = binding.generation;
            }
            Kind::Retire => self.planned = None,
            Kind::Shutdown => self.closing = true,
            _ => {}
        }
        if let Some(sequence) = driver_sequence {
            self.last_driver_sequence = sequence;
        }
        Ok(())
    }
    fn current_wire_binding(&self) -> FeedbackBinding {
        self.planned.unwrap_or(FeedbackBinding {
            epoch: self.epoch,
            control_session: self.control_session.unwrap_or(0),
            slot: 0,
            generation: 0,
        })
    }
    fn accept(&mut self, row: &Value) -> Result<(), String> {
        match row["event"].as_str() {
            Some("steam_input_app_context") => {
                ensure(
                    !self.context_seen
                        && !self.ready
                        && row["sdk_initialized"] == true
                        && row["appid_verified"] == true
                        && row["appid"].as_u64() == Some(u64::from(self.appid))
                        && row["expected_appid"].as_u64() == Some(u64::from(self.appid)),
                    "SDK sink app context is invalid",
                )?;
                ensure(
                    matches!(
                        row["init_method"].as_str(),
                        Some("SteamAPI_Init" | "SteamAPI_InitFlat")
                    ),
                    "SDK sink has no genuine game SDK initialization",
                )?;
                self.context_seen = true;
            }
            Some("steam_input_sink_ready") => {
                ensure(
                    self.context_seen
                        && !self.ready
                        && row["version"] == 1
                        && row["control_frame_bytes"] == 64
                        && row["appid"].as_u64() == Some(u64::from(self.appid))
                        && row["sdk_initialized"] == true
                        && row["identity_verified"] == true
                        && row["appid_verified"] == true
                        && row["physical_device_verified"] == false
                        && row["refresh_ms"] == 250
                        && row["heartbeat_lease_ms"] == 1500,
                    "SDK sink readiness is invalid",
                )?;
                if !row["xinput_warmup_ms"].is_null() || !row["xinput_warmup_calls"].is_null() {
                    let elapsed = row["xinput_warmup_ms"]
                        .as_u64()
                        .and_then(|value| u32::try_from(value).ok())
                        .ok_or("SDK sink warmup duration is invalid")?;
                    ensure(
                        row["xinput_warmup_calls"] == 4,
                        "SDK sink warmup call count is invalid",
                    )?;
                    self.report.xinput_warmup_ms = Some(elapsed);
                    self.report.xinput_warmup_calls = Some(4);
                }
                self.ready = true;
                self.report.ready = true;
            }
            Some("steam_input_sink_suspended") => {
                let binding = self
                    .acknowledged
                    .ok_or("SDK suspension has no acknowledged binding")?;
                validate_row_binding(row, binding, self.last_ack)?;
                ensure(
                    row["kind"] == 0
                        && number(row, "steam_input_handle")? == self.handle
                        && self.handle != 0,
                    "SDK suspension targets another handle",
                )?;
                validate_desired(row, Desired::default())?;
                let stopped = row["sdk_dispatched"]
                    .as_bool()
                    .ok_or("SDK suspension dispatch flag missing")?;
                if stopped {
                    ensure(
                        self.actual_desired.left != 0 || self.actual_desired.right != 0,
                        "Unexpected SDK suspension zero dispatch",
                    )?;
                    self.report.sdk_dispatches += 1;
                }
                self.actual_desired = Desired::default();
                self.report.suspended = true;
                self.report.suspensions += 1;
                if self.planned == Some(binding) {
                    self.planned_desired = Desired::default();
                }
            }
            Some("steam_input_sink_rejected") => {
                let pending = self
                    .pending
                    .front()
                    .ok_or("SDK rejection has no pending command")?;
                ensure(
                    self.report.suspended && pending.kind == Kind::Submit && pending.offset == 64,
                    "SDK rejection is outside a suspended Submit",
                )?;
                validate_row_binding(row, pending.binding, pending.sequence)?;
                ensure(
                    row["kind"] == 2
                        && row["reason"] == "route_suspended"
                        && row["sdk_dispatched"] == false
                        && number(row, "steam_input_handle")? == self.handle
                        && self.acknowledged == Some(pending.binding)
                        && row["requested_left"].as_u64() == Some(u64::from(pending.desired.left))
                        && row["requested_right"].as_u64()
                            == Some(u64::from(pending.desired.right)),
                    "SDK rejection does not match the exact pending request",
                )?;
                validate_desired(row, Desired::default())?;
                let pending = self.pending.pop_front().expect("validated rejected submit");
                self.last_ack = pending.sequence;
                self.actual_desired = Desired::default();
                self.report.acknowledgements += 1;
                self.report.rejected_commands += 1;
                self.report.last_rejected_driver_sequence = pending.driver_sequence;
            }
            Some("steam_input_sink_bind_timing") => {
                let pending = self
                    .pending
                    .front()
                    .ok_or("SDK Bind timing has no pending command")?;
                ensure(
                    self.ready && pending.kind == Kind::Bind && pending.offset == 64,
                    "SDK Bind timing is out of order",
                )?;
                validate_row_binding(row, pending.binding, pending.sequence)?;
                let mut timing = serde_json::Map::new();
                timing.insert("sequence".into(), Value::from(pending.sequence));
                for field in [
                    "identity_ms",
                    "controllers_ms",
                    "mapping_ms",
                    "xinput_ms",
                    "total_ms",
                ] {
                    let value = row[field]
                        .as_u64()
                        .filter(|value| *value <= u64::from(u32::MAX))
                        .ok_or("SDK Bind duration is invalid")?;
                    timing.insert(field.into(), Value::from(value));
                }
                let stage = row["last_stage"]
                    .as_u64()
                    .filter(|stage| (1..=5).contains(stage))
                    .ok_or("SDK Bind timing stage invalid")?;
                let calls = row["xinput_calls"]
                    .as_u64()
                    .filter(|calls| *calls <= 204)
                    .ok_or("SDK Bind XInput call count invalid")?;
                // Older isolated helpers reported only one immediate attempt.
                let attempts = row.get("attempts").map_or(Ok(1), |value| {
                    value.as_u64().ok_or("SDK Bind attempt count invalid")
                })?;
                let wait_ms = row.get("wait_ms").map_or(Ok(0), |value| {
                    value.as_u64().ok_or("SDK Bind wait duration invalid")
                })?;
                ensure(
                    (1..=51).contains(&attempts)
                        && calls <= attempts * 4
                        && wait_ms <= u64::from(u32::MAX),
                    "SDK Bind settling telemetry invalid",
                )?;
                let count = row["controller_count"]
                    .as_i64()
                    .filter(|count| i32::try_from(*count).is_ok())
                    .ok_or("SDK Bind controller count invalid")?;
                let verified = row["route_verified"]
                    .as_bool()
                    .ok_or("SDK Bind route result missing")?;
                timing.insert("last_stage".into(), Value::from(stage));
                timing.insert("xinput_calls".into(), Value::from(calls));
                timing.insert("attempts".into(), Value::from(attempts));
                timing.insert("wait_ms".into(), Value::from(wait_ms));
                timing.insert("controller_count".into(), Value::from(count));
                timing.insert("route_verified".into(), Value::from(verified));
                self.report.bind_route_timing = Some(Value::Object(timing));
            }
            Some("steam_input_sink_ack") => self.accept_ack(row)?,
            Some("steam_input_sink_refresh") => {
                ensure(
                    self.ready && !self.stopped && self.acknowledged.is_some(),
                    "SDK sink refresh has no current binding",
                )?;
                validate_row_binding(row, self.acknowledged.expect("checked"), self.last_ack)?;
                ensure(
                    row["kind"] == 2
                        && row["sdk_dispatched"] == true
                        && number(row, "steam_input_handle")? == self.handle
                        && self.handle != 0,
                    "SDK sink refresh changed its real handle",
                )?;
                validate_desired(row, self.actual_desired)?;
                ensure(
                    self.actual_desired.left != 0 || self.actual_desired.right != 0,
                    "SDK sink refreshed a retired desired state",
                )?;
                self.report.refresh_dispatches += 1;
            }
            Some("steam_input_probe_error") => {
                ensure(
                    !self.ready,
                    "Unexpected SDK probe failure after sink readiness",
                )?;
                let reason = probe_reason(row["reason"].as_str())
                    .ok_or("Unknown SDK sink initialization reason code")?;
                let code = if row["win32_error"].is_null() {
                    None
                } else {
                    Some(
                        row["win32_error"]
                            .as_u64()
                            .and_then(|value| u32::try_from(value).ok())
                            .ok_or("Invalid SDK probe error number")?,
                    )
                };
                self.report.helper_probe_error = Some(reason.into());
                self.report.helper_probe_win32_error = code;
                return Err(format!("SDK sink initialization failed: {reason}"));
            }
            Some("steam_input_sink_stopped") => {
                ensure(
                    self.ready && !self.stopped && row["physical_device_verified"] == false,
                    "Unexpected SDK sink stopped message",
                )?;
                let reason = stopped_reason(row["reason"].as_str())
                    .ok_or("SDK sink stopped with an unknown reason code")?;
                let epoch = number(row, "epoch")?;
                let sequence = number(row, "sequence")?;
                let control = row["control_session"]
                    .as_u64()
                    .and_then(|value| u32::try_from(value).ok())
                    .ok_or("SDK sink stopped session is invalid")?;
                let generation = row["generation"]
                    .as_u64()
                    .and_then(|value| u32::try_from(value).ok())
                    .ok_or("SDK sink stopped generation is invalid")?;
                let transmitted = self
                    .pending
                    .iter()
                    .filter(|pending| pending.offset == 64)
                    .map(|pending| pending.sequence)
                    .max()
                    .unwrap_or(self.last_ack);
                let before_bind = epoch == 0
                    && sequence == 0
                    && control == 0
                    && generation == 0
                    && self.last_ack == 0;
                let current = epoch == self.epoch
                    && Some(control) == self.control_session
                    && sequence >= self.last_ack
                    && sequence <= transmitted
                    && (generation == 0
                        || self
                            .acknowledged
                            .is_some_and(|binding| binding.generation == generation)
                        || self
                            .pending
                            .iter()
                            .any(|pending| pending.binding.generation == generation));
                ensure(
                    before_bind || current,
                    "SDK sink stopped metadata belongs to another connection",
                )?;
                let input_shutdown = row["input_shutdown"]
                    .as_bool()
                    .ok_or("SDK sink shutdown result is missing")?;
                let sdk_shutdown = row["sdk_shutdown_dispatched"]
                    .as_bool()
                    .ok_or("SDK sink shutdown dispatch is missing")?;
                let transport_error = if row["transport_win32_error"].is_null() {
                    None
                } else {
                    Some(
                        row["transport_win32_error"]
                            .as_u64()
                            .and_then(|value| u32::try_from(value).ok())
                            .ok_or("SDK sink transport error is invalid")?,
                    )
                };
                self.report.helper_stopped_reason = Some(reason.into());
                self.report.helper_transport_win32_error = transport_error;
                self.report.helper_input_shutdown = Some(input_shutdown);
                self.report.helper_sdk_shutdown_dispatched = Some(sdk_shutdown);
                if !matches!(reason, "shutdown" | "eof") {
                    return Err(format!("SDK sink stopped: {reason}"));
                }
                ensure(
                    input_shutdown && sdk_shutdown,
                    "SDK sink did not dispatch genuine teardown",
                )?;
                let pending_shutdown = self
                    .pending
                    .front()
                    .filter(|pending| pending.kind == Kind::Shutdown && pending.offset == 64);
                if let Some(pending) = pending_shutdown {
                    ensure(
                        self.pending.len() == 1
                            && row["reason"] == "shutdown"
                            && number(row, "epoch")? == self.epoch
                            && number(row, "sequence")? == pending.sequence
                            && row["control_session"].as_u64()
                                == Some(u64::from(pending.binding.control_session))
                            && row["generation"].as_u64()
                                == Some(u64::from(pending.binding.generation)),
                        "SDK sink shutdown ACK mismatch",
                    )?;
                    self.pending.pop_front();
                } else {
                    ensure(
                        self.pending.is_empty()
                            && row["reason"] == "eof"
                            && number(row, "epoch")?
                                == if self.control_session.is_some() {
                                    self.epoch
                                } else {
                                    0
                                }
                            && number(row, "sequence")? == self.last_ack,
                        "SDK sink stopped with an error or unacknowledged commands",
                    )?;
                }
                self.stopped = true;
                self.report.stopped_message = true;
            }
            _ => return Err("Unrecognized SDK sink status event".into()),
        }
        Ok(())
    }
    fn accept_ack(&mut self, row: &Value) -> Result<(), String> {
        let pending = self
            .pending
            .front()
            .ok_or("SDK sink acknowledged an unqueued command")?;
        ensure(
            self.ready && !self.stopped && pending.offset == 64 && pending.kind != Kind::Shutdown,
            "SDK sink acknowledgement is out of order",
        )?;
        validate_row_binding(row, pending.binding, pending.sequence)?;
        ensure(
            row["kind"].as_u64() == Some(pending.kind as u64),
            "SDK sink acknowledgement kind mismatch",
        )?;
        validate_desired(
            row,
            if pending.kind == Kind::Ping {
                self.actual_desired
            } else {
                pending.desired
            },
        )?;
        let handle = number(row, "steam_input_handle")?;
        let dispatched = row["sdk_dispatched"]
            .as_bool()
            .ok_or("SDK dispatch flag missing")?;
        match pending.kind {
            Kind::Bind => {
                ensure(
                    handle != 0 && !dispatched && self.acknowledged.is_none(),
                    "SDK sink binding handle is not genuine",
                )?;
                self.handle = handle;
                self.acknowledged = Some(pending.binding);
                self.report.suspended = false;
            }
            Kind::Submit => ensure(
                self.acknowledged == Some(pending.binding)
                    && handle == self.handle
                    && handle != 0
                    && dispatched,
                "SDK sink submit ACK lost its exact handle",
            )?,
            Kind::Retire => {
                ensure(
                    self.acknowledged == Some(pending.binding)
                        && handle == self.handle
                        && handle != 0,
                    "SDK sink retire ACK targets another handle",
                )?;
                if dispatched {
                    ensure(
                        self.actual_desired.left != 0 || self.actual_desired.right != 0,
                        "Unexpected retirement SDK dispatch",
                    )?;
                }
                self.report.last_retired_binding = Some(pending.binding);
                self.report.retirements += 1;
                self.acknowledged = None;
                self.handle = 0;
                self.report.suspended = false;
            }
            Kind::Ping => ensure(
                handle == self.handle && !dispatched,
                "SDK sink heartbeat changed handle or dispatched motors",
            )?,
            Kind::Shutdown => unreachable!(),
        }
        let pending = self.pending.pop_front().expect("validated pending");
        let timing = pending.timing();
        self.report.last_ack_timing = Some(timing);
        if pending.kind == Kind::Bind {
            self.report.bind_ack_timing = Some(timing);
        }
        self.report.max_ack_sent_ms = self.report.max_ack_sent_ms.max(timing.sent_ms.unwrap_or(0));
        if pending.kind != Kind::Ping {
            self.actual_desired = pending.desired;
        }
        self.last_ack = pending.sequence;
        self.report.acknowledgements += 1;
        if dispatched {
            self.report.sdk_dispatches += 1;
        }
        if let Some(sequence) = pending.driver_sequence {
            self.report.last_driver_sequence = Some(sequence);
        }
        self.report.bound = self.acknowledged;
        self.report.steam_input_handle = (self.handle != 0).then_some(self.handle);
        Ok(())
    }
}
fn probe_reason(reason: Option<&str>) -> Option<&'static str> {
    const CODES: &[&str] = &[
        "invalid_feedback_sink_opt_in",
        "feedback_sink_requires_real_game_sdk",
        "feedback_sink_xinput_unavailable",
        "feedback_sink_identity_unverified",
        "feedback_sink_writer_unavailable",
        "missing_expected_identity",
        "missing_expected_appid",
        "invalid_sdk_path",
        "load_sdk_failed",
        "sdk_init_exports_missing",
        "sdk_pipe_exports_missing",
        "sdk_init_failed",
        "sdk_classic_init_failed",
        "sdk_connection_handles_missing",
        "steamclient_factory_missing",
        "steamclient_interface_missing",
        "steamclient_identity_mismatch",
        "steamclient_connection_missing",
        "steam_user_missing",
        "steam_utils_interface_missing",
        "steam_appid_mismatch",
        "steam_input_interface_missing",
        "steam_input_init_failed",
        "steamclient_path_must_be_absolute",
        "load_steamclient_failed",
    ];
    CODES.iter().copied().find(|known| Some(*known) == reason)
}

fn stopped_reason(reason: Option<&str>) -> Option<&'static str> {
    const CODES: &[&str] = &[
        "shutdown",
        "eof",
        "malformed_control",
        "invalid_control_identity_or_sequence",
        "motors_outside_submit",
        "wrong_control_connection",
        "missing_control_session",
        "binding_not_fresh",
        "bind_route_unverified",
        "output_queue_unavailable",
        "wrong_binding_generation",
        "submit_without_binding",
        "submit_route_changed",
        "retire_without_binding",
        "heartbeat_identity_lost",
        "heartbeat_route_changed",
        "stdout_writer_failed",
        "initial_bind_timeout",
        "heartbeat_expired",
        "identity_lost",
        "route_lost",
        "route_suspended_timeout",
        "truncated_control_eof",
        "stdin_peek_failed",
        "control_queue_flood",
        "stdin_read_failed",
        "control_rate_flood",
        "partial_control_timeout",
    ];
    CODES.iter().copied().find(|known| Some(*known) == reason)
}

fn number(row: &Value, key: &str) -> Result<u64, String> {
    row[key]
        .as_str()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| format!("SDK sink numeric field invalid: {key}"))
}
fn validate_row_binding(
    row: &Value,
    binding: FeedbackBinding,
    sequence: u64,
) -> Result<(), String> {
    ensure(
        number(row, "epoch")? == binding.epoch
            && number(row, "sequence")? == sequence
            && row["control_session"].as_u64() == Some(u64::from(binding.control_session))
            && row["slot"].as_u64() == Some(u64::from(binding.slot))
            && row["generation"].as_u64() == Some(u64::from(binding.generation))
            && row["physical_device_verified"] == false,
        "SDK sink ACK has stale connection or generation",
    )
}
fn validate_desired(row: &Value, desired: Desired) -> Result<(), String> {
    ensure(
        number(row, "desired_sequence")? == desired.sequence
            && row["left"].as_u64() == Some(u64::from(desired.left))
            && row["right"].as_u64() == Some(u64::from(desired.right)),
        "SDK sink desired motors or origin sequence changed",
    )
}

pub struct Session {
    protocol: Protocol,
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
    tracker: Box<dyn HelperTracker>,
    helper: Option<ProcessIdentity>,
    termination: Option<Termination>,
    launcher_exit: Option<Exit>,
    line: Vec<u8>,
    discarding_line: bool,
    eof: bool,
    last_ping: Instant,
    last_poll_at: Option<Instant>,
    poll_timing: PollTiming,
    started: Instant,
    finished: bool,
    launcher_reaped: bool,
    _same_thread: PhantomData<Rc<()>>,
}
impl Session {
    pub fn start(
        mut command: Command,
        probe: &VerifiedProbe,
        context: Context,
        tracker: impl FnOnce(u32) -> Result<Box<dyn HelperTracker>, String>,
    ) -> Result<Self, String> {
        ensure(
            command.get_args().any(|arg| arg == probe.path.as_os_str()),
            "SDK sink command does not contain the verified probe",
        )?;
        ensure(
            format!(
                "{:x}",
                Sha256::digest(bounded_file(&probe.path, 16 * 1024 * 1024)?)
            ) == probe.sha256,
            "SDK sink payload changed before spawn",
        )?;
        command
            .env("SKINOS_INPUT_FEEDBACK_SINK", "1")
            .env(
                "SKINOS_EXPECTED_STEAMID",
                context.expected_account.to_string(),
            )
            .env("SteamAppId", context.appid.to_string())
            .env_remove("SKINOS_INPUT_RUMBLE_TEST")
            .env_remove("SKINOS_INPUT_RUMBLE_FIXTURE_ID")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|_| "Could not start owned SDK sink")?;
        let setup = (|| {
            let stdin = child.stdin.take().ok_or("SDK sink stdin missing")?;
            let stdout = child.stdout.take().ok_or("SDK sink stdout missing")?;
            nonblocking(stdin.as_raw_fd())?;
            nonblocking(stdout.as_raw_fd())?;
            let tracker = tracker(child.id())?;
            Ok::<_, String>((stdin, stdout, tracker))
        })();
        let (stdin, stdout, tracker) = match setup {
            Ok(value) => value,
            Err(error) => {
                let _ = kill_owned_launcher(&mut child);
                return Err(error);
            }
        };
        Ok(Self {
            protocol: Protocol::new(&context),
            child,
            stdin: Some(stdin),
            stdout,
            tracker,
            helper: None,
            termination: None,
            launcher_exit: None,
            line: Vec::new(),
            discarding_line: false,
            eof: false,
            last_ping: Instant::now(),
            last_poll_at: None,
            poll_timing: PollTiming::default(),
            started: Instant::now(),
            finished: false,
            launcher_reaped: false,
            _same_thread: PhantomData,
        })
    }
    pub fn ready(&self) -> bool {
        self.protocol.ready
            && self.protocol.report.error.is_none()
            && self.helper.is_some()
            && self.termination.is_none()
            && !self.protocol.closing
            && !self.finished
    }
    pub fn command_capacity(&self) -> usize {
        MAX_PENDING.saturating_sub(self.protocol.pending.len())
    }
    pub fn bind(&mut self, binding: FeedbackBinding) -> Result<(), String> {
        self.command(Kind::Bind, binding, None)
    }
    pub fn submit(&mut self, event: FeedbackRumble) -> Result<(), String> {
        self.command(Kind::Submit, event.binding, Some(event))
    }
    pub fn retire(&mut self, binding: FeedbackBinding) -> Result<(), String> {
        self.command(Kind::Retire, binding, None)
    }
    fn command(
        &mut self,
        kind: Kind,
        binding: FeedbackBinding,
        motors: Option<FeedbackRumble>,
    ) -> Result<(), String> {
        if let Some(error) = &self.protocol.report.error {
            return Err(error.clone());
        }
        if self.helper.is_none() || self.termination.is_some() {
            return Err("SDK sink actual helper process is not currently verified".into());
        }
        let result = self.protocol.queue(kind, binding, motors);
        if let Err(error) = &result {
            self.fail(error);
        }
        result
    }
    fn fail(&mut self, error: &str) {
        let reason = self
            .protocol
            .report
            .helper_stopped_reason
            .as_deref()
            .filter(|reason| !matches!(*reason, "shutdown" | "eof"));
        if let Some(reason) = reason {
            self.protocol.report.error = Some(format!("SDK sink stopped: {reason}"));
        } else if let Some(reason) = &self.protocol.report.helper_probe_error {
            self.protocol.report.error = Some(format!("SDK sink initialization failed: {reason}"));
        } else if self.protocol.report.error.is_none() {
            self.protocol.report.error = Some(error.into());
        }
        self.stdin.take();
    }
    pub fn poll(&mut self) -> Result<Report, String> {
        if self.finished {
            return self.health();
        }
        let began = Instant::now();
        self.poll_timing = PollTiming {
            gap_ms: self
                .last_poll_at
                .map(|last| millis(began.duration_since(last)))
                .unwrap_or(0),
            ..Default::default()
        };
        self.last_poll_at = Some(began);
        let result = self.poll_inner();
        self.poll_timing.total_ms = millis(began.elapsed());
        self.protocol.report.last_poll = Some(self.poll_timing);
        if self.protocol.report.timeout_pending.is_some()
            && self.protocol.report.timeout_poll.is_none()
        {
            self.protocol.report.timeout_poll = Some(self.poll_timing);
        }
        if let Err(error) = result {
            self.fail(&error);
            return Err(error);
        }
        self.health()
    }
    fn observe_helper(&mut self, observation: HelperObservation) -> Result<(), String> {
        match observation {
            HelperObservation::Searching => {}
            HelperObservation::Running(identity) => {
                ensure(
                    identity.pid > 1 && self.helper.is_none_or(|old| old == identity),
                    "SDK sink helper identity changed",
                )?;
                ensure(
                    self.termination.is_none(),
                    "Retired SDK sink helper reappeared",
                )?;
                self.helper = Some(identity);
                self.protocol.report.helper_process = Some(identity);
            }
            HelperObservation::Terminated {
                identity,
                reason,
                exit,
            } => {
                ensure(
                    self.helper == Some(identity),
                    "SDK sink terminated without a verified helper identity",
                )?;
                self.termination = Some(reason);
                self.protocol.report.helper_termination = Some(reason);
                self.protocol.report.helper_exit = exit;
                ensure(
                    exit.is_none_or(|exit| !exit.failed()),
                    "Actual SDK sink helper exited with an error",
                )?;
            }
        }
        Ok(())
    }
    fn poll_inner(&mut self) -> Result<(), String> {
        // Read all bounded available status before lifecycle inspection. A
        // exiting helper can have already queued its concrete failure reason.
        let read_began = Instant::now();
        let first_status = self.read_status();
        self.poll_timing.read_before_ms = millis(read_began.elapsed());
        let tracker_began = Instant::now();
        let launcher_result = (|| -> Result<(), String> {
            if self.launcher_exit.is_none()
                && let Some(status) = self
                    .child
                    .try_wait()
                    .map_err(|_| "Cannot inspect owned SDK sink launcher")?
            {
                self.launcher_exit = Some(Exit::from_status(status));
                self.launcher_reaped = true;
            }
            Ok(())
        })();
        let observed = self
            .tracker
            .observe()
            .map_err(|_| "Cannot inspect the scoped SDK sink helper".to_string());
        let helper_result = match observed {
            Ok(observation) => self.observe_helper(observation),
            Err(_) if (self.protocol.closing || self.protocol.stopped) && self.helper.is_some() => {
                // macOS may retire a Wine wrapper before the PID becomes ESRCH.
                // Closing has an explicit finite finish deadline; wait for a
                // kernel proof, never convert this unknown state into exit.
                self.protocol.report.closing_inspection_retries += 1;
                Ok(())
            }
            Err(error) => Err(error),
        };
        self.poll_timing.tracker_ms = millis(tracker_began.elapsed());
        // A valid ACK can arrive while process inspection is in progress.
        // Always drain again before deciding a pending command has timed out.
        let read_began = Instant::now();
        let final_status = self.read_status();
        self.poll_timing.read_after_ms = millis(read_began.elapsed());
        if let Some(reason) = self
            .protocol
            .report
            .helper_stopped_reason
            .as_deref()
            .filter(|reason| !matches!(*reason, "shutdown" | "eof"))
        {
            return Err(format!("SDK sink stopped: {reason}"));
        }
        first_status?;
        final_status?;
        launcher_result?;
        helper_result?;
        if self.protocol.report.error.is_some() {
            return Ok(());
        }
        ensure(
            self.termination.is_none() || (self.protocol.stopped && self.protocol.closing),
            "SDK sink helper exited before requested shutdown proof",
        )?;
        ensure(
            self.launcher_exit.is_none_or(|exit| !exit.failed()),
            "Owned SDK sink launcher exited with an error",
        )?;
        if !self.protocol.ready {
            ensure(
                self.started.elapsed() < Duration::from_secs(10),
                "SDK sink readiness timed out",
            )?;
        }
        if self.eof {
            ensure(
                self.protocol.stopped && self.line.is_empty(),
                "SDK sink stdout closed without complete shutdown proof",
            )?;
        }
        self.check_deadline()?;
        if self.protocol.ready
            && !self.protocol.closing
            && !self.protocol.stopped
            && self.protocol.control_session.is_some()
            && self.last_ping.elapsed() >= PING_INTERVAL
        {
            let binding = self.protocol.current_wire_binding();
            self.protocol.queue(Kind::Ping, binding, None)?;
            self.last_ping = Instant::now();
        }
        let write_began = Instant::now();
        let result = self.write_commands();
        self.poll_timing.write_ms = millis(write_began.elapsed());
        result
    }
    fn check_deadline(&mut self) -> Result<(), String> {
        if !self.protocol.pending.front().is_some_and(Pending::expired) {
            return Ok(());
        }
        // The inspection itself can cross the deadline. Consume already-arrived
        // responses, then re-evaluate the current queue front rather than an
        // obsolete borrow of the command that may have just been acknowledged.
        let began = Instant::now();
        let result = self.read_status();
        self.poll_timing.read_after_ms += millis(began.elapsed());
        result?;
        if let Some(pending) = self
            .protocol
            .pending
            .front()
            .filter(|pending| pending.expired())
        {
            self.protocol
                .report
                .timeout_pending
                .get_or_insert(pending.timing());
            return Err(if pending.sent.is_some() {
                "SDK sink ACK timed out"
            } else {
                "SDK sink command write timed out"
            }
            .into());
        }
        Ok(())
    }
    fn write_commands(&mut self) -> Result<(), String> {
        if let Some(stdin) = self.stdin.as_mut() {
            for pending in &mut self.protocol.pending {
                while pending.offset < 64 {
                    match stdin.write(&pending.bytes[pending.offset..]) {
                        Ok(0) => return Err("SDK sink stdin closed during command".into()),
                        Ok(bytes) => {
                            pending.offset += bytes;
                            if pending.offset == 64 {
                                pending.sent = Some(Instant::now());
                                self.protocol.report.commands_sent += 1;
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            return Ok(());
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => return Err("Could not write SDK sink command".into()),
                    }
                }
            }
        }
        Ok(())
    }
    fn read_status(&mut self) -> Result<(), String> {
        let mut buffer = [0u8; 4096];
        let mut read = 0;
        let mut failure = None;
        while read < MAX_READ_PER_POLL {
            match self.stdout.read(&mut buffer) {
                Ok(0) => {
                    self.eof = true;
                    break;
                }
                Ok(bytes) => {
                    read += bytes;
                    for &byte in &buffer[..bytes] {
                        if byte == b'\n' {
                            if self.discarding_line {
                                self.discarding_line = false;
                                self.line.clear();
                                continue;
                            }
                            if !self.line.is_empty() {
                                let row = serde_json::from_slice::<Value>(&self.line)
                                    .map_err(|_| "Malformed SDK sink status JSON".to_string());
                                self.line.clear();
                                let result = row.and_then(|row| self.protocol.accept(&row));
                                if let Err(error) = result {
                                    failure.get_or_insert(error);
                                }
                            }
                        } else if !self.discarding_line {
                            if self.line.len() == MAX_LINE {
                                self.line.clear();
                                self.discarding_line = true;
                                failure.get_or_insert("SDK sink status line exceeded bound".into());
                            } else {
                                self.line.push(byte);
                            }
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    failure.get_or_insert("Could not read SDK sink status".into());
                    break;
                }
            }
        }
        if read >= MAX_READ_PER_POLL {
            failure.get_or_insert("SDK sink status rate exceeded poll bound".into());
        }
        failure.map_or(Ok(()), Err)
    }
    fn health(&self) -> Result<Report, String> {
        self.protocol
            .report
            .error
            .clone()
            .map_or_else(|| Ok(self.protocol.report.clone()), Err)
    }
    pub fn report(&self) -> Report {
        self.protocol.report.clone()
    }
    /// Stop genuine SDK state BEFORE parent Valve teardown. This deliberately
    /// does not wait for a Wine shim held by the still-live input feeder.
    pub fn finish(&mut self, timeout: Duration, mut pump: impl FnMut()) -> Report {
        if self.finished {
            return self.report();
        }
        if self.protocol.report.error.is_none()
            && self.protocol.ready
            && self.protocol.control_session.is_some()
        {
            let binding = self.protocol.current_wire_binding();
            if let Err(error) = self.protocol.queue(Kind::Shutdown, binding, None) {
                self.fail(&error);
            }
        } else {
            self.protocol.closing = true;
            self.stdin.take();
        }
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            pump();
            let _ = self.poll();
            if self.termination.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        self.stdin.take();
        if self.termination.is_none() {
            self.fail("Actual SDK sink helper termination was not verified");
            if self.helper.is_some() {
                self.protocol.report.helper_forced_cleanup = self.tracker.terminate_owned().is_ok();
            }
            let deadline = Instant::now() + Duration::from_millis(250);
            while self.termination.is_none() && Instant::now() < deadline {
                let _ = self.poll();
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        if !self.protocol.stopped {
            self.fail("SDK sink shutdown transcript is incomplete");
        }
        self.protocol.report.waiting_launcher_pending = !self.launcher_reaped;
        self.finished = true;
        self.report()
    }
    /// Caller invokes only AFTER feeder/transport teardown, independently of
    /// `finish`. A final JSON line never substitutes for actual process exit.
    pub fn reap_launcher(&mut self, transport_stopped: bool, timeout: Duration) -> LauncherReport {
        if !transport_stopped || !self.finished || self.termination.is_none() {
            return LauncherReport {
                error: Some(
                    "SDK sink launcher cleanup precedes verified helper/transport teardown".into(),
                ),
                ..Default::default()
            };
        }
        let deadline = Instant::now() + timeout;
        while !self.launcher_reaped && Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    self.launcher_exit = Some(Exit::from_status(status));
                    self.launcher_reaped = true;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                Err(_) => break,
            }
        }
        let forced = !self.launcher_reaped;
        if forced && let Some(exit) = kill_owned_launcher(&mut self.child) {
            self.launcher_exit = Some(exit);
            self.launcher_reaped = true;
        }
        self.protocol.report.waiting_launcher_pending = !self.launcher_reaped;
        LauncherReport {
            reaped: self.launcher_reaped,
            exit: self.launcher_exit,
            forced_cleanup: forced,
            error: if !self.launcher_reaped {
                Some("Owned SDK sink launcher was not reaped".into())
            } else if !forced && self.launcher_exit.is_some_and(Exit::failed) {
                Some("Owned SDK sink launcher exited unsuccessfully".into())
            } else {
                None
            },
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.stdin.take();
        if self.termination.is_none() && self.helper.is_some() {
            let _ = self.tracker.terminate_owned();
        }
        if !self.launcher_reaped {
            let _ = kill_owned_launcher(&mut self.child);
        }
    }
}
fn kill_owned_launcher(child: &mut Child) -> Option<Exit> {
    let _ = child.kill();
    let deadline = Instant::now() + Duration::from_millis(250);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(Exit::from_status(status)),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => return None,
        }
    }
}

fn nonblocking(fd: i32) -> Result<(), String> {
    let flags = unsafe { fcntl(fd, 3, 0) };
    ensure(flags >= 0, "Cannot inspect SDK sink pipe flags")?;
    ensure(
        unsafe { fcntl(fd, 4, flags | 4) } == 0,
        "Cannot set SDK sink pipe nonblocking",
    )?;
    let actual = unsafe { fcntl(fd, 3, 0) };
    ensure(
        actual >= 0 && actual & 4 != 0,
        "SDK sink pipe remained blocking",
    )
}
fn ensure(value: bool, message: &str) -> Result<(), String> {
    if value { Ok(()) } else { Err(message.into()) }
}
fn bounded_file(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "SDK sink asset unavailable")?;
    ensure(
        metadata.is_file() && metadata.len() <= limit,
        "SDK sink asset is not a bounded regular file",
    )?;
    let mut data = Vec::new();
    fs::File::open(path)
        .map_err(|_| "SDK sink asset cannot be opened")?
        .take(limit + 1)
        .read_to_end(&mut data)
        .map_err(|_| "SDK sink asset cannot be read")?;
    ensure(
        data.len() as u64 <= limit,
        "SDK sink asset grew beyond bound",
    )?;
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    fn binding(generation: u32) -> FeedbackBinding {
        FeedbackBinding {
            epoch: 77,
            control_session: 12,
            slot: 0,
            generation,
        }
    }
    fn context() -> Context {
        Context::new(42, 123, 77).unwrap()
    }
    fn ready(protocol: &mut Protocol) {
        protocol.accept(&json!({"event":"steam_input_app_context","sdk_initialized":true,"appid_verified":true,"appid":42,"expected_appid":42,"init_method":"SteamAPI_InitFlat"})).unwrap();
        protocol.accept(&json!({"event":"steam_input_sink_ready","version":1,"control_frame_bytes":64,"appid":42,"sdk_initialized":true,"identity_verified":true,"appid_verified":true,"physical_device_verified":false,"refresh_ms":250,"heartbeat_lease_ms":1500})).unwrap();
    }
    fn ack(protocol: &mut Protocol, handle: u64, dispatched: bool) -> Value {
        let pending = protocol.pending.front_mut().unwrap();
        pending.offset = 64;
        json!({"event":"steam_input_sink_ack","kind":pending.kind as u32,"epoch":pending.binding.epoch.to_string(),"control_session":pending.binding.control_session,"slot":pending.binding.slot,"generation":pending.binding.generation,"sequence":pending.sequence.to_string(),"desired_sequence":pending.desired.sequence.to_string(),"steam_input_handle":handle.to_string(),"left":pending.desired.left,"right":pending.desired.right,"sdk_dispatched":dispatched,"physical_device_verified":false})
    }
    #[test]
    fn bind_settling_telemetry_is_bounded_and_is_not_a_binding_ack() {
        let mut p = Protocol::new(&context());
        ready(&mut p);
        p.queue(Kind::Bind, binding(3), None).unwrap();
        p.pending[0].offset = 64;
        let mut row = json!({"event":"steam_input_sink_bind_timing","epoch":"77","control_session":12,"slot":0,"generation":3,"sequence":"1","identity_ms":0,"controllers_ms":0,"mapping_ms":0,"xinput_ms":0,"total_ms":30,"last_stage":5,"controller_count":1,"xinput_calls":4,"route_verified":true,"attempts":4,"wait_ms":30,"physical_device_verified":false});
        p.accept(&row).unwrap();
        assert!(p.report.bound.is_none());
        assert_eq!(p.report.acknowledgements, 0);
        assert_eq!(p.report.bind_route_timing.as_ref().unwrap()["attempts"], 4);
        assert_eq!(p.report.bind_route_timing.as_ref().unwrap()["wait_ms"], 30);
        row["attempts"] = json!(52);
        assert!(p.accept(&row).is_err());
        row["attempts"] = json!(1);
        row["xinput_calls"] = json!(5);
        assert!(p.accept(&row).is_err());
    }
    #[test]
    fn binary_protocol_and_ack_order_bind_actual_handle_then_retire_generation() {
        let mut p = Protocol::new(&context());
        ready(&mut p);
        p.queue(Kind::Bind, binding(3), None).unwrap();
        let words = p.pending[0]
            .bytes
            .chunks_exact(4)
            .map(|n| u32::from_le_bytes(n.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(
            words,
            [0x53495253, 1, 64, 1, 77, 0, 12, 0, 3, 1, 0, 0, 0, 0, 0, 0]
        );
        p.queue(
            Kind::Submit,
            binding(3),
            Some(FeedbackRumble {
                binding: binding(3),
                sequence: 99,
                left: 16384,
                right: 0,
            }),
        )
        .unwrap();
        p.queue(Kind::Retire, binding(3), None).unwrap();
        p.queue(Kind::Bind, binding(4), None).unwrap();
        let row = ack(&mut p, 55, false);
        p.accept(&row).unwrap();
        let row = ack(&mut p, 55, true);
        p.accept(&row).unwrap();
        assert_eq!(p.report.last_driver_sequence, Some(99));
        let row = ack(&mut p, 55, true);
        p.accept(&row).unwrap();
        assert!(p.report.bound.is_none());
        let row = ack(&mut p, 88, false);
        p.accept(&row).unwrap();
        assert_eq!(p.report.steam_input_handle, Some(88));
        assert!(
            p.queue(
                Kind::Submit,
                binding(3),
                Some(FeedbackRumble {
                    binding: binding(3),
                    sequence: 100,
                    left: 1,
                    right: 1
                })
            )
            .is_err()
        );
    }
    #[test]
    fn bad_account_context_handle_generation_and_unwritten_ack_never_succeed() {
        let mut p = Protocol::new(&context());
        assert!(
            p.accept(&json!({"event":"steam_input_sink_ready"}))
                .is_err()
        );
        for mutation in 0..6 {
            let mut p = Protocol::new(&context());
            ready(&mut p);
            p.queue(Kind::Bind, binding(3), None).unwrap();
            let mut row = ack(&mut p, 55, false);
            match mutation {
                0 => row["epoch"] = json!("78"),
                1 => row["generation"] = json!(2),
                2 => row["steam_input_handle"] = json!("0"),
                3 => row["sdk_dispatched"] = json!(true),
                4 => row["sequence"] = json!("2"),
                _ => p.pending[0].offset = 0,
            }
            assert!(p.accept(&row).is_err());
            assert_eq!(p.report.acknowledgements, 0);
        }
        let mut p = Protocol::new(&context());
        ready(&mut p);
        p.queue(Kind::Bind, binding(3), None).unwrap();
        let row = ack(&mut p, 55, false);
        p.accept(&row).unwrap();
        p.queue(
            Kind::Submit,
            binding(3),
            Some(FeedbackRumble {
                binding: binding(3),
                sequence: 10,
                left: 1,
                right: 2,
            }),
        )
        .unwrap();
        let row = ack(&mut p, 99, true);
        assert!(p.accept(&row).is_err());
    }
    #[test]
    fn refresh_is_separate_from_driver_ack_and_preserves_latest_submit_origin() {
        let mut p = Protocol::new(&context());
        ready(&mut p);
        p.queue(Kind::Bind, binding(3), None).unwrap();
        let row = ack(&mut p, 55, false);
        p.accept(&row).unwrap();
        p.queue(
            Kind::Submit,
            binding(3),
            Some(FeedbackRumble {
                binding: binding(3),
                sequence: 101,
                left: 1,
                right: 2,
            }),
        )
        .unwrap();
        let mut refresh = ack(&mut p, 55, true);
        p.accept(&refresh).unwrap();
        refresh["event"] = json!("steam_input_sink_refresh");
        p.accept(&refresh).unwrap();
        assert_eq!(p.report.refresh_dispatches, 1);
        assert_eq!(p.report.acknowledgements, 2);
        refresh["desired_sequence"] = json!("1");
        assert!(p.accept(&refresh).is_err());
        assert!(
            p.queue(
                Kind::Submit,
                binding(3),
                Some(FeedbackRumble {
                    binding: binding(3),
                    sequence: 101,
                    left: 1,
                    right: 2
                })
            )
            .is_err()
        );
    }
    #[test]
    fn stopped_reasons_are_allowlisted_diagnostics_not_success_ack() {
        for reason in [
            "bind_route_unverified",
            "stdin_peek_failed",
            "identity_lost",
        ] {
            let mut p = Protocol::new(&context());
            ready(&mut p);
            p.queue(Kind::Bind, binding(3), None).unwrap();
            p.pending[0].offset = 64;
            let row = json!({"event":"steam_input_sink_stopped","reason":reason,"epoch":"0","control_session":0,"generation":0,"sequence":"0","zero_dispatched":false,"input_shutdown":true,"sdk_shutdown_dispatched":true,"transport_win32_error":6,"physical_device_verified":false});
            let error = p.accept(&row).unwrap_err();
            assert!(error.contains(reason));
            assert_eq!(p.report.helper_stopped_reason.as_deref(), Some(reason));
            assert_eq!(p.report.helper_transport_win32_error, Some(6));
            assert!(!p.report.stopped_message);
            assert_eq!(p.report.acknowledgements, 0);
        }
        let mut p = Protocol::new(&context());
        ready(&mut p);
        assert!(p.accept(&json!({"event":"steam_input_sink_stopped","reason":"private raw diagnostic","physical_device_verified":false})).is_err());
        assert!(p.report.helper_stopped_reason.is_none());
        let mut p = Protocol::new(&context());
        assert!(p.accept(&json!({"event":"steam_input_probe_error","reason":"load_steamclient_failed","win32_error":126})).is_err());
        assert_eq!(
            p.report.helper_probe_error.as_deref(),
            Some("load_steamclient_failed")
        );
        assert_eq!(p.report.helper_probe_win32_error, Some(126));
    }
    #[test]
    fn suspension_rejects_old_inflight_motors_and_rebinds_only_after_retire_ack() {
        let mut p = Protocol::new(&context());
        ready(&mut p);
        p.queue(Kind::Bind, binding(3), None).unwrap();
        let row = ack(&mut p, 55, false);
        p.accept(&row).unwrap();
        p.queue(
            Kind::Submit,
            binding(3),
            Some(FeedbackRumble {
                binding: binding(3),
                sequence: 1,
                left: 100,
                right: 200,
            }),
        )
        .unwrap();
        let row = ack(&mut p, 55, true);
        p.accept(&row).unwrap();
        p.queue(
            Kind::Submit,
            binding(3),
            Some(FeedbackRumble {
                binding: binding(3),
                sequence: 2,
                left: 300,
                right: 400,
            }),
        )
        .unwrap();
        p.queue(Kind::Ping, binding(3), None).unwrap();
        let suspended = json!({"event":"steam_input_sink_suspended","kind":0,"epoch":"77","control_session":12,"slot":0,"generation":3,"sequence":"2","desired_sequence":"0","steam_input_handle":"55","left":0,"right":0,"sdk_dispatched":true,"physical_device_verified":false});
        p.accept(&suspended).unwrap();
        assert!(p.report.suspended);
        p.pending[0].offset = 64;
        let rejected = json!({"event":"steam_input_sink_rejected","kind":2,"reason":"route_suspended","epoch":"77","control_session":12,"slot":0,"generation":3,"sequence":"3","desired_sequence":"0","steam_input_handle":"55","left":0,"right":0,"requested_left":300,"requested_right":400,"sdk_dispatched":false,"physical_device_verified":false});
        p.accept(&rejected).unwrap();
        assert_eq!(p.report.rejected_commands, 1);
        assert_eq!(p.report.last_driver_sequence, Some(1));
        let mut ping = ack(&mut p, 55, false);
        ping["desired_sequence"] = json!("0");
        ping["left"] = json!(0);
        ping["right"] = json!(0);
        p.accept(&ping).unwrap();
        p.queue(Kind::Retire, binding(3), None).unwrap();
        let row = ack(&mut p, 55, false);
        p.accept(&row).unwrap();
        assert_eq!(p.report.last_retired_binding, Some(binding(3)));
        assert!(!p.report.suspended);
        p.queue(Kind::Bind, binding(4), None).unwrap();
        let row = ack(&mut p, 88, false);
        p.accept(&row).unwrap();
        assert_eq!(p.report.steam_input_handle, Some(88));
        assert_eq!(p.report.retirements, 1);
    }
    #[test]
    fn queued_commands_and_assets_are_bounded_before_use() {
        let mut p = Protocol::new(&context());
        ready(&mut p);
        p.queue(Kind::Bind, binding(3), None).unwrap();
        for sequence in 1..MAX_PENDING as u64 {
            p.queue(
                Kind::Submit,
                binding(3),
                Some(FeedbackRumble {
                    binding: binding(3),
                    sequence,
                    left: 1,
                    right: 2,
                }),
            )
            .unwrap();
        }
        assert!(p.queue(Kind::Ping, binding(3), None).is_err());
        assert_eq!(p.pending.len(), MAX_PENDING);
        let temp = Temp::new();
        fs::create_dir(temp.0.join("pe")).unwrap();
        let path = temp.0.join("pe/input-probe.exe");
        fs::write(&path, MOCK).unwrap();
        let hash = format!("{:x}", Sha256::digest(MOCK.as_bytes()));
        fs::write(temp.0.join("manifest.json"), json!({"schema_version":1,"experiment":"retained_genuine_sdk_rumble_sink","files":{"pe/input-probe.exe":hash}}).to_string()).unwrap();
        let verified = VerifiedProbe::from_experiment(&temp.0, "input-probe.exe").unwrap();
        fs::write(&path, b"changed before spawn").unwrap();
        assert!(VerifiedProbe::from_experiment(&temp.0, "input-probe.exe").is_err());
        let mut command = Command::new("/usr/bin/python3");
        command.arg(&path);
        assert!(
            Session::start(command, &verified, context(), |_| panic!(
                "must reject before spawn"
            ))
            .is_err()
        );
    }
    const MOCK: &str = r#"import json,os,struct,sys,time
mode=os.environ.get('MOCK_MODE','normal')
if mode=='quiet': time.sleep(30);sys.exit(0)
def out(v): print(json.dumps(v),flush=True)
out(dict(event='steam_input_app_context',sdk_initialized=True,appid_verified=True,appid=42,expected_appid=42,init_method='SteamAPI_InitFlat'))
out(dict(event='steam_input_sink_ready',version=1,control_frame_bytes=64,appid=42,sdk_initialized=True,identity_verified=True,appid_verified=True,physical_device_verified=False,refresh_ms=250,heartbeat_lease_ms=1500))
if mode=='specific_stop':
 out(dict(event='steam_input_sink_stopped',reason='stdin_peek_failed',epoch='0',control_session=0,generation=0,sequence='0',zero_dispatched=False,input_shutdown=True,sdk_shutdown_dispatched=True,transport_win32_error=6,physical_device_verified=False))
 open(__file__+'.ready','w').close()
 time.sleep(30);sys.exit(1)
epoch=session=gen=seq=desired=left=right=handle=0
while True:
 b=sys.stdin.buffer.read(64)
 if not b: reason='eof';break
 if len(b)!=64:sys.exit(2)
 w=struct.unpack('<16I',b);kind=w[3];epoch=w[4]|w[5]<<32;session=w[6];gen=w[8];seq=w[9]|w[10]<<32
 if kind==1:handle=55;desired=left=right=0
 if kind==2:desired=seq;left,right=w[11:13]
 if kind==3:desired=left=right=0
 if kind==5:reason='shutdown';break
 if mode=='gated_ack' and kind==1:
  while not os.path.exists(__file__+'.release_ack'):time.sleep(.001)
 out(dict(event='steam_input_sink_ack',kind=kind,epoch=str(epoch),control_session=session,slot=0,generation=gen+(1 if mode=='bad_ack' else 0),sequence=str(seq),desired_sequence=str(desired),steam_input_handle=str(handle),left=left,right=right,sdk_dispatched=kind==2,physical_device_verified=False))
 if mode=='gated_ack' and kind==1:open(__file__+'.ack_ready','w').close()
 if kind==3:handle=gen=0
out(dict(event='steam_input_sink_stopped',reason=reason,epoch=str(epoch),control_session=session,generation=gen,sequence=str(seq),zero_dispatched=True,input_shutdown=True,sdk_shutdown_dispatched=True,physical_device_verified=False))
if mode=='linger':time.sleep(30)
"#;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "skinos-sink-owner-{}-{}",
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
    struct OwnedMockProcess {
        identity: ProcessIdentity,
    }
    impl HelperTracker for OwnedMockProcess {
        fn observe(&mut self) -> Result<HelperObservation, String> {
            if unsafe { kill(self.identity.pid as i32, 0) } == 0 {
                Ok(HelperObservation::Running(self.identity))
            } else if std::io::Error::last_os_error().raw_os_error() == Some(3) {
                Ok(HelperObservation::Terminated {
                    identity: self.identity,
                    reason: Termination::KernelEsrch,
                    exit: None,
                })
            } else {
                Ok(HelperObservation::Searching)
            }
        }
        fn terminate_owned(&mut self) -> Result<(), String> {
            let result = unsafe { kill(self.identity.pid as i32, 9) };
            ensure(
                result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(3),
                "Cannot stop test-owned helper",
            )
        }
    }
    fn session(mode: &str) -> (Temp, Session) {
        let temp = Temp::new();
        let path = temp.0.join("input-probe.exe");
        fs::write(&path, MOCK).unwrap();
        let probe = VerifiedProbe {
            path: path.clone(),
            sha256: format!("{:x}", Sha256::digest(MOCK.as_bytes())),
        };
        let mut command = Command::new("/usr/bin/python3");
        command.arg("-u").arg(&path).env("MOCK_MODE", mode);
        let session = Session::start(command, &probe, context(), |pid| {
            Ok(Box::new(OwnedMockProcess {
                identity: ProcessIdentity {
                    pid,
                    start_seconds: 1,
                    start_microseconds: 0,
                },
            }))
        })
        .unwrap();
        (temp, session)
    }
    fn poll_until(session: &mut Session, predicate: impl Fn(&Session) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !predicate(session) {
            session.poll().unwrap();
            assert!(Instant::now() < deadline, "mock SDK sink timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn real_pipe_roundtrip_keeps_helper_exit_and_launcher_reap_separate() {
        let (_temp, mut session) = session("normal");
        poll_until(&mut session, Session::ready);
        session.bind(binding(3)).unwrap();
        session
            .submit(FeedbackRumble {
                binding: binding(3),
                sequence: 17,
                left: 1000,
                right: 2000,
            })
            .unwrap();
        poll_until(&mut session, |s| s.report().acknowledgements == 2);
        let report = session.finish(Duration::from_secs(1), || {});
        assert!(report.error.is_none(), "{report:?}");
        assert!(report.stopped_message && report.helper_termination.is_some());
        assert_eq!(report.helper_exit, None);
        assert!(session.reap_launcher(false, Duration::ZERO).error.is_some());
        let launcher = session.reap_launcher(true, Duration::from_secs(1));
        assert!(launcher.reaped && launcher.error.is_none());
        assert_eq!(
            launcher.exit,
            Some(Exit {
                code: Some(0),
                signal: None
            })
        );
        let saved = serde_json::to_string(&report).unwrap();
        assert!(!saved.contains("expected_account"));
        assert!(!saved.contains("expected_account"));
    }
    #[test]
    fn closing_transient_inspection_waits_for_kernel_exit_but_unknown_never_passes() {
        struct Unavailable {
            identity: ProcessIdentity,
            calls: usize,
            recover_after: Option<usize>,
        }
        impl HelperTracker for Unavailable {
            fn observe(&mut self) -> Result<HelperObservation, String> {
                self.calls += 1;
                if self.recover_after.is_some_and(|limit| self.calls > limit) {
                    OwnedMockProcess {
                        identity: self.identity,
                    }
                    .observe()
                } else {
                    Err("wrapper unavailable".into())
                }
            }
            fn terminate_owned(&mut self) -> Result<(), String> {
                Err("identity not revalidated; no signal".into())
            }
        }
        let (_temp, mut transient) = session("normal");
        poll_until(&mut transient, Session::ready);
        transient.bind(binding(3)).unwrap();
        poll_until(&mut transient, |s| s.report().acknowledgements == 1);
        transient.tracker = Box::new(Unavailable {
            identity: transient.helper.unwrap(),
            calls: 0,
            recover_after: Some(3),
        });
        let report = transient.finish(Duration::from_secs(1), || {});
        assert!(report.error.is_none(), "{report:?}");
        assert!(report.stopped_message && report.helper_termination.is_some());
        assert_eq!(report.closing_inspection_retries, 3);
        assert!(!report.helper_forced_cleanup);
        let (_temp, mut persistent) = session("normal");
        poll_until(&mut persistent, Session::ready);
        persistent.bind(binding(3)).unwrap();
        poll_until(&mut persistent, |s| s.report().acknowledgements == 1);
        persistent.tracker = Box::new(Unavailable {
            identity: persistent.helper.unwrap(),
            calls: 0,
            recover_after: None,
        });
        let report = persistent.finish(Duration::from_millis(40), || {});
        assert!(report.error.is_some() && report.helper_termination.is_none());
        assert!(!report.helper_forced_cleanup);
        let (_temp, mut active) = session("normal");
        poll_until(&mut active, Session::ready);
        active.tracker = Box::new(Unavailable {
            identity: active.helper.unwrap(),
            calls: 0,
            recover_after: Some(1),
        });
        assert!(active.poll().is_err());
        assert_eq!(active.report().closing_inspection_retries, 0);
    }
    #[test]
    fn final_json_is_not_process_termination() {
        let (_temp, mut session) = session("linger");
        poll_until(&mut session, Session::ready);
        session.bind(binding(3)).unwrap();
        poll_until(&mut session, |s| s.report().acknowledgements == 1);
        let report = session.finish(Duration::from_millis(100), || {});
        assert!(report.stopped_message && report.helper_forced_cleanup && report.error.is_some());
        let launcher = session.reap_launcher(true, Duration::from_millis(100));
        assert!(launcher.reaped);
        assert_ne!(
            launcher.exit,
            Some(Exit {
                code: Some(0),
                signal: None
            })
        );
    }
    #[test]
    fn malformed_ack_closes_only_sink_and_preserves_an_unrelated_process() {
        let mut other = Command::new("/bin/sh")
            .args(["-c", "read x"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let (_temp, mut session) = session("bad_ack");
        poll_until(&mut session, Session::ready);
        session.bind(binding(3)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if session.poll().is_err() {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(session.stdin.is_none());
        session.finish(Duration::from_millis(250), || {});
        assert!(other.try_wait().unwrap().is_none());
        other.kill().unwrap();
        other.wait().unwrap();
    }
    #[test]
    fn darwin_pipe_flag_readback_and_actual_wouldblock() {
        let (_temp, mut session) = session("quiet");
        session.poll().unwrap();
        let stdin = session.stdin.as_mut().unwrap();
        assert!(unsafe { fcntl(stdin.as_raw_fd(), 3, 0) } & 4 != 0);
        assert!(unsafe { fcntl(session.stdout.as_raw_fd(), 3, 0) } & 4 != 0);
        let mut buffer = [0; 1];
        assert_eq!(
            session.stdout.read(&mut buffer).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        let bytes = [0u8; 65536];
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match stdin.write(&bytes) {
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Ok(_) => {}
                Err(error) => panic!("{error}"),
            }
            assert!(Instant::now() < deadline);
        }
    }
    #[test]
    fn final_pipe_reason_is_drained_before_tracker_inspection_failure() {
        struct Unavailable;
        impl HelperTracker for Unavailable {
            fn observe(&mut self) -> Result<HelperObservation, String> {
                Err("opaque inspection failure".into())
            }
            fn terminate_owned(&mut self) -> Result<(), String> {
                Ok(())
            }
        }
        let (temp, mut session) = session("specific_stop");
        let deadline = Instant::now() + Duration::from_secs(2);
        while !temp.0.join("input-probe.exe.ready").exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        session.tracker = Box::new(Unavailable);
        let error = session.poll().unwrap_err();
        assert_eq!(error, "SDK sink stopped: stdin_peek_failed");
        let report = session.report();
        assert_eq!(
            report.helper_stopped_reason.as_deref(),
            Some("stdin_peek_failed")
        );
        assert_eq!(report.helper_transport_win32_error, Some(6));
        assert!(report.helper_termination.is_none());
        assert!(!report.stopped_message);
    }
    #[test]
    fn sent_ack_budget_is_independent_from_queue_age_and_records_timeout_phase() {
        let mut p = Protocol::new(&context());
        ready(&mut p);
        p.queue(Kind::Bind, binding(3), None).unwrap();
        let pending = &mut p.pending[0];
        pending.queued = Instant::now() - Duration::from_secs(5);
        assert!(pending.expired());
        assert_eq!(pending.timing().sent_ms, None);
        pending.offset = 64;
        pending.sent = Some(Instant::now());
        assert!(!pending.expired());
        assert!(pending.timing().queued_ms >= 5000);
        assert_eq!(pending.timing().written_bytes, 64);
        pending.sent = Some(Instant::now() - ACK_TIMEOUT);
        assert!(pending.expired());
    }
    #[test]
    fn ack_arriving_during_tracker_inspection_is_drained_before_timeout() {
        struct GateTracker {
            identity: ProcessIdentity,
            release: PathBuf,
            ready: PathBuf,
        }
        impl HelperTracker for GateTracker {
            fn observe(&mut self) -> Result<HelperObservation, String> {
                fs::write(&self.release, b"release").unwrap();
                let deadline = Instant::now() + Duration::from_secs(2);
                while !self.ready.exists() {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(5));
                }
                Ok(HelperObservation::Running(self.identity))
            }
            fn terminate_owned(&mut self) -> Result<(), String> {
                OwnedMockProcess {
                    identity: self.identity,
                }
                .terminate_owned()
            }
        }
        let (temp, mut session) = session("gated_ack");
        poll_until(&mut session, Session::ready);
        session.bind(binding(3)).unwrap();
        session.write_commands().unwrap();
        session.protocol.pending[0].sent = Some(Instant::now() - ACK_TIMEOUT);
        let identity = session.helper.unwrap();
        session.tracker = Box::new(GateTracker {
            identity,
            release: temp.0.join("input-probe.exe.release_ack"),
            ready: temp.0.join("input-probe.exe.ack_ready"),
        });
        let report = session.poll().unwrap();
        assert_eq!(report.acknowledgements, 1);
        assert!(report.timeout_pending.is_none());
        assert!(report.bind_ack_timing.unwrap().sent_ms.unwrap() >= 750);
        session.tracker = Box::new(OwnedMockProcess { identity });
        assert!(
            session
                .finish(Duration::from_secs(1), || {})
                .error
                .is_none()
        );
    }
    #[test]
    fn ack_timeout_is_failure_without_an_unbounded_pipe_wait() {
        let (_temp, mut session) = session("normal");
        poll_until(&mut session, Session::ready);
        session.bind(binding(3)).unwrap();
        session.protocol.pending[0].queued = Instant::now() - ACK_TIMEOUT;
        assert!(session.poll().is_err());
        assert!(session.stdin.is_none());
        assert_eq!(session.report().acknowledgements, 0);
        let report = session.finish(Duration::from_millis(250), || {});
        assert!(report.error.is_some());
    }
}
