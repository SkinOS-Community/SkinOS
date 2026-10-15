//! Explicit neutral software fixture. It attaches to the *same already loaded*
//! genuine SDL used by Valve. It does not manufacture Steam handles or inject
//! buttons, axes, keyboard, or mouse events.
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
}

// https://github.com/libsdl-org/SDL/blob/70e9cc86d/include/SDL3/SDL_joystick.h
// SDL_INIT_INTERFACE sets version to sizeof(struct), not the SDL release.
#[repr(C)]
struct VirtualJoystickDesc {
    version: u32,
    kind: u16,
    padding: u16,
    vendor_id: u16,
    product_id: u16,
    naxes: u16,
    nbuttons: u16,
    nballs: u16,
    nhats: u16,
    ntouchpads: u16,
    nsensors: u16,
    padding2: [u16; 2],
    button_mask: u32,
    axis_mask: u32,
    name: *const c_char,
    touchpads: *const c_void,
    sensors: *const c_void,
    userdata: *mut c_void,
    update: Option<unsafe extern "C" fn(*mut c_void)>,
    set_player_index: Option<unsafe extern "C" fn(*mut c_void, c_int)>,
    rumble: Option<unsafe extern "C" fn(*mut c_void, u16, u16) -> bool>,
    rumble_triggers: Option<unsafe extern "C" fn(*mut c_void, u16, u16) -> bool>,
    set_led: Option<unsafe extern "C" fn(*mut c_void, u8, u8, u8) -> bool>,
    send_effect: Option<unsafe extern "C" fn(*mut c_void, *const c_void, c_int) -> bool>,
    set_sensors_enabled: Option<unsafe extern "C" fn(*mut c_void, bool) -> bool>,
    cleanup: Option<unsafe extern "C" fn(*mut c_void)>,
}

fn neutral_description() -> VirtualJoystickDesc {
    // All pointer members and callback Options permit the zero representation.
    let mut desc: VirtualJoystickDesc = unsafe { std::mem::zeroed() };
    desc.version = std::mem::size_of::<VirtualJoystickDesc>() as u32;
    desc.kind = 1; // SDL_JOYSTICK_TYPE_GAMEPAD
    desc.naxes = 4; // sticks only; no trigger axes needing a nonzero rest value
    desc.nbuttons = 15;
    desc.button_mask = 0x7fff;
    desc.axis_mask = 0x0f;
    desc.name = c"SkinOS neutral Steam Input fixture (software)".as_ptr();
    desc
}

pub struct Fixture {
    library: *mut c_void,
    detach: unsafe extern "C" fn(u32) -> bool,
    pub joystick_id: u32,
    pub sdl_version: i32,
    pub sdl_is_gamepad: bool,
    activity: Option<activity::Session<activity::NativeJoystick>>,
    rumble: Option<crate::input_fixture_rumble::Registration>,
}

impl Fixture {
    pub fn rumble(&self) -> Option<&crate::input_fixture_rumble::Registration> {
        self.rumble.as_ref()
    }
    /// Only a QA coordinator may consume its own software attachment. A failed
    /// detach remains an error; the ordinary destructor still attempts cleanup.
    pub fn detach_for_reconnect_test(mut self) -> Result<u32, String> {
        drop(self.activity.take());
        let id = self.joystick_id;
        if id == 0 || !unsafe { (self.detach)(id) } {
            return Err("Could not detach the owned SDL reconnect fixture".into());
        }
        self.joystick_id = 0;
        Ok(id)
    }

