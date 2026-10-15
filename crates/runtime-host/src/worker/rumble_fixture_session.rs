//! Default-off owned SDL sink QA. No physical-device calls or feedback frames.
use super::{game_input_session::Runtime, wine_input_session::OutputProgress};
use crate::{
    input_device_log, input_devices, input_fixture::Fixture, input_fixture_rumble::Monitor,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::Path};

pub(super) struct Session {
    cursor: input_device_log::Cursor,
    monitor: Option<Monitor>,
    parent: Option<Value>,
    pending: Option<Value>,
}
pub(super) struct Grounding<'a> {
    pub runtime: Runtime,
    pub state: &'a Path,
    pub appid: u32,
    pub fixture: &'a Fixture,
    pub config: &'a Value,
    pub focus: &'a Value,
    pub output: Option<OutputProgress>,
}
impl Session {
    pub(super) fn capture(data_root: &Path) -> Result<Self, String> {
        Ok(Self {
            cursor: input_device_log::Cursor::capture(data_root)?,
            monitor: None,
            parent: None,
            pending: None,
        })
    }
    pub(super) fn attached(&mut self, fixture: &Fixture) -> Result<(), String> {
        self.monitor = Some(
            fixture
                .rumble()
                .ok_or("Rumble QA attachment has no callback registration")?
                .monitor(),
        );
        Ok(())
    }
    pub(super) unsafe fn begin(&mut self, grounding: Grounding<'_>) -> Result<bool, String> {
        let Grounding {
            runtime,
            state,
            appid,
            fixture,
            config,
            focus,
            output,
        } = grounding;
        if self.parent.is_some() {
            return Err("Rumble observation was already grounded".into());
        }
        if self.monitor.is_none() || fixture.joystick_id == 0 || !fixture.sdl_is_gamepad {
            return Err("Rumble observation has no live owned fixture".into());
        }
        let inventory = input_devices::Inventory::from_loaded(state)?;
        let live = inventory.snapshot()?;
        let pads = live
            .iter()
            .filter(|device| device.is_gamepad)
            .collect::<Vec<_>>();
        if pads.len() != 1 || pads[0].sdl_id != fixture.joystick_id || !pads[0].is_virtual {
            return Err(
                "Rumble QA requires the sole original SDL gamepad to be its owned software fixture"
                    .into(),
            );
        }
        let mappings = self.cursor.mappings(&live)?;
        let Some(mapping) = mappings
            .iter()
            .find(|mapping| mapping.sdl_id == fixture.joystick_id)
        else {
            return Ok(false);
        };
        validate_parent_config(
            appid,
            fixture.joystick_id,
            mapping.valve_index,
            config,
            focus,
        )?;
        let host = unsafe {
            crate::input_readiness::read_for_index(
                runtime.engine,
                runtime.pipe,
                runtime.base,
                mapping.valve_index,
            )?
        };
        let Some(output) = output else {
            return Ok(false);
        };
        if host.connected_controllers != 1
            || host.gamepad_index != 0
            || !output.slot_zero_ready()
            || output.relay.create_acknowledgements == 0
        {
            return Ok(false);
        }
        fixture
            .rumble()
            .ok_or("Rumble registration missing")?
            .begin_observation(fixture.joystick_id)?;
        self.parent = Some(
            json!({"appid":appid,"sdl_id":fixture.joystick_id,"valve_index":mapping.valve_index,
            "gamepad_slot":host.gamepad_index,"producer_active_mask":output.producer.active_slot_mask,
            "driver_active_mask":output.relay.driver_slots,"create_acknowledgements":output.relay.create_acknowledgements,
            "fresh_log_verified":true,"sole_owned_sdl_gamepad":true,"is_virtual":true,"config_calls_dispatched":true}),
        );
        super::event(
            "steam_input_rumble_observation",
            true,
            json!({"parent_route":self.parent,
            "observation_started":true,"fixture":true,"physical_device_verified":false}),
        );
        Ok(true)
    }
    pub(super) fn readback(
        &self,
        fixture: &Fixture,
        appid: u32,
        rows: &[Value],
    ) -> Result<Value, String> {
        let parent = self
            .parent
            .as_ref()
            .ok_or("Rumble QA parent route was not grounded")?;
        if parent["sdl_id"].as_u64() != Some(u64::from(fixture.joystick_id)) {
            return Err("Rumble fixture identity changed".into());
        }
        let sdk = validate_transcript(rows, appid, fixture.joystick_id)?;
        let readback = fixture
            .rumble()
            .ok_or("Rumble registration missing")?
            .ordered_readback()?;
        Ok(
            json!({"fixture":true,"physical_device_verified":readback.physical_device_verified,
            "parent_route_verified":true,"parent_route":parent,
            "sdk_route":sdk["route"],"sdk_dispatches":sdk["dispatches"],"sdk_commands_complete":sdk["complete"],
            "callback_samples":readback.samples.iter().map(|sample|json!({"sequence":sample.sequence,"elapsed_us":sample.elapsed_us,"low":sample.low,"high":sample.high})).collect::<Vec<_>>(),
            "ordered_callback_verified":readback.ordered_left_zero_right_zero,"cleanup_seen":readback.cleanup_seen,
            "software_sink_verified":false}),
        )
    }
    pub(super) fn defer(&mut self, payload: Value) -> Result<(), String> {
        if self.pending.is_some() || payload["rumble_fixture"]["ordered_callback_verified"] != true
        {
            return Err("Rumble QA cannot defer an unverified callback transcript".into());
        }
        self.pending = Some(payload);
        Ok(())
    }
    /// Called only after Fixture Drop. Monitor is an Arc, never an SDL pointer.
    pub(super) fn finish_after_detach(&mut self) -> Result<Option<Value>, String> {
        if !self.monitor.as_ref().is_some_and(Monitor::cleanup_seen) {
            return Err("Original SDL did not confirm rumble fixture Cleanup".into());
        }
        let Some(mut payload) = self.pending.take() else {
            return Ok(None);
        };
        payload["rumble_fixture"]["cleanup_seen"] = json!(true);
        payload["rumble_fixture"]["software_sink_verified"] = json!(true);
        Ok(Some(payload))
    }
}
fn validate_parent_config(
    appid: u32,
    joystick: u32,
    index: u32,
    config: &Value,
    focus: &Value,
) -> Result<(), String> {
    let legacy_fixture =
        config["fixture"] == "neutral_software" && config["controller_identity"].is_null();
    let real_identity = config["controller_identity"]["sdl_id"].as_u64()
        == Some(u64::from(joystick))
        && config["controller_identity"]["is_virtual"] == true;
    if joystick == 0
        || index >= 16
        || config["appid"].as_u64() != Some(u64::from(appid))
        || config["controller_index"].as_u64() != Some(u64::from(index))
        || config["controller_index_verified_by_caller"] != true
        || config["calls_dispatched"] != true
        || config["config_id"] != 4
        || config["template_sha256"]
            != "57d41acf506a8a492852d465a4531d9068288538e4736dea92a5f80ed9e523fa"
        || !(legacy_fixture || real_identity)
        || focus["gameid"].as_u64() != Some(u64::from(appid))
        || focus["pid"] != 0
    {
        return Err(
            "Rumble QA lacks the exact fixture's current app focus and stock configuration".into(),
        );
    }
    Ok(())
}
fn require(value: bool, message: &str) -> Result<(), String> {
    if value { Ok(()) } else { Err(message.into()) }
}
fn valid_ready(row: &Value, appid: u32) -> bool {
    row["appid"].as_u64() == Some(u64::from(appid))
        && row["identity_verified"] == true
        && row["appid_verified"] == true
        && row["api_initialized"] == true
        && row["sdk_initialized"] == true
}
fn validate_transcript(rows: &[Value], appid: u32, fixture: u32) -> Result<Value, String> {
    let ready = rows
        .iter()
        .position(|row| row["event"] == "steam_input_probe_ready")
        .ok_or("Rumble SDK readiness missing")?;
    require(
        valid_ready(&rows[ready], appid) && rows[ready]["interface"] == "SteamInput006",
        "Rumble SDK identity is not genuinely initialized",
    )?;
    let rumble = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| {
            row["event"]
                .as_str()
                .is_some_and(|name| name.starts_with("steam_input_rumble_"))
        })
        .collect::<Vec<_>>();
    require(
        rumble.len() == 6,
        "Rumble SDK transcript must contain one route, four dispatches and completion",
    )?;
    let (route_position, route) = rumble[0];
    require(
        route_position > ready
            && route["event"] == "steam_input_rumble_route"
            && route["appid"].as_u64() == Some(u64::from(appid))
            && route["fixture_id_from_parent"].as_u64() == Some(u64::from(fixture))
            && route["identity_verified"] == true
            && route["appid_verified"] == true
            && route["reciprocal_route_verified"] == true
            && route["slot"] == 0,
        "Rumble SDK route differs from its grounded parent",
    )?;
    let handle = route["steam_input_handle"]
        .as_str()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|handle| *handle != 0)
        .ok_or("Rumble SDK handle missing")?;
    let snapshots = rows[..route_position]
        .iter()
        .filter(|row| row["event"] == "steam_input_snapshot")
        .rev()
        .take(2)
        .collect::<Vec<_>>();
    require(
        snapshots.len() == 2,
        "Rumble SDK route requires two preceding independent samples",
    )?;
    for sample in snapshots {
        let pads = sample["controllers"]
            .as_array()
            .ok_or("Rumble SDK controller sample missing")?;
        require(
            pads.len() == 1
                && pads[0]["handle"]
                    .as_str()
                    .and_then(|v| v.parse::<u64>().ok())
                    == Some(handle)
                && pads[0]["gamepad_index"] == 0,
            "Rumble SDK route changed from its scan",
        )?;
        let slots = sample["xinput"]
            .as_array()
            .ok_or("Rumble SDK XInput samples missing")?;
        require(
            slots.len() == 4,
            "Rumble SDK must inspect all four XInput slots",
        )?;
        for (index, slot) in slots.iter().enumerate() {
            require(
                slot["slot"].as_u64() == Some(index as u64),
                "Rumble SDK XInput slots are duplicated or reordered",
            )?;
            if index == 0 {
                require(
                    slot["result"] == 0
                        && slot["routed"] == true
                        && slot["steam_input_handle"]
                            .as_str()
                            .and_then(|v| v.parse::<u64>().ok())
                            == Some(handle),
                    "Rumble SDK reciprocal XInput route mismatch",
                )?;
                let pad = &slot["state"];
                require(
                    ["buttons", "left_trigger", "right_trigger"]
                        .iter()
                        .all(|key| pad[key].as_u64() == Some(0))
                        && ["left_x", "left_y", "right_x", "right_y"]
                            .iter()
                            .all(|key| pad[key].as_i64().is_some_and(|v| (-1..=1).contains(&v))),
                    "Rumble fixture is not neutral in the actual SDK scan",
                )?;
            } else {
                require(
                    slot["result"] == 1167,
                    "Rumble scan has another or uninspectable XInput pad",
                )?;
            }
        }
    }
    for (phase, motors) in [(16384, 0), (0, 0), (0, 24576), (0, 0)].iter().enumerate() {
        let row = rumble[phase + 1].1;
        require(
            row["event"] == "steam_input_rumble_dispatch"
                && row["phase"].as_u64() == Some(phase as u64)
                && row["left"] == motors.0
                && row["right"] == motors.1
                && row["sdk_dispatched"] == true
                && row["callback_verified"] == false
                && row["physical_device_verified"] == false,
            "Rumble SDK dispatch order or magnitudes are invalid",
        )?;
    }
    let complete = rumble[5].1;
    require(
        complete["event"] == "steam_input_rumble_commands_complete"
            && complete["dispatch_count"] == 4
            && complete["final_zero_dispatched"] == true
            && complete["callback_verified"] == false
            && complete["physical_device_verified"] == false,
        "Rumble SDK did not complete the four genuine dispatches",
    )?;
    let summary = rows
        .iter()
        .position(|row| row["event"] == "steam_input_probe_summary")
        .ok_or("Rumble SDK summary missing")?;
    require(
        summary > rumble[5].0
            && valid_ready(&rows[summary], appid)
            && rows[summary]["scan_completed"] == true,
        "Rumble SDK final identity or completion failed",
    )?;
    Ok(
        json!({"route":route,"dispatches":rumble[1..5].iter().map(|(_,row)|*row).collect::<Vec<_>>(),"complete":complete}),
    )
}

