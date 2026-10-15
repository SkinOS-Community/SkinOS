//! One-pad feedback owner. Fixture QA keeps its observed callback proof; product
//! sessions keep bounded metadata and never claim a physical motor result.
use super::rumble_sink;
use crate::input_fixture::Fixture;
use crate::input_fixture_rumble::Monitor;
use crate::wine_input_relay::{
    FeedbackBatch, FeedbackBinding, FeedbackEvent, FeedbackReport, FeedbackRumble, RelayReport,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::VecDeque, fs, io::Read, path::Path, process::Command, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    FixtureQa,
    Product,
}
impl Mode {
    fn label(self) -> &'static str {
        match self {
            Self::FixtureQa => "fixture_qa",
            Self::Product => "product",
        }
    }
}
pub(super) struct Session {
    pub epoch: u64,
    mode: Mode,
    appid: u32,
    sink: Option<rumble_sink::Session>,
    monitor: Option<Monitor>,
    events: VecDeque<FeedbackEvent>,
    outputs: VecDeque<FeedbackRumble>,
    binding: Option<FeedbackBinding>,
    candidate: Option<FeedbackBinding>,
    retired: Option<FeedbackBinding>,
    retirement_reported: Option<FeedbackBinding>,
    parent_route: Option<Value>,
    captured: bool,
    ready: bool,
    ever_ready: bool,
    waiting: bool,
    inactive: bool,
    error: Option<String>,
    requested: VecDeque<Value>,
    outputs_seen: u64,
    discarded_retired: u64,
    readback: Option<Value>,
    sink_finished: bool,
    feedback_report: FeedbackReport,
}
impl Session {
    pub(super) fn new(epoch: u64, mode: Mode) -> Self {
        Self {
            epoch,
            mode,
            appid: 0,
            sink: None,
            monitor: None,
            events: VecDeque::new(),
            outputs: VecDeque::new(),
            binding: None,
            candidate: None,
            retired: None,
            retirement_reported: None,
            parent_route: None,
            captured: false,
            ready: false,
            ever_ready: false,
            waiting: false,
            inactive: false,
            error: None,
            requested: VecDeque::new(),
            outputs_seen: 0,
            discarded_retired: 0,
            readback: None,
            sink_finished: false,
            feedback_report: FeedbackReport::default(),
        }
    }
    pub(super) fn is_qa(&self) -> bool {
        self.mode == Mode::FixtureQa
    }
    pub(super) fn needs_start(&self) -> bool {
        !self.inactive && !self.failed() && self.sink.is_none()
    }
    pub(super) fn poll_needed(&self) -> bool {
        !self.inactive && !self.failed() && !self.sink_finished && self.sink.is_some()
    }
    pub(super) fn inactive_no_source(&mut self) {
        self.inactive = true;
        self.ready = false;
        status(self.appid, "no_controller", None, None);
    }
    pub(super) fn attached(&mut self, fixture: &Fixture) -> Result<(), String> {
        if self.is_qa() {
            self.monitor = Some(
                fixture
                    .rumble()
                    .ok_or("Feedback fixture callback registration missing")?
                    .monitor(),
            );
        }
        Ok(())
    }
    pub(super) fn configure(&mut self, appid: u32) {
        self.appid = appid;
    }
    pub(super) fn start(&mut self, sink: rumble_sink::Session) -> Result<(), String> {
        if self.sink.is_some() || self.inactive {
            return Err("Feedback SDK sink started outside its active scope".into());
        }
        self.sink = Some(sink);
        Ok(())
    }
    pub(super) fn failed(&self) -> bool {
        self.error.is_some()
    }
    pub(super) fn ready(&self) -> bool {
        self.ready && !self.failed() && !self.inactive
    }
    pub(super) fn has_driver_binding(&self) -> bool {
        self.candidate.is_some()||self.events.iter().any(|event|matches!(event,FeedbackEvent::Bound(binding)if binding.epoch==self.epoch&&binding.control_session!=0&&binding.slot==0&&binding.generation!=0))
    }
    pub(super) fn fail(&mut self, error: String) {
        if self.error.is_none() {
            self.ready = false;
            self.error = Some(error.clone());
            status(self.appid, "failed", Some(&error), self.binding);
        }
    }
    pub(super) fn accept(&mut self, batch: FeedbackBatch) -> Result<(), String> {
        self.feedback_report = batch.report;
        if !self.feedback_report.selected || self.feedback_report.epoch != Some(self.epoch) {
            return Err("Feedback transport is not selected for this owned epoch".into());
        }
        if self.feedback_report.stop_required || self.feedback_report.error.is_some() {
            return Err("Wine feedback channel stopped or failed".into());
        }
        if self.events.len() + batch.events.len() > 64 {
            return Err("Feedback owner event queue overflow".into());
        }
        for event in batch.events {
            if let FeedbackEvent::Rumble(output) = event {
                record_history(
                    self.mode,
                    &mut self.requested,
                    &mut self.outputs_seen,
                    output,
                )?;
            }
            self.events.push_back(event);
        }
        Ok(())
    }
    /// `route` comes from the fresh phase-gated SourceRoute (or the unchanged
    /// fixture guard). None withdraws motor authorization, not game ownership.
    pub(super) fn poll(
        &mut self,
        route: Option<Value>,
        fixture: Option<&Fixture>,
    ) -> Result<(), String> {
        if self.failed() || self.inactive {
            return Ok(());
        }
        let qa = self.is_qa();
        if let Some(route) = &route {
            if qa {
                let fixture = fixture.ok_or("Feedback QA fixture missing")?;
                if route["sdl_id"].as_u64() != Some(u64::from(fixture.joystick_id))
                    || route["fixture"] != true
                    || !fixture.sdl_is_gamepad
                {
                    return Err("Feedback route does not belong to its actual fixture".into());
                }
            }
            self.parent_route = Some(route.clone());
        }
        let sink = self.sink.as_mut().ok_or("Feedback SDK sink missing")?;
        let mut proof = sink.poll()?;
        if !sink.ready() {
            return Ok(());
        }
        let authorized = route.is_some() && !proof.suspended;
        if !authorized {
            if let Some(binding) = self.binding.take() {
                sink.retire(binding)?;
                self.retired = Some(binding);
            }
            if self.ever_ready && !self.waiting {
                self.waiting = true;
                status(self.appid, "waiting_reconnect", None, self.retired);
            }
            self.ready = false;
        }
        // Lifecycle records must be consumed even when the parent source has
        // disappeared. An unbindable candidate cannot block a following Retired.
        while let Some(event) = self.events.pop_front() {
            match event {
                FeedbackEvent::Bound(binding) => {
                    if binding.epoch != self.epoch
                        || binding.slot != 0
                        || binding.control_session == 0
                        || binding.generation == 0
                        || self.binding.is_some()
                        || self.candidate.is_some()
                    {
                        return Err("Feedback driver binding is ambiguous or stale".into());
                    }
                    self.candidate = Some(binding);
                }
                FeedbackEvent::Rumble(output) => {
                    if self.outputs.len() == 64 {
                        return Err("Feedback pending output queue overflow".into());
                    }
                    self.outputs.push_back(output);
                }
                FeedbackEvent::Retired(binding) => {
                    if self.binding == Some(binding) {
                        sink.retire(binding)?;
                        self.binding = None;
                    }
                    if self.candidate == Some(binding) {
                        self.candidate = None;
                    }
                    self.retired = Some(binding);
                    self.ready = false;
                    let before = self.outputs.len();
                    self.outputs.retain(|output| output.binding != binding);
                    self.discarded_retired += (before - self.outputs.len()) as u64;
                }
            }
        }
        // A source withdrawal can precede the driver Destroy record. Requests
        // from that already retired binding are never replayed to a new handle.
        if let Some(retired) = self.retired {
            let before = self.outputs.len();
            self.outputs.retain(|output| output.binding != retired);
            self.discarded_retired += (before - self.outputs.len()) as u64;
        }
        proof = sink.poll()?;
        if let Some(retired) = proof.last_retired_binding
            && self.retirement_reported != Some(retired)
        {
            self.retirement_reported = Some(retired);
            status(self.appid, "retired", None, Some(retired));
        }
        if route.is_some()
            && !proof.suspended
            && proof.bound.is_none()
            && self.binding.is_none()
            && sink.command_capacity() > 2
        {
            if let Some(binding) = self.candidate.take() {
                if qa && !self.captured {
                    let fixture = fixture.ok_or("Feedback QA fixture missing")?;
                    fixture
                        .rumble()
                        .ok_or("Feedback callback registration disappeared")?
                        .begin_observation(fixture.joystick_id)?;
                    self.captured = true;
                }
                sink.bind(binding)?;
                self.binding = Some(binding);
            }
        }
        while route.is_some() && !proof.suspended && sink.command_capacity() > 2 {
            let Some(output) = self.outputs.front().copied() else {
                break;
            };
            if Some(output.binding) != self.binding {
                break;
            }
            sink.submit(output)?;
            self.outputs.pop_front();
        }
        proof = sink.poll()?;
        if route.is_some()
            && !proof.suspended
            && self.binding.is_some()
            && proof.bound == self.binding
            && proof.steam_input_handle.is_some()
            && !self.ready
        {
            let state = if self.ever_ready {
                "reconnected"
            } else {
                "ready"
            };
            self.ready = true;
            self.ever_ready = true;
            self.waiting = false;
            status(self.appid, state, None, self.binding);
            super::event(
                "steam_input_rumble_feedback_ready",
                true,
                json!({"appid":self.appid,"mode":self.mode.label(),"state":state,"parent_route":self.parent_route,"binding":self.binding,"sink":proof,"fixture":qa,"physical_device_verified":false}),
            );
        }
        Ok(())
    }
    pub(super) fn finish_source(&mut self, fixture: Option<&Fixture>, mut pump: impl FnMut()) {
        if self.sink_finished {
            return;
        }
        if let Some(sink) = &mut self.sink {
            let report = sink.finish(Duration::from_secs(3), &mut pump);
            if let Some(error) = report.error {
                self.fail(error);
            }
        } else if self.is_qa() {
            self.fail("Feedback SDK sink never started".into());
        }
        if self.is_qa() {
            if self.captured {
                let result=fixture.ok_or("Feedback fixture detached before readback".to_string()).and_then(|fixture|{
                    let registration=fixture.rumble().ok_or("Feedback callback registration missing")?;
                    let deadline=std::time::Instant::now()+Duration::from_millis(250);
                    loop{match registration.ordered_readback(){
                        Ok(readback)=>break Ok(json!({"ordered_callback_verified":readback.ordered_left_zero_right_zero,"callback_samples":readback.samples.iter().map(|sample|json!({"sequence":sample.sequence,"elapsed_us":sample.elapsed_us,"low":sample.low,"high":sample.high})).collect::<Vec<_>>(),"physical_device_verified":readback.physical_device_verified})),
                        Err(error)if error=="Rumble callback is still executing; retry closed readback"&&std::time::Instant::now()<deadline=>{pump();std::thread::sleep(Duration::from_millis(5));},
                        Err(error)=>break Err(error),
                    }}
                });
                match result {
                    Ok(value) => self.readback = Some(value),
                    Err(error) => self.fail(error),
                }
            } else {
                self.fail("Feedback callback observation was never grounded".into());
            }
        }
        self.sink_finished = true;
    }
    pub(super) fn finalize(
        &mut self,
        state: &Path,
        engine_stopped: bool,
        game_ok: bool,
        relay: Option<RelayReport>,
        zero_controller_finished: bool,
    ) {
        // The input owner separately verifies the legitimate zero-device path,
        // including engine stop, zero producer activity and launcher cleanup.
        // An absent relay alone never proves that resources were released.
        let inactive_finished =
            self.inactive && self.sink.is_none() && relay.is_none() && zero_controller_finished;
        let resources_closed = engine_stopped
            && (inactive_finished
                || relay.as_ref().is_some_and(|relay| {
                    relay.child_reaped
                        && relay.threads_joined
                        && relay.closed
                        && relay.driver_active_slots == 0
                        && relay.producer_active_slots == 0
                }));
        let launcher = self
            .sink
            .as_mut()
            .map(|sink| sink.reap_launcher(resources_closed, Duration::from_secs(5)));
        let cleanup = self.monitor.as_ref().is_some_and(Monitor::cleanup_seen);
        let sink = self.sink.as_ref().map(|sink| sink.report());
        let final_feedback = relay
            .as_ref()
            .map(|relay| relay.feedback.clone())
            .unwrap_or_else(|| self.feedback_report.clone());
        let all_history = self.requested.iter().cloned().collect::<Vec<_>>();
        let matched = if self.is_qa() {
            match_requests(&all_history)
        } else {
            None
        };
        let session_ok = self.error.is_none()
            && self.sink_finished
            && game_ok
            && resources_closed
            && (inactive_finished || relay.as_ref().is_some_and(|relay| relay.ok))
            && if self.inactive && sink.is_none() {
                true
            } else {
                final_feedback.error.is_none()
                    && sink.as_ref().is_some_and(|sink| {
                        sink.error.is_none()
                            && sink.stopped_message
                            && sink.helper_termination.is_some()
                    })
                    && launcher
                        .as_ref()
                        .is_some_and(|launcher| launcher.reaped && launcher.error.is_none())
            };
        let ordered = self
            .readback
            .as_ref()
            .is_some_and(|readback| readback["ordered_callback_verified"] == true);
        let software = self.is_qa()
            && session_ok
            && self.ever_ready
            && cleanup
            && ordered
            && matched.is_some();
        let proof = json!({"appid":self.appid,"mode":self.mode.label(),"fixture":self.is_qa(),"physical_device_verified":false,"session_ok":session_ok,"software_feedback_verified":software,
            "inactive_no_controller":self.inactive,"inactive_output_finished":inactive_finished,"parent_route_verified":self.parent_route.is_some(),"parent_route":self.parent_route,"requested_feedback":matched,
            "all_feedback":all_history,"feedback_outputs_seen":self.outputs_seen,"history_truncated":self.outputs_seen>self.requested.len()as u64,"discarded_retired_outputs":self.discarded_retired,
            "callback_readback":self.readback,"ordered_callback_verified":ordered,"cleanup_seen":cleanup,
            "sink":sink,"sink_launcher":launcher,"feedback":final_feedback,"relay":relay,"engine_stopped":engine_stopped,"game_completed":game_ok,"error":self.error});
        if let Ok(encoded) = serde_json::to_vec_pretty(&proof) {
            let _ = fs::write(
                state.join("steam-input-rumble-feedback-proof.json"),
                encoded,
            );
        }
        super::event(
            "steam_input_rumble_feedback_proof",
            if self.is_qa() { software } else { session_ok },
            proof,
        );
    }
}
fn record_history(
    mode: Mode,
    history: &mut VecDeque<Value>,
    count: &mut u64,
    output: FeedbackRumble,
) -> Result<(), String> {
    if mode == Mode::FixtureQa && history.len() >= 64 {
        return Err("Feedback QA exceeded its bounded output trace".into());
    }
    if mode == Mode::Product && history.len() == 32 {
        history.pop_front();
    }
    *count = count
        .checked_add(1)
        .ok_or("Feedback output counter overflow")?;
    history.push_back(json!({"binding":output.binding,"driver_sequence":output.sequence,"left":output.left,"right":output.right}));
    Ok(())
}
fn match_requests(rows: &[Value]) -> Option<Vec<Value>> {
    let mut phase = 0;
    let mut matched = Vec::new();
    for row in rows {
        let pair = (row["left"].as_u64()?, row["right"].as_u64()?);
        match (phase, pair) {
            (0 | 2 | 4, (0, 0)) | (1, (16384, 0)) | (3, (0, 24576)) => {}
            (0, (16384, 0)) | (1, (0, 0)) | (2, (0, 24576)) | (3, (0, 0)) => {
                phase += 1;
                matched.push(row.clone());
            }
            _ => return None,
        }
    }
    (phase == 4).then_some(matched)
}

