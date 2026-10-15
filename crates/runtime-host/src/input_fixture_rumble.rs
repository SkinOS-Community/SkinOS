//! Explicit opt-in observation of the OWNED SDL software fixture's rumble.
//! This module never invokes SDL_Rumble*, Steam APIs, or physical hardware.
//! Pinned SDL 70e9cc86d: virtual/SDL_virtualjoystick.c:57-69,124-146,312-320.
//! Detach invokes Cleanup synchronously under the joystick lock; partial attach
//! failure can also invoke Cleanup, whereas failure before descriptor copy does
//! not. One separate Arc reference belongs to SDL until that exact boundary.

use std::ffi::c_void;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Instant;

const MAX_CALLBACKS: usize = 64;

/// Must run before fixture attachment. An inherited request cannot arm a
/// normal game, physical-device scan, or another active fixture experiment.
pub fn validate_environment(diagnostic: bool) -> Result<bool, String> {
    validate_options(
        std::env::var("SKINOS_INPUT_RUMBLE_TEST").ok().as_deref(),
        std::env::var("SKINOS_STEAM_INPUT_FIXTURE").ok().as_deref(),
        diagnostic,
        std::env::var("SKINOS_INPUT_ACTIVITY").ok().as_deref(),
        std::env::var("SKINOS_INPUT_RECONNECT_TEST").ok().as_deref(),
    )
}
/// Separate normal-game feedback QA. It records callbacks but never generates
/// the direct SDK rumble sequence; a real Windows XInput consumer supplies it.
pub fn validate_feedback_environment(game: bool) -> Result<bool, String> {
    let direct = std::env::var("SKINOS_INPUT_RUMBLE_TEST").ok();
    let feedback = std::env::var("SKINOS_INPUT_FEEDBACK_TEST").ok();
    registration_flag(direct.as_deref(), feedback.as_deref())?;
    validate_options(
        feedback.as_deref(),
        std::env::var("SKINOS_STEAM_INPUT_FIXTURE").ok().as_deref(),
        game,
        std::env::var("SKINOS_INPUT_ACTIVITY").ok().as_deref(),
        std::env::var("SKINOS_INPUT_RECONNECT_TEST").ok().as_deref(),
    ).map_err(|_| "Feedback QA requires an explicit neutral-fixture game without direct rumble, activity or reconnect".into())
}
fn registration_flag<'a>(
    direct: Option<&'a str>,
    feedback: Option<&'a str>,
) -> Result<Option<&'a str>, String> {
    let direct = direct.filter(|flag| !flag.is_empty());
    let feedback = feedback.filter(|flag| !flag.is_empty());
    match (direct, feedback) {
        (None, None) => Ok(None),
        (Some("1"), None) | (None, Some("1")) => Ok(Some("1")),
        _ => Err("Rumble fixture requires exactly one supported test mode".into()),
    }
}
fn validate_options(
    flag: Option<&str>,
    fixture: Option<&str>,
    diagnostic: bool,
    activity: Option<&str>,
    reconnect: Option<&str>,
) -> Result<bool, String> {
    match flag {
        None | Some("") => Ok(false),
        Some("1") if fixture == Some("neutral") && diagnostic
            && activity.is_none_or(str::is_empty) && reconnect.is_none_or(str::is_empty) => Ok(true),
        _ => Err("Rumble QA requires an explicit neutral-fixture diagnostic without activity or reconnect".into()),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallbackSample {
    pub sequence: u64,
    pub elapsed_us: u64,
    pub low: u16,
    pub high: u16,
}

#[derive(Clone, Debug)]
pub struct Readback {
    pub samples: Vec<CallbackSample>,
    pub ordered_left_zero_right_zero: bool,
    pub physical_device_verified: bool,
    pub cleanup_seen: bool,
}

struct Trace {
    began: Instant,
    samples: Vec<CallbackSample>,
}
struct State {
    trace: Mutex<Trace>,
    cleanup_seen: AtomicBool,
    sdl_reference_released: AtomicBool,
    lost: AtomicBool,
    observation_started: AtomicBool,
    recording: AtomicBool,
    callbacks_inflight: AtomicUsize,
}

/// Own this in Fixture after successful attachment. Dropping it never frees
/// the separate SDL reference while a successful attachment still owns it.
pub struct Registration {
    state: Arc<State>,
    attach_finished: bool,
    joystick_id: u32,
}

/// Retain this across Fixture's consuming detach_for_reconnect_test/Drop to
/// observe SDL Cleanup without changing either existing lifecycle method.
#[derive(Clone)]
pub struct Monitor {
    state: Arc<State>,
}
impl Monitor {
    pub fn cleanup_seen(&self) -> bool {
        self.state.cleanup_seen.load(Ordering::Acquire)
    }
}

pub struct Callbacks {
    pub userdata: *mut c_void,
    pub rumble: unsafe extern "C" fn(*mut c_void, u16, u16) -> bool,
    pub cleanup: unsafe extern "C" fn(*mut c_void),
}

impl Registration {
    pub fn from_environment() -> Result<Option<Self>, String> {
        let flag = std::env::var("SKINOS_INPUT_RUMBLE_TEST").ok();
        let feedback = std::env::var("SKINOS_INPUT_FEEDBACK_TEST").ok();
        let fixture = std::env::var("SKINOS_STEAM_INPUT_FIXTURE").ok();
        Self::from_options(
            registration_flag(flag.as_deref(), feedback.as_deref())?,
            fixture.as_deref(),
        )
    }

    fn from_options(flag: Option<&str>, fixture: Option<&str>) -> Result<Option<Self>, String> {
        match flag {
            None | Some("") => return Ok(None),
            Some("1") if fixture == Some("neutral") => {}
            _ => {
                return Err(
                    "Rumble test requires explicit 1 and the owned neutral software fixture".into(),
                );
            }
        }
        let state = Arc::new(State {
            trace: Mutex::new(Trace {
                began: Instant::now(),
                samples: Vec::with_capacity(MAX_CALLBACKS),
            }),
            cleanup_seen: AtomicBool::new(false),
            sdl_reference_released: AtomicBool::new(false),
            lost: AtomicBool::new(false),
            observation_started: AtomicBool::new(false),
            recording: AtomicBool::new(false),
            callbacks_inflight: AtomicUsize::new(0),
        });
        // This reference is released by Cleanup, or finish_attach(0) if SDL
        // failed before copying the descriptor. Both paths share one CAS.
        let _ = Arc::into_raw(Arc::clone(&state));
        Ok(Some(Self {
            state,
            attach_finished: false,
            joystick_id: 0,
        }))
    }

    pub fn callbacks(&self) -> Callbacks {
        Callbacks {
            userdata: Arc::as_ptr(&self.state).cast_mut().cast(),
            rumble: observe_rumble,
            cleanup: cleanup_userdata,
        }
    }

    pub fn monitor(&self) -> Monitor {
        Monitor {
            state: Arc::clone(&self.state),
        }
    }

    /// Call immediately after the synchronous pinned SDL_AttachVirtualJoystick.
    /// Do not pass these callbacks to any other owner or re-use the descriptor.
    pub fn finish_attach(&mut self, joystick_id: u32) -> Result<(), String> {
        if self.attach_finished {
            return Err("Rumble attachment completed twice".into());
        }
        self.attach_finished = true;
        if joystick_id == 0 {
            unsafe {
                release_sdl_reference(Arc::as_ptr(&self.state));
            }
            return Err("Original SDL did not attach the rumble software fixture".into());
        }
        if self.state.cleanup_seen.load(Ordering::Acquire) {
            return Err("SDL reported attachment after cleaning its userdata".into());
        }
        self.joystick_id = joystick_id;
        Ok(())
    }

    /// Call after proving this exact software fixture's genuine handle/route,
    /// before spawning the SDK rumble probe. This does not arm physical devices.
    pub fn begin_observation(&self, owned_joystick_id: u32) -> Result<(), String> {
        if !self.attach_finished
            || owned_joystick_id == 0
            || owned_joystick_id != self.joystick_id
            || self.state.cleanup_seen.load(Ordering::Acquire)
        {
            return Err(
                "Rumble observation does not belong to an attached software fixture".into(),
            );
        }
        let mut trace = self
            .state
            .trace
            .lock()
            .map_err(|_| "Rumble trace lock poisoned")?;
        if self.state.lost.load(Ordering::Acquire) {
            return Err("Rumble callbacks were lost".into());
        }
        if self.state.observation_started.swap(true, Ordering::SeqCst) {
            return Err("Rumble observation already started".into());
        }
        trace.samples.clear();
        trace.began = Instant::now();
        drop(trace);
        self.state.recording.store(true, Ordering::SeqCst);
        Ok(())
    }

    pub fn cleanup_seen(&self) -> bool {
        self.state.cleanup_seen.load(Ordering::Acquire)
    }

    /// Callback observation only; the caller must separately validate the SDK
    /// dispatch transcript, identity, AppID and reciprocal route. Repeated equal
    /// callback values are allowed; no cross-channel or reordered phase is.
    pub fn ordered_readback(&self) -> Result<Readback, String> {
        if !self.state.observation_started.load(Ordering::SeqCst) {
            return Err("Rumble observation was never begun".into());
        }
        // Close the recording window before inspecting the trace. A callback
        // entering after this point may accept a software request but cannot
        // mutate this proof. Already-entered callbacks must finish first; never
        // block while holding SDL's joystick lock or claim a partial snapshot.
        self.state.recording.store(false, Ordering::SeqCst);
        if self.state.callbacks_inflight.load(Ordering::SeqCst) != 0 {
            return Err("Rumble callback is still executing; retry closed readback".into());
        }
        if self.state.lost.load(Ordering::Acquire) {
            return Err("Rumble callback trace overflowed or lost a sample".into());
        }
        let samples = self
            .state
            .trace
            .lock()
            .map_err(|_| "Rumble trace lock poisoned")?
            .samples
            .clone();
        let mut phase = 0;
        for sample in &samples {
            phase = match (phase, sample.low != 0, sample.high != 0) {
                (0, false, false) => 0,
                (0 | 1, true, false) => 1,
                (1 | 2, false, false) => 2,
                (2 | 3, false, true) => 3,
                (3 | 4, false, false) => 4,
                _ => return Err("Rumble callback order/channel mismatch".into()),
            };
        }
        if phase != 4 {
            return Err("Missing observed left, zero, right, final zero sequence".into());
        }
        Ok(Readback {
            samples,
            ordered_left_zero_right_zero: true,
            physical_device_verified: false,
            cleanup_seen: self.cleanup_seen(),
        })
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        if !self.attach_finished {
            // No successful attach was committed. Caller must finish_attach
            // immediately after the FFI call and before any fallible work.
            unsafe {
                release_sdl_reference(Arc::as_ptr(&self.state));
            }
        }
        // A committed live attachment keeps its SDL ref even if detach fails.
        // A bounded leak until SDL Cleanup is safer than a callback use-after-free.
    }
}

unsafe fn release_sdl_reference(state: *const State) {
    if !unsafe { &*state }
        .sdl_reference_released
        .swap(true, Ordering::AcqRel)
    {
        unsafe {
            drop(Arc::from_raw(state));
        }
    }
}

unsafe extern "C" fn cleanup_userdata(userdata: *mut c_void) {
    if userdata.is_null() {
        return;
    }
    let state = userdata.cast::<State>();
    unsafe { &*state }
        .cleanup_seen
        .store(true, Ordering::Release);
    // Pinned SDL holds its joystick lock across callbacks and cleanup. No
    // further callback can race this final release after SDL frees hwdata.
    unsafe {
        release_sdl_reference(state);
    }
}

unsafe extern "C" fn observe_rumble(userdata: *mut c_void, low: u16, high: u16) -> bool {
    if userdata.is_null() {
        return false;
    }
    let state = unsafe { &*userdata.cast::<State>() };
    if state.cleanup_seen.load(Ordering::Acquire) {
        return false;
    }
    struct Inflight<'a>(&'a State);
    impl Drop for Inflight<'_> {
        fn drop(&mut self) {
            self.0.callbacks_inflight.fetch_sub(1, Ordering::SeqCst);
        }
    }
    state.callbacks_inflight.fetch_add(1, Ordering::SeqCst);
    let _inflight = Inflight(state);
    if !state.recording.load(Ordering::SeqCst) {
        return true;
    }
    // Never block SDL's joystick lock or unwind through the foreign callback.
    let Ok(mut trace) = state.trace.try_lock() else {
        state.lost.store(true, Ordering::Release);
        return false;
    };
    if trace.samples.len() >= MAX_CALLBACKS {
        state.lost.store(true, Ordering::Release);
        return false;
    }
    let sample = CallbackSample {
        sequence: trace.samples.len() as u64 + 1,
        elapsed_us: trace.began.elapsed().as_micros().min(u128::from(u64::MAX)) as u64,
        low,
        high,
    };
    trace.samples.push(sample);
    true // The explicit software fixture recorded this request; no motor claim.
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callback_registration_requires_one_explicit_mode() {
        assert_eq!(registration_flag(None, None).unwrap(), None);
        assert_eq!(registration_flag(Some("1"), None).unwrap(), Some("1"));
        assert_eq!(registration_flag(None, Some("1")).unwrap(), Some("1"));
        assert!(registration_flag(Some("1"), Some("1")).is_err());
        assert!(registration_flag(None, Some("true")).is_err());
        assert!(validate_options(Some("1"), None, true, None, None).is_err());
        assert!(validate_options(Some("1"), Some("neutral"), false, None, None).is_err());
    }
    #[test]
    fn default_off_and_exclusive_diagnostic_policy() {
        assert!(!validate_options(None, None, false, Some("1"), Some("1")).unwrap());
        assert!(validate_options(Some("1"), Some("neutral"), true, None, None).unwrap());
        for (flag, fixture, diagnostic, activity, reconnect) in [
            ("1", "neutral", false, None, None),
            ("1", "physical", true, None, None),
            ("true", "neutral", true, None, None),
            ("1", "neutral", true, Some("1"), None),
            ("1", "neutral", true, None, Some("1")),
        ] {
            assert!(
                validate_options(Some(flag), Some(fixture), diagnostic, activity, reconnect)
                    .is_err()
            );
        }
    }
    fn registration() -> Registration {
        Registration::from_options(Some("1"), Some("neutral"))
            .unwrap()
            .unwrap()
    }
    #[test]
    fn opt_in_requires_owned_fixture() {
        assert!(Registration::from_options(None, None).unwrap().is_none());
        assert!(Registration::from_options(Some("0"), Some("neutral")).is_err());
        assert!(Registration::from_options(Some("1"), None).is_err());
    }
    #[test]
    fn attach_failure_before_descriptor_copy_releases_once() {
        let mut r = registration();
        assert_eq!(Arc::strong_count(&r.state), 2);
        assert!(r.finish_attach(0).is_err());
        assert_eq!(Arc::strong_count(&r.state), 1);
        assert!(!r.cleanup_seen());
    }
    #[test]
    fn partial_attach_cleanup_does_not_double_free() {
        let mut r = registration();
        let c = r.callbacks();
        unsafe {
            (c.cleanup)(c.userdata);
        }
        assert!(r.finish_attach(0).is_err());
        assert_eq!(Arc::strong_count(&r.state), 1);
        assert!(r.cleanup_seen());
    }
    #[test]
    fn live_attachment_outlives_registration_owner() {
        let mut r = registration();
        let c = r.callbacks();
        let weak = Arc::downgrade(&r.state);
        r.finish_attach(77).unwrap();
        drop(r);
        assert!(weak.upgrade().is_some());
        assert!(unsafe { (c.rumble)(c.userdata, 123, 0) });
        unsafe {
            (c.cleanup)(c.userdata);
        }
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn monitor_survives_consuming_fixture_detach() {
        let mut r = registration();
        let c = r.callbacks();
        let monitor = r.monitor();
        r.finish_attach(77).unwrap();
        assert!(!monitor.cleanup_seen());
        unsafe {
            (c.cleanup)(c.userdata);
        }
        drop(r);
        assert!(monitor.cleanup_seen());
        assert_eq!(Arc::strong_count(&monitor.state), 1);
    }
    #[test]
    fn ordered_real_callback_values_are_retained() {
        let mut r = registration();
        let c = r.callbacks();
        r.finish_attach(77).unwrap();
        r.begin_observation(77).unwrap();
        for (l, h) in [(0, 0), (123, 0), (123, 0), (0, 0), (0, 456), (0, 0)] {
            assert!(unsafe { (c.rumble)(c.userdata, l, h) });
        }
        let proof = r.ordered_readback().unwrap();
        assert_eq!(proof.samples[1].low, 123);
        assert_eq!(proof.samples[4].high, 456);
        assert!(!proof.physical_device_verified);
        unsafe {
            (c.cleanup)(c.userdata);
        }
        assert!(r.cleanup_seen());
    }
    #[test]
    fn wrong_order_and_missing_stop_are_errors() {
        for values in [
            vec![(123, 0), (0, 0), (0, 456)],
            vec![(0, 456), (0, 0)],
            vec![(123, 456), (0, 0)],
        ] {
            let mut r = registration();
            let c = r.callbacks();
            r.finish_attach(77).unwrap();
            r.begin_observation(77).unwrap();
            for (l, h) in values {
                unsafe {
                    (c.rumble)(c.userdata, l, h);
                }
            }
            assert!(r.ordered_readback().is_err());
            unsafe {
                (c.cleanup)(c.userdata);
            }
        }
    }
    #[test]
    fn callback_contention_and_overflow_fail_closed() {
        let mut r = registration();
        let c = r.callbacks();
        r.finish_attach(77).unwrap();
        r.begin_observation(77).unwrap();
        {
            let _held = r.state.trace.lock().unwrap();
            assert!(!unsafe { (c.rumble)(c.userdata, 1, 0) });
        }
        assert!(r.ordered_readback().is_err());
        unsafe {
            (c.cleanup)(c.userdata);
        }
        let mut r = registration();
        let c = r.callbacks();
        r.finish_attach(88).unwrap();
        r.begin_observation(88).unwrap();
        for _ in 0..MAX_CALLBACKS {
            assert!(unsafe { (c.rumble)(c.userdata, 0, 0) });
        }
        assert!(!unsafe { (c.rumble)(c.userdata, 0, 0) });
        assert!(r.ordered_readback().is_err());
        unsafe {
            (c.cleanup)(c.userdata);
        }
    }
    #[test]
    fn wrong_fixture_or_after_cleanup_cannot_begin_observation() {
        let mut r = registration();
        let c = r.callbacks();
        r.finish_attach(77).unwrap();
        assert!(r.begin_observation(78).is_err());
        unsafe {
            (c.cleanup)(c.userdata);
        }
        assert!(r.begin_observation(77).is_err());
        assert!(!unsafe { (c.rumble)(c.userdata, 0, 0) });
    }
    #[test]
    fn readback_closes_window_and_rejects_inflight_or_unbegun_samples() {
        let mut r = registration();
        let c = r.callbacks();
        r.finish_attach(77).unwrap();
        for (l, h) in [(1, 0), (0, 0), (0, 1), (0, 0)] {
            assert!(unsafe { (c.rumble)(c.userdata, l, h) });
        }
        assert!(r.ordered_readback().is_err());
        r.begin_observation(77).unwrap();
        for (l, h) in [(1, 0), (0, 0), (0, 1), (0, 0)] {
            assert!(unsafe { (c.rumble)(c.userdata, l, h) });
        }
        r.state.callbacks_inflight.store(1, Ordering::SeqCst);
        assert!(r.ordered_readback().is_err());
        r.state.callbacks_inflight.store(0, Ordering::SeqCst);
        let before = r.ordered_readback().unwrap();
        assert!(unsafe { (c.rumble)(c.userdata, 99, 99) });
        assert_eq!(r.ordered_readback().unwrap().samples, before.samples);
        assert!(r.begin_observation(77).is_err());
        unsafe {
            (c.cleanup)(c.userdata);
        }
    }
}
