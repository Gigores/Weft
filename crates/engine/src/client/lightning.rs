use std::time::Instant;

use glam::{DVec3, Vec3};
use weft_abi::entry_kind;
use weft_noise::Rng;

use super::{acoustics::Soundscape, world_render::Camera};
use crate::audio::Play;
use crate::content::content;
use crate::gpu_layout::{GpuSky, sky};
use crate::net::Lightning;
use crate::weather::LIGHTNING_RANGE;

const FLASH_TIME: f32 = 0.30;
pub(super) const FLASH_LIGHTS: usize = 3;

#[derive(Clone, Copy)]
struct Segment {
    from: DVec3,
    to: DVec3,
    width: f32,
}

impl Segment {
    fn closest(self, pos: DVec3) -> DVec3 {
        let line = self.to - self.from;
        let fraction = ((pos - self.from).dot(line) / line.length_squared().max(1e-8)).clamp(0.0, 1.0);
        self.from + line * fraction
    }
}

struct Discharge {
    strike: Lightning,
    received: Instant,
    late_by: f32,
    segments: Vec<Segment>,
}

impl Discharge {
    fn new(strike: Lightning, late_by: f32, listener: DVec3, sounds: &mut Soundscape) -> Self {
        let mut rng = Rng(strike.seed);
        let mut segments = Vec::with_capacity(if strike.ground { 32 } else { 16 });
        let trunk = bolt_path::<17>(strike.origin, strike.end, &mut rng);
        segments.extend(trunk.windows(2).enumerate().map(|(i, pair)| Segment {
            from: pair[0],
            to: pair[1],
            width: 0.72 - i as f32 * 0.023,
        }));
        if strike.ground {
            for joint in [3, 6, 9, 12] {
                let at = trunk[joint];
                let angle = f64::from(rng.float()) * std::f64::consts::TAU;
                let width = 8.0 + f64::from(rng.float()) * 29.0;
                let down = (at.y - strike.end.y) * (0.36 + f64::from(rng.float()) * 0.32);
                let end = at + DVec3::new(angle.cos() * width, -down, angle.sin() * width);
                let fork = bolt_path::<5>(at, end, &mut rng);
                segments.extend(fork.windows(2).enumerate().map(|(i, pair)| Segment {
                    from: pair[0],
                    to: pair[1],
                    width: 0.30 - i as f32 * 0.055,
                }));
            }
        }
        let discharge = Self { strike, received: Instant::now(), late_by, segments };
        discharge.emit_thunder(listener, [trunk[4], trunk[12]], sounds);
        discharge
    }

    fn age(&self) -> f32 {
        self.received.elapsed().as_secs_f32() + self.late_by
    }

    fn flash(&self) -> f32 {
        let age = self.age();
        if age >= FLASH_TIME {
            return 0.0;
        }
        let pulse = 1.0 - smooth(0.026, 0.088, age) + 0.82 * (-((age - 0.119) * 32.0).powi(2)).exp();
        pulse * self.strike.strength * (1.0 - smooth(0.22, FLASH_TIME, age))
    }

    fn emit_thunder(&self, listener: DVec3, tail: [DVec3; 2], sounds: &mut Soundscape) {
        let nearest = self
            .segments
            .iter()
            .map(|segment| segment.closest(listener))
            .min_by(|a, b| {
                a.distance_squared(listener)
                    .total_cmp(&b.distance_squared(listener))
            })
            .unwrap_or(self.strike.origin);
        for (i, pos) in [nearest, tail[0], tail[1]].into_iter().enumerate() {
            let distance = listener.distance(pos) as f32;
            let power = self.strike.strength.min(1.0);
            let pitch = 0.91 + 0.17 * weft_noise::unit(weft_noise::mix(self.strike.seed ^ i as u64)) as f32;
            if i == 0 {
                let crack = (1.0 - smooth(45.0, 420.0, distance)) * power;
                emit(sounds, "core:weather.thunder_crack", pos, crack, pitch, self.late_by);
            }
            let spread = if i == 0 { 0.79 } else { 0.24 * smooth(14.0, 95.0, nearest.distance(pos) as f32) };
            let low = smooth(180.0, 970.0, distance);
            emit(sounds, "core:weather.thunder_roll", pos, spread * (1.0 - low) * power, pitch, self.late_by);
            emit(sounds, "core:weather.thunder_rumble", pos, spread * low * power, pitch, self.late_by);
        }
    }
}

