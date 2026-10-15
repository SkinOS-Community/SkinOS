//! Explicit stock-layout attachment. No persistent selection is written and no
//! controller index or returned handle is invented. This module dispatches the
//! pinned UI's genuine Warm -> Load VDF -> Activate sequence; downstream Steam
//! Input state must independently prove whether it became usable.
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::ffi::{CString, c_char, c_void};
use std::io::Write;
use std::path::Path;

const ENGINE_SHA: &str = "035722a42c74989fa38c7efe0d735fdb38444e1102c92246652b5d913d019840";
const TEMPLATE: &str = "controller_base/templates/controller_generic_gamepad_joystick.vdf";
const TEMPLATE_SHA: &str = "57d41acf506a8a492852d465a4531d9068288538e4736dea92a5f80ed9e523fa";
// NOTE: value is the stock template enum, not a Workshop published file ID.
// steamui 0x488364..0x48839c derives it from the filename via table 0xf06b30.
const GAMEPAD_JOYSTICK_CONFIG_ID: u64 = 4;

#[repr(C)]
struct StockSettings28 {
    use_steam_controller_config: u32,
    opaque_flag_4: u8,
    padding_5: [u8; 3],
    rumble: i32,
    rumble_intensity: u16,
    padding_14: [u8; 2],
    opaque_value_16: i32,
    opaque_flag_20: u8,
    padding_21: [u8; 3],
    steam_deck_touchscreen: u32,
}

impl StockSettings28 {
    fn upstream_fallback() -> Self {
        // Exact field values emitted by steamui's stock load caller at
        // 0x488a84..0x488aa8 and its app-model constructor 0x55ddd4..0x55ddf4.
        // These are the upstream fallback policy, not discovered per-app user
        // overrides. Padding is zeroed; no field semantics are invented.
        Self {
            use_steam_controller_config: 1,
            opaque_flag_4: 0,
            padding_5: [0; 3],
            rumble: -1,
            rumble_intensity: 320,
            padding_14: [0; 2],
            opaque_value_16: -1,
            opaque_flag_20: 0,
            padding_21: [0; 3],
            steam_deck_touchscreen: 1,
        }
    }
}

type GetController = unsafe extern "C" fn(*mut c_void, i32) -> *mut c_void;
type Warm = unsafe extern "C" fn(*mut c_void, u32, u32);
type Load = unsafe extern "C" fn(
    *mut c_void,
    u32,
    u32,
    *const c_char,
    *const c_void,
    u64,
    *const StockSettings28,
);
type Activate = unsafe extern "C" fn(*mut c_void, u32, u32, bool);

#[derive(Debug, Serialize)]
pub struct ConfigProof {
    pub appid: u32,
    pub controller_index: u32,
    pub controller_index_verified_by_caller: bool,
    pub fixture: Option<&'static str>,
    pub controller_identity: Option<ControllerIdentity>,
    pub template: &'static str,
    pub template_sha256: &'static str,
    pub config_id: u64,
    pub metadata_policy: &'static str,
    pub calls_dispatched: bool,
    pub binding_loaded_verified: bool,
    pub input_delivery_verified: bool,
}

/// Numeric identity only. Device names and HID paths can contain personal
/// identifiers, so neither those nor serial-number fields enter this proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ControllerIdentity {
    pub sdl_id: u32,
    pub vendor_id: u16,
    pub product_id: u16,
    pub gamepad_type: i32,
    pub is_virtual: bool,
}

struct Target {
    index: u32,
    fixture: Option<&'static str>,
    controller_identity: Option<ControllerIdentity>,
}

/// Opt-in diagnostic only; an absent flag performs no filesystem or Valve work.
/// `verified_controller_index` must come from the owned host's actual controller
/// details/readiness check for the neutral SDL fixture, never an assumed zero.
/// A void call's completion is reported only as dispatch, not successful input.
///
/// # Safety
/// The caller has passed the exact live ARM64 engine SHA256 gate. `engine`,
/// `pipe`, and `base` belong to that live image, and AppID ownership is already
/// verified. Calls must run on the host's controller/engine thread before teardown.
pub unsafe fn attach(
    engine: *mut c_void,
    pipe: i32,
    base: usize,
    state: &Path,
    appid: u32,
    verified_controller_index: Option<u32>,
) -> Result<Option<ConfigProof>, String> {
    if std::env::var("SKINOS_INPUT_CONFIG").ok().as_deref() != Some("1") {
        return Ok(None);
    }
    if std::env::var("SKINOS_STEAM_INPUT_FIXTURE").ok().as_deref() != Some("neutral") {
        return Err(
            "Stock input configuration requires the explicit neutral software fixture".into(),
        );
    }
    let index = validated_index(verified_controller_index)?;
    unsafe {
        dispatch(
            engine,
            pipe,
            base,
            state,
            appid,
            Target {
                index,
                fixture: Some("neutral_software"),
                controller_identity: None,
            },
        )
        .map(Some)
    }
}

