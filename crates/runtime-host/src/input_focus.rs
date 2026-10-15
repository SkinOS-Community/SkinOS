//! Scoped runtime focus state; no GUI/OS focus operation and no input
//! injection. Not wired by this module. Requires the existing engine SHA gate.
//! Restore/drop before the parent user/pipe teardown. Child SDK exit is safe:
//! this guard retains only the parent runtime's own utils interface.
use std::ffi::c_void;
use std::io::Write;

const AUDITED_SHA256: &str = "035722a42c74989fa38c7efe0d735fdb38444e1102c92246652b5d913d019840";
// Pinned CSteamEngine constructor 0xa081e0 initializes its focus field
// this+0x1ba1 to 0x301 at 0xa08698, and PID this+0x1bac to zero.
// GetFocusedGameID reads exactly that field (utils=this+0xde8, +0xdb9).
// This is an observed runtime constructor baseline, not an arbitrary app ID.
const CONSTRUCTOR_FOCUS_GAMEID: u64 = 769;
type GetUtils = unsafe extern "C" fn(*mut c_void, i32) -> *mut c_void;
type ReadGame = unsafe extern "C" fn(*mut c_void, *const c_void, *mut u64) -> bool;
type GetPid = unsafe extern "C" fn(*mut c_void) -> u32;
type SetFocus = unsafe extern "C" fn(*mut c_void, *const u64, bool, bool, u32, u16, u16);

