use glam::{DVec2, DVec3, IVec2};

use crate::content::Content;

pub(crate) const CLOUD_CELL: f64 = 12.0;
pub(crate) const CLOUD_PERIOD: i32 = 1 << 20;
pub(crate) const AUTOMATIC: u32 = u32::MAX;

/// A sampleable layer shared by simulation and rendering. Its drift follows elapsed world ticks,
/// so setting the time of day does not move the weather.
#[derive(Clone, Copy)]
pub(crate) struct CloudField {
    pub seed: u32,
    pub drift: f64,
    pub rain_shift: f32,
    pub height: f64,
    pub rain: u16,
    pub thunder: u16,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct WeatherField {
    pub clouds: Option<CloudField>,
    pub forced: Option<u16>,
}

impl WeatherField {
    pub fn new(c: &Content, dim: u32, seed: u64, ticks: f64, forced: Option<u16>) -> Self {
        let d = &c.dims[dim as usize];
        let clouds = c.game.sky.get(dim as usize).filter(|_| d.has_weather());
        let Some(s) = clouds.and_then(|sky| {
            sky.iter()
                .rev()
                .find(|s| s.kind == weft_abi::sky_kind::CLOUDS)
        }) else {
            return Self { clouds: None, forced };
        };
        let seed = seed ^ weft_abi::fnv(d.name.as_bytes());
        let variants = weft_noise::hash(seed, ticks as i64 / 24000, 0, 0);
        let choose = |lightning: bool, roll: u32| {
            let candidates = || {
                d.weathers
                    .iter()
                    .copied()
                    .filter(|&id| c.game.weathers[id as usize].lightning() == lightning)
            };
            let total: u64 = candidates()
                .map(|id| c.game.weathers[id as usize].weight as u64)
                .sum();
            if total == 0 {
                return None;
            }
            let mut target = roll as u64 % total;
            for id in candidates() {
                let w = c.game.weathers[id as usize].weight as u64;
                if target < w {
                    return Some(id as u16);
                }
                target -= w;
            }
            None
        };
        let rain = choose(false, variants as u32).unwrap_or(0);
        let thunder = choose(true, (variants >> 32) as u32).unwrap_or(rain);
        Self {
            clouds: Some(CloudField {
                seed: (seed ^ (seed >> 32)) as u32,
                drift: ticks * s.drift as f64,
                rain_shift: (ticks / 80.0).rem_euclid(CLOUD_PERIOD as f64) as f32,
                height: s.height as f64,
                rain,
                thunder,
            }),
            forced,
        }
    }

    pub fn at(self, xz: DVec2) -> u16 {
        if let Some(id) = self.forced {
            return id;
        }
        self.clouds.map_or(0, |cloud| cloud.at(xz))
    }

    pub fn at_pos(self, pos: DVec3) -> u16 {
        if self.forced.is_none() && self.clouds.is_some_and(|cloud| pos.y >= cloud.height) {
            return 0;
        }
        self.at(DVec2::new(pos.x, pos.z))
    }
}

impl CloudField {
    pub fn coords(self, xz: DVec2) -> DVec2 {
        (xz + DVec2::new(self.drift, 0.0)) / CLOUD_CELL
    }

    fn at(self, xz: DVec2) -> u16 {
        let cell = self
            .coords(xz)
            .floor()
            .rem_euclid(DVec2::splat(CLOUD_PERIOD as f64))
            .as_ivec2();
        if noise(self.seed, cell, 8) < 0.48 {
            return 0;
        }
        let shift = self.rain_shift.floor() as i32;
        let at = cell + IVec2::new(shift, 0);
        let a = noise(self.seed ^ 0xa341316c, at, 32);
        let b = noise(self.seed ^ 0xa341316c, at + IVec2::X, 32);
        match lerp(a, b, self.rain_shift.fract()) {
            v if v >= 0.78 => self.thunder,
            v if v >= 0.56 => self.rain,
            _ => 0,
        }
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

// The same integer lattice and interpolation as cloud_noise in common.wgsl.
fn noise(seed: u32, cell: IVec2, span: i32) -> f32 {
    let period = (CLOUD_PERIOD / span - 1) as u32;
    let grid = cell.div_euclid(IVec2::splat(span));
    let f = cell.rem_euclid(IVec2::splat(span)).as_vec2() / span as f32;
    let f = f * f * (3.0 - 2.0 * f);
    let corner = |dx: i32, dz: i32| {
        let x = (grid.x + dx) as u32 & period;
        let z = (grid.y + dz) as u32 & period;
        let h = seed ^ x.wrapping_mul(0x9e3779b1) ^ z.wrapping_mul(0x85ebca77);
        let h = (h ^ (h >> 16)).wrapping_mul(0x7feb352d);
        ((h ^ (h >> 15)) >> 8) as f32 / 16777216.0
    };
    lerp(lerp(corner(0, 0), corner(1, 0), f.x), lerp(corner(0, 1), corner(1, 1), f.x), f.y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moving_weather_leaves_holes_and_stays_below_the_cloud_layer() {
        let cloud = CloudField { seed: 42, drift: 24.0, rain_shift: 9.75, height: 200.0, rain: 1, thunder: 2 };
        let field = WeatherField { clouds: Some(cloud), forced: None };
        let mut seen = [false; 3];
        for z in -32..32 {
            for x in -32..32 {
                let xz = DVec2::new(x as f64 * 72.0, z as f64 * 72.0);
                let pos = DVec3::new(xz.x, 64.0, xz.y);
                let weather = field.at_pos(pos);
                seen[weather as usize] = true;
                if weather != 0 {
                    let cell = cloud.coords(xz).floor().as_ivec2();
                    assert!(noise(cloud.seed, cell, 8) >= 0.48);
                }
                assert_eq!(field.at_pos(DVec3::new(xz.x, cloud.height, xz.y)), 0);
                assert_eq!(WeatherField { forced: Some(1), ..field }.at_pos(pos), 1);
                assert_eq!(WeatherField { forced: Some(0), ..field }.at_pos(pos), 0);
            }
        }
        assert_eq!(seen, [true; 3]);
    }
}
