//! Read-only inventory of the already loaded, audited Valve SDL instance.
//! Public ABI: libsdl-org/SDL@70e9cc86d, SDL_joystick.h and SDL_gamepad.h.
//! Never initializes SDL, opens a joystick, pumps events or writes device state.
#![allow(dead_code)] // The ordinary-session caller is wired independently.

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

const SDL_SHA256: &str = "56fad2994d5c1c618862459d3aaa6e7c6048a83f1a4dc3e3ef543edf2272dfed";
const MAX_DEVICES: usize = 64;
const MAX_NAME_BYTES: usize = 512;

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Device {
    pub sdl_id: u32,
    pub name: String,
    pub vendor_id: u16,
    pub product_id: u16,
    /// SDL_GamepadType from the pinned public enum; not a Valve index.
    pub gamepad_type: i32,
    pub is_gamepad: bool,
    pub is_virtual: bool,
}

struct Library(*mut c_void);
impl Drop for Library {
    fn drop(&mut self) {
        unsafe {
            dlclose(self.0);
        }
    }
}

pub struct Inventory {
    _library: Library,
    api: Api,
}

struct Api {
    lock: unsafe extern "C" fn(),
    unlock: unsafe extern "C" fn(),
    joysticks: unsafe extern "C" fn(*mut c_int) -> *mut u32,
    free: unsafe extern "C" fn(*mut c_void),
    name: unsafe extern "C" fn(u32) -> *const c_char,
    vendor: unsafe extern "C" fn(u32) -> u16,
    product: unsafe extern "C" fn(u32) -> u16,
    kind: unsafe extern "C" fn(u32) -> c_int,
    gamepad: unsafe extern "C" fn(u32) -> bool,
    virtual_device: unsafe extern "C" fn(u32) -> bool,
    was_init: unsafe extern "C" fn(u32) -> u32,
}

impl Inventory {
    pub fn from_loaded(state: &Path) -> Result<Self, String> {
        let path = state.join("runtime/libSDL3.dylib");
        verify_file(&path)?;
        let name = CString::new(path.as_os_str().as_bytes()).map_err(|_| "Invalid SDL path")?;
        let handle = unsafe { dlopen(name.as_ptr(), 0x2 | 0x10) }; // RTLD_NOW | RTLD_NOLOAD
        if handle.is_null() {
            return Err("Valve has not loaded the owned SDL instance".into());
        }
        let library = Library(handle);
        unsafe {
            let version: unsafe extern "C" fn() -> c_int =
                std::mem::transmute(export(handle, c"SDL_GetVersion")?);
            let revision: unsafe extern "C" fn() -> *const c_char =
                std::mem::transmute(export(handle, c"SDL_GetRevision")?);
            let revision = revision();
            if version() != 3_005_000
                || revision.is_null()
                || CStr::from_ptr(revision).to_bytes() != b"SDL-release-3.4.0-1359-g70e9cc86d"
            {
                return Err("SDL inventory requires the audited 70e9cc86d build".into());
            }
            let api = Api {
                lock: std::mem::transmute::<*mut c_void, unsafe extern "C" fn()>(export(
                    handle,
                    c"SDL_LockJoysticks",
                )?),
                unlock: std::mem::transmute::<*mut c_void, unsafe extern "C" fn()>(export(
                    handle,
                    c"SDL_UnlockJoysticks",
                )?),
                joysticks: std::mem::transmute::<
                    *mut c_void,
                    unsafe extern "C" fn(*mut c_int) -> *mut u32,
                >(export(handle, c"SDL_GetJoysticks")?),
                free: std::mem::transmute::<*mut c_void, unsafe extern "C" fn(*mut c_void)>(
                    export(handle, c"SDL_free")?,
                ),
                name: std::mem::transmute::<*mut c_void, unsafe extern "C" fn(u32) -> *const c_char>(
                    export(handle, c"SDL_GetJoystickNameForID")?,
                ),
                vendor: std::mem::transmute::<*mut c_void, unsafe extern "C" fn(u32) -> u16>(
                    export(handle, c"SDL_GetJoystickVendorForID")?,
                ),
                product: std::mem::transmute::<*mut c_void, unsafe extern "C" fn(u32) -> u16>(
                    export(handle, c"SDL_GetJoystickProductForID")?,
                ),
                kind: std::mem::transmute::<*mut c_void, unsafe extern "C" fn(u32) -> c_int>(
                    export(handle, c"SDL_GetGamepadTypeForID")?,
                ),
                gamepad: std::mem::transmute::<*mut c_void, unsafe extern "C" fn(u32) -> bool>(
                    export(handle, c"SDL_IsGamepad")?,
                ),
                virtual_device: std::mem::transmute::<*mut c_void, unsafe extern "C" fn(u32) -> bool>(
                    export(handle, c"SDL_IsJoystickVirtual")?,
                ),
                was_init: std::mem::transmute::<*mut c_void, unsafe extern "C" fn(u32) -> u32>(
                    export(handle, c"SDL_WasInit")?,
                ),
            };
            // Hold no independent SDL subsystem ownership; Valve owns its life.
            if (api.was_init)(0x2000) & 0x2000 == 0 {
                return Err("Valve SDL gamepad subsystem is not initialized".into());
            }
            Ok(Self {
                _library: library,
                api,
            })
        }
    }