unsafe extern "C" {
    fn skinos_input_read_focus_gameid(
        utils: *mut c_void,
        getter: *const c_void,
        output: *mut u64,
    ) -> bool;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Snapshot {
    pub gameid: u64,
    pub pid: u32,
}

#[derive(Clone, Copy)]
struct Api {
    utils: *mut c_void,
    game_getter: *const c_void,
    read_game: ReadGame,
    get_pid: GetPid,
    set_focus: SetFocus,
}

pub struct FocusGuard {
    api: Api,
    target: u32,
    original: Snapshot,
    restore_pending: bool,
    diagnostic: bool,
}

/// `SKINOS_INPUT_FOCUS=1` enables the experiment; otherwise no Valve call occurs.
/// The caller must already have verified ownership of the requested AppID.
/// Diagnostic consumers separately verify their real SDK AppID before scanning.
/// PID 0 exactly follows the observed forced UI call; no
/// unrelated process is labelled as the game's foreground process.
///
/// # Safety
/// The engine/base must be the live ARM64 image accepted by AUDITED_RUNTIME's
/// exact checksum gate. The pipe must belong to it. Restore/drop the guard
/// before parent pipe release or dylib unload.
pub unsafe fn attach(
    engine: *mut c_void,
    pipe: i32,
    base: usize,
    appid: u32,
) -> Result<Option<FocusGuard>, String> {
    if std::env::var("SKINOS_INPUT_FOCUS").ok().as_deref() != Some("1") {
        return Ok(None);
    }
    unsafe { attach_impl(engine, pipe, base, appid, true).map(Some) }
}

/// Focus the verified AppID in this explicitly selected owned input session.
/// No environment or diagnostic-fixture flag is consulted; the exact original
/// baseline and read-back/restoration checks remain required.
///
/// # Safety
/// Engine, base and pipe must satisfy `attach`'s audited-image requirements.
/// The caller has verified AppID ownership and drops/restores before teardown.
pub unsafe fn attach_for_session(
    engine: *mut c_void,
    pipe: i32,
    base: usize,
    appid: u32,
) -> Result<FocusGuard, String> {
    unsafe { attach_impl(engine, pipe, base, appid, false) }
}

unsafe fn attach_impl(
    engine: *mut c_void,
    pipe: i32,
    base: usize,
    appid: u32,
    diagnostic: bool,
) -> Result<FocusGuard, String> {
    if !cfg!(target_arch = "aarch64")
        || crate::AUDITED_RUNTIME != AUDITED_SHA256
        || engine.is_null()
        || base == 0
        || pipe == 0
        || appid == 0
        || appid > 0x00ff_ffff
    {
        return Err("Input focus requires the audited runtime and valid native AppID".into());
    }
    unsafe {
        let get_utils: GetUtils = std::mem::transmute(checked_method(engine, 14, base, 0xa04d74)?);
        let utils = get_utils(engine, pipe);
        let api = Api {
            utils,
            game_getter: checked_method(utils, 103, base, 0x8d4d1c)?,
            read_game: skinos_input_read_focus_gameid,
            get_pid: std::mem::transmute::<*const c_void, GetPid>(checked_method(
                utils, 104, base, 0x8d4e8c,
            )?),
            set_focus: std::mem::transmute::<*const c_void, SetFocus>(checked_method(
                utils, 48, base, 0x8cf530,
            )?),
        };
        FocusGuard::begin(api, appid, diagnostic)
    }
}

unsafe fn checked_method(
    object: *mut c_void,
    slot: usize,
    base: usize,
    offset: usize,
) -> Result<*const c_void, String> {
    if object.is_null() {
        return Err("Input focus interface unavailable".into());
    }
    let pointer = unsafe { *(*(object as *const *const *const c_void)).add(slot) };
    if (pointer as usize).checked_sub(base) != Some(offset) {
        return Err(format!("Input focus ABI mismatch at slot {slot}"));
    }
    Ok(pointer)
}

impl Api {
    fn snapshot(self) -> Result<Snapshot, String> {
        let mut gameid = 0;
        if !unsafe { (self.read_game)(self.utils, self.game_getter, &mut gameid) } {
            return Err("C++ focus getter adapter failed".into());
        }
        Ok(Snapshot {
            gameid,
            pid: unsafe { (self.get_pid)(self.utils) },
        })
    }
    fn set(self, gameid: u64) {
        // bool1 forces notification even if gameid is unchanged. bool2 is the
        // notification flag; true matches both observed forced UI callsites.
        // Empty masks match the audited call; they do not imply default bindings.
        unsafe { (self.set_focus)(self.utils, &gameid, true, true, 0, 0, 0) };
    }
}

impl FocusGuard {
    fn begin(api: Api, target: u32, diagnostic: bool) -> Result<Self, String> {
        let before = api.snapshot()?;
        emit("before", true, target, before, diagnostic);
        if before.pid != 0 || !matches!(before.gameid, 0 | CONSTRUCTOR_FOCUS_GAMEID) {
            return Err(
                "Input focus requires the audited owned runtime baseline (gameid 0 or 769, PID 0)"
                    .into(),
            );
        }
        let guard = Self {
            api,
            target,
            original: before,
            restore_pending: true,
            diagnostic,
        };
        api.set(u64::from(target));
        let actual = api.snapshot()?;
        let matched = actual
            == (Snapshot {
                gameid: u64::from(target),
                pid: 0,
            });
        emit("applied", matched, target, actual, diagnostic);
        if !matched {
            return Err("Genuine runtime did not confirm requested focus gameid/PID".into());
        }
        Ok(guard)
    }
    pub fn snapshot(&self) -> Result<Snapshot, String> {
        self.api.snapshot()
    }
    pub fn restore(&mut self) -> Result<(), String> {
        if !self.restore_pending {
            return Ok(());
        }
        self.api.set(self.original.gameid);
        let actual = self.api.snapshot()?;
        let matched = actual == self.original;
        emit("restored", matched, self.target, actual, self.diagnostic);
        if !matched {
            return Err("Genuine runtime did not restore original focus gameid/PID".into());
        }
        self.restore_pending = false;
        Ok(())
    }
}

impl Drop for FocusGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

fn emit(stage: &str, ok: bool, target: u32, actual: Snapshot, diagnostic: bool) {
    let _ = writeln!(
        std::io::stdout(),
        "SKINOS_EVENT {{\"event\":\"steam_input_focus\",\"ok\":{ok},\"detail\":{{\"stage\":\"{stage}\",\"diagnostic\":{diagnostic},\"requested_appid\":{target},\"actual_gameid\":{},\"actual_pid\":{},\"force_update\":true,\"callback_flag\":true,\"controller_mask_a\":0,\"controller_mask_b\":0,\"layout_activation_verified\":false}}}}",
        actual.gameid,
        actual.pid
    );
    let _ = std::io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct State {
        gameid: u64,
        pid: u32,
        calls: usize,
        reject: bool,
    }
    unsafe fn state<'a>(p: *mut c_void) -> &'a mut State {
        unsafe { &mut *p.cast::<State>() }
    }
    unsafe extern "C" fn read(p: *mut c_void, _: *const c_void, out: *mut u64) -> bool {
        unsafe { *out = state(p).gameid };
        true
    }
    unsafe extern "C" fn pid(p: *mut c_void) -> u32 {
        unsafe { state(p).pid }
    }
    unsafe extern "C" fn set(
        p: *mut c_void,
        g: *const u64,
        f: bool,
        c: bool,
        pid: u32,
        a: u16,
        b: u16,
    ) {
        assert!(f && c);
        assert_eq!((pid, a, b), (0, 0, 0));
        let s = unsafe { state(p) };
        s.calls += 1;
        if !s.reject {
            s.gameid = unsafe { *g };
            s.pid = pid;
        }
    }
    fn api(s: &mut State) -> Api {
        Api {
            utils: std::ptr::from_mut(s).cast(),
            game_getter: std::ptr::null(),
            read_game: read,
            get_pid: pid,
            set_focus: set,
        }
    }
    #[test]
    fn restores_idle_focus_on_drop() {
        let mut s = State::default();
        let g = FocusGuard::begin(api(&mut s), 648800, true).unwrap();
        assert_eq!(g.snapshot().unwrap().gameid, 648800);
        drop(g);
        assert_eq!(s.gameid, 0);
        assert_eq!(s.calls, 2);
    }
    #[test]
    fn restores_pinned_constructor_baseline_instead_of_zero() {
        let mut s = State {
            gameid: CONSTRUCTOR_FOCUS_GAMEID,
            ..State::default()
        };
        let mut g = FocusGuard::begin(api(&mut s), 648800, true).unwrap();
        assert_eq!(g.snapshot().unwrap().gameid, 648800);
        g.restore().unwrap();
        drop(g);
        assert_eq!(s.gameid, CONSTRUCTOR_FOCUS_GAMEID);
        assert_eq!(s.pid, 0);
        assert_eq!(s.calls, 2);
    }
    #[test]
    fn refuses_existing_game_before_any_write() {
        let mut s = State {
            gameid: 10,
            ..State::default()
        };
        assert!(FocusGuard::begin(api(&mut s), 648800, true).is_err());
        assert_eq!(s.calls, 0);
        s.gameid = 0;
        s.pid = 42;
        assert!(FocusGuard::begin(api(&mut s), 648800, true).is_err());
        assert_eq!(s.calls, 0);
        s.gameid = CONSTRUCTOR_FOCUS_GAMEID;
        assert!(FocusGuard::begin(api(&mut s), 648800, true).is_err());
        assert_eq!(s.calls, 0);
    }
    #[test]
    fn rejected_change_is_not_reported_as_applied() {
        let mut s = State {
            reject: true,
            ..State::default()
        };
        assert!(FocusGuard::begin(api(&mut s), 648800, true).is_err());
        assert_eq!(s.gameid, 0);
        assert_eq!(s.calls, 2);
    }
    #[test]
    fn ordinary_session_restores_same_owned_baseline_without_diagnostic_label() {
        let mut s = State {
            gameid: CONSTRUCTOR_FOCUS_GAMEID,
            ..State::default()
        };
        let mut guard = FocusGuard::begin(api(&mut s), 648800, false).unwrap();
        assert!(!guard.diagnostic);
        assert_eq!(
            guard.snapshot().unwrap(),
            Snapshot {
                gameid: 648800,
                pid: 0
            }
        );
        guard.restore().unwrap();
        drop(guard);
        assert_eq!(s.gameid, CONSTRUCTOR_FOCUS_GAMEID);
        assert_eq!(s.calls, 2);
    }
    #[test]
    fn ordinary_session_rejects_missing_runtime_before_any_interface_access() {
        assert!(unsafe { attach_for_session(std::ptr::null_mut(), 0, 0, 648800) }.is_err());
    }
}