/// Attach the same audited generic stock template to a freshly identified
/// live gamepad. No diagnostic environment or fixture identity is used.
///
/// # Safety
/// Engine, pipe, base and AppID obey `attach`'s original requirements. `device`
/// must come from a fresh owned SDL inventory and `mapping` from the matching
/// current-run discovery log. Re-enumerate before dispatch; a cached SDL ID or
/// an assumed Valve index cannot authorize configuration changes.
pub unsafe fn attach_for_device(
    engine: *mut c_void,
    pipe: i32,
    base: usize,
    state: &Path,
    appid: u32,
    device: &crate::input_devices::Device,
    mapping: &crate::input_device_log::Mapping,
) -> Result<ConfigProof, String> {
    let target = device_target(device, mapping)?;
    unsafe { dispatch(engine, pipe, base, state, appid, target) }
}

fn device_target(
    device: &crate::input_devices::Device,
    mapping: &crate::input_device_log::Mapping,
) -> Result<Target, String> {
    if !device.is_gamepad
        || device.sdl_id == 0
        || device.sdl_id != mapping.sdl_id
        || mapping.valve_index >= 16
    {
        return Err(
            "Stock input configuration requires a live matching gamepad and verified Valve index"
                .into(),
        );
    }
    Ok(Target {
        index: mapping.valve_index,
        fixture: None,
        controller_identity: Some(ControllerIdentity {
            sdl_id: device.sdl_id,
            vendor_id: device.vendor_id,
            product_id: device.product_id,
            gamepad_type: device.gamepad_type,
            is_virtual: device.is_virtual,
        }),
    })
}

unsafe fn dispatch(
    engine: *mut c_void,
    pipe: i32,
    base: usize,
    state: &Path,
    appid: u32,
    target: Target,
) -> Result<ConfigProof, String> {
    let index = target.index;
    if !cfg!(target_arch = "aarch64")
        || crate::AUDITED_RUNTIME != ENGINE_SHA
        || engine.is_null()
        || pipe == 0
        || base == 0
        || appid == 0
        || appid > 0x00ff_ffff
        || std::mem::size_of::<StockSettings28>() != 28
    {
        return Err(
            "Stock input configuration requires the audited ARM64 engine and owned AppID".into(),
        );
    }
    let mut path = state.join("runtime");
    for component in TEMPLATE.split('/') {
        path.push(component);
        let metadata = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err("Stock input template must stay inside the staged runtime".into());
        }
    }
    let metadata = std::fs::metadata(&path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > 512 * 1024 {
        return Err("Stock input template must be a bounded regular file".into());
    }
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    if format!("{:x}", Sha256::digest(&bytes)) != TEMPLATE_SHA {
        return Err("Stock input template checksum does not match the audited template".into());
    }
    let vdf = CString::new(bytes).map_err(|_| "Stock input template contains a NUL byte")?;
    let settings = StockSettings28::upstream_fallback();
    unsafe {
        let get_controller: GetController =
            std::mem::transmute(checked_method(engine, 66, base, 0xa05514)?);
        let controller = get_controller(engine, pipe);
        // Check all addresses before any mutation. These offsets match named
        // proxy dispatch, host thunk and the original stock-template call site.
        let warm: Warm = std::mem::transmute(checked_method(controller, 40, base, 0x944994)?);
        let load: Load = std::mem::transmute(checked_method(controller, 36, base, 0x944284)?);
        let activate: Activate =
            std::mem::transmute(checked_method(controller, 39, base, 0x9447fc)?);
        warm(controller, index, appid);
        load(
            controller,
            index,
            appid,
            vdf.as_ptr(),
            std::ptr::null(),
            GAMEPAD_JOYSTICK_CONFIG_ID,
            &settings,
        );
        activate(controller, index, appid, false);
    }
    let proof = ConfigProof {
        appid,
        controller_index: index,
        controller_index_verified_by_caller: true,
        fixture: target.fixture,
        controller_identity: target.controller_identity,
        template: TEMPLATE,
        template_sha256: TEMPLATE_SHA,
        config_id: GAMEPAD_JOYSTICK_CONFIG_ID,
        metadata_policy: "observed_upstream_fallback_28_bytes",
        calls_dispatched: true,
        binding_loaded_verified: false,
        input_delivery_verified: false,
    };
    let detail = serde_json::to_value(&proof).map_err(|e| e.to_string())?;
    let event = serde_json::json!({"event":"steam_input_config", "detail":{"stage":"calls_dispatched","proof":detail}});
    let _ = writeln!(std::io::stdout(), "SKINOS_EVENT {event}");
    let _ = std::io::stdout().flush();
    Ok(proof)
}

