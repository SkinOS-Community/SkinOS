//! Optional native SteamInput006 differential consumer. No input is injected.
use crate::{Library, Result};
use serde::Serialize;
use std::{
    ffi::c_void,
    time::{Duration, Instant},
};

pub const REQUIRED_SYMBOLS: &[&str] = &[
    "SteamAPI_SteamInput_v006",
    "SteamAPI_ISteamInput_Init",
    "SteamAPI_ISteamInput_RunFrame",
    "SteamAPI_ISteamInput_GetConnectedControllers",
    "SteamAPI_ISteamInput_GetGamepadIndexForController",
    "SteamAPI_ISteamInput_Shutdown",
    "SteamAPI_SteamUtils_v010",
    "SteamAPI_ISteamUtils_GetAppID",
];
const SCAN_DURATION: Duration = Duration::from_secs(7);
const MAX_CONTROLLERS: usize = 16;

#[derive(Serialize)]
pub struct InputController {
    pub handle: String,
    pub gamepad_index: i32,
}
#[derive(Serialize)]
pub struct InputReport {
    pub interface: &'static str,
    pub api_initialized: bool,
    pub appid_verified: bool,
    pub appid: u32,
    pub scan_completed: bool,
    pub max_controllers: usize,
    pub controller_seen: bool,
    pub controllers: Vec<InputController>,
    pub controller_union_truncated: bool,
    pub sample_count: usize,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_data_frames: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_data_frames_before_runframe: Option<usize>,
    pub shutdown_succeeded: Option<bool>,
    pub physical_device_verified: bool,
    pub error: Option<String>,
}
impl InputReport {
    pub fn empty() -> Self {
        Self {
            interface: "SteamInput006",
            api_initialized: false,
            appid_verified: false,
            appid: 0,
            scan_completed: false,
            max_controllers: 0,
            controller_seen: false,
            controllers: vec![],
            controller_union_truncated: false,
            sample_count: 0,
            elapsed_ms: 0,
            new_data_frames: None,
            new_data_frames_before_runframe: None,
            shutdown_succeeded: None,
            physical_device_verified: false,
            error: None,
        }
    }
    pub fn success(&self) -> bool {
        self.api_initialized && self.appid_verified && self.scan_completed && self.error.is_none()
    }
}

trait InputApi {
    fn appid(&self) -> u32;
    fn identity_matches(&self) -> bool;
    fn init(&mut self) -> bool;
    fn pump(&mut self);
    fn new_data_available(&self) -> Option<bool>;
    fn connected_controllers(&mut self, handles: &mut [u64; MAX_CONTROLLERS]) -> i32;
    fn gamepad_index(&self, handle: u64) -> i32;
    fn shutdown(&mut self) -> bool;
}

struct InputSession<'a, A: InputApi> {
    api: &'a mut A,
    active: bool,
}
impl<A: InputApi> InputSession<'_, A> {
    fn shutdown(&mut self) -> bool {
        self.active = false;
        self.api.shutdown()
    }
}
impl<A: InputApi> Drop for InputSession<'_, A> {
    fn drop(&mut self) {
        if self.active {
            self.api.shutdown();
        }
    }
}

