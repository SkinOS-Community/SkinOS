//! Session-scoped temporary generic-gamepad opt-in. Called only after the runtime's existing
//! exact SHA256 gate, and drop/restore it before releasing the user/pipe or dylib.
use std::ffi::{c_char, c_void};
use std::io::Write;

const KEY: &std::ffi::CStr = c"SteamController_GenericGamepadSupport";
const USER_LOCAL: u32 = 3;
const AUDITED_SHA256: &str = "035722a42c74989fa38c7efe0d735fdb38444e1102c92246652b5d913d019840";

type GetInterface = unsafe extern "C" fn(*mut c_void, i32, i32) -> *mut c_void;
type GetPipeInterface = unsafe extern "C" fn(*mut c_void, i32) -> *mut c_void;
type IsSet = unsafe extern "C" fn(*mut c_void, u32, *const c_char) -> bool;
type GetBool = unsafe extern "C" fn(*mut c_void, u32, *const c_char, bool) -> bool;
type SetBool = unsafe extern "C" fn(*mut c_void, u32, *const c_char, bool) -> bool;
type RemoveKey = unsafe extern "C" fn(*mut c_void, u32, *const c_char) -> bool;
type ResetCache = unsafe extern "C" fn(*mut c_void);

#[derive(Clone, Copy)]
struct Api {
    config: *mut c_void,
    controller: *mut c_void,
    is_set: IsSet,
    get: GetBool,
    set: SetBool,
    remove: RemoveKey,
    reset: ResetCache,
}

pub struct TemporaryGenericSupport {
    api: Api,
    original_present: bool,
    original_value: bool,
    restore_pending: bool,
    diagnostic: bool,
}

/// Enable only for the exact explicit diagnostic environment flag. No flag
/// means no interface access and no configuration change.
///
/// # Safety
/// `engine` and `base` must refer to the live ARM64 image which has already passed
/// the caller's `AUDITED_RUNTIME` SHA256 check. User and pipe must belong to it.
/// The returned guard must be restored/dropped before any of those are released.
pub unsafe fn attach(
    engine: *mut c_void,
    user: i32,
    pipe: i32,
    base: usize,
) -> Result<Option<TemporaryGenericSupport>, String> {
    if std::env::var("SKINOS_INPUT_GENERIC_SUPPORT")
        .ok()
        .as_deref()
        != Some("1")
    {
        return Ok(None);
    }
    unsafe { attach_impl(engine, user, pipe, base, true).map(Some) }
}

/// Enable generic gamepad support for this explicitly selected owned session.
/// No environment or diagnostic-fixture flag is consulted.
///
/// # Safety
/// The caller has verified the same live audited engine, user and pipe as for
/// `attach`. Restore/drop this guard before releasing any of those objects.
pub unsafe fn attach_for_session(
    engine: *mut c_void,
    user: i32,
    pipe: i32,
    base: usize,
) -> Result<TemporaryGenericSupport, String> {
    unsafe { attach_impl(engine, user, pipe, base, false) }
}

unsafe fn attach_impl(
    engine: *mut c_void,
    user: i32,
    pipe: i32,
    base: usize,
    diagnostic: bool,
) -> Result<TemporaryGenericSupport, String> {
    if !cfg!(target_arch = "aarch64") || crate::AUDITED_RUNTIME != AUDITED_SHA256 {
        return Err("Generic input support requires the audited ARM64 runtime".into());
    }
    if engine.is_null() || base == 0 || user == 0 || pipe == 0 {
        return Err("Generic input support requires a live owned user/pipe".into());
    }
    unsafe {
        // Offsets are from the pinned engine and named IPC proxy stubs, not the
        // incompatible generated C# serialized-interface ordering.
        let get_config: GetInterface =
            std::mem::transmute(checked_method(engine, 44, base, 0xa03f10)?);
        let get_controller: GetPipeInterface =
            std::mem::transmute(checked_method(engine, 66, base, 0xa05514)?);
        let config = get_config(engine, user, pipe);
        let controller = get_controller(engine, pipe);
        let api = Api {
            config,
            controller,
            is_set: std::mem::transmute::<*const c_void, IsSet>(checked_method(
                config, 0, base, 0x90502c,
            )?),
            get: std::mem::transmute::<*const c_void, GetBool>(checked_method(
                config, 1, base, 0x9051c8,
            )?),
            set: std::mem::transmute::<*const c_void, SetBool>(checked_method(
                config, 9, base, 0x905fcc,
            )?),
            remove: std::mem::transmute::<*const c_void, RemoveKey>(checked_method(
                config, 16, base, 0x906bf0,
            )?),
            reset: std::mem::transmute::<*const c_void, ResetCache>(checked_method(
                controller, 131, base, 0x94db64,
            )?),
        };
        TemporaryGenericSupport::enable(api, diagnostic)
    }
}

