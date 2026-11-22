use std::time::{Duration, Instant};

use glam::{DVec3, IVec3};

use super::world::{ClientWorld, LiquidSampler, SeenCells};
use crate::assets::Assets;
use crate::audio::{Arrival, Audio, Play, SourceFilter, travel_time};
use crate::content::{Content, content};
use crate::physics::{Hit, raycast};

const PROBE_REACH: f64 = 24.0;
const DIRECT_REACH: f64 = 48.0;
const CHECK_INTERVAL: Duration = Duration::from_millis(100);
const SCHEDULE_AHEAD: f64 = 0.045;
const WALL_EPSILON: f64 = 0.015;
const REFLECTION_CHECKS: usize = 24;
const MIN_REFLECTION: f32 = 0.025;
const AXES: [DVec3; 6] = [DVec3::NEG_X, DVec3::X, DVec3::NEG_Y, DVec3::Y, DVec3::NEG_Z, DVec3::Z];
const PROBES: [[f64; 3]; 14] = [
    [-1.0, 0.0, 0.0],
    [1.0, 0.0, 0.0],
    [0.0, -1.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, 0.0, -1.0],
    [0.0, 0.0, 1.0],
    [-1.0, -1.0, -1.0],
    [-1.0, -1.0, 1.0],
    [-1.0, 1.0, -1.0],
    [-1.0, 1.0, 1.0],
    [1.0, -1.0, -1.0],
    [1.0, -1.0, 1.0],
    [1.0, 1.0, -1.0],
    [1.0, 1.0, 1.0],
];

#[derive(Clone, Copy)]
struct Wall {
    face: u8,
    plane: f64,
    reflectivity: f32,
}

impl Wall {
    fn bounce(self, source: DVec3, listener: DVec3) -> Option<(DVec3, f64)> {
        let axis = self.face as usize / 2;
        let side = AXES[self.face as usize][axis];
        let ear = (listener[axis] - self.plane) * side;
        let src = (source[axis] - self.plane) * side;
        if ear <= WALL_EPSILON || src <= WALL_EPSILON {
            return None;
        }
        let mut mirror = source;
        mirror[axis] = 2.0 * self.plane - source[axis];
        let point = listener.lerp(mirror, ear / (ear + src));
        Some((point, listener.distance(mirror)))
    }
}

struct Emission {
    play: Play,
    received: Instant,
    age_at_receipt: f64,
}

#[derive(Default)]
pub(super) struct Soundscape {
    walls: Vec<Wall>,
    cubes: Vec<(IVec3, u32)>,
    checked: Option<Instant>,
    sampled_at: DVec3,
    pending: Vec<Emission>,
    pub reverb: f32,
    pub feedback: f32,
    pub damping: f32,
}