fn status(appid: u32, state: &str, error: Option<&str>, binding: Option<FeedbackBinding>) {
    use std::io::Write;
    let _ = writeln!(
        std::io::stdout(),
        "SKINOS_LIVE {}",
        json!({"event":"steam_input_rumble_status","payload":{"appid":appid,"state":state,"message":error,"binding":binding,"game_continues":true,"physical_device_verified":false}})
    );
    let _ = std::io::stdout().flush();
}

pub(super) fn experimental_feeder(
    plan: &skinos_compat::LaunchPlan,
    context: &skinos_compat::direct_wine::Context,
) -> Result<Command, String> {
    let root = plan
        .sdk_probe
        .parent()
        .ok_or("Feedback payload directory missing")?
        .join("wine-input-rumble-experiment");
    let manifest: Value = serde_json::from_slice(&bounded(&root.join("manifest.json"), 65536)?)
        .map_err(|_| "Invalid feedback manifest")?;
    if manifest["schema_version"] != 1 || manifest["experiment"] != "owned_wine_haptics_feedback" {
        return Err("Wrong experimental feedback payload".into());
    }
    let arch = context.driver_arch.label();
    for name in ["feeder.exe", "skinosinput.sys", "skinosinput.inf"] {
        let relative = format!("{arch}/{name}");
        let expected = manifest["files"][&relative]
            .as_str()
            .ok_or("Experimental feedback file hash missing")?;
        if format!(
            "{:x}",
            Sha256::digest(bounded(&root.join(relative), 16 * 1024 * 1024)?)
        ) != expected
        {
            return Err("Experimental feedback payload checksum mismatch".into());
        }
    }
    context.ensure_feedback_feeder_command(
        &root.join(arch).join("feeder.exe"),
        &root.join(arch).join("skinosinput.inf"),
    )
}
fn bounded(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "Feedback payload unavailable")?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err("Feedback payload is not a bounded regular file".into());
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|_| "Cannot open feedback payload")?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read feedback payload")?;
    if bytes.len() as u64 > limit {
        return Err("Feedback payload exceeded bound".into());
    }
    Ok(bytes)
}

