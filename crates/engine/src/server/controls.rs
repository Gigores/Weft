use glam::{DVec3, IVec3};
use weft_abi as abi;

use crate::content::Content;
use crate::controller::{self, Bind, Guide, Helm, Motion, Pilot, Rotor, Rule, Settings};
use crate::net::codec::Wire;
use crate::world::GameMode;

use super::{Dim, DimEvent};

fn default_for(c: &Content, state: u32) -> Option<Settings> {
    let signal = c.states.get(state as usize)?.signal;
    if signal.kind != abi::signal_kind::BODY_MOUNT {
        return None;
    }
    let flags = signal.flags as u32;
    if flags & abi::signal_flags::HELM != 0 {
        Some(Settings::Helm(Helm::default()))
    } else if flags & abi::signal_flags::SLIDES != 0 {
        Some(Settings::Guide(Guide::default()))
    } else if flags & abi::signal_flags::ROTATES != 0 {
        Some(Settings::Rotor(Rotor { hold: flags & abi::signal_flags::HOLD != 0, ..Rotor::default() }))
    } else {
        None
    }
}

impl Dim {
    pub(super) fn guide_settings(&self, c: &Content, frame: u64, at: IVec3) -> Guide {
        self.entities
            .anchored(frame, at)
            .and_then(|id| self.controller_settings(c, id))
            .and_then(|s| match s {
                Settings::Guide(g) => Some(g),
                _ => None,
            })
            .unwrap_or_default()
    }

    fn controller_settings(&self, c: &Content, id: u64) -> Option<Settings> {
        Settings::from_bytes(self.entities.comps.get(&(id, c.game.fields.controller))?)
    }

    pub(super) fn rotor_program(&self, c: &Content, frame: u64, at: IVec3) -> Option<Rotor> {
        match self.controller_settings(c, self.entities.anchored(frame, at)?)? {
            Settings::Rotor(rotor) if !rotor.rules.is_empty() => Some(rotor),
            _ => None,
        }
    }

    pub(super) fn edit_controller(&mut self, space_id: u64, c: &Content, at: IVec3, state: u32, face: u8, player: u64) -> bool {
        let Some(default) = default_for(c, state) else { return false };
        let Some(controller) = self.anchored_part(space_id, c, at, state) else { return false };
        let data = self
            .controller_settings(c, controller)
            .unwrap_or(default)
            .to_bytes();
        let face = c.states[state as usize]
            .turn
            .inverse()
            .face(usize::from(face).min(5)) as u8;
        self.events
            .push(DimEvent::Controller { player, controller, face, data });
        true
    }

    pub(crate) fn set_controller_bindings(
        &mut self,
        space_id: u64,
        c: &'static Content,
        at: IVec3,
        bindings: &[abi::ControllerBinding],
    ) -> bool {
        let Some(state) = self.get(space_id, at) else { return false };
        let Some(Settings::Helm(default)) = default_for(c, state) else { return false };
        let Some(id) = self.anchored_part(space_id, c, at, state) else { return false };
        let mut profile = match self.controller_settings(c, id) {
            Some(Settings::Helm(helm)) => helm,
            _ => default,
        };
        profile.output.fill(None);
        for bind in bindings {
            let face = usize::from(bind.face);
            if face >= 6 || usize::from(bind.level) >= controller::LEVELS {
                return false;
            }
            let Some(key) = crate::input::Key::parse(unsafe { bind.key.as_str() }) else { return false };
            let start = face * controller::BINDS_PER_FACE;
            let Some(slot) = profile.output[start..start + controller::BINDS_PER_FACE]
                .iter_mut()
                .find(|slot| slot.is_none())
            else {
                return false;
            };
            *slot = Some(Bind { key, level: bind.level });
        }
        if !self.write_controller(space_id, c, at, id, state, Settings::Helm(profile)) {
            return false;
        }
        if let Some(player) = self.rider_in(space_id, 0) {
            self.refresh_helm(c, space_id, player);
        }
        true
    }

    pub(crate) fn set_rotor_program(
        &mut self,
        space_id: u64,
        c: &'static Content,
        at: IVec3,
        rules: &[abi::RotorRule],
        hold: bool,
    ) -> bool {
        let Some(state) = self.get(space_id, at) else { return false };
        let Some(Settings::Rotor(_)) = default_for(c, state) else { return false };
        let Some(id) = self.anchored_part(space_id, c, at, state) else { return false };
        let mut rotor = Rotor { hold, ..Rotor::default() };
        for r in rules {
            let Some(&motion) = Motion::ALL.get(r.motion as usize) else { return false };
            let rule = Rule { min: r.min, max: r.max, motion, degrees: r.degrees, speed: r.speed };
            if !rule.valid()
                || rotor
                    .rules
                    .iter()
                    .any(|r| r.min <= rule.max && rule.min <= r.max)
            {
                return false;
            }
            rotor.rules.push(rule);
        }
        self.write_controller(space_id, c, at, id, state, Settings::Rotor(rotor))
    }

    fn write_controller(&mut self, space_id: u64, c: &'static Content, at: IVec3, id: u64, state: u32, settings: Settings) -> bool {
        if !c.dims[self.id as usize].editable {
            return false;
        }
        if let Settings::Rotor(rotor) = &settings {
            let mode = if rotor.hold { "hold" } else { "disassemble" };
            if let Some(state) = c.state_with(state, "mode", mode) {
                self.set(space_id, c, at, state);
            }
            self.space_mut(space_id).signals.recheck_device(at);
        }
        self.entities
            .comps
            .insert((id, c.game.fields.controller), settings.to_bytes().into_boxed_slice());
        true
    }

