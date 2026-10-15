//! Explicit software-fixture conformance, never a physical-controller claim.
//! Observes the independent Windows probe before changing the owned SDL device.
use crate::input_fixture::{Fixture, VerifiedGamepadFixtureRoute};
use serde_json::{Value, json};
use std::{fs, path::Path};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Neutral,
    Pressed,
    Released,
    Complete,
}

pub struct Sequence {
    appid: u32,
    joystick: u32,
    ready: bool,
    last_sample: Option<u64>,
    phase: Phase,
    consecutive: u8,
    handle: Option<u64>,
    route: Option<VerifiedGamepadFixtureRoute>,
    readbacks: Vec<Value>,
}
impl Sequence {
    pub fn from_environment(appid: u32, joystick: Option<u32>) -> Result<Option<Self>, String> {
        if std::env::var("SKINOS_INPUT_ACTIVITY").ok().as_deref() != Some("1") {
            return Ok(None);
        }
        for (name, expected) in [
            ("SKINOS_STEAM_INPUT_FIXTURE", "neutral"),
            ("SKINOS_INPUT_FOCUS", "1"),
            ("SKINOS_INPUT_CONFIG", "1"),
            ("SKINOS_INPUT_OUTPUT_BACKEND", "wine"),
        ] {
            if std::env::var(name).ok().as_deref() != Some(expected) {
                return Err("Active input test requires the owned neutral fixture, stock layout, focus and Wine output".into());
            }
        }
        let joystick = joystick
            .filter(|id| *id != 0)
            .ok_or("Owned fixture identity missing")?;
        Ok(Some(Self::new(appid, joystick)))
    }
    fn new(appid: u32, joystick: u32) -> Self {
        Self {
            appid,
            joystick,
            ready: false,
            last_sample: None,
            phase: Phase::Neutral,
            consecutive: 0,
            handle: None,
            route: None,
            readbacks: vec![],
        }
    }
    /// `config_dispatched` attests the original audited stock-config call,
    /// not its success. The actual bidirectional route and state below are
    /// independent downstream requirements before any active input is sent.
    pub fn poll(
        &mut self,
        log: &Path,
        focused_appid: u64,
        config_dispatched: bool,
        fixture: &mut Fixture,
    ) -> Result<(), String> {
        let result = self.poll_inner(log, focused_appid, config_dispatched, fixture);
        if result.is_err() && self.route.is_some() {
            let _ = fixture.reset_gamepad_test_state();
        }
        result
    }
    fn poll_inner(
        &mut self,
        log: &Path,
        focused_appid: u64,
        config_dispatched: bool,
        fixture: &mut Fixture,
    ) -> Result<(), String> {
        if focused_appid != u64::from(self.appid)
            || !config_dispatched
            || fixture.joystick_id != self.joystick
        {
            return Err("Input fixture lost its verified app, stock layout or SDL identity".into());
        }
        let metadata = fs::symlink_metadata(log).map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.len() > 2 * 1024 * 1024 {
            return Err("Input fixture probe log is not a bounded regular file".into());
        }
        let bytes = fs::read(log).map_err(|e| e.to_string())?;
        if bytes.len() > 2 * 1024 * 1024 {
            return Err("Input probe output exceeded limit".into());
        }
        // A writer may still be in the middle of the final JSON line.
        let end = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |i| i + 1);
        for line in bytes[..end]
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let Ok(value) = serde_json::from_slice::<Value>(line) else {
                continue;
            };
            if value["event"] == "steam_input_probe_ready" {
                if value["appid"].as_u64() != Some(u64::from(self.appid))
                    || value["identity_verified"] != true
                    || value["appid_verified"] != true
                    || value["api_initialized"] != true
                {
                    return Err("Input probe identity or app mismatch".into());
                }
                self.ready = true;
            }
            if value["event"] != "steam_input_snapshot" {
                continue;
            }
            let sample = value["sample"]
                .as_u64()
                .ok_or("Missing input sample number")?;
            if self.last_sample.is_some_and(|last| sample <= last) {
                continue;
            }
            self.last_sample = Some(sample);
            if !self.ready {
                return Err("Input snapshot preceded genuine probe readiness".into());
            }
            if let Some(press) = self.observe(&value)? {
                if press {
                    let route = VerifiedGamepadFixtureRoute::from_verified(
                        self.appid,
                        self.joystick,
                        self.handle
                            .ok_or("Input fixture has no Steam Input handle")?,
                        0,
                    )?;
                    fixture.open_original_virtual_for_test(route.clone())?;
                    self.route = Some(route);
                }
                let route = self.route.as_ref().ok_or("Input fixture is not armed")?;
                let readback = fixture.set_gamepad_test_state(route, press)?;
                self.readbacks
                    .push(json!({"phase":if press {"pressed"} else {"released"}, "sdl":readback}));
                // Do not evaluate snapshots produced before this new input.
                break;
            }
        }
        Ok(())
    }
    fn observe(&mut self, snapshot: &Value) -> Result<Option<bool>, String> {
        let controllers = snapshot["controllers"]
            .as_array()
            .ok_or("Controller array missing")?;
        let pads = snapshot["xinput"]
            .as_array()
            .ok_or("XInput array missing")?;
        let connected = pads
            .iter()
            .filter(|pad| pad["result"] == 0)
            .collect::<Vec<_>>();
        let route = (controllers.len() == 1 && connected.len() == 1)
            .then(|| {
                let controller = &controllers[0];
                let pad = connected[0];
                let handle = controller["handle"].as_str()?.parse::<u64>().ok()?;
                (handle != 0
                    && controller["gamepad_index"] == 0
                    && pad["slot"] == 0
                    && pad["routed"] == true
                    && pad["steam_input_handle"] == controller["handle"])
                    .then_some((handle, &pad["state"]))
            })
            .flatten();
        let Some((handle, state)) = route else {
            self.consecutive = 0;
            return if self.phase == Phase::Neutral && self.handle.is_none() {
                Ok(None)
            } else {
                Err(
                    "Input fixture lost its exclusive bidirectional Steam Input/XInput route"
                        .into(),
                )
            };
        };
        if self.handle.is_some_and(|old| old != handle) {
            return Err("Input fixture handle changed".into());
        }
        self.handle = Some(handle);
        if self.phase == Phase::Complete {
            return if expected_state(state, false) {
                Ok(None)
            } else {
                Err("Input fixture did not remain released".into())
            };
        }
        let pressed = self.phase == Phase::Pressed;
        if !expected_state(state, pressed) {
            self.consecutive = 0;
            // A transition can take several IPC frames; only the two expected
            // endpoints are acceptable. Unrelated input is never accepted.
            if !expected_state(state, !pressed) {
                return Err("Input fixture observed unexpected gamepad state".into());
            }
            return Ok(None);
        }
        self.consecutive += 1;
        if self.consecutive < 2 {
            return Ok(None);
        }
        self.consecutive = 0;
        match self.phase {
            Phase::Neutral => {
                self.phase = Phase::Pressed;
                Ok(Some(true))
            }
            Phase::Pressed => {
                self.phase = Phase::Released;
                Ok(Some(false))
            }
            Phase::Released => {
                self.phase = Phase::Complete;
                Ok(None)
            }
            Phase::Complete => Ok(None),
        }
    }
    pub fn finish(&self) -> Result<Value, String> {
        if self.phase != Phase::Complete || self.readbacks.len() != 2 {
            return Err(
                "Steam Input fixture did not complete neutral, press and release roundtrip".into(),
            );
        }
        Ok(
            json!({"software_fixture":true,"physical_device_verified":false,"appid":self.appid,
            "sdl_joystick_id":self.joystick,"steam_input_handle":self.handle.map(|id|id.to_string()),
            "xinput_slot":0,"pressed_and_released_verified":true,"samples_per_phase":2,
            "mapped_center_tolerance_lsb":1,"pressed_left_x_expected":32767,"readbacks":self.readbacks}),
        )
    }
}
fn expected_state(state: &Value, pressed: bool) -> bool {
    // The original stock mapper emits Y=1 for the fixture's SDL center=0 after
    // its first active update (activity-1), whereas the new Wine device begins
    // at exactly zero. Allow one signed16 quantization unit at centered axes;
    // buttons, triggers and the deliberately fully deflected X remain exact.
    [
        ("buttons", if pressed { 0x1000 } else { 0 }),
        ("left_trigger", 0),
        ("right_trigger", 0),
    ]
    .into_iter()
    .all(|(key, expected)| state[key].as_i64() == Some(expected))
        && ["left_x", "left_y", "right_x", "right_y"]
            .into_iter()
            .all(|key| {
                state[key].as_i64().is_some_and(|value| {
                    if key == "left_x" && pressed {
                        value == 32767
                    } else {
                        (-1..=1).contains(&value)
                    }
                })
            })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(pressed: bool) -> Value {
        json!({"controllers":[{"handle":"1234","gamepad_index":0}],"xinput":[{"slot":0,"result":0,
            "routed":true,"steam_input_handle":"1234","state":{"buttons":if pressed{4096}else{0},
                "left_x":if pressed{32767}else{0},"left_y":0,"right_x":0,"right_y":0,"left_trigger":0,"right_trigger":0}}]})
    }
    #[test]
    fn requires_two_samples_at_each_endpoint_and_release() {
        let mut seq = Sequence::new(648800, 1);
        assert_eq!(seq.observe(&sample(false)).unwrap(), None);
        assert_eq!(seq.observe(&sample(false)).unwrap(), Some(true));
        assert!(seq.finish().is_err());
        assert_eq!(seq.observe(&sample(false)).unwrap(), None);
        assert_eq!(seq.observe(&sample(true)).unwrap(), None);
        assert_eq!(seq.observe(&sample(true)).unwrap(), Some(false));
        assert_eq!(seq.observe(&sample(false)).unwrap(), None);
        assert_eq!(seq.observe(&sample(false)).unwrap(), None);
        assert_eq!(seq.phase, Phase::Complete);
        assert!(seq.finish().is_err()); // SDL readback is separately required.
    }
    #[test]
    fn never_drives_a_foreign_or_changed_route() {
        let mut seq = Sequence::new(648800, 1);
        let mut foreign = sample(false);
        foreign["xinput"][0]["steam_input_handle"] = json!("555");
        assert_eq!(seq.observe(&foreign).unwrap(), None);
        seq.observe(&sample(false)).unwrap();
        assert!(seq.observe(&foreign).is_err());
        let mut changed = sample(false);
        changed["controllers"][0]["handle"] = json!("555");
        changed["xinput"][0]["steam_input_handle"] = json!("555");
        assert!(seq.observe(&changed).is_err());
    }
    #[test]
    fn refuses_unrelated_buttons_axes_and_extra_controller() {
        let mut seq = Sequence::new(648800, 1);
        let mut pad = sample(false);
        pad["xinput"][0]["state"]["right_trigger"] = json!(1);
        assert!(seq.observe(&pad).is_err());
        let mut seq = Sequence::new(648800, 1);
        seq.observe(&sample(false)).unwrap();
        let mut many = sample(false);
        many["controllers"]
            .as_array_mut()
            .unwrap()
            .push(json!({"handle":"2","gamepad_index":1}));
        assert!(seq.observe(&many).is_err());
    }
    #[test]
    fn center_quantization_does_not_relax_active_axis_or_button_checks() {
        let mut state = sample(true)["xinput"][0]["state"].clone();
        state["left_y"] = json!(1);
        state["right_y"] = json!(1);
        assert!(expected_state(&state, true));
        state["left_x"] = json!(32766);
        assert!(!expected_state(&state, true));
        state["left_x"] = json!(32767);
        state["right_y"] = json!(2);
        assert!(!expected_state(&state, true));
        state["right_y"] = json!(1);
        state["buttons"] = json!(0);
        assert!(!expected_state(&state, true));
        let mut released = sample(false)["xinput"][0]["state"].clone();
        released["left_y"] = json!(1);
        released["right_y"] = json!(1);
        assert!(expected_state(&released, false));
        released["buttons"] = json!(4096);
        assert!(!expected_state(&released, false));
    }
}