unsafe fn checked_method(
    object: *mut c_void,
    slot: usize,
    base: usize,
    expected_offset: usize,
) -> Result<*const c_void, String> {
    if object.is_null() {
        return Err("Required input configuration interface unavailable".into());
    }
    let method = unsafe { *(*(object as *const *const *const c_void)).add(slot) };
    if (method as usize).checked_sub(base) != Some(expected_offset) {
        return Err(format!(
            "Input configuration ABI differs at interface slot {slot}"
        ));
    }
    Ok(method)
}

impl TemporaryGenericSupport {
    unsafe fn enable(api: Api, diagnostic: bool) -> Result<Self, String> {
        let original_present = unsafe { (api.is_set)(api.config, USER_LOCAL, KEY.as_ptr()) };
        let original_value = unsafe { (api.get)(api.config, USER_LOCAL, KEY.as_ptr(), false) };
        let mut guard = Self {
            api,
            original_present,
            original_value,
            restore_pending: false,
            diagnostic,
        };
        if original_value {
            guard.emit("already_enabled", true, true, original_present, false);
            return Ok(guard);
        }
        // Even a failed setter can mutate state before reporting failure, so
        // arm rollback before the call rather than after it.
        guard.restore_pending = true;
        let accepted = unsafe { (api.set)(api.config, USER_LOCAL, KEY.as_ptr(), true) };
        let actual = unsafe { (api.get)(api.config, USER_LOCAL, KEY.as_ptr(), false) };
        let present = unsafe { (api.is_set)(api.config, USER_LOCAL, KEY.as_ptr()) };
        let ok = accepted && actual && present;
        if ok {
            unsafe { (api.reset)(api.controller) };
        }
        guard.emit("enabled", ok, actual, present, ok);
        if !ok {
            return Err("Genuine ConfigStore did not confirm generic gamepad opt-in".into());
        }
        Ok(guard)
    }

    /// Explicit restoration lets the worker report failure before releasing
    /// the runtime. Drop is a fallback for all ordinary error-return paths.
    pub fn restore(&mut self) -> Result<(), String> {
        if !self.restore_pending {
            return Ok(());
        }
        let api = self.api;
        let accepted = unsafe {
            if self.original_present {
                (api.set)(api.config, USER_LOCAL, KEY.as_ptr(), self.original_value)
            } else {
                (api.remove)(api.config, USER_LOCAL, KEY.as_ptr())
            }
        };
        let actual = unsafe { (api.get)(api.config, USER_LOCAL, KEY.as_ptr(), false) };
        let present = unsafe { (api.is_set)(api.config, USER_LOCAL, KEY.as_ptr()) };
        let ok = accepted && actual == self.original_value && present == self.original_present;
        if ok {
            self.restore_pending = false;
            unsafe { (api.reset)(api.controller) };
        }
        self.emit("restored", ok, actual, present, ok);
        if ok {
            Ok(())
        } else {
            Err("Could not restore original generic gamepad setting".into())
        }
    }

    fn emit(&self, stage: &str, ok: bool, actual: bool, present: bool, cache_reset_called: bool) {
        // All interpolated values are internal labels or booleans. No account,
        // AppID, token, environment, arbitrary key or config payload is logged.
        let _ = writeln!(
            std::io::stdout(),
            "SKINOS_EVENT {{\"event\":\"steam_input_generic_support\",\"ok\":{ok},\"detail\":{{\"stage\":\"{stage}\",\"diagnostic\":{},\"original_present\":{},\"original_value\":{},\"actual_present\":{present},\"actual_value\":{actual},\"cache_reset_called\":{cache_reset_called}}}}}",
            self.diagnostic,
            self.original_present,
            self.original_value
        );
        let _ = std::io::stdout().flush();
    }
}