pub(super) struct Tracker {
    plan: skinos_compat::LaunchPlan,
    appid: u32,
    name: String,
    launched_after: u128,
    process: Option<super::TrackedGameProcess>,
}
impl Tracker {
    pub(super) fn new(
        plan: skinos_compat::LaunchPlan,
        appid: u32,
        name: String,
        launched_after: u128,
    ) -> Self {
        Self {
            plan,
            appid,
            name,
            launched_after,
            process: None,
        }
    }
}
impl rumble_sink::HelperTracker for Tracker {
    fn observe(&mut self) -> Result<rumble_sink::HelperObservation, String> {
        if self.process.is_none() {
            self.process =
                super::find_compat_game(&self.plan, &self.name, self.appid, self.launched_after)?;
        }
        let Some(process) = &self.process else {
            return Ok(rumble_sink::HelperObservation::Searching);
        };
        let identity = rumble_sink::ProcessIdentity {
            pid: process.pid as u32,
            start_seconds: process.start.0,
            start_microseconds: process.start.1,
        };
        observe_scoped_process(
            identity,
            || tracked_termination(process, identity),
            || process.is_current(),
        )
    }
    fn terminate_owned(&mut self) -> Result<(), String> {
        let Some(process) = &self.process else {
            return Ok(());
        };
        if process.termination_observation().is_some() {
            return Ok(());
        }
        if !process.is_current()
            || !super::wrapper_allowed(&process.executable, &self.plan)
            || !super::process_matches_compatibility(process.pid, &self.plan, self.appid)
        {
            return Err("Refusing to signal an unverified feedback SDK helper".into());
        }
        if unsafe { super::kill(process.pid, 9) } != 0
            && std::io::Error::last_os_error().raw_os_error() != Some(3)
        {
            return Err("Could not stop owned feedback SDK helper".into());
        }
        Ok(())
    }
}