    pub(super) fn save_controller(&mut self, c: &'static Content, player: u64, controller: u64, bytes: &[u8]) {
        if bytes.len() > 2048 {
            return;
        }
        let Some(mut settings) = Settings::from_bytes(bytes) else { return };
        let Some((frame, at)) = self
            .entities
            .get(controller)
            .and_then(|e| Some((e.block_frame(), e.anchor?)))
        else {
            return;
        };
        let Some(e) = self.entities.get(player) else { return };
        if e.mode != GameMode::CREATIVE
            && let Settings::Helm(profile) = &mut settings
        {
            profile.debug_drive = matches!(self.controller_settings(c, controller), Some(Settings::Helm(previous)) if previous.debug_drive);
        }
        let eye = e.pos + DVec3::Y * e.eye(c);
        let reach = self.block_reach(0, c, player);
        let saved = self.with_space(c, frame, |d, space_id, pose| {
            let center = at.as_dvec3() + DVec3::splat(0.5);
            if (pose.to_local(eye) - center).length_squared() > (reach + 0.9).powi(2) {
                return false;
            }
            let Some(state) = d.get(space_id, at) else { return false };
            let Some(expected) = default_for(c, state) else { return false };
            if std::mem::discriminant(&expected) != std::mem::discriminant(&settings)
                || d.anchored_part(space_id, c, at, state) != Some(controller)
            {
                return false;
            }
            let mut q = abi::Query { entity: player, pos: at.to_array().into(), state, ..abi::Query::DEFAULT };
            if !d.ask(space_id, c, abi::action::USE_BLOCK, &mut q) {
                return false;
            }
            d.write_controller(space_id, c, at, controller, state, settings)
        });
        if saved == Some(true)
            && let Some(pilot) = self.rider_in(frame, 0)
        {
            self.refresh_helm(c, frame, pilot);
        }
    }

    pub(super) fn refresh_helm(&mut self, c: &Content, mount: u64, player: u64) {
        if !self.entities.get(player).is_some_and(|e| e.player) {
            return;
        }
        let pilot = self
            .active_helm(c, mount)
            .and_then(|(at, _)| self.entities.anchored(mount, at))
            .map(|controller| {
                let profile = match self.controller_settings(c, controller) {
                    Some(Settings::Helm(profile)) => profile,
                    _ => Helm::default(),
                };
                Pilot { mount, controller, profile, held: 0, pulses: 0 }
            });
        let data = pilot
            .as_ref()
            .map_or_else(Vec::new, |p| Settings::Helm(p.profile.clone()).to_bytes());
        let controller = pilot.as_ref().map_or(0, |p| p.controller);
        if let Some(e) = self.entities.get_mut(player) {
            e.helm_pilot = pilot.map(Box::new);
        }
        self.events
            .push(DimEvent::HelmProfile { player, mount, controller, data });
    }

    pub(super) fn helm_held(&mut self, player: u64, mount: u64, controller: u64, held: u64) {
        if let Some(e) = self.entities.get_mut(player)
            && e.vehicle == Some((mount, 0))
            && e.alive()
            && let Some(p) = &mut e.helm_pilot
            && p.mount == mount
            && p.controller == controller
        {
            let wheel = p
                .profile
                .output
                .iter()
                .enumerate()
                .filter(|(_, bind)| bind.is_some_and(|b| matches!(b.key, crate::input::Key::Wheel(_))))
                .fold(0, |mask, (i, _)| mask | 1 << i);
            p.held = held & !wheel;
            // The next input frame can arrive before the redstone tick.
            p.pulses |= held & wheel;
        }
    }

    pub(super) fn update_helm_outputs(&mut self, c: &Content, mount: u64, at: Option<IVec3>) {
        let rider = self.rider_in(mount, 0);
        let controller = at.and_then(|at| self.entities.anchored(mount, at));
        if let Some(player) = rider
            && self
                .entities
                .get(player)
                .is_some_and(|e| e.player && e.helm_pilot.as_ref().map(|p| (p.mount, p.controller)) != controller.map(|id| (mount, id)))
        {
            self.refresh_helm(c, mount, player);
        }
        let pilot = rider
            .and_then(|id| self.entities.get_mut(id))
            .filter(|e| e.alive())
            .and_then(|e| e.helm_pilot.as_deref_mut());
        let output = at.and_then(|at| {
            let pilot = pilot.filter(|p| p.mount == mount && controller == Some(p.controller))?;
            let active = pilot.held | std::mem::take(&mut pilot.pulses);
            (active != 0).then(|| (at, pilot.profile.levels(active)))
        });
        let Some(body) = self.bodies.get_mut(&mount) else { return };
        if let Some(previous) = body
            .live_helm
            .filter(|p| output.is_none_or(|(at, _)| at != *p))
        {
            body.signals.set_outputs(previous, [0; 6]);
        }
        body.live_helm = output.map(|(at, _)| at);
        if let Some((at, levels)) = output
            && let Some(state) = body.get(at)
        {
            let turn = c.states[state as usize].turn;
            let mut rotated = [0; 6];
            for (face, power) in levels.into_iter().enumerate() {
                rotated[turn.face(face)] = power;
            }
            body.signals.set_outputs(at, rotated);
        }
    }
}