    pub fn from_environment(state: &Path) -> Result<Option<Self>, String> {
        match std::env::var("SKINOS_STEAM_INPUT_FIXTURE").as_deref() {
            Err(std::env::VarError::NotPresent) | Ok("") => return Ok(None),
            Ok("neutral") => {}
            _ => return Err("Invalid Steam Input fixture mode; only neutral is supported".into()),
        }
        if std::mem::size_of::<VirtualJoystickDesc>() != 136 {
            return Err("Unsupported SDL fixture ABI".into());
        }
        let path = CString::new(state.join("runtime/libSDL3.dylib").as_os_str().as_bytes())
            .map_err(|_| "Invalid SDL fixture path")?;
        // RTLD_NOW | RTLD_NOLOAD: discovery must have loaded the owned SDL first.
        let library = unsafe { dlopen(path.as_ptr(), 0x2 | 0x10) };
        if library.is_null() {
            return Err("Valve has not loaded the owned SDL; refusing separate fixture SDL".into());
        }
        let result = unsafe { Self::attach(library) };
        if result.is_err() {
            unsafe { dlclose(library) };
        }
        result.map(Some)
    }

    unsafe fn attach(library: *mut c_void) -> Result<Self, String> {
        unsafe fn export(library: *mut c_void, name: &CStr) -> Result<*mut c_void, String> {
            let value = unsafe { dlsym(library, name.as_ptr()) };
            if value.is_null() {
                Err(format!(
                    "SDL fixture export missing: {}",
                    name.to_string_lossy()
                ))
            } else {
                Ok(value)
            }
        }
        // Each prototype is pinned to the shipped revision's public headers.
        unsafe {
            let version: unsafe extern "C" fn() -> i32 =
                std::mem::transmute(export(library, c"SDL_GetVersion")?);
            let revision: unsafe extern "C" fn() -> *const c_char =
                std::mem::transmute(export(library, c"SDL_GetRevision")?);
            let was_init: unsafe extern "C" fn(u32) -> u32 =
                std::mem::transmute(export(library, c"SDL_WasInit")?);
            let attach: unsafe extern "C" fn(*const VirtualJoystickDesc) -> u32 =
                std::mem::transmute(export(library, c"SDL_AttachVirtualJoystick")?);
            let detach: unsafe extern "C" fn(u32) -> bool =
                std::mem::transmute(export(library, c"SDL_DetachVirtualJoystick")?);
            let is_gamepad: unsafe extern "C" fn(u32) -> bool =
                std::mem::transmute(export(library, c"SDL_IsGamepad")?);
            let sdl_version = version();
            let revision = revision();
            if sdl_version != 3_005_000
                || revision.is_null()
                || CStr::from_ptr(revision).to_bytes() != b"SDL-release-3.4.0-1359-g70e9cc86d"
            {
                return Err(format!(
                    "SDL fixture requires the audited 70e9cc86d build (version reported {sdl_version})"
                ));
            }
            if was_init(0x2000) & 0x2000 == 0 {
                return Err("Valve SDL gamepad subsystem is not initialized".into());
            }
            let mut rumble = crate::input_fixture_rumble::Registration::from_environment()?;
            let mut description = neutral_description();
            if let Some(registration) = &rumble {
                let callbacks = registration.callbacks();
                description.userdata = callbacks.userdata;
                description.rumble = Some(callbacks.rumble);
                description.cleanup = Some(callbacks.cleanup);
            }
            let joystick_id = attach(&description);
            // SDL owns a separate userdata reference until actual Cleanup.
            // Commit its synchronous attach result before any fallible work.
            if let Some(registration) = &mut rumble {
                registration.finish_attach(joystick_id)?;
            }
            if joystick_id == 0 {
                return Err("Original SDL could not attach the neutral software fixture".into());
            }
            Ok(Self {
                library,
                detach,
                joystick_id,
                sdl_version,
                sdl_is_gamepad: is_gamepad(joystick_id),
                activity: None,
                rumble,
            })
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // The SDL library and attachment outlive neutralization and our open reference.
        drop(self.activity.take());
        unsafe {
            if self.joystick_id != 0 {
                (self.detach)(self.joystick_id);
            }
            dlclose(self.library);
        }
    }
}

#[allow(unused_imports)]
pub use activity::{GamepadTestReadback, VerifiedGamepadFixtureRoute};

#[allow(dead_code)]
impl Fixture {
    /// The caller must first prove the stock gamepad layout, actual focused app
    /// and the fixture's genuine bidirectional SteamInput/XInput route.
    pub fn open_original_virtual_for_test(
        &mut self,
        route: VerifiedGamepadFixtureRoute,
    ) -> Result<(), String> {
        if self.activity.is_some() {
            return Err("Fixture activity reference is already open".into());
        }
        if route.joystick_id() != self.joystick_id || !self.sdl_is_gamepad {
            return Err("Verified route does not refer to this SDL fixture".into());
        }
        let io = unsafe { activity::NativeJoystick::open(self.library, self.joystick_id) }?;
        self.activity = Some(activity::Session::open(io, route)?);
        Ok(())
    }

