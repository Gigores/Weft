use glam::{DVec3, IVec3};
use weft_abi::{self as abi, ParticleBurst, entry_kind as ek, ray};

use super::{
    Client,
    particles::Particles,
    wake::{Crest, Stern, SurfaceLine},
    world::{ClientWorld, SeenCells},
};
use crate::content::{Content, content};
use crate::physics::{self, Aabb, Cells, Transform};
use crate::world::{DIRS, TICK};

const EFFECT_RANGE: f64 = 64.0;
const MOTION_TICKS: f64 = 2.0;
const FOAM_HEIGHT: f64 = 0.035;

type DryVolume<'a> = (Transform, &'a [(IVec3, IVec3)]);

struct Kinds {
    flame: u32,
    smoke: u32,
}

impl Client {
    pub(super) fn motion_effects(&mut self, elapsed: f64) {
        let c = content();
        let kinds = c.game.catalog.ids(ek::PARTICLE);
        let kinds = Kinds { flame: kinds["core:thruster_exhaust"], smoke: kinds["core:thruster_smoke"] };
        let tick = self.anim_tick();
        let eye = self.cameras.shown.pos;
        let cells = SeenCells { world: &self.world, air_if_missing: false };
        let dry: Vec<DryVolume<'_>> = self
            .entities
            .values()
            .filter(|e| !e.dry_boxes.is_empty())
            .map(|e| (e.place_at(tick), e.dry_boxes.as_slice()))
            .collect();
        let mut exhaust = 120;
        let mut samples = 128;
        let phase = f64::from(self.rng.float());
        let world_frame = Transform::default();
        self.wake_render.advance(tick);
        emit_exhaust(c, cells, &self.world, world_frame, world_frame, eye, &dry, &kinds, &mut self.particles, &mut exhaust);
        let mine = (!self.dead && !self.mode.has(crate::tag_bits::game_mode::HIDDEN)).then_some((&self.player_id, &self.me));
        for (&id, e) in mine.into_iter().chain(self.entities.iter()) {
            let pos = e.surface_pos;
            let body = e.structure.as_deref();
            let (bounds, place, previous, seconds) = if let Some(body) = body {
                let Some((min, max)) = body.bounds else { continue };
                (
                    Aabb { min: min.as_dvec3(), max: max.as_dvec3() + DVec3::ONE },
                    e.place_at(tick),
                    e.place_at(tick - MOTION_TICKS),
                    MOTION_TICKS * TICK,
                )
            } else {
                if samples == 0 || e.pulled.is_some() || pos.y - e.pos.y <= 0.8 * elapsed {
                    continue;
                }
                let size = e.dims();
                (
                    Aabb::around(DVec3::ZERO, size.half_width, size.height),
                    Transform::rotated(e.pos, glam::DQuat::IDENTITY),
                    Transform::rotated(pos, glam::DQuat::IDENTITY),
                    elapsed,
                )
            };
            let world = place.box_to_world(bounds);
            if eye.distance_squared(eye.clamp(world.min, world.max)) >= EFFECT_RANGE * EFFECT_RANGE {
                continue;
            }
            if let Some(body) = body {
                emit_exhaust(c, cells, body, place, previous, eye, &dry, &kinds, &mut self.particles, &mut exhaust);
            }
            if samples > 0 {
                let mut wake = Wake { c, cells, body, bounds, place, previous, seconds, eye, phase, samples: &mut samples };
                let line = wake.sample(e.frame == 0);
                if line.impact.is_empty() && line.bow.is_empty() && line.stern.is_none() {
                    continue;
                }
                self.wake_render.waterline(id, line, tick, |at, liquid| {
                    exposed_surface(c, &cells, at.floor().as_ivec3())
                        .is_some_and(|(kind, height)| kind == liquid && (height + FOAM_HEIGHT - at.y).abs() < 0.27)
                        && !dry
                            .iter()
                            .any(|&volume| physics::inside_dry_volume(volume, at))
                });
            }
        }
        self.wake_render.spray(tick, phase, |kind, pos, velocity| {
            self.particles
                .spawn(&burst(kind, pos, velocity, 0.055, 0.9, 1), &self.world);
        });
        self.me.surface_pos = self.me.pos;
        for e in self.entities.values_mut() {
            e.surface_pos = e.pos;
        }
    }
}