fn validated_index(value: Option<u32>) -> Result<u32, String> {
    value.filter(|index| *index < 16).ok_or_else(|| {
        "No verified host controller index for the neutral fixture; configuration calls skipped"
            .into()
    })
}

unsafe fn checked_method(
    object: *mut c_void,
    slot: usize,
    base: usize,
    offset: usize,
) -> Result<*const c_void, String> {
    if object.is_null() {
        return Err("Controller configuration interface unavailable".into());
    }
    let pointer = unsafe { *(*(object as *const *const *const c_void)).add(slot) };
    if (pointer as usize).checked_sub(base) != Some(offset) {
        return Err(format!(
            "Controller configuration ABI mismatch at slot {slot}"
        ));
    }
    Ok(pointer)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_observed_metadata_has_correct_layout_and_no_guessed_zero_defaults() {
        assert_eq!(std::mem::size_of::<StockSettings28>(), 28);
        let settings = StockSettings28::upstream_fallback();
        let bytes = unsafe {
            std::slice::from_raw_parts((&settings as *const StockSettings28).cast::<u8>(), 28)
        };
        assert_eq!(
            bytes,
            &[
                1, 0, 0, 0, 0, 0, 0, 0, 255, 255, 255, 255, 64, 1, 0, 0, 255, 255, 255, 255, 0, 0,
                0, 0, 1, 0, 0, 0
            ]
        );
    }
    #[test]
    fn missing_or_out_of_range_index_never_becomes_assumed_controller_zero() {
        assert!(validated_index(None).is_err());
        assert!(validated_index(Some(16)).is_err());
        assert!(validated_index(Some(u32::MAX)).is_err());
        assert_eq!(validated_index(Some(0)).unwrap(), 0);
    }
    fn device() -> crate::input_devices::Device {
        crate::input_devices::Device {
            sdl_id: 42,
            name: "private-name-or-serial-must-not-be-logged".into(),
            vendor_id: 0x045e,
            product_id: 0x028e,
            gamepad_type: 2,
            is_gamepad: true,
            is_virtual: false,
        }
    }
    #[test]
    fn real_device_target_uses_verified_nonzero_index_and_no_fixture_label_or_name() {
        let device = device();
        for index in [3, 7, 15] {
            let mapping = crate::input_device_log::Mapping {
                sdl_id: 42,
                valve_index: index,
            };
            let target = device_target(&device, &mapping).unwrap();
            assert_eq!(target.index, index);
            assert!(target.fixture.is_none());
            let identity = serde_json::to_value(target.controller_identity.unwrap()).unwrap();
            assert_eq!(identity["sdl_id"], 42);
            assert_eq!(identity["vendor_id"], 0x045e);
            assert_eq!(identity["product_id"], 0x028e);
            assert_eq!(identity["is_virtual"], false);
            assert_eq!(identity.as_object().unwrap().len(), 5);
            assert!(!identity.to_string().contains("private-name"));
        }
    }
    #[test]
    fn mismatched_absent_or_nongamepad_device_never_reaches_configuration_calls() {
        let device = device();
        for mapping in [
            crate::input_device_log::Mapping {
                sdl_id: 43,
                valve_index: 0,
            },
            crate::input_device_log::Mapping {
                sdl_id: 42,
                valve_index: 16,
            },
        ] {
            assert!(device_target(&device, &mapping).is_err());
        }
        let mapping = crate::input_device_log::Mapping {
            sdl_id: 42,
            valve_index: 7,
        };
        let mut not_gamepad = device.clone();
        not_gamepad.is_gamepad = false;
        assert!(device_target(&not_gamepad, &mapping).is_err());
        let mut absent = device.clone();
        absent.sdl_id = 0;
        assert!(
            device_target(
                &absent,
                &crate::input_device_log::Mapping {
                    sdl_id: 0,
                    valve_index: 7
                }
            )
            .is_err()
        );
        // Even a valid identity cannot access an absent or unaudited engine.
        assert!(
            unsafe {
                attach_for_device(
                    std::ptr::null_mut(),
                    0,
                    0,
                    Path::new("/nonexistent"),
                    648800,
                    &device,
                    &mapping,
                )
            }
            .is_err()
        );
    }
}