impl Soundscape {
    pub fn refresh(&mut self, world: &ClientWorld, listener: DVec3) {
        let now = Instant::now();
        if self.checked.is_some_and(|last| now - last < CHECK_INTERVAL) {
            return;
        }
        self.checked = Some(now);
        let min = crate::world::cube_of((listener - PROBE_REACH).floor().as_ivec3());
        let max = crate::world::cube_of((listener + PROBE_REACH).floor().as_ivec3());
        let cubes = (min.y..=max.y).flat_map(|y| (min.z..=max.z).flat_map(move |z| (min.x..=max.x).map(move |x| IVec3::new(x, y, z))));
        let stamps = cubes.filter_map(|p| world.cube_rev(p).map(|(_, rev)| (p, rev)));
        if listener.distance_squared(self.sampled_at) < 0.75 * 0.75 && stamps.clone().eq(self.cubes.iter().copied()) {
            return;
        }
        self.cubes.clear();
        self.cubes.extend(stamps);
        self.sampled_at = listener;
        self.walls.clear();
        let c = content();
        let cells = SeenCells { world, air_if_missing: true };
        let mut hits = 0u32;
        let mut reflected = 0.0;
        let mut span = 0.0;
        for dir in PROBES.map(|dir| DVec3::from_array(dir).normalize()) {
            let Some(hit) = raycast(c, &cells, listener, dir, PROBE_REACH, weft_abi::ray::SOLID) else { continue };
            if hit.dist < WALL_EPSILON {
                continue;
            }
            let point = listener + dir * hit.dist;
            let plane = point[hit.face as usize / 2];
            let reflectivity = reflectivity(c, hit);
            hits += 1;
            reflected += reflectivity;
            span += hit.dist as f32;
            if let Some(wall) = self
                .walls
                .iter_mut()
                .find(|w| w.face == hit.face && (w.plane - plane).abs() < WALL_EPSILON)
            {
                wall.reflectivity = wall.reflectivity.max(reflectivity);
            } else {
                self.walls
                    .push(Wall { face: hit.face, plane, reflectivity });
            }
        }
        let closed = hits as f32 / PROBES.len() as f32;
        let reflectivity = reflected / hits.max(1) as f32;
        let size = (span / hits.max(1) as f32 / 12.0).clamp(0.0, 1.0);
        self.reverb = 0.48 * closed.powi(3) * reflectivity.powi(2);
        self.feedback = 0.35 + (0.33 + 0.28 * size) * closed * reflectivity;
        self.damping = 0.24 + 0.68 * (1.0 - reflectivity);
        log::trace!(target: "acoustics", "room at {listener}: walls {hits}, reflectivity {reflectivity:.3}, wet {:.3}, built in {} us",
            self.reverb, now.elapsed().as_micros());
    }

    pub fn emit(&mut self, play: Play, age_at_receipt: f32) {
        self.pending
            .push(Emission { play, received: Instant::now(), age_at_receipt: f64::from(age_at_receipt) });
    }

    pub fn discard_pending(&mut self) {
        self.pending.clear();
    }

    pub fn play(
        &mut self,
        liquids: LiquidSampler<'_>,
        listener: DVec3,
        audio: &mut Audio,
        assets: &Assets,
        sounds: impl IntoIterator<Item = Play>,
    ) {
        let now = Instant::now();
        let c = content();
        let cells = SeenCells { world: liquids.world, air_if_missing: true };
        let mut listener_liquid = None;
        let mut bounce_checks = REFLECTION_CHECKS;
        let mut routed = 0usize;
        let mut propagate = |p: Play, age: f64| {
            let Some(source) = p.pos else {
                audio.play(assets, p);
                return true;
            };
            let Some(def) = c.game.sounds.get(p.sound as usize) else { return true };
            let distance = source.distance(listener);
            let volume = p.volume * def.volume;
            let reach = def.reach(volume).max(1.5);
            let travel = travel_time(distance);
            if !distance.is_finite() || !volume.is_finite() || volume <= 0.0 || distance >= reach || age > travel + 1.0 {
                return true;
            }
            if travel - age > SCHEDULE_AHEAD {
                return false;
            }
            let blocked = obstruction(c, &cells, listener, source, DIRECT_REACH);
            let mut direct = Arrival { pos: source, distance, gain: 1.0, cutoff: 20_000.0 };
            if blocked.is_some() {
                direct.gain = 0.045f32.max(def.through_walls);
                direct.cutoff = 420.0 + def.through_walls * 800.0;
            }
            let mut paths = [direct; 3];
            let mut count = 1;
            if distance <= PROBE_REACH * 2.0 && volume >= 0.12 && bounce_checks != 0 {
                count += reflections(c, &cells, &self.walls, source, listener, distance, reach, &mut bounce_checks, &mut paths[1..]);
            }
            if let Some(liquid) = liquids.at_source(source, listener) {
                let at_listener = *listener_liquid.get_or_insert_with(|| liquids.at(listener));
                let filter = SourceFilter::from_liquid(liquid, at_listener);
                for path in &mut paths[..count] {
                    path.gain *= filter.gain;
                    path.cutoff = path.cutoff.min(filter.cutoff);
                }
            }
            let direct = paths[0];
            log::trace!(target: "acoustics::paths", "{} at {:.1} blocks: gain {:.3}, cutoff {:.0}, {} reflections, arrival {:.3}s (age {:.3}s)",
                def.name, distance, direct.gain, direct.cutoff, count - 1, travel, age);
            audio.play_paths(assets, p, age, &paths[..count]);
            routed += 1;
            true
        };
        self.pending
            .retain(|e| !propagate(e.play, (now - e.received).as_secs_f64() + e.age_at_receipt));
        for play in sounds {
            if !propagate(play, 0.0) {
                self.pending
                    .push(Emission { play, received: now, age_at_receipt: 0.0 });
            }
        }
        if routed != 0 {
            log::debug!(target: "acoustics::cost", "{routed} positional sounds in {} us ({} reflection checks)",
                now.elapsed().as_micros(), REFLECTION_CHECKS - bounce_checks);
        }
    }
}