fn emit_exhaust(
    c: &Content,
    cells: SeenCells<'_>,
    body: &ClientWorld,
    place: Transform,
    previous: Transform,
    eye: DVec3,
    dry: &[DryVolume<'_>],
    kinds: &Kinds,
    particles: &mut Particles,
    remaining: &mut u32,
) {
    let world = cells.world;
    for (p, signal) in body.exhausts(place.to_local(eye), EFFECT_RANGE) {
        if *remaining == 0 {
            break;
        }
        let Some(dir) = DIRS.get(signal.front as usize) else { continue };
        let rear = -dir.as_dvec3();
        let nozzle = p.as_dvec3() + DVec3::splat(0.5) + rear * 0.56;
        let pos = place.to_world(nozzle);
        if eye.distance_squared(pos) >= EFFECT_RANGE * EFFECT_RANGE {
            continue;
        }
        let direction = place.vector_to_world(rear);
        let velocity = ((pos - previous.to_world(nozzle)) / (MOTION_TICKS * TICK)).clamp_length_max(32.0);
        let power = f64::from(signal.level.min(15)) / 15.0;
        if let Some(liquid) = physics::liquid_at(c, &cells, pos)
            && !dry
                .iter()
                .any(|&volume| physics::inside_dry_volume(volume, pos))
        {
            if let Some(kind) = liquid_particle(c, liquid, abi::comp::BUBBLE) {
                let burst = burst(kind, pos, velocity + direction * (2.0 + 6.0 * power), 0.12, 1.0, 1 + u32::from(signal.level >= 8));
                spawn(particles, world, remaining, burst);
                continue;
            }
        }
        let jet = burst(
            kinds.flame,
            pos,
            velocity + direction * (3.5 + 11.0 * power),
            0.11,
            0.65 + power as f32,
            1 + u32::from(signal.level / 4),
        );
        spawn(particles, world, remaining, jet);
        if signal.level >= 4 {
            let smoke = burst(kinds.smoke, pos + direction * 0.33, velocity + direction * (2.0 + 5.0 * power), 0.10, 0.85, 1);
            spawn(particles, world, remaining, smoke);
        }
    }
}

fn liquid_particle(c: &Content, liquid: u16, name: &str) -> Option<u32> {
    let state = c.liquids[liquid as usize].first_state;
    let value = c
        .components
        .text(crate::components::BLOCK, state, c.components.id(name)?)?;
    c.game.catalog.find(ek::PARTICLE, value)
}

fn burst(kind: u32, pos: DVec3, velocity: DVec3, spread: f64, speed: f32, count: u32) -> ParticleBurst {
    ParticleBurst {
        kind,
        pos: pos.to_array().into(),
        spread: DVec3::splat(spread).to_array().into(),
        velocity: velocity.to_array().into(),
        speed,
        count,
        ..ParticleBurst::DEFAULT
    }
}

fn spawn(particles: &mut Particles, world: &ClientWorld, remaining: &mut u32, mut burst: ParticleBurst) {
    burst.count = burst.count.min(*remaining);
    if burst.count != 0 {
        *remaining -= burst.count;
        particles.spawn(&burst, world);
    }
}

struct Wake<'a> {
    c: &'a Content,
    cells: SeenCells<'a>,
    body: Option<&'a ClientWorld>,
    bounds: Aabb,
    place: Transform,
    previous: Transform,
    seconds: f64,
    eye: DVec3,
    phase: f64,
    samples: &'a mut u32,
}

#[derive(Clone, Copy)]
struct Rim {
    pos: DVec3,
    outward: DVec3,
    velocity: DVec3,
    push: f64,
    drop: f64,
    liquid: u16,
    droplets: u32,
}

impl Rim {
    fn crest(self, power: f64, size: f64) -> Crest {
        Crest { at: self.pos, outward: self.outward, velocity: self.velocity, power, size, droplets: self.droplets }
    }
}

