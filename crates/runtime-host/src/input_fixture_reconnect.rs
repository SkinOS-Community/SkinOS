//! Explicit test-only device lifecycle, using the same original SDL as Valve.
//! No buttons, controller handles, output frames or game API results are made up.
use crate::input_fixture::Fixture;
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    path::Path,
    time::{Duration, Instant},
};

#[derive(Debug, PartialEq, Eq)]
enum Phase {
    WaitingForGame,
    Attached,
    Detached,
    Complete,
}

pub struct Sequence {
    phase: Phase,
    since: Instant,
    old_id: Option<u32>,
    new_id: Option<u32>,
}

impl Sequence {
    pub fn from_environment(game_mode: bool) -> Result<Option<Self>, String> {
        let enabled = match std::env::var("SKINOS_INPUT_RECONNECT_TEST").as_deref() {
            Err(std::env::VarError::NotPresent) | Ok("") => return Ok(None),
            Ok("1") => true,
            _ => return Err("Invalid owned reconnect fixture selection".into()),
        };
        if enabled
            && (!game_mode
                || std::env::var("SKINOS_STEAM_INPUT_FIXTURE").ok().as_deref() != Some("neutral"))
        {
            return Err(
                "Reconnect QA requires normal game input and the explicitly owned SDL fixture"
                    .into(),
            );
        }
        Ok(Some(Self {
            phase: Phase::WaitingForGame,
            since: Instant::now(),
            old_id: None,
            new_id: None,
        }))
    }

    pub fn poll(
        &mut self,
        state: &Path,
        fixture: &RefCell<Option<Fixture>>,
        game_running: bool,
    ) -> Result<Option<Value>, String> {
        if !game_running {
            return Ok(None);
        }
        let now = Instant::now();
        match self.phase {
            Phase::WaitingForGame => {
                let slot = fixture.borrow();
                let current = slot
                    .as_ref()
                    .ok_or("Reconnect QA has no owned SDL attachment")?;
                self.old_id = Some(current.joystick_id);
                self.since = now;
                self.phase = Phase::Attached;
                Ok(None)
            }
            Phase::Attached if now.duration_since(self.since) >= Duration::from_secs(4) => {
                let current = fixture
                    .borrow_mut()
                    .take()
                    .ok_or("Reconnect QA lost its owned SDL attachment")?;
                let id = current.detach_for_reconnect_test()?;
                if Some(id) != self.old_id {
                    return Err("Reconnect QA attachment identity changed".into());
                }
                self.phase = Phase::Detached;
                self.since = now;
                Ok(Some(self.event("detached")))
            }
            Phase::Detached if now.duration_since(self.since) >= Duration::from_secs(3) => {
                if fixture.borrow().is_some() {
                    return Err("Reconnect QA refuses to replace another attachment".into());
                }
                let next = Fixture::from_environment(state)?
                    .ok_or("Reconnect fixture was disabled during QA")?;
                if Some(next.joystick_id) == self.old_id
                    || next.joystick_id == 0
                    || !next.sdl_is_gamepad
                {
                    return Err(
                        "Reattached SDL fixture did not acquire a fresh gamepad identity".into(),
                    );
                }
                self.new_id = Some(next.joystick_id);
                *fixture.borrow_mut() = Some(next);
                self.phase = Phase::Complete;
                Ok(Some(self.event("reattached")))
            }
            _ => Ok(None),
        }
    }

    pub fn finish(&self) -> Result<Value, String> {
        if self.phase != Phase::Complete {
            return Err("Reconnect QA ended before its real SDL detach/reattach completed".into());
        }
        Ok(self.event("complete"))
    }

    fn event(&self, phase: &str) -> Value {
        json!({"fixture":true,"physical_device_verified":false,"phase":phase,
            "old_sdl_id":self.old_id,"new_sdl_id":self.new_id,"game_input_verified":false})
    }
}