fn scan(api: &mut impl InputApi, expected_appid: u32, duration: Duration) -> InputReport {
    let mut report = InputReport::empty();
    report.appid = api.appid();
    report.appid_verified = report.appid == expected_appid;
    if !report.appid_verified || !api.identity_matches() {
        report.error = Some("Native Steam Input SDK identity or AppID did not match".into());
        return report;
    }
    report.api_initialized = api.init();
    if !report.api_initialized {
        report.error = Some("Native SteamInput006 Init failed".into());
        return report;
    }
    let mut session = InputSession { api, active: true };
    let started = Instant::now();
    loop {
        if let Some(available) = session.api.new_data_available() {
            *report.new_data_frames_before_runframe.get_or_insert(0) += usize::from(available);
        }
        session.api.pump();
        if !session.api.identity_matches() || session.api.appid() != expected_appid {
            report.error = Some("SDK identity or AppID changed during native input scan".into());
            break;
        }
        if let Some(available) = session.api.new_data_available() {
            *report.new_data_frames.get_or_insert(0) += usize::from(available);
        }
        let mut handles = [0; MAX_CONTROLLERS];
        let count = session.api.connected_controllers(&mut handles);
        if !(0..=MAX_CONTROLLERS as i32).contains(&count) {
            report.error = Some("Native Steam Input returned an invalid controller count".into());
            break;
        }
        report.sample_count += 1;
        report.max_controllers = report.max_controllers.max(count as usize);
        report.controller_seen |= count > 0;
        for handle in &handles[..count as usize] {
            if *handle == 0 {
                report.error = Some("Native Steam Input returned a zero controller handle".into());
                break;
            }
            let key = handle.to_string();
            let index = session.api.gamepad_index(*handle);
            if let Some(previous) = report
                .controllers
                .iter_mut()
                .find(|entry| entry.handle == key)
            {
                previous.gamepad_index = index;
            } else if report.controllers.len() < MAX_CONTROLLERS {
                report.controllers.push(InputController {
                    handle: key,
                    gamepad_index: index,
                });
            } else {
                report.controller_union_truncated = true;
            }
        }
        if report.error.is_some() {
            break;
        }
        if started.elapsed() >= duration {
            report.scan_completed = true;
            break;
        }
        std::thread::sleep(
            Duration::from_millis(100).min(duration.saturating_sub(started.elapsed())),
        );
    }
    report.elapsed_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    let shutdown = session.shutdown();
    report.shutdown_succeeded = Some(shutdown);
    if !shutdown && report.error.is_none() {
        report.error = Some("Native Steam Input Shutdown failed".into());
    }
    report
}

type Factory = unsafe extern "C" fn() -> *mut c_void;
type Init = unsafe extern "C" fn(*mut c_void, bool) -> bool;
type RunFrame = unsafe extern "C" fn(*mut c_void, bool);
type Connected = unsafe extern "C" fn(*mut c_void, *mut u64) -> i32;
type GamepadIndex = unsafe extern "C" fn(*mut c_void, u64) -> i32;
type Shutdown = unsafe extern "C" fn(*mut c_void) -> bool;
type GetAppId = unsafe extern "C" fn(*mut c_void) -> u32;
type NewData = unsafe extern "C" fn(*mut c_void) -> bool;
type Callbacks = unsafe extern "C" fn();

struct NativeApi<'a> {
    input: *mut c_void,
    utils: *mut c_void,
    initialize: Init,
    run_frame: RunFrame,
    connected: Connected,
    index: GamepadIndex,
    stop: Shutdown,
    get_appid: GetAppId,
    new_data: Option<NewData>,
    callbacks: Callbacks,
    context: &'a dyn Fn() -> bool,
}
impl<'a> NativeApi<'a> {
    fn load(sdk: &Library, callbacks: Callbacks, context: &'a dyn Fn() -> bool) -> Result<Self> {
        // Valve's pinned SDK 1.53a steam_api_flat.h: SteamInput006 and
        // SteamUtils010 C exports; no private vtable offsets are called.
        unsafe {
            let input_factory: Factory =
                std::mem::transmute(sdk.symbol(c"SteamAPI_SteamInput_v006")?);
            let utils_factory: Factory =
                std::mem::transmute(sdk.symbol(c"SteamAPI_SteamUtils_v010")?);
            let input = input_factory();
            let utils = utils_factory();
            if input.is_null() || utils.is_null() {
                return Err("Native SteamInput006 or SteamUtils010 is unavailable".into());
            }
            Ok(Self {
                input,
                utils,
                initialize: std::mem::transmute::<*mut c_void, Init>(
                    sdk.symbol(c"SteamAPI_ISteamInput_Init")?,
                ),
                run_frame: std::mem::transmute::<*mut c_void, RunFrame>(
                    sdk.symbol(c"SteamAPI_ISteamInput_RunFrame")?,
                ),
                connected: std::mem::transmute::<*mut c_void, Connected>(
                    sdk.symbol(c"SteamAPI_ISteamInput_GetConnectedControllers")?,
                ),
                index: std::mem::transmute::<*mut c_void, GamepadIndex>(
                    sdk.symbol(c"SteamAPI_ISteamInput_GetGamepadIndexForController")?,
                ),
                stop: std::mem::transmute::<*mut c_void, Shutdown>(
                    sdk.symbol(c"SteamAPI_ISteamInput_Shutdown")?,
                ),
                get_appid: std::mem::transmute::<*mut c_void, GetAppId>(
                    sdk.symbol(c"SteamAPI_ISteamUtils_GetAppID")?,
                ),
                new_data: sdk
                    .symbol(c"SteamAPI_ISteamInput_BNewDataAvailable")
                    .ok()
                    .map(|pointer| std::mem::transmute::<*mut c_void, NewData>(pointer)),
                callbacks,
                context,
            })
        }
    }
}
impl InputApi for NativeApi<'_> {
    fn appid(&self) -> u32 {
        unsafe { (self.get_appid)(self.utils) }
    }
    fn identity_matches(&self) -> bool {
        (self.context)()
    }
    fn init(&mut self) -> bool {
        unsafe { (self.initialize)(self.input, true) }
    }
    fn pump(&mut self) {
        unsafe {
            (self.callbacks)();
            (self.run_frame)(self.input, true);
        }
    }
    fn new_data_available(&self) -> Option<bool> {
        self.new_data
            .map(|function| unsafe { function(self.input) })
    }
    fn connected_controllers(&mut self, handles: &mut [u64; MAX_CONTROLLERS]) -> i32 {
        unsafe { (self.connected)(self.input, handles.as_mut_ptr()) }
    }
    fn gamepad_index(&self, handle: u64) -> i32 {
        unsafe { (self.index)(self.input, handle) }
    }
    fn shutdown(&mut self) -> bool {
        unsafe { (self.stop)(self.input) }
    }
}