impl Wake<'_> {
    fn sample(&mut self, trail: bool) -> SurfaceLine {
        let mut line = SurfaceLine::default();
        let bounds = self.bounds;
        let mid = (bounds.min + bounds.max) * 0.5;
        let center = self.place.to_world(mid);
        let half = (bounds.max - bounds.min) * 0.5;
        let motion = (center - self.previous.to_world(mid)) / self.seconds;
        let rotation = self.place.rotation() * self.previous.rotation().conjugate();
        let angular_travel = rotation.to_scaled_axis().length() * half.length() / self.seconds;
        if motion.length_squared() > 80.0 * 80.0 || motion.with_y(0.0).length() + angular_travel < 0.35 && motion.y > -0.8 {
            return line;
        }
        let mut run = motion.with_y(0.0).normalize_or_zero();
        if motion.with_y(0.0).length_squared() < 0.1 {
            run = self
                .place
                .vector_to_world(DVec3::Z)
                .with_y(0.0)
                .normalize_or_zero();
            if run == DVec3::ZERO {
                run = DVec3::Z;
            }
        }
        let across = DVec3::Y.cross(run);
        let place = self.place;
        let extent = |axis| half.dot(place.vector_to_local(axis).abs());
        let world = self.place.box_to_world(bounds);
        let fall = self.previous.box_to_world(bounds).min.y - world.min.y;
        let h = world.max.y.min(world.min.y + 48.0) + fall.clamp(0.0, 4.0) + 0.15;
        let depth = h - world.min.y + 0.25;
        let size = (2.0 * extent(across)).sqrt().clamp(0.25, 3.0);
        let speed = motion.with_y(0.0).length();
        let mut perimeter = Vec::new();
        'sides: for (span, outward) in [(run, -across), (across, run), (-run, across), (-across, -run)] {
            let width = extent(span);
            let reach = extent(outward) + 0.21;
            let lanes = (width * 2.0 / 1.5).ceil().clamp(2.0, 8.0) as u32;
            let rear = outward.dot(run) < -0.5;
            for i in 0..lanes {
                if *self.samples == 0 {
                    perimeter.push(None);
                    break 'sides;
                }
                *self.samples -= 1;
                let margin = (0.17 / width.max(0.18)).min(0.45);
                let jitter = if rear || i == 0 || i + 1 == lanes { 0.0 } else { 0.19 * (self.phase - 0.5) };
                let along = ((f64::from(i) + jitter) / f64::from(lanes - 1) * 2.0 - 1.0) * width * (1.0 - margin);
                let start = (center + span * along + outward * reach).with_y(h);
                let hit = self.rim(start, outward, depth, (2.0 * reach).min(128.0), world.min.y, fall, size);
                perimeter.push(hit);
            }
        }
        if self.body.is_some() && trail && speed > 0.6 {
            let mut wet = perimeter.iter().flatten().map(|rim| rim.pos - center);
            if let Some(first) = wet.next() {
                let (mut left, mut right) = (first.dot(across), first.dot(across));
                let mut aft = first.dot(run);
                let (mut height, mut count) = (first.y, 1.0);
                for point in wet {
                    left = left.min(point.dot(across));
                    right = right.max(point.dot(across));
                    aft = aft.min(point.dot(run));
                    height += point.y;
                    count += 1.0;
                }
                line.stern = Some(Stern {
                    center: center.with_y(center.y + height / count),
                    half_width: ((right - left) * 0.5).clamp(0.45, extent(across).max(0.45)),
                    clearance: -aft.min(0.0) * 0.90,
                    velocity: motion.with_y(0.0),
                });
            }
        }
        line.impact.extend(
            perimeter
                .iter()
                .flatten()
                .filter(|rim| rim.drop > 0.0)
                .map(|rim| (rim.liquid, rim.crest(rim.drop, size))),
        );
        if self.body.is_none() {
            return line;
        }
        let max_gap = (extent(run).max(extent(across)) * 0.55).clamp(2.7, 16.0);
        for (a, b) in perimeter.iter().zip(perimeter.iter().cycle().skip(1)) {
            let (Some(a), Some(b)) = (a, b) else { continue };
            if a.push.max(b.push) <= 0.55 {
                continue;
            }
            if a.liquid != b.liquid || a.pos.distance_squared(b.pos) > max_gap * max_gap || (a.pos.y - b.pos.y).abs() > 0.27 {
                continue;
            }
            if exposed_surface(self.c, &self.cells, ((a.pos + b.pos) * 0.5).floor().as_ivec3()).is_none() {
                continue;
            }
            line.bow
                .push([a.crest(a.push, size), b.crest(b.push, size)]);
        }
        line
    }

    fn rim(&self, start: DVec3, outward: DVec3, depth: f64, reach: f64, keel_y: f64, fall: f64, size: f64) -> Option<Rim> {
        let surface = physics::raycast(self.c, &self.cells, start, DVec3::NEG_Y, depth, ray::LIQUIDS | ray::SOLID)?;
        let (liquid, y) = exposed_surface(self.c, &self.cells, surface.block)?;
        if self.c.states[surface.state as usize].liquid != Some(liquid) {
            return None;
        }
        let droplets = liquid_particle(self.c, liquid, abi::comp::SPLASH)?;
        let immersion = ((self.bounds.max.y - self.bounds.min.y) * 0.5).min(0.18);
        let ray_origin = start.with_y(y - (immersion * 0.5).min(0.08));
        let cast = |place: Transform, from| {
            let origin = place.to_local(from);
            let direction = place.vector_to_local(-outward);
            if let Some(world) = self.body {
                let cells = SeenCells { world, air_if_missing: true };
                physics::raycast(self.c, &cells, origin, direction, reach, ray::SOLID).map(|hit| hit.dist)
            } else {
                physics::ray_aabb(origin, direction, &self.bounds, reach).map(|hit| hit.0)
            }
        };
        let entry = || {
            // Use the same entry pose along the contour, even if taller blocks still cross the waterline.
            if fall < 0.1 {
                return None;
            }
            let earlier = (y - keel_y - immersion) / fall;
            if !(0.0..=if self.body.is_some() { 0.5 } else { 1.0 }).contains(&earlier) {
                return None;
            }
            let pos = self
                .place
                .position()
                .lerp(self.previous.position(), earlier);
            let turn = self
                .place
                .rotation()
                .slerp(self.previous.rotation(), earlier);
            let sample = Transform::rotated(pos, turn);
            let from = ray_origin + (pos - self.place.position()).with_y(0.0);
            Some((from - outward * cast(sample, from)?, sample))
        };
        let (rim, sample) = entry().or_else(|| Some((ray_origin - outward * cast(self.place, ray_origin)?, self.place)))?;
        let open = rim + outward * (0.38 * size.min(1.0));
        let (here, y) = exposed_surface(self.c, &self.cells, open.floor().as_ivec3())?;
        if here != liquid || (y - rim.y).abs() > 0.25 || open.distance_squared(self.eye) > EFFECT_RANGE * EFFECT_RANGE {
            return None;
        }
        let local = sample.to_local(rim);
        let before = self.previous.to_world(local);
        let velocity = (self.place.to_world(local) - before) / self.seconds;
        let travel = velocity.with_y(0.0);
        let drop = if before.y >= y { (-velocity.y - 0.75).clamp(0.0, 12.0) } else { 0.0 };
        Some(Rim {
            pos: open.with_y(y + FOAM_HEIGHT),
            outward,
            velocity: travel,
            push: travel.dot(outward).clamp(0.0, 22.0),
            drop,
            liquid,
            droplets,
        })
    }
}

fn exposed_surface(c: &Content, world: &impl Cells, at: IVec3) -> Option<(u16, f64)> {
    let (liquid, height) = physics::liquid_surface(c, world, at)?;
    (!c.liquids[liquid as usize].rises && world.liquid(c, at + IVec3::Y).is_none()).then_some((liquid, height))
}