    /// Snapshot only. Caller must re-enumerate before using a log-derived index.
    pub fn snapshot(&self) -> Result<Vec<Device>, String> {
        unsafe { self.api.snapshot() }
    }
}

unsafe fn export(library: *mut c_void, name: &CStr) -> Result<*mut c_void, String> {
    let value = unsafe { dlsym(library, name.as_ptr()) };
    if value.is_null() {
        Err(format!(
            "SDL inventory export missing: {}",
            name.to_string_lossy()
        ))
    } else {
        Ok(value)
    }
}

fn verify_file(path: &Path) -> Result<(), String> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(0x100)
        .open(path)
        .map_err(|e| format!("Cannot inspect owned SDL: {e}"))?; // Darwin O_NOFOLLOW
    let before = file.metadata().map_err(|e| e.to_string())?;
    if !before.is_file() || before.len() != 4_560_800 {
        return Err("Unreviewed SDL image size".into());
    }
    let mut file = file.take(before.len() + 1);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 32 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let after = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !after.is_file()
        || before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || format!("{:x}", hasher.finalize()) != SDL_SHA256
    {
        return Err("Owned SDL identity/hash differs from the audited image".into());
    }
    Ok(())
}

impl Api {
    unsafe fn snapshot(&self) -> Result<Vec<Device>, String> {
        struct Unlock(unsafe extern "C" fn());
        impl Drop for Unlock {
            fn drop(&mut self) {
                unsafe {
                    (self.0)();
                }
            }
        }
        struct Allocation(*mut u32, unsafe extern "C" fn(*mut c_void));
        impl Drop for Allocation {
            fn drop(&mut self) {
                unsafe {
                    (self.1)(self.0.cast());
                }
            }
        }
        unsafe {
            (self.lock)();
            let _unlock = Unlock(self.unlock);
            if (self.was_init)(0x2000) & 0x2000 == 0 {
                return Err("Valve SDL subsystem ended".into());
            }
            let mut count = 0;
            let pointer = (self.joysticks)(&mut count);
            if pointer.is_null() {
                return Err("SDL could not enumerate connected joysticks".into());
            }
            let _allocation = Allocation(pointer, self.free);
            if count < 0 || count as usize > MAX_DEVICES {
                return Err("SDL inventory exceeds supported bound".into());
            }
            let ids = std::slice::from_raw_parts(pointer, count as usize + 1);
            validate_ids(ids)?;
            let mut devices = Vec::with_capacity(count as usize);
            for &id in &ids[..count as usize] {
                let name = (self.name)(id);
                if name.is_null() {
                    return Err("SDL device disappeared while reading inventory".into());
                }
                let bytes = CStr::from_ptr(name).to_bytes();
                if bytes.len() > MAX_NAME_BYTES {
                    return Err("SDL device name exceeds supported bound".into());
                }
                let name =
                    std::str::from_utf8(bytes).map_err(|_| "SDL device name is not UTF-8")?;
                if name.is_empty() || name.chars().any(char::is_control) {
                    return Err("SDL device name is not a printable identity".into());
                }
                let is_gamepad = (self.gamepad)(id);
                let gamepad_type = if is_gamepad { (self.kind)(id) } else { 0 };
                if !(0..13).contains(&gamepad_type) {
                    return Err("SDL returned an unaudited gamepad type".into());
                }
                devices.push(Device {
                    sdl_id: id,
                    name: name.into(),
                    vendor_id: (self.vendor)(id),
                    product_id: (self.product)(id),
                    gamepad_type,
                    is_gamepad,
                    is_virtual: (self.virtual_device)(id),
                });
            }
            Ok(devices)
        }
    }
}

fn validate_ids(ids: &[u32]) -> Result<(), String> {
    if ids.is_empty() || ids.len() > MAX_DEVICES + 1 || ids.last() != Some(&0) {
        return Err("SDL inventory has invalid size or terminator".into());
    }
    let mut seen = BTreeSet::new();
    if ids[..ids.len() - 1]
        .iter()
        .any(|id| *id == 0 || !seen.insert(*id))
    {
        return Err("SDL inventory has zero or duplicate instance IDs".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn live_ids_are_nonzero_unique_and_explicitly_terminated() {
        assert!(validate_ids(&[0]).is_ok());
        assert!(validate_ids(&[19, 2, 0]).is_ok());
        for ids in [&[1, 1, 0][..], &[0, 1, 0], &[1, 2], &[]] {
            assert!(validate_ids(ids).is_err());
        }
        assert!(validate_ids(&vec![1; MAX_DEVICES + 2]).is_err());
    }
}