impl Drop for TemporaryGenericSupport {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Store {
        present: bool,
        value: bool,
        writes: usize,
        removals: usize,
        resets: usize,
        fail_set: bool,
    }
    unsafe fn store<'a>(p: *mut c_void) -> &'a mut Store {
        unsafe { &mut *p.cast::<Store>() }
    }
    unsafe extern "C" fn is_set(p: *mut c_void, s: u32, _: *const c_char) -> bool {
        assert_eq!(s, 3);
        unsafe { store(p).present }
    }
    unsafe extern "C" fn get(p: *mut c_void, _: u32, _: *const c_char, default: bool) -> bool {
        let s = unsafe { store(p) };
        if s.present { s.value } else { default }
    }
    unsafe extern "C" fn set(p: *mut c_void, _: u32, _: *const c_char, value: bool) -> bool {
        let s = unsafe { store(p) };
        s.present = true;
        s.value = value;
        s.writes += 1;
        !s.fail_set
    }
    unsafe extern "C" fn remove(p: *mut c_void, _: u32, _: *const c_char) -> bool {
        let s = unsafe { store(p) };
        s.present = false;
        s.removals += 1;
        true
    }
    unsafe extern "C" fn reset(p: *mut c_void) {
        unsafe { store(p) }.resets += 1;
    }
    fn api(s: &mut Store) -> Api {
        let p = std::ptr::from_mut(s).cast();
        Api {
            config: p,
            controller: p,
            is_set,
            get,
            set,
            remove,
            reset,
        }
    }

    #[test]
    fn absent_key_is_removed_on_drop() {
        let mut s = Store::default();
        let g = unsafe { TemporaryGenericSupport::enable(api(&mut s), true) }.unwrap();
        assert!(s.present && s.value);
        drop(g);
        assert!(!s.present);
        assert_eq!(s.removals, 1);
        assert_eq!(s.resets, 2);
    }
    #[test]
    fn explicit_false_restores_as_present_false() {
        let mut s = Store {
            present: true,
            ..Store::default()
        };
        let mut g = unsafe { TemporaryGenericSupport::enable(api(&mut s), true) }.unwrap();
        g.restore().unwrap();
        drop(g);
        assert!(s.present && !s.value);
        assert_eq!(s.writes, 2);
        assert_eq!(s.removals, 0);
    }
    #[test]
    fn already_true_does_not_mutate_or_reset() {
        let mut s = Store {
            present: true,
            value: true,
            ..Store::default()
        };
        drop(unsafe { TemporaryGenericSupport::enable(api(&mut s), true) }.unwrap());
        assert!(s.value);
        assert_eq!((s.writes, s.removals, s.resets), (0, 0, 0));
    }
    #[test]
    fn failed_setter_that_mutated_still_rolls_back() {
        let mut s = Store {
            fail_set: true,
            ..Store::default()
        };
        assert!(unsafe { TemporaryGenericSupport::enable(api(&mut s), true) }.is_err());
        assert!(!s.present);
        assert_eq!(s.removals, 1);
    }
    #[test]
    fn unexpected_method_fails_before_invocation() {
        let table = [0x100usize as *const c_void];
        let object = table.as_ptr();
        let p = std::ptr::from_ref(&object).cast_mut().cast();
        assert!(unsafe { checked_method(p, 0, 0x80, 0x90) }.is_err());
    }
    #[test]
    fn ordinary_session_restores_absent_setting_and_keeps_its_purpose() {
        let mut s = Store::default();
        let mut guard = unsafe { TemporaryGenericSupport::enable(api(&mut s), false) }.unwrap();
        assert!(!guard.diagnostic);
        assert!(s.present && s.value);
        guard.restore().unwrap();
        drop(guard);
        assert!(!s.present);
        assert!(!unsafe {
            get(
                std::ptr::from_mut(&mut s).cast(),
                USER_LOCAL,
                KEY.as_ptr(),
                false,
            )
        });
        assert_eq!((s.writes, s.removals, s.resets), (1, 1, 2));
    }
    #[test]
    fn ordinary_session_rejects_missing_runtime_before_any_interface_access() {
        assert!(unsafe { attach_for_session(std::ptr::null_mut(), 0, 0, 0) }.is_err());
    }
}
