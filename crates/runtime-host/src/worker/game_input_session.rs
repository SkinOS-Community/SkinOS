//! Explicit ordinary-game input scope. Inventory is read from Valve's already
//! loaded SDL; only fresh log-correlated devices can receive the stock layout.
use super::wine_input_session::OutputProgress;
use crate::{input_config, input_device_log, input_devices, input_focus, input_settings};
use std::{
    collections::BTreeMap,
    ffi::c_void,
    path::Path,
    time::{Duration, Instant},
};

pub(super) struct Session {
    cursor: input_device_log::Cursor,
    inventory: Option<input_devices::Inventory>,
    generic: Option<input_settings::TemporaryGenericSupport>,
    focus: Option<input_focus::FocusGuard>,
    selected: Option<(input_devices::Device, input_device_log::Mapping)>,
    prepared: bool,
    activated: bool,
    last_topology_check: Option<Instant>,
    configuration_proof: Option<serde_json::Value>,
    prepared_filter: BTreeMap<String, String>,
    reconnect: Option<Reconnect>,
    clock: Instant,
}
#[derive(Clone, Copy)]
pub(super) struct Runtime {
    pub engine: *mut c_void,
    pub user: i32,
    pub pipe: i32,
    pub base: usize,
}
/// A fresh source-to-output observation, constructed only after this session's
/// SDL, log, filter, configuration and focus guards pass. No device names or
/// raw HID paths leave the inventory through this value. It is not a retained
/// lease: callers must obtain a new route before dispatching feedback.
pub(super) struct SourceRoute {
    appid: u32,
    source: input_config::ControllerIdentity,
    valve_index: u32,
    gamepad_slot: i32,
    producer_active_mask: u32,
    driver_active_mask: u16,
    create_acknowledgements: u64,
    host_connected_controllers: u32,
    focus: input_focus::Snapshot,
}
#[allow(dead_code)] // Generic feedback consumers are integrated independently.
impl SourceRoute {
    pub(super) fn appid(&self) -> u32 {
        self.appid
    }
    pub(super) fn sdl_id(&self) -> u32 {
        self.source.sdl_id
    }
    pub(super) fn valve_index(&self) -> u32 {
        self.valve_index
    }
    pub(super) fn gamepad_slot(&self) -> i32 {
        self.gamepad_slot
    }
    pub(super) fn is_virtual(&self) -> bool {
        self.source.is_virtual
    }
    pub(super) fn source_identity(&self) -> &input_config::ControllerIdentity {
        &self.source
    }
    /// Evidence from this observation only; no serials, names, paths or claims
    /// that a physical motor has been exercised are included.
    pub(super) fn proof(&self) -> serde_json::Value {
        serde_json::json!({"appid":self.appid,"sdl_id":self.source.sdl_id,
            "vendor_id":self.source.vendor_id,"product_id":self.source.product_id,
            "gamepad_type":self.source.gamepad_type,"is_virtual":self.source.is_virtual,
            "valve_index":self.valve_index,"gamepad_slot":self.gamepad_slot,
            "producer_active_mask":self.producer_active_mask,"driver_active_mask":self.driver_active_mask,
            "create_acknowledgements":self.create_acknowledgements,"host_connected_controllers":self.host_connected_controllers,
            "focus_gameid":self.focus.gameid,"focus_pid":self.focus.pid,
            "fresh_log_verified":true,"sole_sdl_gamepad":true,"physical_device_verified":false})
    }
    fn ready_output(
        appid: u32,
        source: &input_devices::Device,
        mapping: &input_device_log::Mapping,
        host: crate::input_readiness::IndexedSnapshot,
        focus: input_focus::Snapshot,
        output: Option<OutputProgress>,
    ) -> Option<Self> {
        let output = output?;
        // prepare/activate already required one source before stock load.
        // After Create the global count can include the owned output; the
        // fresh source identity and its exact mapped slot remain authoritative.
        if host.connected_controllers == 0
            || host.gamepad_index != 0
            || !output.slot_zero_ready()
            || output.relay.create_acknowledgements == 0
        {
            return None;
        }
        Some(Self {
            appid,
            source: input_config::ControllerIdentity {
                sdl_id: source.sdl_id,
                vendor_id: source.vendor_id,
                product_id: source.product_id,
                gamepad_type: source.gamepad_type,
                is_virtual: source.is_virtual,
            },
            valve_index: mapping.valve_index,
            gamepad_slot: host.gamepad_index,
            producer_active_mask: output.producer.active_slot_mask,
            driver_active_mask: output.relay.driver_slots,
            create_acknowledgements: output.relay.create_acknowledgements,
            host_connected_controllers: host.connected_controllers,
            focus,
        })
    }
    fn fixture_proof(self) -> serde_json::Value {
        serde_json::json!({"appid":self.appid,"sdl_id":self.source.sdl_id,"valve_index":self.valve_index,"gamepad_slot":self.gamepad_slot,
            "producer_active_mask":self.producer_active_mask,"driver_active_mask":self.driver_active_mask,"create_acknowledgements":self.create_acknowledgements,"host_connected_controllers":self.host_connected_controllers,
            "fresh_log_verified":true,"sole_owned_sdl_gamepad":true,"fixture":true,"physical_device_verified":false})
    }
}

