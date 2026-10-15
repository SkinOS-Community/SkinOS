//! Read-only controller readiness for the labelled neutral software fixture.
//! Source of ABI: pinned ARM64 IClientControllerSerializedMap vtable 0x1747b60,
//! named IPC stubs GetNumConnectedControllers 0x942e14 and
//! GetGamepadIndexForControllerIndex 0x944b14. Both return four bytes; the latter
//! serializes one four-byte controller-index argument. No configuration writes.
use std::ffi::c_void;

const AUDITED_SHA256: &str = "035722a42c74989fa38c7efe0d735fdb38444e1102c92246652b5d913d019840";
type GetController = unsafe extern "C" fn(*mut c_void, i32) -> *mut c_void;
type GetCount = unsafe extern "C" fn(*mut c_void) -> u32;
type GetGamepadIndex = unsafe extern "C" fn(*mut c_void, u32) -> i32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapshot {
    pub connected_controllers: u32,
    pub controller_zero_gamepad_index: Option<i32>,
}

impl Snapshot {
    /// Count and slot alone cannot identify a device. `current_run_fixture_zero`
    /// must come from a fresh, bounded log segment for this owned host run,
    /// showing the exact neutral fixture name opened as Valve index 0. A match
    /// in an older persistent controller log is insufficient.
    pub fn verified_fixture_index(self, current_run_fixture_zero: bool) -> Option<u32> {
        (current_run_fixture_zero
            && self.connected_controllers == 1
            // -1 is expected before a configuration assigns an XInput slot.
            // Requiring the output route here would make initialization circular.
            // The fresh log identifies Valve index 0; no slot is invented.
            && matches!(self.controller_zero_gamepad_index, Some(-1 | 0)))
        .then_some(0)
    }
}

/// Read the actual Valve controller count and, only when exactly one controller
/// exists, its index-0 gamepad slot. This does not claim SDK routing is working.
///
/// # Safety
/// `engine`/`base` must be the live ARM64 image accepted by the caller's existing
/// AUDITED_RUNTIME SHA256 gate, with a valid pipe. Every method address is also
/// checked before invocation. Do not retain pointers beyond this call.
pub unsafe fn read(engine: *mut c_void, pipe: i32, base: usize) -> Result<Snapshot, String> {
    let actual = unsafe { read_index(engine, pipe, base, 0, true)? };
    Ok(Snapshot {
        connected_controllers: actual.connected_controllers,
        controller_zero_gamepad_index: (actual.connected_controllers == 1)
            .then_some(actual.gamepad_index),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexedSnapshot {
    pub connected_controllers: u32,
    pub gamepad_index: i32,
}

/// Query the fresh log-correlated Valve index, which is independent of the
/// XInput gamepad slot. Same audited ABI and lifetime preconditions as `read`.
///
/// # Safety
/// The live audited engine and pipe must satisfy `read`'s requirements.
pub unsafe fn read_for_index(
    engine: *mut c_void,
    pipe: i32,
    base: usize,
    controller_index: u32,
) -> Result<IndexedSnapshot, String> {
    unsafe { read_index(engine, pipe, base, controller_index, false) }
}

unsafe fn read_index(
    engine: *mut c_void,
    pipe: i32,
    base: usize,
    controller_index: u32,
    only_single: bool,
) -> Result<IndexedSnapshot, String> {
    if controller_index >= 16 {
        return Err("Input readiness controller index is outside the audited range".into());
    }
    if !cfg!(target_arch = "aarch64")
        || crate::AUDITED_RUNTIME != AUDITED_SHA256
        || engine.is_null()
        || base == 0
        || pipe == 0
    {
        return Err("Input readiness requires the audited owned runtime and live pipe".into());
    }
    unsafe {
        let get: GetController = std::mem::transmute(checked_method(engine, 66, base, 0xa05514)?);
        let controller = get(engine, pipe);
        let get_count: GetCount =
            std::mem::transmute(checked_method(controller, 23, base, 0x942e14)?);
        let get_gamepad_index: GetGamepadIndex =
            std::mem::transmute(checked_method(controller, 41, base, 0x944b14)?);
        let connected_controllers = get_count(controller);
        let gamepad_index = if only_single && connected_controllers != 1 {
            -1 // Legacy `read` omits this field and makes no index query.
        } else {
            get_gamepad_index(controller, controller_index)
        };
        Ok(IndexedSnapshot {
            connected_controllers,
            gamepad_index,
        })
    }
}

unsafe fn checked_method(
    object: *mut c_void,
    slot: usize,
    base: usize,
    offset: usize,
) -> Result<*const c_void, String> {
    if object.is_null() {
        return Err("Input readiness interface unavailable".into());
    }
    let pointer = unsafe { *(*(object as *const *const *const c_void)).add(slot) };
    if (pointer as usize).checked_sub(base) != Some(offset) {
        return Err(format!("Input readiness ABI mismatch at slot {slot}"));
    }
    Ok(pointer)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observed_count_and_slot_alone_do_not_identify_fixture() {
        let actual = Snapshot {
            connected_controllers: 1,
            controller_zero_gamepad_index: Some(0),
        };
        assert_eq!(actual.verified_fixture_index(false), None);
        assert_eq!(actual.verified_fixture_index(true), Some(0));
    }
    #[test]
    fn ambiguous_or_unmapped_controller_refuses_binding_target() {
        for (count, slot) in [(0, None), (2, None), (1, Some(1)), (1, None)] {
            assert_eq!(
                Snapshot {
                    connected_controllers: count,
                    controller_zero_gamepad_index: slot,
                }
                .verified_fixture_index(true),
                None
            );
        }
    }
    #[test]
    fn unmapped_fixture_can_receive_its_first_configuration() {
        let actual = Snapshot {
            connected_controllers: 1,
            controller_zero_gamepad_index: Some(-1),
        };
        assert_eq!(actual.verified_fixture_index(false), None);
        assert_eq!(actual.verified_fixture_index(true), Some(0));
        assert_eq!(actual.controller_zero_gamepad_index, Some(-1));
    }
}
