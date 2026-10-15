//! Worker-thread ownership of the opt-in Wine output transport. This module
//! never installs a driver or calls into the Valve engine from another thread.
use crate::wine_input_output::{Channel, OutputLease, OutputSelection, OutputStats};
use crate::wine_input_relay::{
    FeedbackBatch, FeedbackReport, FeedbackSelection, Relay, RelayReport,
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

pub(super) struct Session {
    // Explicit finish handles this order too. On an early error the relay must
    // abort/join before the image reference and channel can be dropped.
    relay: Option<Relay>,
    shim: Option<RetainedShim>,
    lease: Option<OutputLease>,
    channel: Option<Channel>,
    mode: OutputSelection,
    feedback_selection: FeedbackSelection,
    #[allow(dead_code)] // Explicit QA integration is wired by the caller later.
    pending_feedback: FeedbackReport,
    state: PathBuf,
    attach_attempted: bool,
    finished: bool,
    failure: Option<String>,
    pending_diagnostic: Option<Value>,
    zero_devices: bool,
    game_running: bool,
    completion: Option<Result<(), String>>,
}
impl Session {
    /// The producer lease remains owned by this session. Returned values contain
    /// no library pointers and may only attest frames actually acknowledged.
    pub(super) fn progress(&mut self) -> Result<Option<OutputProgress>, String> {
        self.check()?;
        match (&mut self.lease, &self.relay) {
            (Some(lease), Some(relay)) => Ok(Some(OutputProgress {
                producer: lease.check()?,
                relay: relay.progress()?,
            })),
            _ => Ok(None),
        }
    }
    /// # Safety
    /// Must run after the trace re-exec but before Valve or any worker threads
    /// are started: Channel creates the inherited-environment FD selection.
    pub(super) unsafe fn create(state: &Path, mode: OutputSelection) -> Result<Self, String> {
        Ok(Self {
            relay: None,
            shim: None,
            lease: None,
            channel: unsafe { Channel::create(mode)? },
            mode,
            feedback_selection: FeedbackSelection::Disabled,
            pending_feedback: FeedbackReport::default(),
            state: state.into(),
            attach_attempted: false,
            finished: false,
            failure: None,
            pending_diagnostic: None,
            zero_devices: false,
            game_running: false,
            completion: None,
        })
    }
    pub(super) fn enabled(&self) -> bool {
        self.mode.enabled()
    }
    pub(super) fn is_diagnostic(&self) -> bool {
        self.mode.is_diagnostic()
    }
    pub(super) fn is_game(&self) -> bool {
        self.mode.is_game()
    }
    pub(super) fn uses_production_preparation(&self) -> bool {
        self.mode.uses_production_preparation()
    }
    pub(super) fn is_product_diagnostic(&self) -> bool {
        self.mode.is_product_diagnostic()
    }
    /// Select the separately bundled feedback experiment before any helper is
    /// attached. This is never inferred from game settings or inherited flags.
    /// The caller verifies its owned QA fixture for Game or Diagnostic mode.
    /// No engine function is called here; the worker consumes feedback itself.
    #[allow(dead_code)] // Explicit QA integration; default product path stays off.
    pub(super) fn enable_experimental_feedback(&mut self, epoch: u64) -> Result<(), String> {
        if epoch == 0
            || !self.enabled()
            || !(self.is_game() || self.is_diagnostic())
            || self.game_running
            || self.attach_attempted
            || self.relay.is_some()
            || self.finished
            || self.pending_feedback.selected
        {
            return Err("Experimental Wine feedback requires one initial enabled Game/Diagnostic opt-in and a nonzero epoch".into());
        }
        self.feedback_selection = FeedbackSelection::Experimental { epoch };
        self.pending_feedback = FeedbackReport {
            selected: true,
            enabled: false,
            epoch: Some(epoch),
            ..FeedbackReport::default()
        };
        Ok(())
    }
    /// Draining this bounded queue is independent of input health and never
    /// dispatches Valve calls. Before attachment it is an empty pending batch.
    #[allow(dead_code)]
    pub(super) fn take_feedback(&self, max_events: usize) -> FeedbackBatch {
        if let Some(relay) = &self.relay {
            // A pre-attach disable keeps its diagnostic cause even though the
            // input relay was subsequently started with feedback Disabled.
            if self.pending_feedback.error.is_none() {
                return relay.take_feedback(max_events);
            }
        }
        FeedbackBatch {
            events: Vec::new(),
            report: self.pending_feedback.clone(),
        }
    }
    /// Preferred SDK-owner-thread drain: None means contention, not disabled
    /// rumble. It never waits for either the relay lock or child process IO.
    #[allow(dead_code)]
    pub(super) fn try_take_feedback(&self, max_events: usize) -> Option<FeedbackBatch> {
        if let Some(relay) = &self.relay {
            if self.pending_feedback.error.is_none() {
                return relay.try_take_feedback(max_events);
            }
        }
        Some(FeedbackBatch {
            events: Vec::new(),
            report: self.pending_feedback.clone(),
        })
    }
    #[allow(dead_code)]
    pub(super) fn feedback_report(&self) -> FeedbackReport {
        if let Some(relay) = &self.relay {
            if self.pending_feedback.error.is_none() {
                return relay.feedback_report();
            }
        }
        self.pending_feedback.clone()
    }
    /// Actual final relay evidence, independent of Game's degraded-input policy.
    pub(super) fn output_final_report(&self) -> Option<RelayReport> {
        self.relay.as_ref().and_then(Relay::finished_report)
    }
    /// The legitimate empty-game path never starts a feeder. `finish` must
    /// still verify zero producer activity, engine stop and owned shim cleanup;
    /// mere absence of a relay is not completion evidence.
    pub(super) fn zero_controller_finished_without_relay(&self) -> bool {
        self.is_game()
            && self.finished
            && self.zero_devices
            && !self.attach_attempted
            && self.relay.is_none()
            && self.failure.is_none()
            && self
                .completion
                .as_ref()
                .is_some_and(|result| result.is_ok())
    }
    /// Feedback failure must not abort the independent game-input transport.
    #[allow(dead_code)]
    pub(super) fn disable_feedback(&mut self, reason: &str) {
        if let Some(relay) = &self.relay {
            relay.disable_feedback(reason);
        } else if self.pending_feedback.selected {
            self.feedback_selection = FeedbackSelection::Disabled;
            self.pending_feedback.enabled = false;
            // Uniform worker contract: every selected-feedback disable asks
            // the worker to retire/stop its route, even before one is bound.
            self.pending_feedback.stop_required = true;
            if self.pending_feedback.error.is_none() {
                self.pending_feedback.error = Some(reason.chars().take(300).collect());
            }
        }
    }
    pub(super) fn note_no_gamepad(&mut self) -> Result<(), String> {
        if !self.uses_production_preparation() || self.attach_attempted || self.game_running {
            return Err("No-gamepad result is outside initial game preparation".into());
        }
        self.zero_devices = true;
        Ok(())
    }
    pub(super) fn mark_game_running(&mut self) {
        self.game_running = true;
    }
    pub(super) fn retain_game_shim(
        &mut self,
        child: Child,
        game: Value,
        expected_stop: bool,
    ) -> Result<(), String> {
        let mut shim = RetainedShim::new(child, game);
        shim.expected_stop = expected_stop;
        shim.game = true;
        if !self.is_game() || self.shim.is_some() || self.finished {
            return Err("Cannot retain game launcher outside its owned input session".into());
        }
        self.shim = Some(shim);
        Ok(())
    }
    pub(super) fn record_input_failure(&mut self, error: String) -> Result<(), String> {
        self.fail(error)
    }
    pub(super) fn retain_shim(&mut self, child: Child, probe: Value) -> Result<(), String> {
        let shim = RetainedShim::new(child, probe);
        let valid_output =
            self.relay.is_some() || (self.is_product_diagnostic() && self.zero_devices);
        if !self.is_diagnostic() || !valid_output || self.shim.is_some() || self.finished {
            return Err("Cannot retain diagnostic shim outside its ready Wine session".into());
        }
        self.shim = Some(shim);
        Ok(())
    }
    pub(super) fn defer_diagnostic(&mut self, payload: Value) -> Result<(), String> {
        if !self.is_diagnostic()
            || self.shim.is_none()
            || self.pending_diagnostic.is_some()
            || self.finished
        {
            return Err(
                "Cannot publish diagnostic before its owned shim lifecycle is retained".into(),
            );
        }
        self.pending_diagnostic = Some(payload);
        Ok(())
    }
    pub(super) fn validate_mode(&self, request: &serde_json::Value) -> Result<(), String> {
        self.mode.validate_request(request)
    }

    /// The command is a direct owned Wine loader executing only --feed. It
    /// opens an already-installed driver; no registration/install is permitted.
    pub(super) fn attach(&mut self, command: Command, pump: impl FnMut()) -> Result<(), String> {
        if !self.enabled() || self.attach_attempted || self.finished {
            return Err("Wine input relay attachment is outside its one owned diagnostic".into());
        }
        self.check()?;
        self.attach_attempted = true;
        let receiver = self
            .channel
            .as_mut()
            .and_then(Channel::take_receiver)
            .ok_or("Wine input channel receiver has already been transferred")?;
        match Relay::start_with_feedback(
            command,
            receiver,
            &self.state,
            Duration::from_secs(if self.uses_production_preparation() {
                30
            } else {
                10
            }),
            self.feedback_selection,
            pump,
        ) {
            Ok(relay) => self.relay = Some(relay),
            Err(error) => return self.fail(error),
        }
        self.check()?;
        super::event(
            "wine_input_relay_ready",
            true,
            json!({
                "diagnostic_only":self.is_diagnostic(),"game_mode":self.is_game(),"driver_ensure_requested":self.uses_production_preparation(),
                "game_input_verified":false
            }),
        );
        Ok(())
    }

    /// Called separately after a real engine pump. It never manufactures a
    /// successful frame or hides errors inside the non-fallible pump callback.
    pub(super) fn check(&mut self) -> Result<(), String> {
        if !self.enabled() {
            return Ok(());
        }
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        let result = (|| {
            if self.lease.is_none() {
                self.lease = OutputLease::try_loaded(&self.state, self.mode)?;
            }
            if let Some(lease) = &mut self.lease {
                lease.check()?;
            }
            if let Some(relay) = &self.relay {
                relay.check()?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => Ok(()),
            Err(error) => self.fail(error),
        }
    }
    fn fail(&mut self, error: String) -> Result<(), String> {
        if self.failure.is_none() {
            self.failure = Some(error);
        }
        let error = self.failure.as_ref().expect("recorded failure").clone();
        if let Some(relay) = &mut self.relay {
            let report = relay.abort(&error);
            super::event("wine_input_relay_failed", false, json!(report));
        } else if let Some(channel) = &mut self.channel {
            // In a zero-device session no relay owns the endpoint yet. Close
            // it on invalidation so late unbound output cannot keep queuing.
            drop(channel.take_receiver());
        }
        Err(error)
    }

    /// Call only after fixture detach, user/pipe release, engine shutdown and
    /// its dlclose attempt. The own output lease remains alive during all of
    /// those operations and while the final queued frames are drained.
    pub(super) fn finish(&mut self, engine_stopped: bool) -> Result<(), String> {
        if !self.enabled() {
            return Ok(());
        }
        if self.finished {
            return self
                .completion
                .clone()
                .unwrap_or_else(|| Err("Missing input session completion result".into()));
        }
        if !engine_stopped {
            let _ =
                self.fail("Valve engine did not stop; Wine input relay cannot drain safely".into());
        } else {
            let _ = self.check();
        }
        let zero_game =
            self.uses_production_preparation() && self.zero_devices && self.relay.is_none();
        if self.relay.is_none() && !zero_game && self.failure.is_none() {
            self.failure = Some("Wine input diagnostic never attached a ready feeder".into());
        }
        let report = self.relay.as_mut().map(|relay| {
            if let Some(error) = &self.failure {
                relay.abort(error)
            } else {
                relay.finish(Duration::from_secs(5))
            }
        });
        let stats = match &mut self.lease {
            Some(lease) => lease.check().map(Some),
            None if zero_game => Ok(None),
            None => Err("Selected Wine output library was never observed loaded".into()),
        };
        let verified = match (&stats, &report) {
            (Ok(Some(stats)), Some(report)) => validate_finished(stats, report),
            (Ok(stats), None) if zero_game => validate_zero_device_finish(stats.as_ref()),
            (Err(error), _) => Err(error.clone()),
            _ => Err("Wine input relay produced no final report or producer stats".into()),
        };
        if self.failure.is_none() {
            self.failure = verified.err();
        }
        // steam.exe waits for Wine's shutdown event, not only its probe child.
        // Relay must close the feeder before waiting for this owned launcher.
        let relay_resources_closed = zero_game
            || report
                .as_ref()
                .is_some_and(|report| report.child_reaped && report.threads_joined);
        let allow_game_launcher_cleanup =
            self.is_game() && self.game_running && engine_stopped && relay_resources_closed;
        let shim = self.shim.as_mut().map(|shim| {
            if allow_game_launcher_cleanup {
                shim.finish_with_game_cleanup(Duration::from_secs(5), true)
            } else {
                shim.finish(Duration::from_secs(5))
            }
        });
        let shim_failure = match &shim {
            Some(report) if report.ok => None,
            Some(report) => Some(
                report
                    .error
                    .clone()
                    .unwrap_or_else(|| "Diagnostic shim failed after feeder shutdown".into()),
            ),
            None => Some("Diagnostic shim was not retained for verified shutdown".into()),
        };
        if self.failure.is_none() {
            self.failure = shim_failure.clone();
        }
        if self.failure.is_none() {
            if let Some(mut payload) = self.pending_diagnostic.take() {
                payload["wine_output_lifecycle"] =
                    json!({"stats":stats.as_ref().ok(),"relay":report,"shim":shim});
                self.failure = super::publish_input_diagnostic(&self.state, payload).err();
            }
        }
        self.finished = true;
        super::event(
            "wine_input_output_finished",
            self.failure.is_none(),
            json!({
                "engine_stopped":engine_stopped,"stats":stats.as_ref().ok(),"relay":report,
                "shim":shim,"error":self.failure,"game_input_verified":false,"zero_devices":zero_game,
                "mode":self.mode,"game_continued_after_input_failure":self.is_game()&&self.game_running&&self.failure.is_some()
            }),
        );
        // Keep the image and channel until Session drops in the caller. No
        // output callback or stats function is used after that point.
        let result = completion_result(
            self.mode,
            self.game_running,
            engine_stopped,
            self.failure.clone(),
            shim_failure,
        );
        self.completion = Some(result.clone());
        result
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct OutputProgress {
    pub producer: OutputStats,
    pub relay: crate::wine_input_relay::RelayProgress,
}
impl OutputProgress {
    pub(super) fn drained(&self) -> bool {
        self.producer.active_slot_mask == 0
            && self.relay.intended_slots == 0
            && self.relay.driver_slots == 0
            && self.relay.pending == 0
            && self.producer.queued_frames == self.relay.received
            && self.relay.received == self.relay.forwarded
            && self.relay.forwarded == self.relay.acknowledged
    }
    pub(super) fn slot_zero_ready(&self) -> bool {
        self.producer.active_slot_mask == 1
            && self.relay.intended_slots == 1
            && self.relay.driver_slots == 1
    }
}

fn completion_result(
    mode: OutputSelection,
    game_running: bool,
    engine_stopped: bool,
    input_failure: Option<String>,
    shim_failure: Option<String>,
) -> Result<(), String> {
    if !engine_stopped {
        Err("Valve engine did not stop after game input teardown".into())
    } else if mode.is_game() && game_running {
        shim_failure.map_or(Ok(()), Err)
    } else {
        input_failure.map_or(Ok(()), Err)
    }
}

#[derive(Clone, Debug, Serialize)]
struct ShimReport {
    ok: bool,
    pid: u32,
    actual_probe: Value,
    actual_game: Option<Value>,
    exit_code: Option<i32>,
    termination_signal: Option<i32>,
    timed_out: bool,
    forced_launcher_cleanup: bool,
    game_exit_independently_observed: bool,
    launcher_cleanup_only: bool,
    child_reaped: bool,
    error: Option<String>,
    expected_stop: bool,
}
struct RetainedShim {
    child: Child,
    probe: Value,
    reaped: bool,
    report: Option<ShimReport>,
    expected_stop: bool,
    game: bool,
}
impl RetainedShim {
    fn new(child: Child, probe: Value) -> Self {
        Self {
            child,
            probe,
            reaped: false,
            report: None,
            expected_stop: false,
            game: false,
        }
    }
    fn finish(&mut self, timeout: Duration) -> ShimReport {
        self.finish_with_game_cleanup(timeout, false)
    }
    fn finish_with_game_cleanup(
        &mut self,
        timeout: Duration,
        allow_game_cleanup: bool,
    ) -> ShimReport {
        if let Some(report) = &self.report {
            return report.clone();
        }
        let deadline = Instant::now() + timeout;
        let mut error = None;
        let mut timed_out = false;
        let mut forced_launcher_cleanup = false;
        let status = loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    self.reaped = true;
                    break Some(status);
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Ok(None) => {
                    timed_out = true;
                    error = Some("Owned launcher did not exit after feeder shutdown".into());
                    break None;
                }
                Err(failure) => {
                    error = Some(format!("Cannot wait for diagnostic shim: {failure}"));
                    break None;
                }
            }
        };
        let status = status.or_else(|| {
            forced_launcher_cleanup = self.child.kill().is_ok();
            let status = self.child.wait().ok();
            self.reaped = status.is_some();
            status
        });
        let game_exit_independently_observed = self.game && verified_game_termination(&self.probe);
        // A Windows service can intentionally survive its game's process and
        // hold the original shim's whole-prefix shutdown event. Only the owned
        // waiting launcher may be reaped here. Its SIGKILL is recorded as such,
        // never rewritten to exit0 or attributed to the independently dead game.
        let launcher_cleanup_only = allow_game_cleanup
            && game_exit_independently_observed
            && timed_out
            && forced_launcher_cleanup
            && self.reaped
            && status.is_some_and(|status| status.signal() == Some(9));
        if launcher_cleanup_only {
            error = None;
        }
        if error.is_none()
            && !launcher_cleanup_only
            && !status.is_some_and(|status| {
                status.success()
                    || (self.expected_stop
                        && super::ProcessExit::from_status(status).expected_after_stop())
            })
        {
            error = Some("Diagnostic shim returned a nonzero exit or signal".into());
        }
        let report = ShimReport {
            ok: error.is_none() && self.reaped,
            pid: self.child.id(),
            actual_probe: if self.game {
                Value::Null
            } else {
                self.probe.clone()
            },
            actual_game: self.game.then(|| self.probe.clone()),
            exit_code: status.and_then(|status| status.code()),
            termination_signal: status.and_then(|status| status.signal()),
            timed_out,
            forced_launcher_cleanup,
            game_exit_independently_observed,
            launcher_cleanup_only,
            child_reaped: self.reaped,
            error,
            expected_stop: self.expected_stop,
        };
        self.report = Some(report.clone());
        report
    }
}

fn verified_game_termination(game: &Value) -> bool {
    game["prefix_appid_verified"] == true
        && game["pid"]
            .as_u64()
            .is_some_and(|pid| pid > 0 && pid <= i32::MAX as u64)
        && game["start_seconds"]
            .as_u64()
            .is_some_and(|seconds| seconds > 0)
        && game["start_microseconds"]
            .as_u64()
            .is_some_and(|micros| micros < 1_000_000)
        && (matches!(
            game["termination_observation"].as_str(),
            Some("kernel_zombie" | "kernel_esrch" | "pid_reused")
        ) || game["exit"]["code"].as_i64().is_some()
            || game["exit"]["signal"].as_i64().is_some())
}
impl Drop for RetainedShim {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn validate_finished(stats: &OutputStats, report: &RelayReport) -> Result<(), String> {
    stats.validate()?;
    if stats.had_loss() || !report.ok {
        return Err(report
            .error
            .clone()
            .unwrap_or_else(|| "Wine input output did not close without loss".into()));
    }
    if stats.queued_frames != report.frames_received
        || report.frames_received != report.frames_forwarded
        || report.frames_forwarded != report.frames_acknowledged
    {
        return Err("Wine input producer/relay final frame counts differ".into());
    }
    Ok(())
}

fn validate_zero_device_finish(stats: Option<&OutputStats>) -> Result<(), String> {
    if let Some(stats) = stats {
        stats.validate()?;
        if stats.had_loss() || stats.queued_frames != 0 || stats.active_slot_mask != 0 {
            return Err(
                "A gamepad output appeared outside the verified zero-device game session".into(),
            );
        }
    }
    Ok(())
}

/// Feeder comes only from the bundled NotProton payload matching this plan.
/// The manifest authenticates package consistency, not an external publisher.
pub(super) fn bundled_feeder(
    plan: &skinos_compat::LaunchPlan,
    arch: &str,
) -> Result<PathBuf, String> {
    let root = plan
        .sdk_probe
        .parent()
        .ok_or("SDK payload directory missing")?
        .join("wine-input");
    verified_feeder(&root, arch)
}

pub(super) fn bundled_driver(
    plan: &skinos_compat::LaunchPlan,
    arch: &str,
) -> Result<(PathBuf, PathBuf), String> {
    let root = plan
        .sdk_probe
        .parent()
        .ok_or("SDK payload directory missing")?
        .join("wine-input");
    verified_driver(&root, arch)
}

/// The feedback driver is an explicit sibling payload; never fall back to the
/// ordinary input driver or a path supplied through inherited environment.
#[allow(dead_code)] // Explicit QA resources only; no fallback to normal payload.
pub(super) fn bundled_experimental_driver(
    plan: &skinos_compat::LaunchPlan,
    arch: &str,
) -> Result<(PathBuf, PathBuf), String> {
    let root = plan
        .sdk_probe
        .parent()
        .ok_or("SDK payload directory missing")?
        .join("wine-input-rumble-experiment");
    verified_experimental_driver(&root, arch)
}

#[allow(dead_code)]
fn validate_experimental_manifest(manifest: &Value) -> Result<(), String> {
    if manifest["schema_version"] != 1
        || manifest["experiment"] != "owned_wine_haptics_feedback"
        || manifest["feedback_bytes"] != 48
        || manifest["input_hid_bytes"] != 15
        || manifest["output_hid_bytes"] != 5
        || manifest["feature_hid_bytes"] != 17
        || manifest["input_report_id"] != 0
        || manifest["output_report_id"] != 1
    {
        return Err(
            "Bundled Wine feedback manifest does not declare the owned 48/15/5/17 experiment"
                .into(),
        );
    }
    Ok(())
}

#[allow(dead_code)]
fn verified_experimental_driver(root: &Path, arch: &str) -> Result<(PathBuf, PathBuf), String> {
    if !matches!(arch, "arm64" | "x64") {
        return Err("Unsupported Wine feedback driver architecture".into());
    }
    // Read one manifest snapshot for both the ABI declaration and all file
    // hashes, so separately reread manifests cannot mix payload generations.
    let manifest: Value =
        serde_json::from_slice(&bounded_file(&root.join("manifest.json"), 64 * 1024)?)
            .map_err(|_| "Invalid bundled Wine feedback manifest")?;
    validate_experimental_manifest(&manifest)?;
    for name in ["feeder.exe", "skinosinput.sys", "skinosinput.inf"] {
        let relative = format!("{arch}/{name}");
        let expected = manifest["files"][&relative]
            .as_str()
            .ok_or("Wine feedback component is absent from its bundled manifest")?;
        let actual = format!(
            "{:x}",
            Sha256::digest(bounded_file(&root.join(&relative), 16 * 1024 * 1024)?)
        );
        if actual != expected {
            return Err("Wine feedback component differs from its bundled manifest".into());
        }
    }
    Ok((
        root.join(arch).join("feeder.exe"),
        root.join(arch).join("skinosinput.inf"),
    ))
}

fn verified_driver(root: &Path, arch: &str) -> Result<(PathBuf, PathBuf), String> {
    let feeder = verified_feeder(root, arch)?;
    let manifest: Value =
        serde_json::from_slice(&bounded_file(&root.join("manifest.json"), 64 * 1024)?)
            .map_err(|_| "Invalid bundled Wine input manifest")?;
    for name in ["skinosinput.sys", "skinosinput.inf"] {
        let relative = format!("{arch}/{name}");
        let expected = manifest["files"][&relative]
            .as_str()
            .ok_or("Input driver file is absent from bundled manifest")?;
        if format!(
            "{:x}",
            Sha256::digest(bounded_file(&root.join(&relative), 16 * 1024 * 1024)?)
        ) != expected
        {
            return Err("Input driver file differs from its bundled manifest".into());
        }
    }
    Ok((feeder, root.join(arch).join("skinosinput.inf")))
}

fn verified_feeder(root: &Path, arch: &str) -> Result<PathBuf, String> {
    if !matches!(arch, "arm64" | "x64") {
        return Err("Unsupported Wine input driver architecture".into());
    }
    let relative = format!("{arch}/feeder.exe");
    let file = root.join(&relative);
    let manifest: serde_json::Value =
        serde_json::from_slice(&bounded_file(&root.join("manifest.json"), 64 * 1024)?)
            .map_err(|_| "Invalid bundled Wine input manifest")?;
    if manifest["schema_version"] != 1 {
        return Err("Unsupported bundled Wine input manifest".into());
    }
    let expected = manifest["files"][&relative]
        .as_str()
        .ok_or("Wine input feeder is absent from manifest")?;
    let actual = format!(
        "{:x}",
        Sha256::digest(bounded_file(&file, 16 * 1024 * 1024)?)
    );
    if actual != expected {
        return Err("Wine input feeder differs from its bundled manifest".into());
    }
    Ok(file)
}

fn bounded_file(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    use std::{fs::OpenOptions, io::Read, os::unix::fs::OpenOptionsExt};
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(0x100 /* Darwin O_NOFOLLOW */)
        .open(path)
        .map_err(|e| format!("Cannot open bundled Wine input component: {e}"))?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > limit {
        return Err("Bundled Wine input component is not a bounded regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.is_empty() || bytes.len() as u64 > limit {
        return Err("Bundled Wine input component changed size while reading".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::symlink};
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "skinos-input-session-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(path.join("arm64")).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    // No Channel creation, inherited environment edits, engine loading or
    // helper processes are needed to test the pre-attach feedback policy.
    fn unattached_session(mode: OutputSelection) -> Session {
        Session {
            relay: None,
            shim: None,
            lease: None,
            channel: None,
            mode,
            feedback_selection: FeedbackSelection::Disabled,
            pending_feedback: FeedbackReport::default(),
            state: PathBuf::new(),
            attach_attempted: false,
            finished: false,
            failure: None,
            pending_diagnostic: None,
            zero_devices: false,
            game_running: false,
            completion: None,
        }
    }

    #[test]
    fn feedback_stays_off_until_one_nonzero_initial_explicit_opt_in() {
        {
            let mut session = unattached_session(OutputSelection::Disabled);
            assert!(!session.feedback_report().selected);
            assert!(session.enable_experimental_feedback(7).is_err());
            assert!(matches!(
                session.feedback_selection,
                FeedbackSelection::Disabled
            ));
            assert!(session.take_feedback(64).events.is_empty());
            assert!(session.failure.is_none());
        }
        for mode in [
            OutputSelection::Game,
            OutputSelection::Diagnostic,
            OutputSelection::ProductDiagnostic,
        ] {
            let mut session = unattached_session(mode);
            assert!(session.enable_experimental_feedback(0).is_err());
            assert!(!session.feedback_report().selected);
            session.enable_experimental_feedback(7).unwrap();
            assert!(matches!(
                session.feedback_selection,
                FeedbackSelection::Experimental { epoch: 7 }
            ));
            let pending = session.take_feedback(0);
            assert!(pending.events.is_empty());
            assert!(pending.report.selected && !pending.report.enabled);
            assert_eq!(pending.report.epoch, Some(7));
            assert!(!pending.report.stop_required && pending.report.error.is_none());
            assert!(session.enable_experimental_feedback(8).is_err());
            assert_eq!(session.feedback_report().epoch, Some(7));
        }
    }

    #[test]
    fn feedback_opt_in_rejects_attach_finish_and_game_running_boundaries() {
        for gate in 0..3 {
            let mut session = unattached_session(OutputSelection::Diagnostic);
            match gate {
                0 => session.attach_attempted = true,
                1 => session.finished = true,
                _ => session.game_running = true,
            }
            assert!(session.enable_experimental_feedback(1).is_err());
            assert!(!session.feedback_report().selected);
            assert!(session.failure.is_none());
        }
    }

    #[test]
    fn disabling_pending_feedback_is_sticky_and_does_not_fail_input() {
        let mut session = unattached_session(OutputSelection::Diagnostic);
        session.disable_feedback("unselected feedback is a no-op");
        assert!(session.feedback_report().error.is_none());
        session.enable_experimental_feedback(42).unwrap();
        session.disable_feedback("owned route retired");
        session.disable_feedback("later reason");
        assert!(matches!(
            session.feedback_selection,
            FeedbackSelection::Disabled
        ));
        let report = session.feedback_report();
        assert!(report.selected && !report.enabled && report.stop_required);
        assert_eq!(report.epoch, Some(42));
        assert_eq!(report.error.as_deref(), Some("owned route retired"));
        assert!(session.take_feedback(64).events.is_empty());
        assert!(session.failure.is_none() && session.enabled());
        assert!(session.enable_experimental_feedback(43).is_err());
    }

    fn feedback_manifest() -> Value {
        json!({"schema_version":1,"experiment":"owned_wine_haptics_feedback",
            "feedback_bytes":48,"input_hid_bytes":15,"output_hid_bytes":5,
            "feature_hid_bytes":17,"input_report_id":0,"output_report_id":1})
    }

    fn write_feedback_payload(root: &Path) {
        fs::create_dir_all(root.join("arm64")).unwrap();
        let mut manifest = feedback_manifest();
        for name in ["feeder.exe", "skinosinput.sys", "skinosinput.inf"] {
            let relative = format!("arm64/{name}");
            let bytes = format!("labelled non-executable fixture: {name}");
            fs::write(root.join(&relative), &bytes).unwrap();
            manifest["files"][&relative] = json!(format!("{:x}", Sha256::digest(bytes.as_bytes())));
        }
        fs::write(root.join("manifest.json"), manifest.to_string()).unwrap();
    }

    #[test]
    fn feedback_manifest_requires_every_exact_abi_field() {
        let manifest = feedback_manifest();
        validate_experimental_manifest(&manifest).unwrap();
        for key in [
            "schema_version",
            "experiment",
            "feedback_bytes",
            "input_hid_bytes",
            "output_hid_bytes",
            "feature_hid_bytes",
            "input_report_id",
            "output_report_id",
        ] {
            for value in [Value::Null, json!(99), json!(true), json!("48")] {
                let mut changed = manifest.clone();
                changed[key] = value;
                assert!(
                    validate_experimental_manifest(&changed).is_err(),
                    "accepted {key}"
                );
            }
        }
    }

    #[test]
    fn feedback_payload_checks_all_components_and_rejects_normal_manifest() {
        let temporary = Temp::new();
        write_feedback_payload(&temporary.0);
        let (feeder, inf) = verified_experimental_driver(&temporary.0, "arm64").unwrap();
        assert_eq!(feeder, temporary.0.join("arm64/feeder.exe"));
        assert_eq!(inf, temporary.0.join("arm64/skinosinput.inf"));
        for arch in ["x64", "x86", "../arm64"] {
            assert!(verified_experimental_driver(&temporary.0, arch).is_err());
        }
        for name in ["feeder.exe", "skinosinput.sys", "skinosinput.inf"] {
            fs::write(temporary.0.join("arm64").join(name), "modified").unwrap();
            assert!(verified_experimental_driver(&temporary.0, "arm64").is_err());
            write_feedback_payload(&temporary.0);
        }
        let path = temporary.0.join("manifest.json");
        let mut ordinary: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        ordinary.as_object_mut().unwrap().remove("experiment");
        fs::write(&path, ordinary.to_string()).unwrap();
        assert!(verified_driver(&temporary.0, "arm64").is_ok());
        assert!(verified_experimental_driver(&temporary.0, "arm64").is_err());
    }

    #[test]
    fn feedback_driver_resolves_only_the_exact_sdk_payload_sibling() {
        let temporary = Temp::new();
        let plan = skinos_compat::LaunchPlan {
            program: PathBuf::new(),
            args: vec![],
            env: Default::default(),
            working_dir: PathBuf::new(),
            read_roots: vec![],
            write_roots: vec![],
            execute_roots: vec![],
            prefix_dir: PathBuf::new(),
            runner_root: PathBuf::new(),
            sdk_probe: temporary.0.join("sdk-probe.exe"),
            windows_sdk: None,
            sdk_args: vec![],
            sdk_env: Default::default(),
        };
        write_feedback_payload(&temporary.0.join("wine-input"));
        assert!(bundled_experimental_driver(&plan, "arm64").is_err());
        let root = temporary.0.join("wine-input-rumble-experiment");
        write_feedback_payload(&root);
        assert_eq!(
            bundled_experimental_driver(&plan, "arm64").unwrap(),
            (
                root.join("arm64/feeder.exe"),
                root.join("arm64/skinosinput.inf")
            )
        );
    }

    #[test]
    fn bundled_feeder_detects_modified_or_cross_arch_component() {
        let temporary = Temp::new();
        let feeder = temporary.0.join("arm64/feeder.exe");
        fs::write(&feeder, b"labelled test fixture").unwrap();
        let manifest = json!({"schema_version":1,"files":{
            "arm64/feeder.exe":format!("{:x}", Sha256::digest(b"labelled test fixture"))
        }});
        fs::write(temporary.0.join("manifest.json"), manifest.to_string()).unwrap();
        assert_eq!(verified_feeder(&temporary.0, "arm64").unwrap(), feeder);
        assert!(verified_feeder(&temporary.0, "x64").is_err());
        assert!(verified_feeder(&temporary.0, "../arm64").is_err());
        fs::write(&feeder, b"changed component").unwrap();
        assert!(verified_feeder(&temporary.0, "arm64").is_err());
    }
    #[test]
    fn bundled_component_rejects_symlink_or_oversize_before_reading() {
        let temporary = Temp::new();
        let file = temporary.0.join("component");
        let link = temporary.0.join("link");
        fs::write(&file, b"payload").unwrap();
        symlink(&file, &link).unwrap();
        assert!(bounded_file(&link, 64).is_err());
        assert!(bounded_file(&file, 6).is_err());
        assert_eq!(bounded_file(&file, 7).unwrap(), b"payload");
    }
    #[test]
    fn every_driver_component_is_verified_before_ensure_command() {
        let temporary = Temp::new();
        let mut files = serde_json::Map::new();
        for name in ["feeder.exe", "skinosinput.sys", "skinosinput.inf"] {
            let relative = format!("arm64/{name}");
            fs::write(temporary.0.join(&relative), name.as_bytes()).unwrap();
            files.insert(
                relative,
                json!(format!("{:x}", Sha256::digest(name.as_bytes()))),
            );
        }
        fs::write(
            temporary.0.join("manifest.json"),
            json!({"schema_version":1,"files":files}).to_string(),
        )
        .unwrap();
        assert!(verified_driver(&temporary.0, "arm64").is_ok());
        for name in ["skinosinput.sys", "skinosinput.inf"] {
            fs::write(temporary.0.join("arm64").join(name), "changed").unwrap();
            assert!(verified_driver(&temporary.0, "arm64").is_err());
            fs::write(temporary.0.join("arm64").join(name), name).unwrap();
        }
    }
    #[test]
    fn zero_device_game_accepts_lazy_unloaded_library_but_not_unbound_output() {
        assert!(validate_zero_device_finish(None).is_ok());
        let mut stats = OutputStats {
            version: 1,
            bytes: 48,
            ..OutputStats::default()
        };
        assert!(validate_zero_device_finish(Some(&stats)).is_ok());
        stats.queued_frames = 1;
        assert!(validate_zero_device_finish(Some(&stats)).is_err());
        stats.queued_frames = 0;
        stats.active_slot_mask = 1;
        assert!(validate_zero_device_finish(Some(&stats)).is_err());
    }
    #[test]
    fn zero_device_product_diagnostic_accepts_no_library_but_keeps_strict_completion() {
        assert!(OutputSelection::ProductDiagnostic.uses_production_preparation());
        assert!(OutputSelection::ProductDiagnostic.is_diagnostic());
        assert!(!OutputSelection::ProductDiagnostic.is_game());
        assert!(validate_zero_device_finish(None).is_ok());
        assert!(
            completion_result(OutputSelection::ProductDiagnostic, false, true, None, None).is_ok()
        );
        assert!(
            completion_result(
                OutputSelection::ProductDiagnostic,
                false,
                true,
                Some("strict diagnostic shim timeout".into()),
                Some("strict diagnostic shim timeout".into())
            )
            .is_err()
        );
    }
    #[test]
    fn input_degradation_does_not_replace_a_successful_game_outcome() {
        assert!(
            completion_result(
                OutputSelection::Game,
                true,
                true,
                Some("input lost".into()),
                None
            )
            .is_ok()
        );
        assert!(
            completion_result(
                OutputSelection::Game,
                false,
                true,
                Some("input lost".into()),
                None
            )
            .is_err()
        );
        assert!(
            completion_result(
                OutputSelection::Diagnostic,
                false,
                true,
                Some("input lost".into()),
                None
            )
            .is_err()
        );
        assert!(completion_result(OutputSelection::Game, true, false, None, None).is_err());
        assert!(
            completion_result(
                OutputSelection::Game,
                true,
                true,
                Some("input lost".into()),
                Some("game exited nonzero".into())
            )
            .is_err()
        );
    }
    #[test]
    fn only_a_requested_game_stop_accepts_the_expected_signal_status() {
        for expected_stop in [false, true] {
            let child = Command::new("/bin/sh")
                .args(["-c", "exit 143"])
                .spawn()
                .unwrap();
            let mut shim = RetainedShim::new(child, json!({"exit":null}));
            shim.game = true;
            shim.expected_stop = expected_stop;
            let report = shim.finish(Duration::from_secs(2));
            assert_eq!(report.ok, expected_stop);
            assert!(report.actual_probe.is_null());
            assert!(report.actual_game.unwrap()["exit"].is_null());
        }
    }

    #[test]
    fn retained_shim_stays_owned_until_real_exit_and_keeps_probe_exit_unknown() {
        use std::io::Write;
        let mut child = Command::new("/bin/sh")
            .args(["-c", "read line; exit 0"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        let mut shim = RetainedShim::new(
            child,
            json!({"pid":123,"termination_observation":"kernel_esrch","exit":null}),
        );
        assert!(shim.child.try_wait().unwrap().is_none());
        input.write_all(b"finish\n").unwrap();
        drop(input);
        let report = shim.finish(Duration::from_secs(2));
        assert!(report.ok && report.child_reaped && !report.timed_out);
        assert_eq!(report.exit_code, Some(0));
        assert!(report.actual_probe["exit"].is_null());
    }

    #[test]
    fn retained_shim_nonzero_timeout_and_drop_are_not_success() {
        let child = Command::new("/bin/sh")
            .args(["-c", "exit 7"])
            .spawn()
            .unwrap();
        let mut shim = RetainedShim::new(child, json!({}));
        let report = shim.finish(Duration::from_secs(2));
        assert!(!report.ok && report.child_reaped);
        assert_eq!(report.exit_code, Some(7));
        let child = Command::new("/bin/sh")
            .args(["-c", "read line"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut shim = RetainedShim::new(child, json!({}));
        let report = shim.finish(Duration::from_millis(20));
        assert!(!report.ok && report.timed_out && report.child_reaped);
        assert_eq!(report.termination_signal, Some(9));
        let child = Command::new("/bin/sh")
            .args(["-c", "read line"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        drop(RetainedShim::new(child, json!({})));
        assert_eq!(unsafe { super::super::kill(pid, 0) }, -1);
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(3));
    }
    fn observed_game() -> Value {
        json!({"pid":123,"start_seconds":1234,"start_microseconds":5,
            "prefix_appid_verified":true,"termination_observation":"kernel_esrch","exit":null})
    }
    fn waiting_child() -> Child {
        Command::new("/bin/sh")
            .args(["-c", "read line"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    }
    #[test]
    fn verified_game_can_reap_only_its_waiting_launcher_without_touching_other_service() {
        let service = waiting_child();
        let mut unrelated = RetainedShim::new(service, json!({}));
        let mut launcher = RetainedShim::new(waiting_child(), observed_game());
        launcher.game = true;
        let report = launcher.finish_with_game_cleanup(Duration::from_millis(20), true);
        assert!(
            report.ok
                && report.timed_out
                && report.child_reaped
                && report.forced_launcher_cleanup
                && report.launcher_cleanup_only
        );
        assert!(report.game_exit_independently_observed);
        assert_eq!(report.exit_code, None);
        assert_eq!(report.termination_signal, Some(9));
        assert!(report.actual_game.unwrap()["exit"].is_null());
        assert!(
            unrelated.child.try_wait().unwrap().is_none(),
            "Unrelated Wine-service stand-in was terminated"
        );
        assert!(launcher.finish_with_game_cleanup(Duration::ZERO, true).ok);
        drop(unrelated);
    }
    #[test]
    fn forced_launcher_cleanup_requires_ordinary_game_verified_exit_and_closed_relay() {
        for (game, observed, relay_closed) in [
            (false, true, true),
            (true, false, true),
            (true, true, false),
        ] {
            let mut launcher = RetainedShim::new(
                waiting_child(),
                if observed {
                    observed_game()
                } else {
                    json!({"exit":null})
                },
            );
            launcher.game = game;
            let report = launcher.finish_with_game_cleanup(Duration::from_millis(20), relay_closed);
            assert!(!report.ok && report.timed_out && report.child_reaped);
            assert!(!report.launcher_cleanup_only);
        }
        let child = Command::new("/bin/sh")
            .args(["-c", "exit 7"])
            .spawn()
            .unwrap();
        let mut launcher = RetainedShim::new(child, observed_game());
        launcher.game = true;
        let report = launcher.finish_with_game_cleanup(Duration::from_secs(1), true);
        assert!(!report.ok && !report.timed_out && !report.forced_launcher_cleanup);
        assert_eq!(report.exit_code, Some(7));
    }
}