#[derive(Clone, Copy)]
enum SourcePresence {
    Optional,
    RequiredFixture,
}
impl Session {
    pub(super) fn capture_before_load(data_root: &Path) -> Result<Self, String> {
        Ok(Self {
            cursor: input_device_log::Cursor::capture(data_root)?,
            inventory: None,
            generic: None,
            focus: None,
            selected: None,
            prepared: false,
            activated: false,
            last_topology_check: None,
            configuration_proof: None,
            prepared_filter: BTreeMap::new(),
            reconnect: None,
            clock: Instant::now(),
        })
    }
    pub(super) fn has_device(&self) -> bool {
        self.selected.is_some()
    }
    pub(super) fn configuration_proof(&self) -> Option<serde_json::Value> {
        self.configuration_proof.clone()
    }
    pub(super) fn focus_snapshot(&self) -> Result<Option<serde_json::Value>, String> {
        self.focus
            .as_ref()
            .map(|focus| {
                focus.snapshot().map(
                    |snapshot| serde_json::json!({"gameid":snapshot.gameid,"pid":snapshot.pid}),
                )
            })
            .transpose()
    }
    /// Fresh parent authorization for the explicitly owned feedback fixture.
    /// A cached controller number or producer mask alone is never a route.
    pub(super) unsafe fn fixture_feedback_route(
        &self,
        runtime: Runtime,
        appid: u32,
        fixture_id: u32,
        output: Option<OutputProgress>,
    ) -> Result<Option<serde_json::Value>, String> {
        let (expected, _) = self
            .selected
            .as_ref()
            .ok_or("Feedback fixture has no selected source")?;
        validate_fixture_source(self.activated, expected, fixture_id)?;
        unsafe {
            self.inspect_source_route(runtime, appid, output, SourcePresence::RequiredFixture)
        }
        .map(|route| route.map(SourceRoute::fixture_proof))
    }
    /// Generic one-gamepad route. A missing source or output that is not ready
    /// is temporarily unavailable, not authorization to reuse an older route.
    /// The worker must first advance `poll_game`: awaiting an initial/new
    /// Create, draining or waiting for reconnection can never authorize output.
    #[allow(dead_code)] // Product feedback ownership is integrated separately.
    pub(super) unsafe fn source_route(
        &self,
        runtime: Runtime,
        appid: u32,
        output: Option<OutputProgress>,
    ) -> Result<Option<SourceRoute>, String> {
        if !self.activated {
            return Err("Feedback source session is not activated".into());
        }
        let Some((source, mapping)) = self.reconnect.as_ref().and_then(Reconnect::bound_route)
        else {
            return Ok(None);
        };
        if self
            .selected
            .as_ref()
            .is_none_or(|(selected, selected_mapping)| {
                selected != source || selected_mapping != mapping
            })
        {
            return Err("Feedback selected source differs from its confirmed connection".into());
        }
        unsafe { self.inspect_source_route(runtime, appid, output, SourcePresence::Optional) }
    }
    unsafe fn inspect_source_route(
        &self,
        runtime: Runtime,
        appid: u32,
        output: Option<OutputProgress>,
        presence: SourcePresence,
    ) -> Result<Option<SourceRoute>, String> {
        if !self.activated {
            return Err("Feedback source session is not activated".into());
        }
        let Some((expected, mapping)) = self.selected.as_ref() else {
            return missing_source(presence);
        };
        let live = self
            .inventory
            .as_ref()
            .ok_or("Feedback SDL inventory missing")?
            .snapshot()?;
        let Some(current) = single_gamepad(&live)? else {
            return missing_source(presence);
        };
        if current != expected
            || !self
                .cursor
                .mappings(&live)?
                .iter()
                .any(|actual| actual == mapping)
            || crate::input_filter_policy::for_device(current, &live)? != self.prepared_filter
        {
            return Err("Feedback fixture lost its fresh unambiguous SDL/Valve route".into());
        }
        let config = self
            .configuration_proof
            .as_ref()
            .ok_or("Feedback stock configuration missing")?;
        if config["appid"].as_u64() != Some(u64::from(appid))
            || config["controller_index"].as_u64() != Some(u64::from(mapping.valve_index))
            || config["calls_dispatched"] != true
        {
            return Err(
                "Feedback fixture stock configuration no longer matches its app/index".into(),
            );
        }
        let focus = self
            .focus
            .as_ref()
            .ok_or("Feedback focus scope missing")?
            .snapshot()?;
        if focus.gameid != u64::from(appid) || focus.pid != 0 {
            return Err("Feedback fixture app focus changed".into());
        }
        let host = unsafe {
            crate::input_readiness::read_for_index(
                runtime.engine,
                runtime.pipe,
                runtime.base,
                mapping.valve_index,
            )?
        };
        Ok(SourceRoute::ready_output(
            appid, current, mapping, host, focus, output,
        ))
    }
    /// Call after authentication/ownership validation, before the first prefix
    /// preparation. All child commands must receive the same returned overlay.
    pub(super) unsafe fn prepare(
        &mut self,
        runtime: Runtime,
        state: &Path,
        mut pump: impl FnMut(),
    ) -> Result<BTreeMap<String, String>, String> {
        if self.prepared {
            return Err("Game input session was prepared twice".into());
        }
        self.generic = Some(unsafe {
            input_settings::attach_for_session(
                runtime.engine,
                runtime.user,
                runtime.pipe,
                runtime.base,
            )?
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            pump();
            if self.inventory.is_none() {
                match input_devices::Inventory::from_loaded(state) {
                    Ok(inventory) => self.inventory = Some(inventory),
                    Err(error)
                        if Instant::now() < deadline
                            && (error == "Valve has not loaded the owned SDL instance"
                                || error == "Valve SDL gamepad subsystem is not initialized") =>
                    {
                        std::thread::sleep(Duration::from_millis(25));
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            }
            let live = self
                .inventory
                .as_ref()
                .expect("inventory opened")
                .snapshot()?;
            let device = single_gamepad(&live)?;
            let Some(device) = device else {
                let host = unsafe {
                    crate::input_readiness::read(runtime.engine, runtime.pipe, runtime.base)?
                };
                if host.connected_controllers != 0 {
                    return Err(
                        "Контроллер обнаружен, но его подключение пока не поддерживается. Переподключите контроллер и повторите попытку"
                            .into(),
                    );
                }
                // A real zero-device inventory is not a successful input test.
                if let Some(mut generic) = self.generic.take() {
                    generic.restore()?;
                }
                self.prepared = true;
                super::event(
                    "game_input_inventory",
                    true,
                    serde_json::json!({"gamepads":0,"input_verified":false,"filter_applied":false}),
                );
                let overlay = crate::input_filter_policy::for_empty();
                validate_overlay(&overlay)?;
                self.prepared_filter = overlay.clone();
                return Ok(overlay);
            };
            let mappings = self.cursor.mappings(&live)?;
            if let Some(mapping) = mappings
                .into_iter()
                .find(|mapping| mapping.sdl_id == device.sdl_id)
            {
                let host = unsafe {
                    crate::input_readiness::read(runtime.engine, runtime.pipe, runtime.base)?
                };
                if host.connected_controllers != 1 {
                    return Err(
                        "Обнаружено несколько контроллеров или неоднозначное подключение. Оставьте подключённым один контроллер"
                            .into(),
                    );
                }
                let overlay = crate::input_filter_policy::for_device(device, &live)?;
                validate_overlay(&overlay)?;
                self.selected = Some((device.clone(), mapping));
                self.prepared = true;
                super::event(
                    "game_input_inventory",
                    true,
                    serde_json::json!({"gamepads":1,"sdl_id":device.sdl_id,
                    "vendor_id":device.vendor_id,"product_id":device.product_id,"is_virtual":device.is_virtual,
                    "input_verified":false,"filter_applied":false}),
                );
                self.prepared_filter = overlay.clone();
                return Ok(overlay);
            }
            if Instant::now() >= deadline {
                return Err(
                    "Steam Input не удалось определить подключённый контроллер. Переподключите его и повторите попытку".into(),
                );
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    /// The Wine output driver/relay must already be genuinely ready. This is
    /// one initial binding only; it never guesses a new hot-plug route.
    pub(super) unsafe fn activate(
        &mut self,
        runtime: Runtime,
        state: &Path,
        appid: u32,
    ) -> Result<(), String> {
        if !self.prepared || self.activated {
            return Err("Game input activation is outside its prepared session".into());
        }
        let Some((expected, expected_mapping)) = &self.selected else {
            self.activated = true;
            return Ok(());
        };
        let live = self
            .inventory
            .as_ref()
            .ok_or("Game input inventory lease missing")?
            .snapshot()?;
        let current = single_gamepad(&live)?.ok_or(
            "Контроллер отключился во время подготовки. Подключите его и повторите попытку",
        )?;
        if current != expected {
            return Err(
                "Подключённый контроллер изменился во время подготовки. Повторите попытку".into(),
            );
        }
        let mapping = self
            .cursor
            .mappings(&live)?
            .into_iter()
            .find(|mapping| mapping.sdl_id == current.sdl_id)
            .ok_or("Steam Input потерял подключение к контроллеру. Переподключите его и повторите попытку")?;
        if &mapping != expected_mapping {
            return Err(
                "Подключение контроллера изменилось во время подготовки. Повторите попытку".into(),
            );
        }
        let host =
            unsafe { crate::input_readiness::read(runtime.engine, runtime.pipe, runtime.base)? };
        if host.connected_controllers != 1 {
            return Err("Список контроллеров изменился во время подготовки. Оставьте подключённым один контроллер и повторите попытку".into());
        }
        self.focus = Some(unsafe {
            input_focus::attach_for_session(runtime.engine, runtime.pipe, runtime.base, appid)?
        });
        let proof = unsafe {
            input_config::attach_for_device(
                runtime.engine,
                runtime.pipe,
                runtime.base,
                state,
                appid,
                current,
                &mapping,
            )?
        };
        self.configuration_proof =
            Some(serde_json::to_value(&proof).map_err(|error| error.to_string())?);
        self.reconnect = Some(Reconnect::new(
            current.clone(),
            mapping,
            self.clock.elapsed(),
        ));
        self.activated = true;
        super::event(
            "game_input_configuration",
            true,
            serde_json::json!({"proof":proof,"input_verified":false}),
        );
        Ok(())
    }
    /// Invalidate only; a new physical route is never silently bound. The
    /// owning worker degrades input/cleans its virtual pad and keeps the game.
    pub(super) fn check_topology(&mut self) -> Result<(), String> {
        if !self.activated {
            return Ok(());
        }
        if self
            .last_topology_check
            .is_some_and(|last| last.elapsed() < Duration::from_millis(250))
        {
            return Ok(());
        }
        self.last_topology_check = Some(Instant::now());
        let live = self
            .inventory
            .as_ref()
            .ok_or("Game input inventory lease is missing")?
            .snapshot()?;
        let current = single_gamepad(&live)?;
        match (&self.selected, current) {
            (None, None) => Ok(()),
            (Some((expected, mapping)), Some(current)) if current == expected => {
                if self
                    .cursor
                    .mappings(&live)?
                    .iter()
                    .any(|actual| actual == mapping)
                {
                    Ok(())
                } else {
                    Err("Подключение контроллера изменилось. Для восстановления Steam Input потребуется новый запуск".into())
                }
            }
            _ => Err(
                "Контроллер отключён или изменился. Steam Input отключён, игра продолжает работать"
                    .into(),
            ),
        }
    }
    /// Ordinary games can recover only the same already-filtered source. No
    /// prefix setup or raw report injection is performed while reconnecting.
    pub(super) unsafe fn poll_game(
        &mut self,
        runtime: Runtime,
        state: &Path,
        appid: u32,
        output: Option<OutputProgress>,
    ) -> Result<Option<serde_json::Value>, String> {
        if !self.activated
            || self
                .last_topology_check
                .is_some_and(|last| last.elapsed() < Duration::from_millis(100))
        {
            return Ok(None);
        }
        self.last_topology_check = Some(Instant::now());
        let live = self
            .inventory
            .as_ref()
            .ok_or("Game input inventory lease is missing")?
            .snapshot()?;
        let current = single_gamepad(&live)?;
        let Some(machine) = &mut self.reconnect else {
            return if current.is_none() {
                Ok(None)
            } else {
                Err("Контроллер подключён после запуска игры. Чтобы включить Steam Input, перезапустите игру".into())
            };
        };
        let mapping = self
            .cursor
            .mappings(&live)?
            .into_iter()
            .find(|mapping| current.is_some_and(|device| device.sdl_id == mapping.sdl_id));
        let filter_unchanged = current
            .map(|device| crate::input_filter_policy::for_device(device, &live))
            .transpose()?
            .is_none_or(|filter| filter == self.prepared_filter);
        let host = mapping
            .as_ref()
            .map(|mapping| unsafe {
                crate::input_readiness::read_for_index(
                    runtime.engine,
                    runtime.pipe,
                    runtime.base,
                    mapping.valve_index,
                )
            })
            .transpose()?;
        let observation = Observation {
            current,
            mapping: mapping.as_ref(),
            output,
            host_count: host.map(|host| host.connected_controllers),
            gamepad_slot: host.map(|host| host.gamepad_index),
            filter_unchanged,
        };
        let action = machine.observe(self.clock.elapsed(), observation)?;
        let Some(action) = action else {
            return Ok(None);
        };
        if action == Action::Configure {
            let current = current.ok_or("Reconnect source disappeared before configuration")?;
            let mapping = mapping
                .as_ref()
                .ok_or("Reconnect mapping disappeared before configuration")?;
            let focus = self
                .focus
                .as_ref()
                .ok_or("Reconnect focus scope missing")?
                .snapshot()?;
            if focus.gameid != u64::from(appid) || focus.pid != 0 {
                return Err("Фокус Steam Input изменился. Перезапустите игру для восстановления контроллера".into());
            }
            let proof = unsafe {
                input_config::attach_for_device(
                    runtime.engine,
                    runtime.pipe,
                    runtime.base,
                    state,
                    appid,
                    current,
                    mapping,
                )?
            };
            self.configuration_proof =
                Some(serde_json::to_value(&proof).map_err(|error| error.to_string())?);
            self.selected = Some((current.clone(), mapping.clone()));
            super::event(
                "game_input_configuration",
                true,
                serde_json::json!({"proof":proof,"reconnect":true,"input_verified":false}),
            );
        }
        Ok(Some(
            serde_json::json!({"appid":appid,"state":action.state(),"message":action.message(),
            "game_continues":true,"game_input_verified":false,
            "sdl_id":machine.device.sdl_id,"valve_index":machine.mapping.valve_index,
            "gamepad_slot":host.map(|host| host.gamepad_index),
            "create_acknowledgements":output.map(|output|output.relay.create_acknowledgements),
            "destroy_acknowledgements":output.map(|output|output.relay.destroy_acknowledgements),
            "producer_active_mask":output.map(|output|output.producer.active_slot_mask),
            "driver_active_mask":output.map(|output|output.relay.driver_slots)}),
        ))
    }
    pub(super) fn finish_before_shutdown(&mut self) -> Result<(), String> {
        let focus = self.focus.as_mut().map(|guard| guard.restore()).transpose();
        let generic = self
            .generic
            .as_mut()
            .map(|guard| guard.restore())
            .transpose();
        self.focus.take();
        self.generic.take();
        // No SDL calls or retained SDL function pointers remain when Valve
        // releases its user/pipe and unloads the subsystem.
        self.inventory.take();
        focus.and(generic).map(|_| ())
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.finish_before_shutdown();
    }
}

fn validate_fixture_source(
    activated: bool,
    source: &input_devices::Device,
    fixture_id: u32,
) -> Result<(), String> {
    if !activated || fixture_id == 0 || source.sdl_id != fixture_id || !source.is_virtual {
        return Err("Feedback QA source is not its activated owned SDL fixture".into());
    }
    Ok(())
}

fn missing_source(presence: SourcePresence) -> Result<Option<SourceRoute>, String> {
    match presence {
        SourcePresence::Optional => Ok(None),
        SourcePresence::RequiredFixture => Err("Feedback fixture disconnected".into()),
    }
}

const DESTROY_GRACE: Duration = Duration::from_secs(2);
const REBIND_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug)]
enum ReconnectPhase {
    AwaitingOutput {
        since: Duration,
        creates_before: u64,
        reconnect: bool,
    },
    Bound {
        destroys_before: u64,
    },
    Draining {
        since: Duration,
        destroys_before: u64,
    },
    Waiting {
        candidate_since: Option<Duration>,
    },
}
struct Reconnect {
    device: input_devices::Device,
    mapping: input_device_log::Mapping,
    phase: ReconnectPhase,
}
struct Observation<'a> {
    current: Option<&'a input_devices::Device>,
    mapping: Option<&'a input_device_log::Mapping>,
    output: Option<OutputProgress>,
    host_count: Option<u32>,
    gamepad_slot: Option<i32>,
    filter_unchanged: bool,
}
#[derive(Debug, PartialEq, Eq)]
enum Action {
    Ready,
    Disconnected,
    Waiting,
    Configure,
    Reconnected,
}
impl Action {
    fn state(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Disconnected => "disconnected",
            Self::Waiting => "waiting_reconnect",
            Self::Configure => "reconnecting",
            Self::Reconnected => "reconnected",
        }
    }
    fn message(&self) -> &'static str {
        match self {
            Self::Ready => "Steam Input подключён",
            Self::Disconnected => "Контроллер отключён. Завершаем его подключение",
            Self::Waiting => "Ожидаем повторного подключения того же контроллера",
            Self::Configure => "Восстанавливаем Steam Input",
            Self::Reconnected => "Steam Input восстановлен",
        }
    }
}
impl Reconnect {
    /// Only the state machine's actual Create acknowledgement can enter Bound.
    /// A ready-looking mask or old acknowledgement cannot substitute for it.
    fn bound_route(&self) -> Option<(&input_devices::Device, &input_device_log::Mapping)> {
        matches!(self.phase, ReconnectPhase::Bound { .. }).then_some((&self.device, &self.mapping))
    }
    fn new(
        device: input_devices::Device,
        mapping: input_device_log::Mapping,
        now: Duration,
    ) -> Self {
        Self {
            device,
            mapping,
            phase: ReconnectPhase::AwaitingOutput {
                since: now,
                creates_before: 0,
                reconnect: false,
            },
        }
    }
    fn observe(
        &mut self,
        now: Duration,
        observation: Observation<'_>,
    ) -> Result<Option<Action>, String> {
        let Observation {
            current,
            mapping,
            output,
            host_count,
            gamepad_slot,
            filter_unchanged,
        } = observation;
        if !filter_unchanged || current.is_some_and(|current| !same_source(&self.device, current)) {
            return Err("Подключён другой контроллер или изменились условия подключения. Steam Input отключён; игра продолжает работать".into());
        }
        if output.is_some_and(|output| {
            output.producer.active_slot_mask & !1 != 0 || output.relay.driver_slots & !1 != 0
        }) {
            return Err(
                "Steam Input обнаружил неподдерживаемый слот контроллера. Игра продолжает работать"
                    .into(),
            );
        }
        match &mut self.phase {
            ReconnectPhase::AwaitingOutput {
                since,
                creates_before,
                reconnect,
            } => {
                if current != Some(&self.device) || mapping != Some(&self.mapping) {
                    return Err(
                        "Подключение контроллера изменилось во время восстановления Steam Input"
                            .into(),
                    );
                }
                if output.is_some_and(|output| {
                    output.relay.create_acknowledgements > *creates_before
                        && output.slot_zero_ready()
                }) && gamepad_slot == Some(0)
                {
                    let action = if *reconnect {
                        Action::Reconnected
                    } else {
                        Action::Ready
                    };
                    self.phase = ReconnectPhase::Bound {
                        destroys_before: output
                            .expect("verified output")
                            .relay
                            .destroy_acknowledgements,
                    };
                    return Ok(Some(action));
                }
                if now.saturating_sub(*since) >= REBIND_GRACE {
                    return Err("Steam Input не подтвердил подключение контроллера. Игра продолжает работать".into());
                }
            }
            ReconnectPhase::Bound { destroys_before } => {
                if current == Some(&self.device) {
                    if mapping != Some(&self.mapping) {
                        return Err("Steam Input потерял подтверждённое подключение контроллера. Игра продолжает работать".into());
                    }
                } else {
                    self.phase = ReconnectPhase::Draining {
                        since: now,
                        destroys_before: *destroys_before,
                    };
                    return Ok(Some(Action::Disconnected));
                }
            }
            ReconnectPhase::Draining {
                since,
                destroys_before,
            } => {
                if output.is_some_and(|output| {
                    output.drained() && output.relay.destroy_acknowledgements > *destroys_before
                }) {
                    self.phase = ReconnectPhase::Waiting {
                        candidate_since: None,
                    };
                    return Ok(Some(Action::Waiting));
                }
                if now.saturating_sub(*since) >= DESTROY_GRACE {
                    return Err("Steam Input не подтвердил отключение контроллера. Ввод отключён, игра продолжает работать".into());
                }
            }
            ReconnectPhase::Waiting { candidate_since } => {
                if let Some(current) = current {
                    if current.sdl_id == self.device.sdl_id {
                        return Err("Steam Input не получил новое подключение контроллера. Игра продолжает работать".into());
                    }
                    let candidate_at = *candidate_since.get_or_insert(now);
                    if let (Some(mapping), Some(output)) = (mapping, output)
                        && mapping.sdl_id == current.sdl_id
                        && mapping.valve_index < 16
                        && output.drained()
                        && host_count == Some(1)
                        && matches!(gamepad_slot, Some(-1 | 0))
                    {
                        self.device = current.clone();
                        self.mapping = mapping.clone();
                        self.phase = ReconnectPhase::AwaitingOutput {
                            since: now,
                            creates_before: output.relay.create_acknowledgements,
                            reconnect: true,
                        };
                        return Ok(Some(Action::Configure));
                    }
                    if now.saturating_sub(candidate_at) >= REBIND_GRACE {
                        return Err("Steam Input не удалось определить переподключённый контроллер. Игра продолжает работать".into());
                    }
                } else {
                    *candidate_since = None;
                }
                // Absence is not a transport fault. This nonblocking state can
                // last for the game lifetime; only a new candidate starts the
                // bounded identity/configuration deadline above.
            }
        }
        Ok(None)
    }
}
fn same_source(previous: &input_devices::Device, current: &input_devices::Device) -> bool {
    current.is_gamepad
        && previous.vendor_id == current.vendor_id
        && previous.product_id == current.product_id
        && previous.gamepad_type == current.gamepad_type
        && previous.is_virtual == current.is_virtual
}

fn single_gamepad(
    live: &[input_devices::Device],
) -> Result<Option<&input_devices::Device>, String> {
    let mut pads = live.iter().filter(|device| device.is_gamepad);
    let first = pads.next();
    if pads.next().is_some() {
        return Err(
            "Сейчас Steam Input поддерживает один подключённый контроллер. Отключите остальные и повторите попытку".into(),
        );
    }
    Ok(first)
}
fn validate_overlay(overlay: &BTreeMap<String, String>) -> Result<(), String> {
    if overlay.keys().any(|key| {
        !matches!(
            key.as_str(),
            "SDL_GAMECONTROLLER_IGNORE_DEVICES"
                | "SDL_GAMECONTROLLER_IGNORE_DEVICES_EXCEPT"
                | "NOTPROTON_RAW_CONTROLLERS"
                | "SKINOS_INPUT_FILTER_EXACT"
        )
    }) {
        return Err("Controller filter policy returned an unrelated process setting".into());
    }
    if overlay
        .values()
        .any(|value| value.contains(['\0', '\n', '\r']))
    {
        return Err("Controller filter policy returned a malformed value".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn mapping(id: u32, index: u32) -> input_device_log::Mapping {
        input_device_log::Mapping {
            sdl_id: id,
            valve_index: index,
        }
    }
    fn output(creates: u64, destroys: u64, mask: u16) -> OutputProgress {
        let frames = creates + destroys;
        OutputProgress {
            producer: crate::wine_input_output::OutputStats {
                queued_frames: frames,
                active_slot_mask: u32::from(mask),
                ..Default::default()
            },
            relay: crate::wine_input_relay::RelayProgress {
                create_acknowledgements: creates,
                destroy_acknowledgements: destroys,
                received: frames,
                forwarded: frames,
                acknowledged: frames,
                intended_slots: mask,
                driver_slots: mask,
                ..Default::default()
            },
        }
    }
    fn focus(appid: u32) -> input_focus::Snapshot {
        input_focus::Snapshot {
            gameid: u64::from(appid),
            pid: 0,
        }
    }
    fn observe<'a>(
        current: Option<&'a input_devices::Device>,
        mapping: Option<&'a input_device_log::Mapping>,
        output: OutputProgress,
    ) -> Observation<'a> {
        Observation {
            current,
            mapping,
            output: Some(output),
            host_count: Some(1),
            gamepad_slot: Some(0),
            filter_unchanged: true,
        }
    }
    fn bound() -> Reconnect {
        let device = device(7, true);
        let mapping = mapping(7, 3);
        let mut machine = Reconnect::new(device.clone(), mapping.clone(), Duration::ZERO);
        assert_eq!(
            machine
                .observe(
                    Duration::ZERO,
                    observe(Some(&device), Some(&mapping), output(1, 0, 1))
                )
                .unwrap(),
            Some(Action::Ready)
        );
        machine
    }
    fn disconnected() -> Reconnect {
        let mut machine = bound();
        assert_eq!(
            machine
                .observe(Duration::from_secs(1), observe(None, None, output(1, 0, 1)))
                .unwrap(),
            Some(Action::Disconnected)
        );
        assert_eq!(
            machine
                .observe(
                    Duration::from_millis(1100),
                    observe(None, None, output(1, 1, 0))
                )
                .unwrap(),
            Some(Action::Waiting)
        );
        machine
    }
    #[test]
    fn reconnect_waits_real_destroy_and_new_create_at_actual_valve_index() {
        let mut machine = bound();
        assert!(machine.bound_route().is_some());
        assert_eq!(
            machine
                .observe(Duration::from_secs(1), observe(None, None, output(1, 0, 1)))
                .unwrap(),
            Some(Action::Disconnected)
        );
        // Old active masks survive until Destroy, but cannot authorize a route.
        assert!(machine.bound_route().is_none());
        // Even zero masks do not substitute for acknowledgement of Destroy.
        assert_eq!(
            machine
                .observe(
                    Duration::from_millis(1100),
                    observe(None, None, output(1, 0, 0))
                )
                .unwrap(),
            None
        );
        assert!(machine.bound_route().is_none());
        let mut pending = output(1, 1, 0);
        pending.relay.pending = 1;
        assert_eq!(
            machine
                .observe(Duration::from_millis(1200), observe(None, None, pending))
                .unwrap(),
            None
        );
        assert_eq!(
            machine
                .observe(
                    Duration::from_millis(1300),
                    observe(None, None, output(1, 1, 0))
                )
                .unwrap(),
            Some(Action::Waiting)
        );
        assert!(machine.bound_route().is_none());
        let new_device = device(42, true);
        let new_mapping = mapping(42, 9); // Valve index is not XInput slot zero.
        let now = Duration::from_secs(2);
        assert_eq!(
            machine
                .observe(now, observe(Some(&new_device), None, output(1, 1, 0)))
                .unwrap(),
            None
        );
        assert_eq!(
            machine
                .observe(
                    now,
                    observe(Some(&new_device), Some(&new_mapping), output(1, 1, 0))
                )
                .unwrap(),
            Some(Action::Configure)
        );
        assert!(machine.bound_route().is_none());
        assert_eq!(machine.mapping.valve_index, 9);
        // Neither dispatched configuration nor the old Create ACK is success.
        assert_eq!(
            machine
                .observe(
                    now,
                    observe(Some(&new_device), Some(&new_mapping), output(1, 1, 1))
                )
                .unwrap(),
            None
        );
        assert!(machine.bound_route().is_none());
        let mut wrong_slot = observe(Some(&new_device), Some(&new_mapping), output(2, 1, 1));
        wrong_slot.gamepad_slot = Some(1);
        assert_eq!(machine.observe(now, wrong_slot).unwrap(), None);
        assert!(machine.bound_route().is_none());
        assert_eq!(
            machine
                .observe(
                    now,
                    observe(Some(&new_device), Some(&new_mapping), output(2, 1, 1))
                )
                .unwrap(),
            Some(Action::Reconnected)
        );
        assert_eq!(machine.bound_route(), Some((&new_device, &new_mapping)));
        assert_eq!(
            machine
                .observe(
                    now,
                    observe(Some(&new_device), Some(&new_mapping), output(2, 1, 1))
                )
                .unwrap(),
            None
        );
        // A second disconnect needs its own new Destroy, not the first one's ACK.
        assert_eq!(
            machine
                .observe(now, observe(None, None, output(2, 1, 1)))
                .unwrap(),
            Some(Action::Disconnected)
        );
        assert!(machine.bound_route().is_none());
        assert_eq!(
            machine
                .observe(now, observe(None, None, output(2, 1, 0)))
                .unwrap(),
            None
        );
        assert_eq!(
            machine
                .observe(now, observe(None, None, output(2, 2, 0)))
                .unwrap(),
            Some(Action::Waiting)
        );
        assert!(machine.bound_route().is_none());
    }
    #[test]
    fn initial_route_requires_the_state_machine_to_acknowledge_create() {
        let source = device(42, true);
        let mapping = mapping(42, 9);
        let mut machine = Reconnect::new(source.clone(), mapping.clone(), Duration::ZERO);
        assert!(machine.bound_route().is_none());
        assert_eq!(
            machine
                .observe(
                    Duration::ZERO,
                    observe(Some(&source), Some(&mapping), output(0, 0, 1)),
                )
                .unwrap(),
            None,
        );
        assert!(machine.bound_route().is_none());
        assert_eq!(
            machine
                .observe(
                    Duration::from_millis(100),
                    observe(Some(&source), Some(&mapping), output(1, 0, 1)),
                )
                .unwrap(),
            Some(Action::Ready),
        );
        assert_eq!(machine.bound_route(), Some((&source, &mapping)));
    }
    #[test]
    fn disconnect_and_rebind_waits_are_bounded() {
        let mut machine = bound();
        machine
            .observe(Duration::ZERO, observe(None, None, output(1, 0, 1)))
            .unwrap();
        assert!(
            machine
                .observe(DESTROY_GRACE, observe(None, None, output(1, 0, 1)))
                .is_err()
        );
        let mut machine = disconnected();
        let current = device(42, true);
        machine
            .observe(
                Duration::from_secs(2),
                observe(Some(&current), None, output(1, 1, 0)),
            )
            .unwrap();
        assert!(
            machine
                .observe(
                    Duration::from_secs(7),
                    observe(Some(&current), None, output(1, 1, 0))
                )
                .is_err()
        );
    }
    #[test]
    fn long_device_absence_remains_waiting_and_same_source_can_still_rebind() {
        let mut machine = disconnected();
        let now = Duration::from_secs(6 * 60 * 60);
        assert_eq!(
            machine
                .observe(now, observe(None, None, output(1, 1, 0)))
                .unwrap(),
            None
        );
        assert!(matches!(
            machine.phase,
            ReconnectPhase::Waiting {
                candidate_since: None
            }
        ));
        let current = device(42, true);
        let mapping = mapping(42, 9);
        assert_eq!(
            machine
                .observe(
                    now,
                    observe(Some(&current), Some(&mapping), output(1, 1, 0))
                )
                .unwrap(),
            Some(Action::Configure)
        );
        assert_eq!(
            machine
                .observe(
                    now + Duration::from_millis(100),
                    observe(Some(&current), Some(&mapping), output(2, 1, 1))
                )
                .unwrap(),
            Some(Action::Reconnected)
        );
    }
    #[test]
    fn changed_device_filter_stale_mapping_and_hidden_second_controller_do_not_rebind() {
        let current = device(42, true);
        let fresh = mapping(42, 4);
        for kind in 0..5 {
            let mut other = current.clone();
            match kind {
                0 => other.vendor_id += 1,
                1 => other.product_id += 1,
                2 => other.gamepad_type += 1,
                3 => other.is_virtual = true,
                _ => other.is_gamepad = false,
            }
            assert!(
                disconnected()
                    .observe(
                        Duration::from_secs(2),
                        observe(Some(&other), Some(&fresh), output(1, 1, 0))
                    )
                    .is_err()
            );
        }
        let mut changed = observe(Some(&current), Some(&fresh), output(1, 1, 0));
        changed.filter_unchanged = false;
        assert!(
            disconnected()
                .observe(Duration::from_secs(2), changed)
                .is_err()
        );
        let mut hidden_second = observe(Some(&current), Some(&fresh), output(1, 1, 0));
        hidden_second.host_count = Some(2);
        assert_eq!(
            disconnected()
                .observe(Duration::from_secs(2), hidden_second)
                .unwrap(),
            None
        );
        let stale = mapping(7, 0);
        assert_eq!(
            disconnected()
                .observe(
                    Duration::from_secs(2),
                    observe(Some(&current), Some(&stale), output(1, 1, 0))
                )
                .unwrap(),
            None
        );
        let old = device(7, true);
        assert!(
            disconnected()
                .observe(
                    Duration::from_secs(2),
                    observe(Some(&old), Some(&stale), output(1, 1, 0))
                )
                .is_err()
        );
    }
    fn device(id: u32, pad: bool) -> input_devices::Device {
        input_devices::Device {
            sdl_id: id,
            name: "test".into(),
            vendor_id: 1,
            product_id: 2,
            gamepad_type: 1,
            is_gamepad: pad,
            is_virtual: false,
        }
    }
    #[test]
    fn physical_source_keeps_its_numeric_identity_without_satisfying_fixture_guard() {
        let source = device(42, true);
        let mapping = mapping(42, 9);
        let route = SourceRoute::ready_output(
            629520,
            &source,
            &mapping,
            crate::input_readiness::IndexedSnapshot {
                connected_controllers: 1,
                gamepad_index: 0,
            },
            focus(629520),
            Some(output(1, 0, 1)),
        )
        .unwrap();
        assert_eq!(route.appid(), 629520);
        assert_eq!(route.sdl_id(), 42);
        assert!(!route.is_virtual());
        assert_eq!(route.valve_index(), 9);
        assert_eq!(route.gamepad_slot(), 0);
        assert_eq!(route.source_identity().vendor_id, source.vendor_id);
        assert_eq!(
            route.proof(),
            serde_json::json!({"appid":629520,"sdl_id":42,"vendor_id":1,"product_id":2,
                "gamepad_type":1,"is_virtual":false,"valve_index":9,"gamepad_slot":0,
                "producer_active_mask":1,"driver_active_mask":1,"create_acknowledgements":1,
                "host_connected_controllers":1,"focus_gameid":629520,"focus_pid":0,
                "fresh_log_verified":true,"sole_sdl_gamepad":true,"physical_device_verified":false})
        );
        assert!(validate_fixture_source(true, &source, 42).is_err());
        let mut fixture = source;
        fixture.is_virtual = true;
        assert!(validate_fixture_source(true, &fixture, 42).is_ok());
        assert!(validate_fixture_source(false, &fixture, 42).is_err());
        assert!(validate_fixture_source(true, &fixture, 0).is_err());
        assert!(validate_fixture_source(true, &fixture, 7).is_err());
    }
    #[test]
    fn generic_source_absence_is_unavailable_but_fixture_absence_remains_an_error() {
        assert!(missing_source(SourcePresence::Optional).unwrap().is_none());
        assert_eq!(
            missing_source(SourcePresence::RequiredFixture)
                .err()
                .as_deref(),
            Some("Feedback fixture disconnected")
        );
    }
    #[test]
    fn source_route_requires_actual_slot_zero_masks_and_create_ack() {
        let source = device(42, true);
        let mapping = mapping(42, 9);
        let host = crate::input_readiness::IndexedSnapshot {
            connected_controllers: 1,
            gamepad_index: 0,
        };
        let ready = output(1, 0, 1);
        assert!(SourceRoute::ready_output(42, &source, &mapping, host, focus(42), None).is_none());
        for field in 0..6 {
            let mut output = ready;
            let mut host = host;
            match field {
                0 => host.connected_controllers = 0,
                1 => host.gamepad_index = 1,
                2 => output.producer.active_slot_mask = 0,
                3 => output.relay.intended_slots = 0,
                4 => output.relay.driver_slots = 0,
                _ => output.relay.create_acknowledgements = 0,
            }
            assert!(
                SourceRoute::ready_output(42, &source, &mapping, host, focus(42), Some(output))
                    .is_none(),
                "readiness guard {field}"
            );
        }
        // Preserve the existing post-Create rule: the owned output may increase
        // Valve's global count without changing the fresh source's mapped slot.
        let host = crate::input_readiness::IndexedSnapshot {
            connected_controllers: 2,
            ..host
        };
        assert!(
            SourceRoute::ready_output(42, &source, &mapping, host, focus(42), Some(ready))
                .is_some()
        );
    }
    #[test]
    fn fixture_route_keeps_the_existing_proof_shape() {
        let mut source = device(42, true);
        source.is_virtual = true;
        validate_fixture_source(true, &source, 42).unwrap();
        let route = SourceRoute::ready_output(
            629520,
            &source,
            &mapping(42, 9),
            crate::input_readiness::IndexedSnapshot {
                connected_controllers: 1,
                gamepad_index: 0,
            },
            focus(629520),
            Some(output(1, 0, 1)),
        )
        .unwrap();
        assert_eq!(
            route.fixture_proof(),
            serde_json::json!({"appid":629520,"sdl_id":42,"valve_index":9,"gamepad_slot":0,
                "producer_active_mask":1,"driver_active_mask":1,"create_acknowledgements":1,"host_connected_controllers":1,
                "fresh_log_verified":true,"sole_owned_sdl_gamepad":true,"fixture":true,"physical_device_verified":false})
        );
    }
    #[test]
    fn refuses_enumeration_order_and_accepts_actual_zero() {
        assert!(single_gamepad(&[]).unwrap().is_none());
        assert!(single_gamepad(&[device(1, false)]).unwrap().is_none());
        let live = [device(7, false), device(42, true)];
        assert_eq!(single_gamepad(&live).unwrap().unwrap().sdl_id, 42);
        assert!(single_gamepad(&[device(42, true), device(1, true)]).is_err());
    }
    #[test]
    fn policy_cannot_override_loader_or_add_multiline_environment() {
        assert!(
            validate_overlay(&BTreeMap::from([("WINELOADER".into(), "/other".into())])).is_err()
        );
        assert!(
            validate_overlay(&BTreeMap::from([(
                "SDL_GAMECONTROLLER_IGNORE_DEVICES".into(),
                "a\nb".into()
            )]))
            .is_err()
        );
    }
}