pub(super) fn verify_probe(path: &Path) -> Result<(), String> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("Invalid rumble SDK probe path")?;
    require(
        matches!(name, "input-probe.exe" | "input-probe32.exe"),
        "Unexpected rumble SDK probe name",
    )?;
    let root = path.parent().ok_or("Rumble SDK payload root missing")?;
    let manifest: Value = serde_json::from_slice(&bounded_file(
        &root.join("input-probe-manifest.json"),
        64 * 1024,
    )?)
    .map_err(|e| e.to_string())?;
    require(
        manifest["schema_version"] == 1,
        "Unsupported rumble SDK manifest",
    )?;
    let expected = manifest["files"][name]
        .as_str()
        .ok_or("Rumble SDK probe missing from bundled manifest")?;
    require(
        format!(
            "{:x}",
            Sha256::digest(bounded_file(path, 16 * 1024 * 1024)?)
        ) == expected,
        "Rumble SDK probe differs from its bundled manifest",
    )
}
fn bounded_file(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    require(
        metadata.is_file() && metadata.len() <= limit,
        "Rumble QA asset is not a bounded regular file",
    )?;
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    require(
        bytes.len() as u64 <= limit,
        "Rumble QA asset exceeded bound",
    )?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rows() -> Vec<Value> {
        let mut result = vec![
            json!({"event":"steam_input_probe_ready","appid":42,"identity_verified":true,"appid_verified":true,"api_initialized":true,"sdk_initialized":true,"interface":"SteamInput006"}),
        ];
        let mut slots = vec![
            json!({"slot":0,"result":0,"routed":true,"steam_input_handle":"123456","state":{"buttons":0,"left_trigger":0,"right_trigger":0,"left_x":0,"left_y":1,"right_x":0,"right_y":0}}),
        ];
        for slot in 1..4 {
            slots.push(json!({"slot":slot,"result":1167}));
        }
        for sample in 0..2 {
            result.push(json!({"event":"steam_input_snapshot","sample":sample,"controllers":[{"handle":"123456","gamepad_index":0}],"xinput":slots}));
        }
        result.push(json!({"event":"steam_input_rumble_route","appid":42,"identity_verified":true,"appid_verified":true,"reciprocal_route_verified":true,"fixture_id_from_parent":7,"steam_input_handle":"123456","slot":0}));
        for (phase, (left, right)) in [(16384, 0), (0, 0), (0, 24576), (0, 0)]
            .into_iter()
            .enumerate()
        {
            result.push(json!({"event":"steam_input_rumble_dispatch","phase":phase,"left":left,"right":right,"sdk_dispatched":true,"callback_verified":false,"physical_device_verified":false}));
        }
        result.push(json!({"event":"steam_input_rumble_commands_complete","dispatch_count":4,"final_zero_dispatched":true,"callback_verified":false,"physical_device_verified":false}));
        result.push(json!({"event":"steam_input_probe_summary","appid":42,"identity_verified":true,"appid_verified":true,"api_initialized":true,"sdk_initialized":true,"scan_completed":true}));
        result
    }
    #[test]
    fn sdk_transcript_requires_unique_reciprocal_identity_and_four_ordered_dispatches() {
        let rows = rows();
        let proof = validate_transcript(&rows, 42, 7).unwrap();
        assert_eq!(proof["dispatches"].as_array().unwrap().len(), 4);
        assert!(validate_transcript(&rows, 43, 7).is_err());
        assert!(validate_transcript(&rows, 42, 8).is_err());
        for change in 0..12 {
            let mut rows = rows.clone();
            match change {
                0 => rows[0]["sdk_initialized"] = json!(false),
                1 => rows[3]["steam_input_handle"] = json!("999"),
                2 => rows[2]["controllers"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"handle":"456","gamepad_index":1})),
                3 => rows[1]["xinput"][0]["routed"] = json!(false),
                4 => rows[2]["xinput"][1]["result"] = json!(5),
                5 => rows[1]["xinput"][0]["state"]["left_x"] = json!(2),
                6 => rows[4]["left"] = json!(1),
                7 => rows.swap(5, 6),
                8 => {
                    rows.remove(7);
                }
                9 => rows[8]["final_zero_dispatched"] = json!(false),
                10 => rows[9]["appid_verified"] = json!(false),
                _ => {
                    rows.push(json!({"event":"steam_input_rumble_cleanup","zero_dispatched":true}))
                }
            }
            assert!(
                validate_transcript(&rows, 42, 7).is_err(),
                "accepted mutation {change}"
            );
        }
    }
    #[test]
    fn parent_config_is_actual_index_and_fixture_identity_not_assumed_zero() {
        let mut config = json!({"appid":42,"controller_index":9,"controller_index_verified_by_caller":true,"calls_dispatched":true,"config_id":4,
            "template_sha256":"57d41acf506a8a492852d465a4531d9068288538e4736dea92a5f80ed9e523fa","controller_identity":{"sdl_id":7,"is_virtual":true}});
        let focus = json!({"gameid":42,"pid":0});
        assert!(validate_parent_config(42, 7, 9, &config, &focus).is_ok());
        assert!(validate_parent_config(42, 7, 0, &config, &focus).is_err());
        assert!(validate_parent_config(42, 8, 9, &config, &focus).is_err());
        assert!(validate_parent_config(43, 7, 9, &config, &focus).is_err());
        config["controller_identity"]["is_virtual"] = json!(false);
        assert!(validate_parent_config(42, 7, 9, &config, &focus).is_err());
        config["controller_identity"] = Value::Null;
        config["fixture"] = json!("neutral_software");
        assert!(validate_parent_config(42, 7, 9, &config, &focus).is_ok());
        config["calls_dispatched"] = json!(false);
        assert!(validate_parent_config(42, 7, 9, &config, &focus).is_err());
    }
    #[test]
    fn package_manifest_binds_actual_probe_bytes_and_rejects_symlinks() {
        let directory =
            std::env::temp_dir().join(format!("skinos-rumble-package-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let probe = directory.join("input-probe.exe");
        fs::write(&probe, b"owned fixture bytes").unwrap();
        fs::write(directory.join("input-probe-manifest.json"),json!({"schema_version":1,"files":{"input-probe.exe":format!("{:x}",Sha256::digest(b"owned fixture bytes"))}}).to_string()).unwrap();
        assert!(verify_probe(&probe).is_ok());
        fs::write(&probe, b"changed").unwrap();
        assert!(verify_probe(&probe).is_err());
        fs::remove_file(&probe).unwrap();
        std::os::unix::fs::symlink("input-probe-manifest.json", &probe).unwrap();
        assert!(verify_probe(&probe).is_err());
        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn deferred_publication_requires_actual_sdl_cleanup() {
        let directory =
            std::env::temp_dir().join(format!("skinos-rumble-no-cleanup-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let mut session = Session::capture(&directory).unwrap();
        session
            .defer(json!({"rumble_fixture":{"ordered_callback_verified":true}}))
            .unwrap();
        assert!(session.finish_after_detach().is_err());
        assert!(session.pending.is_some());
        fs::remove_dir_all(directory).unwrap();
    }
}