fn smooth(lo: f32, hi: f32, value: f32) -> f32 {
    let t = ((value - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn cloud_lift() -> f64 {
    crate::config::by_name("core:cloud_height").map_or(0.0, crate::config::get)
}

fn emit(out: &mut Soundscape, name: &str, pos: DVec3, volume: f32, pitch: f32, age: f32) {
    if volume >= 0.003
        && let Some(&sound) = content().game.catalog.ids(entry_kind::SOUND).get(name)
    {
        out.emit(Play { sound, pos: Some(pos), volume, pitch }, age);
    }
}

fn bolt_path<const N: usize>(from: DVec3, to: DVec3, rng: &mut Rng) -> [DVec3; N] {
    let mut path = [from; N];
    path[N - 1] = to;
    let mut stride = N - 1;
    while stride > 1 {
        for first in (0..N - 1).step_by(stride) {
            let a = path[first];
            let b = path[first + stride];
            let size = a.distance(b) * 0.30;
            let jitter = DVec3::new(f64::from(rng.float()) - 0.5, 0.0, f64::from(rng.float()) - 0.5);
            path[first + stride / 2] = a.lerp(b, 0.5) + jitter * size;
        }
        stride /= 2;
    }
    path
}

#[derive(Default)]
pub(super) struct LightningEffects {
    discharges: Vec<Discharge>,
}

impl LightningEffects {
    pub fn clear(&mut self) {
        self.discharges.clear();
    }

    pub fn receive(&mut self, strike: Lightning, late: f32, listener: DVec3, sounds: &mut Soundscape) {
        let late = late.max(0.0);
        if self
            .discharges
            .iter()
            .any(|d| d.strike.seed == strike.seed && d.strike.tick == strike.tick)
        {
            return;
        }
        log::debug!("flash {} from {} to {} (ground: {}, late: {:.3}s)", strike.seed, strike.origin, strike.end, strike.ground, late);
        let discharge = Discharge::new(strike, late, listener, sounds);
        if late < FLASH_TIME {
            self.discharges.push(discharge);
        }
    }

    pub fn advance(&mut self) {
        self.discharges.retain(|d| d.age() < FLASH_TIME);
    }

    pub fn lights(&self, camera: DVec3) -> ([[f32; 4]; FLASH_LIGHTS], [[f32; 4]; FLASH_LIGHTS]) {
        let mut strongest: [(f32, Option<(&Discharge, f32)>); FLASH_LIGHTS] = [(0.0, None); FLASH_LIGHTS];
        for discharge in &self.discharges {
            let flash = discharge.flash();
            if flash <= 0.0 {
                continue;
            }
            let hit = discharge.strike;
            let nearest = hit
                .origin
                .distance_squared(camera)
                .min(hit.end.distance_squared(camera));
            let priority = flash / (1.0 + nearest as f32 / (384.0_f32 * 384.0));
            if let Some(i) = strongest.iter().position(|&(old, _)| old < priority) {
                strongest[i..].rotate_right(1);
                strongest[i] = (priority, Some((discharge, flash)));
            }
        }
        let mut starts = [[0.0; 4]; FLASH_LIGHTS];
        let mut ends = [[0.0; 4]; FLASH_LIGHTS];
        for (i, (_, flash)) in strongest.into_iter().enumerate() {
            if let Some((discharge, flash)) = flash {
                let hit = discharge.strike;
                let lift = DVec3::Y * cloud_lift();
                let end = if hit.ground { hit.end } else { hit.end + lift };
                starts[i] = (hit.origin + lift - camera)
                    .as_vec3()
                    .extend(flash)
                    .to_array();
                ends[i] = (end - camera)
                    .as_vec3()
                    .extend(if hit.ground { 1.0 } else { 0.0 })
                    .to_array();
            }
        }
        (starts, ends)
    }

    pub fn append_bolts(&self, list: &mut Vec<GpuSky>, cam: &Camera, height: u32, opacity: f32) {
        let pixel = 2.0 * (cam.fov.to_radians() * 0.5).tan() / height.max(1) as f32;
        for discharge in self.discharges.iter().filter(|d| d.strike.ground) {
            let flash = discharge.flash();
            let distance = discharge.strike.origin.distance(cam.pos) as f32;
            let alpha = flash * opacity * (1.0 - smooth(2500.0, LIGHTNING_RANGE as f32, distance));
            if alpha < 0.004 {
                continue;
            }
            let hit = discharge.strike;
            let lift = cloud_lift();
            let lift_point = |p: DVec3| {
                let t = ((p.y - hit.end.y) / (hit.origin.y - hit.end.y).max(1.0)).clamp(0.0, 1.0);
                (p + DVec3::Y * (t * lift) - cam.pos).as_vec3()
            };
            for (i, segment) in discharge.segments.iter().enumerate() {
                let from = lift_point(segment.from);
                let to = lift_point(segment.to);
                let branch = i >= 16;
                list.push(GpuSky {
                    axes: [from.extend(0.0).to_array(), to.extend(0.0).to_array(), [0.0; 4]],
                    color: Vec3::new(0.66, 0.78, 1.0)
                        .extend(alpha * if branch { 0.66 } else { 1.0 })
                        .to_array(),
                    extra: [segment.width, pixel * if branch { 0.65 } else { 1.2 }, 0.0, 0.0],
                    ids: [u32::MAX, sky::LIGHTNING, 0, 0],
                    ..Default::default()
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_strike_arrives_first_from_the_nearest_part_of_the_channel() {
        let segment = Segment { from: DVec3::Y * 300.0, to: DVec3::ZERO, width: 1.0 };
        let listener = DVec3::new(343.0, 170.0, 0.0);
        let nearest = segment.closest(listener);
        assert!(nearest.distance(DVec3::Y * 170.0) < 1e-8);
        assert!((crate::audio::travel_time(listener.distance(nearest)) - 1.0).abs() < 1e-8);
        assert!(crate::audio::travel_time(listener.distance(segment.to)) > 1.1);
    }
}