fn reflectivity(c: &Content, hit: Hit) -> f32 {
    1.0 - c.states[hit.state as usize].sound_absorption
}

fn obstruction(c: &Content, cells: &SeenCells<'_>, from: DVec3, source: DVec3, reach: f64) -> Option<Hit> {
    let delta = source - from;
    let distance = delta.length();
    if distance <= WALL_EPSILON {
        return None;
    }
    raycast(c, cells, from, delta, (distance - WALL_EPSILON).min(reach), weft_abi::ray::SOLID)
        // A block sound can originate inside the block that made it.
        .filter(|hit| hit.origin != source.floor().as_ivec3())
}

fn reflections(
    c: &Content,
    cells: &SeenCells<'_>,
    walls: &[Wall],
    source: DVec3,
    listener: DVec3,
    direct: f64,
    reach: f64,
    budget: &mut usize,
    out: &mut [Arrival],
) -> usize {
    let mut candidates = [(0.0f32, None); PROBES.len()];
    let mut count = 0;
    for wall in walls {
        let Some((point, distance)) = wall.bounce(source, listener) else { continue };
        if distance >= reach || distance - direct < 0.8 || point.distance_squared(listener) > PROBE_REACH * PROBE_REACH {
            continue;
        }
        let strength = reflection_gain(wall.reflectivity, direct, distance);
        if strength >= MIN_REFLECTION {
            candidates[count] = (strength, Some((wall.face, point, distance)));
            count += 1;
        }
    }
    candidates[..count].sort_unstable_by(|a, b| b.0.total_cmp(&a.0));
    let mut found = 0;
    for (_, candidate) in &candidates[..count] {
        if found == out.len() || *budget < 2 {
            break;
        }
        let (face, point, distance) = candidate.unwrap();
        let from_bounce = point - listener;
        let leg = from_bounce.length();
        *budget -= 2;
        let Some(hit) = raycast(c, cells, listener, from_bounce, leg + WALL_EPSILON, weft_abi::ray::SOLID) else { continue };
        if hit.face != face
            || (hit.dist - leg).abs() > WALL_EPSILON
            || obstruction(c, cells, point + AXES[face as usize] * WALL_EPSILON, source, distance).is_some()
        {
            continue;
        }
        let reflectivity = reflectivity(c, hit);
        let gain = reflection_gain(reflectivity, direct, distance);
        if gain < MIN_REFLECTION {
            continue;
        }
        out[found] = Arrival {
            // Put the virtual source at the path length, on the ray arriving from the wall.
            pos: listener + from_bounce * (distance / leg),
            distance,
            gain,
            cutoff: 900.0 + 13_000.0 * reflectivity,
        };
        found += 1;
    }
    found
}

fn reflection_gain(reflectivity: f32, direct: f64, distance: f64) -> f32 {
    0.74 * reflectivity * ((direct + 2.0) / (distance + 2.0)) as f32
}