fn tracked_termination(
    process: &super::TrackedGameProcess,
    identity: rumble_sink::ProcessIdentity,
) -> Option<rumble_sink::HelperObservation> {
    let exit = process.monitored_exit().map(|exit| rumble_sink::Exit {
        code: exit.code,
        signal: exit.signal,
    });
    let reason = if exit.is_some() {
        Some(rumble_sink::Termination::KernelZombie)
    } else {
        match process.termination_observation() {
            Some("kernel_esrch") => Some(rumble_sink::Termination::KernelEsrch),
            Some("pid_reused") => Some(rumble_sink::Termination::PidReused),
            Some("kernel_zombie") => Some(rumble_sink::Termination::KernelZombie),
            _ => None,
        }
    }?;
    Some(rumble_sink::HelperObservation::Terminated {
        identity,
        reason,
        exit,
    })
}
fn observe_scoped_process(
    identity: rumble_sink::ProcessIdentity,
    mut termination: impl FnMut() -> Option<rumble_sink::HelperObservation>,
    current: impl FnOnce() -> bool,
) -> Result<rumble_sink::HelperObservation, String> {
    if let Some(observed) = termination() {
        return Ok(observed);
    }
    if current() {
        return Ok(rumble_sink::HelperObservation::Running(identity));
    }
    // Exit can race the first kernel observation and vnode/path capture. Only
    // another genuine kernel exit proof can resolve that race; never infer exit
    // from an unavailable wrapper or a failed inspection by itself.
    termination().ok_or_else(|| "Cannot revalidate owned feedback SDK helper".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn helper_exit_race_rechecks_kernel_proof_without_weakening_identity() {
        let identity = rumble_sink::ProcessIdentity {
            pid: 42,
            start_seconds: 123,
            start_microseconds: 456,
        };
        let ended = rumble_sink::HelperObservation::Terminated {
            identity,
            reason: rumble_sink::Termination::KernelEsrch,
            exit: None,
        };
        let mut calls = 0;
        assert_eq!(
            observe_scoped_process(
                identity,
                || {
                    calls += 1;
                    (calls == 2).then_some(ended)
                },
                || false
            )
            .unwrap(),
            ended
        );
        assert_eq!(calls, 2);
        assert!(observe_scoped_process(identity, || None, || false).is_err());
        assert_eq!(
            observe_scoped_process(identity, || None, || true).unwrap(),
            rumble_sink::HelperObservation::Running(identity)
        );
    }
    fn row(sequence: u64, left: u16, right: u16) -> Value {
        json!({"driver_sequence":sequence,"left":left,"right":right})
    }
    #[test]
    fn ordered_driver_pairs_preserve_leading_and_duplicate_actual_values() {
        let rows = vec![
            row(1, 0, 0),
            row(2, 0, 0),
            row(3, 16384, 0),
            row(4, 16384, 0),
            row(5, 0, 0),
            row(6, 0, 0),
            row(7, 0, 24576),
            row(8, 0, 24576),
            row(9, 0, 0),
            row(10, 0, 0),
        ];
        let matched = match_requests(&rows).unwrap();
        assert_eq!(
            matched
                .iter()
                .map(|value| value["driver_sequence"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [3, 5, 7, 9]
        );
        assert_eq!(rows.len(), 10);
        for bad in [
            vec![row(1, 16384, 0), row(2, 0, 24576), row(3, 0, 0)],
            vec![
                row(1, 16383, 0),
                row(2, 0, 0),
                row(3, 0, 24576),
                row(4, 0, 0),
            ],
            vec![row(1, 16384, 0), row(2, 0, 0), row(3, 0, 24576)],
        ] {
            assert!(match_requests(&bad).is_none());
        }
    }
    #[test]
    fn product_history_is_bounded_without_limiting_real_requests_and_zero_scope_is_idle() {
        let binding = FeedbackBinding {
            epoch: 7,
            control_session: 12,
            slot: 0,
            generation: 1,
        };
        let mut history = VecDeque::new();
        let mut count = 0;
        for sequence in 1..=81 {
            record_history(
                Mode::Product,
                &mut history,
                &mut count,
                FeedbackRumble {
                    binding,
                    sequence,
                    left: sequence as u16,
                    right: 0,
                },
            )
            .unwrap();
        }
        assert_eq!(count, 81);
        assert_eq!(history.len(), 32);
        assert_eq!(history.front().unwrap()["driver_sequence"], 50);
        assert_eq!(history.back().unwrap()["driver_sequence"], 81);
        let mut idle = Session::new(7, Mode::Product);
        idle.inactive_no_source();
        idle.finish_source(None, || {});
        assert!(idle.error.is_none());
        assert!(!idle.needs_start() && !idle.poll_needed());
        assert!(idle.readback.is_none());
        let mut unavailable = Session::new(7, Mode::Product);
        unavailable.fail("game SDK unavailable".into());
        unavailable.finish_source(None, || {});
        assert_eq!(unavailable.error.as_deref(), Some("game SDK unavailable"));
    }
    #[test]
    fn feedback_scope_refuses_unrelated_epoch_failure_and_unbounded_queue() {
        let mut scope = Session::new(7, Mode::FixtureQa);
        let binding = FeedbackBinding {
            epoch: 7,
            control_session: 12,
            slot: 0,
            generation: 2,
        };
        let report = FeedbackReport {
            selected: true,
            enabled: true,
            epoch: Some(7),
            ..Default::default()
        };
        scope
            .accept(FeedbackBatch {
                events: vec![FeedbackEvent::Bound(binding)],
                report: report.clone(),
            })
            .unwrap();
        assert!(scope.has_driver_binding());
        let mut wrong = report.clone();
        wrong.epoch = Some(8);
        assert!(
            scope
                .accept(FeedbackBatch {
                    events: vec![],
                    report: wrong
                })
                .is_err()
        );
        let mut failed = report.clone();
        failed.stop_required = true;
        assert!(
            scope
                .accept(FeedbackBatch {
                    events: vec![],
                    report: failed
                })
                .is_err()
        );
        assert!(
            scope
                .accept(FeedbackBatch {
                    events: vec![FeedbackEvent::Bound(binding); 64],
                    report
                })
                .is_err()
        );
    }
}