    /// Revalidate the caller-owned focus/layout/route evidence before each call.
    /// Only the owned SDL virtual A button and left-X axis can be changed.
    pub fn set_gamepad_test_state(
        &mut self,
        route: &VerifiedGamepadFixtureRoute,
        pressed: bool,
    ) -> Result<GamepadTestReadback, String> {
        self.activity
            .as_mut()
            .ok_or("Fixture activity is not explicitly armed")?
            .set(route, pressed)
    }

    /// Cleanup deliberately does not require the route/focus still to be valid.
    pub fn reset_gamepad_test_state(&mut self) -> Result<GamepadTestReadback, String> {
        self.activity
            .as_mut()
            .ok_or("Fixture activity is not explicitly armed")?
            .reset()
    }
}

#[allow(dead_code)]
mod activity {
    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq)]
    #[must_use]
    pub struct VerifiedGamepadFixtureRoute {
        appid: u32,
        joystick_id: u32,
        steam_input_handle: u64,
        xinput_slot: u8,
    }
    impl VerifiedGamepadFixtureRoute {
        /// An attestation from the runtime caller, not an ABI probe or a bypass.
        /// Construct only after proving the active stock layout and current
        /// target-app focus, and mapping this exact SDL fixture to handle/slot.
        pub fn from_verified(
            appid: u32,
            joystick_id: u32,
            steam_input_handle: u64,
            xinput_slot: i32,
        ) -> Result<Self, String> {
            if appid == 0
                || joystick_id == 0
                || steam_input_handle == 0
                || !(0..=3).contains(&xinput_slot)
            {
                return Err("Invalid verified fixture route identity".into());
            }
            Ok(Self {
                appid,
                joystick_id,
                steam_input_handle,
                xinput_slot: xinput_slot as u8,
            })
        }
        pub fn joystick_id(&self) -> u32 {
            self.joystick_id
        }
        pub fn appid(&self) -> u32 {
            self.appid
        }
        pub fn steam_input_handle(&self) -> u64 {
            self.steam_input_handle
        }
        pub fn xinput_slot(&self) -> u8 {
            self.xinput_slot
        }
    }

    #[derive(Debug, serde::Serialize)]
    pub struct GamepadTestReadback {
        pub target_appid: u32,
        pub joystick_id: u32,
        pub steam_input_handle: u64,
        pub xinput_slot: u8,
        pub button_south: bool,
        pub left_x: i16,
        pub neutral: bool,
        pub fixture: bool,
        pub physical_device_verified: bool,
    }
    impl GamepadTestReadback {
        fn new(route: &VerifiedGamepadFixtureRoute, button: bool, axis: i16) -> Self {
            Self {
                target_appid: route.appid,
                joystick_id: route.joystick_id,
                steam_input_handle: route.steam_input_handle,
                xinput_slot: route.xinput_slot,
                button_south: button,
                left_x: axis,
                neutral: !button && axis == 0,
                fixture: true,
                physical_device_verified: false,
            }
        }
    }

    pub(super) trait JoystickIo {
        fn validate_identity(&self, id: u32) -> Result<(), String>;
        fn validate_mapping(&self) -> Result<(), String>;
        fn set_button(&mut self, down: bool) -> Result<(), String>;
        fn set_axis(&mut self, value: i16) -> Result<(), String>;
        fn update(&mut self);
        fn read(&self) -> (bool, i16);
    }
    pub(super) struct Session<I: JoystickIo> {
        io: I,
        route: VerifiedGamepadFixtureRoute,
    }
    impl<I: JoystickIo> Session<I> {
        pub(super) fn open(io: I, route: VerifiedGamepadFixtureRoute) -> Result<Self, String> {
            io.validate_identity(route.joystick_id)?;
            io.validate_mapping()?;
            if io.read() != (false, 0) {
                return Err("Fixture is not neutral before activity test".into());
            }
            Ok(Self { io, route })
        }
        fn apply(&mut self, pressed: bool) -> Result<GamepadTestReadback, String> {
            self.io.validate_identity(self.route.joystick_id)?;
            self.io.validate_mapping()?;
            self.io.set_button(pressed)?;
            self.io.set_axis(if pressed { i16::MAX } else { 0 })?;
            self.io.update();
            self.io.validate_identity(self.route.joystick_id)?;
            let (button, axis) = self.io.read();
            if (button, axis) != (pressed, if pressed { i16::MAX } else { 0 }) {
                return Err("SDL fixture getter did not confirm the requested state".into());
            }
            Ok(GamepadTestReadback::new(&self.route, button, axis))
        }
        pub(super) fn set(
            &mut self,
            route: &VerifiedGamepadFixtureRoute,
            pressed: bool,
        ) -> Result<GamepadTestReadback, String> {
            let result = if route != &self.route {
                Err("Fixture route identity changed".into())
            } else {
                self.apply(pressed)
            };
            match result {
                Ok(readback) => Ok(readback),
                Err(error) => match self.reset() {
                    Ok(_) => Err(error),
                    Err(reset) => Err(format!("{error}; neutral rollback failed: {reset}")),
                },
            }
        }
        pub(super) fn reset(&mut self) -> Result<GamepadTestReadback, String> {
            self.io.validate_identity(self.route.joystick_id)?;
            // Attempt BOTH releases even if one setter fails. A layout change
            // must not prevent releasing the raw values that this session set.
            let button = self.io.set_button(false);
            let axis = self.io.set_axis(0);
            self.io.update();
            button?;
            axis?;
            self.io.validate_identity(self.route.joystick_id)?;
            let (button, axis) = self.io.read();
            if button || axis != 0 {
                return Err("SDL fixture did not return to neutral".into());
            }
            Ok(GamepadTestReadback::new(&self.route, button, axis))
        }
    }
    impl<I: JoystickIo> Drop for Session<I> {
        fn drop(&mut self) {
            let _ = self.reset();
        }
    }

    fn mapping_indices_match(mapping: &str) -> bool {
        let mut a = None;
        let mut x = None;
        for binding in mapping.split(',') {
            if let Some((name, value)) = binding.split_once(':') {
                match name {
                    "a" if a.is_none() => a = Some(value),
                    "leftx" if x.is_none() => x = Some(value),
                    "a" | "leftx" => return false,
                    _ => {}
                }
            }
        }
        a == Some("b0") && x == Some("a0")
    }

    type Close = unsafe extern "C" fn(*mut c_void);
    type GetId = unsafe extern "C" fn(*mut c_void) -> u32;
    type IsVirtual = unsafe extern "C" fn(u32) -> bool;
    type Connected = unsafe extern "C" fn(*mut c_void) -> bool;
    type Count = unsafe extern "C" fn(*mut c_void) -> c_int;
    type Name = unsafe extern "C" fn(*mut c_void) -> *const c_char;
    type Mapping = unsafe extern "C" fn(u32) -> *mut c_char;
    type Free = unsafe extern "C" fn(*mut c_void);
    type SetButton = unsafe extern "C" fn(*mut c_void, c_int, bool) -> bool;
    type SetAxis = unsafe extern "C" fn(*mut c_void, c_int, i16) -> bool;
    type Update = unsafe extern "C" fn();
    type GetButton = unsafe extern "C" fn(*mut c_void, c_int) -> bool;
    type GetAxis = unsafe extern "C" fn(*mut c_void, c_int) -> i16;

    pub(super) struct NativeJoystick {
        joystick: *mut c_void,
        id: u32,
        close: Close,
        get_id: GetId,
        is_virtual: IsVirtual,
        connected: Connected,
        axes: Count,
        buttons: Count,
        name: Name,
        mapping: Mapping,
        free: Free,
        set_button: SetButton,
        set_axis: SetAxis,
        update: Update,
        get_button: GetButton,
        get_axis: GetAxis,
    }
    impl NativeJoystick {
        pub(super) unsafe fn open(library: *mut c_void, id: u32) -> Result<Self, String> {
            unsafe fn symbol(library: *mut c_void, name: &CStr) -> Result<*mut c_void, String> {
                let pointer = unsafe { dlsym(library, name.as_ptr()) };
                if pointer.is_null() {
                    Err(format!(
                        "SDL activity export missing: {}",
                        name.to_string_lossy()
                    ))
                } else {
                    Ok(pointer)
                }
            }
            // Prototypes from the already-audited SDL 70e9cc86d public headers.
            // Resolve every export before opening a new reference.
            unsafe {
                let open: unsafe extern "C" fn(u32) -> *mut c_void =
                    std::mem::transmute(symbol(library, c"SDL_OpenJoystick")?);
                let mut io = Self {
                    joystick: std::ptr::null_mut(),
                    id,
                    close: std::mem::transmute::<*mut c_void, Close>(symbol(
                        library,
                        c"SDL_CloseJoystick",
                    )?),
                    get_id: std::mem::transmute::<*mut c_void, GetId>(symbol(
                        library,
                        c"SDL_GetJoystickID",
                    )?),
                    is_virtual: std::mem::transmute::<*mut c_void, IsVirtual>(symbol(
                        library,
                        c"SDL_IsJoystickVirtual",
                    )?),
                    connected: std::mem::transmute::<*mut c_void, Connected>(symbol(
                        library,
                        c"SDL_JoystickConnected",
                    )?),
                    axes: std::mem::transmute::<*mut c_void, Count>(symbol(
                        library,
                        c"SDL_GetNumJoystickAxes",
                    )?),
                    buttons: std::mem::transmute::<*mut c_void, Count>(symbol(
                        library,
                        c"SDL_GetNumJoystickButtons",
                    )?),
                    name: std::mem::transmute::<*mut c_void, Name>(symbol(
                        library,
                        c"SDL_GetJoystickName",
                    )?),
                    mapping: std::mem::transmute::<*mut c_void, Mapping>(symbol(
                        library,
                        c"SDL_GetGamepadMappingForID",
                    )?),
                    free: std::mem::transmute::<*mut c_void, Free>(symbol(library, c"SDL_free")?),
                    set_button: std::mem::transmute::<*mut c_void, SetButton>(symbol(
                        library,
                        c"SDL_SetJoystickVirtualButton",
                    )?),
                    set_axis: std::mem::transmute::<*mut c_void, SetAxis>(symbol(
                        library,
                        c"SDL_SetJoystickVirtualAxis",
                    )?),
                    update: std::mem::transmute::<*mut c_void, Update>(symbol(
                        library,
                        c"SDL_UpdateJoysticks",
                    )?),
                    get_button: std::mem::transmute::<*mut c_void, GetButton>(symbol(
                        library,
                        c"SDL_GetJoystickButton",
                    )?),
                    get_axis: std::mem::transmute::<*mut c_void, GetAxis>(symbol(
                        library,
                        c"SDL_GetJoystickAxis",
                    )?),
                };
                io.joystick = open(id);
                if io.joystick.is_null() {
                    return Err("Cannot open owned virtual SDL joystick".into());
                }
                Ok(io)
            }
        }
    }
    impl JoystickIo for NativeJoystick {
        fn validate_identity(&self, id: u32) -> Result<(), String> {
            unsafe {
                if id != self.id
                    || self.joystick.is_null()
                    || (self.get_id)(self.joystick) != id
                    || !(self.is_virtual)(id)
                    || !(self.connected)(self.joystick)
                    || (self.axes)(self.joystick) != 4
                    || (self.buttons)(self.joystick) != 15
                {
                    return Err("SDL fixture identity, connection or dimensions changed".into());
                }
                let name = (self.name)(self.joystick);
                if name.is_null()
                    || CStr::from_ptr(name).to_bytes()
                        != c"SkinOS neutral Steam Input fixture (software)".to_bytes()
                {
                    return Err("SDL joystick is not the owned SkinOS fixture".into());
                }
            }
            Ok(())
        }
        fn validate_mapping(&self) -> Result<(), String> {
            unsafe {
                let mapping = (self.mapping)(self.id);
                if mapping.is_null() {
                    return Err("SDL gamepad mapping is unavailable".into());
                }
                let valid = CStr::from_ptr(mapping)
                    .to_str()
                    .is_ok_and(mapping_indices_match);
                (self.free)(mapping.cast());
                if !valid {
                    return Err("SDL mapping does not bind south to b0 and left-X to a0".into());
                }
            }
            Ok(())
        }
        fn set_button(&mut self, down: bool) -> Result<(), String> {
            if unsafe { (self.set_button)(self.joystick, 0, down) } {
                Ok(())
            } else {
                Err("SDL virtual button update failed".into())
            }
        }
        fn set_axis(&mut self, value: i16) -> Result<(), String> {
            if unsafe { (self.set_axis)(self.joystick, 0, value) } {
                Ok(())
            } else {
                Err("SDL virtual axis update failed".into())
            }
        }
        fn update(&mut self) {
            unsafe {
                (self.update)();
            }
        }
        fn read(&self) -> (bool, i16) {
            unsafe {
                (
                    (self.get_button)(self.joystick, 0),
                    (self.get_axis)(self.joystick, 0),
                )
            }
        }
    }
    impl Drop for NativeJoystick {
        fn drop(&mut self) {
            if !self.joystick.is_null() {
                unsafe {
                    (self.close)(self.joystick);
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::{cell::RefCell, rc::Rc};
        #[derive(Default)]
        struct State {
            button: bool,
            axis: i16,
            fail_press_axis: bool,
            reject_pressed_readback: bool,
            invalid_identity: bool,
            calls: Vec<&'static str>,
        }
        struct Mock(Rc<RefCell<State>>);
        impl JoystickIo for Mock {
            fn validate_identity(&self, id: u32) -> Result<(), String> {
                if id == 9 && !self.0.borrow().invalid_identity {
                    Ok(())
                } else {
                    Err("identity".into())
                }
            }
            fn validate_mapping(&self) -> Result<(), String> {
                Ok(())
            }
            fn set_button(&mut self, down: bool) -> Result<(), String> {
                let mut s = self.0.borrow_mut();
                s.calls.push(if down { "press" } else { "release" });
                s.button = down;
                Ok(())
            }
            fn set_axis(&mut self, value: i16) -> Result<(), String> {
                let mut s = self.0.borrow_mut();
                s.calls.push(if value == 0 { "center" } else { "axis" });
                if value != 0 && s.fail_press_axis {
                    return Err("axis failure".into());
                }
                s.axis = value;
                Ok(())
            }
            fn update(&mut self) {
                self.0.borrow_mut().calls.push("update");
            }
            fn read(&self) -> (bool, i16) {
                let s = self.0.borrow();
                if s.reject_pressed_readback && s.button {
                    (false, 0)
                } else {
                    (s.button, s.axis)
                }
            }
        }
        impl Drop for Mock {
            fn drop(&mut self) {
                self.0.borrow_mut().calls.push("close");
            }
        }
        fn route() -> VerifiedGamepadFixtureRoute {
            VerifiedGamepadFixtureRoute::from_verified(648800, 9, 77, 0).unwrap()
        }
        #[test]
        fn explicit_route_and_original_mapping_are_required() {
            for tuple in [
                (0, 9, 77, 0),
                (648800, 0, 77, 0),
                (648800, 9, 0, 0),
                (648800, 9, 77, -1),
                (648800, 9, 77, 4),
            ] {
                assert!(
                    VerifiedGamepadFixtureRoute::from_verified(tuple.0, tuple.1, tuple.2, tuple.3)
                        .is_err()
                );
            }
            assert!(mapping_indices_match(
                "guid,Fixture,a:b0,leftx:a0,platform:Mac OS X,"
            ));
            for mapping in [
                "guid,Fixture,a:b1,leftx:a0,",
                "guid,Fixture,a:b0,leftx:-a0,",
                "guid,Fixture,a:b0,leftx:a0,a:b1,",
                "guid,Fixture,a:b0,",
            ] {
                assert!(!mapping_indices_match(mapping));
            }
        }
        #[test]
        fn opening_is_neutral_and_drop_releases_before_own_close() {
            let state = Rc::new(RefCell::new(State::default()));
            {
                let mut session = Session::open(Mock(state.clone()), route()).unwrap();
                assert!(state.borrow().calls.is_empty());
                let value = session.set(&route(), true).unwrap();
                assert!(value.button_south);
                assert_eq!(value.left_x, i16::MAX);
                assert!(!value.physical_device_verified);
            }
            let state = state.borrow();
            assert!(!state.button);
            assert_eq!(state.axis, 0);
            assert!(
                state
                    .calls
                    .ends_with(&["release", "center", "update", "close"])
            );
        }
        #[test]
        fn partial_set_failure_rolls_back_both_controls() {
            let state = Rc::new(RefCell::new(State {
                fail_press_axis: true,
                ..State::default()
            }));
            let mut session = Session::open(Mock(state.clone()), route()).unwrap();
            assert!(session.set(&route(), true).is_err());
            assert!(!state.borrow().button);
            assert_eq!(state.borrow().axis, 0);
            assert!(
                state
                    .borrow()
                    .calls
                    .ends_with(&["release", "center", "update"])
            );
        }
        #[test]
        fn setter_success_without_getter_confirmation_is_rolled_back() {
            let state = Rc::new(RefCell::new(State {
                reject_pressed_readback: true,
                ..State::default()
            }));
            let mut session = Session::open(Mock(state.clone()), route()).unwrap();
            let error = session.set(&route(), true).unwrap_err();
            assert!(error.contains("getter"));
            assert!(!state.borrow().button);
            assert_eq!(state.borrow().axis, 0);
        }

        #[test]
        fn changed_route_cannot_press_and_resets_previous_input() {
            let state = Rc::new(RefCell::new(State::default()));
            let mut session = Session::open(Mock(state.clone()), route()).unwrap();
            session.set(&route(), true).unwrap();
            let other = VerifiedGamepadFixtureRoute::from_verified(42, 9, 88, 1).unwrap();
            assert!(session.set(&other, true).is_err());
            assert!(!state.borrow().button);
            assert_eq!(state.borrow().axis, 0);
        }
        #[test]
        fn invalid_ownership_does_not_write_to_a_joystick() {
            let state = Rc::new(RefCell::new(State {
                invalid_identity: true,
                ..State::default()
            }));
            assert!(Session::open(Mock(state.clone()), route()).is_err());
            assert_eq!(state.borrow().calls, vec!["close"]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn descriptor_matches_shipped_header_offsets() {
        assert_eq!(std::mem::size_of::<VirtualJoystickDesc>(), 136);
        assert_eq!(std::mem::offset_of!(VirtualJoystickDesc, button_mask), 28);
        assert_eq!(std::mem::offset_of!(VirtualJoystickDesc, axis_mask), 32);
        assert_eq!(std::mem::offset_of!(VirtualJoystickDesc, name), 40);
        assert_eq!(std::mem::offset_of!(VirtualJoystickDesc, update), 72);
        assert_eq!(std::mem::offset_of!(VirtualJoystickDesc, cleanup), 128);
    }
    #[test]
    fn fixture_has_no_active_input_or_callbacks() {
        let desc = neutral_description();
        assert_eq!(desc.version, 136);
        assert_eq!(desc.naxes, 4);
        assert_eq!(desc.ntouchpads, 0);
        assert_eq!(desc.nsensors, 0);
        assert!(desc.update.is_none() && desc.send_effect.is_none() && desc.set_led.is_none());
        assert_eq!(desc.vendor_id, 0);
        assert_eq!(desc.product_id, 0);
    }
}