pub fn check(
    sdk: &Library,
    appid: u32,
    callbacks: Callbacks,
    context: &dyn Fn() -> bool,
) -> InputReport {
    match NativeApi::load(sdk, callbacks, context) {
        Ok(mut api) => scan(&mut api, appid, SCAN_DURATION),
        Err(error) => {
            let mut report = InputReport::empty();
            report.error = Some(error);
            report
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct MockApi {
        appid: u32,
        identity: bool,
        init_count: usize,
        shutdown_count: usize,
        count: i32,
        handle: u64,
    }
    impl MockApi {
        fn new(count: i32, handle: u64) -> Self {
            Self {
                appid: 648800,
                identity: true,
                init_count: 0,
                shutdown_count: 0,
                count,
                handle,
            }
        }
    }
    impl InputApi for MockApi {
        fn appid(&self) -> u32 {
            self.appid
        }
        fn identity_matches(&self) -> bool {
            self.identity
        }
        fn init(&mut self) -> bool {
            self.init_count += 1;
            true
        }
        fn pump(&mut self) {}
        fn new_data_available(&self) -> Option<bool> {
            Some(true)
        }
        fn connected_controllers(&mut self, handles: &mut [u64; MAX_CONTROLLERS]) -> i32 {
            handles[0] = self.handle;
            self.count
        }
        fn gamepad_index(&self, _: u64) -> i32 {
            2
        }
        fn shutdown(&mut self) -> bool {
            self.shutdown_count += 1;
            true
        }
    }
    #[test]
    fn zero_controllers_is_a_valid_scan_not_physical_success() {
        let mut api = MockApi::new(0, 0);
        let report = scan(&mut api, 648800, Duration::ZERO);
        assert!(report.success());
        assert_eq!(report.max_controllers, 0);
        assert!(!report.controller_seen);
        assert!(!report.physical_device_verified);
        assert_eq!(api.shutdown_count, 1);
    }
    #[test]
    fn identity_and_actual_appid_gate_input_initialization() {
        let mut api = MockApi::new(1, 77);
        api.appid = 42;
        let report = scan(&mut api, 648800, Duration::ZERO);
        assert!(!report.success());
        assert!(!report.appid_verified);
        assert_eq!(api.init_count, 0);
        api.appid = 648800;
        api.identity = false;
        assert!(!scan(&mut api, 648800, Duration::ZERO).success());
        assert_eq!(api.init_count, 0);
    }
    #[test]
    fn actual_handle_and_index_are_reported_without_synthetic_mapping() {
        let mut api = MockApi::new(1, 77);
        let report = scan(&mut api, 648800, Duration::ZERO);
        assert!(report.success());
        assert_eq!(report.controllers[0].handle, "77");
        assert_eq!(report.controllers[0].gamepad_index, 2);
        assert_eq!(report.new_data_frames, Some(1));
        assert_eq!(api.shutdown_count, 1);
    }
    #[test]
    fn malformed_controller_results_fail_and_shutdown_once() {
        for (count, handle) in [(17, 77), (-1, 77), (1, 0)] {
            let mut api = MockApi::new(count, handle);
            let report = scan(&mut api, 648800, Duration::ZERO);
            assert!(!report.success());
            assert!(!report.scan_completed);
            assert_eq!(api.shutdown_count, 1);
        }
    }
}
