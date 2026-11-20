use glam::{DVec2, DVec3, IVec2, Vec2};

use crate::content::Content;

pub(crate) const CLOUD_CELL: f64 = 320.0;
pub(crate) const CLOUD_HEIGHT: f32 = 75.0;
// Square cloud columns per noise cell along each axis. Keep in sync with common.wgsl.
const CLOUD_COLUMNS: f64 = 24.0;
pub(crate) const AUTOMATIC: u32 = u32::MAX;

/// A sampleable layer shared by simulation and rendering. Its drift follows elapsed world ticks,
/// so setting the time of day does not move the weather.
#[derive(Clone, Copy)]
pub(crate) struct CloudField {
    pub seed: u32,
    pub drift: f64,
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
        let at = self.coords(xz);
        let origin = at.floor();
        for band in [2, 1, 3, 0, 4] {
            let column = cloud_column(self.seed, origin.as_ivec2(), at - origin, band);
            if column.fills_band(band) {
                return [0, self.rain, self.thunder][column.kind];
            }
        }
        0
    }
}

#[derive(Default)]
struct CloudColumn {
    cover: f32,
    kind: usize,
    detail: f32,
}

impl CloudColumn {
    fn fills_band(&self, band: i32) -> bool {
        let middle = 2.0 + 3.2 * (self.detail - 0.5);
        let thickness = 0.68 + self.cover.min(1.0) * (1.30 + self.kind as f32 * 0.19);
        self.cover > 0.0 && thickness > (band as f32 - middle).abs()
    }
}

fn cloud_column(seed: u32, origin: IVec2, at: DVec2, band: i32) -> CloudColumn {
    let shift = DVec2::new(0.375, 0.625) * f64::from(band - 2);
    let shift = shift - shift.floor();
    let center = ((at * CLOUD_COLUMNS - shift).floor() + shift + DVec2::splat(0.5)) / CLOUD_COLUMNS;
    cloud_sample(seed, origin, center.as_vec2())
}

fn cloud_hash(seed: u32, cell: IVec2) -> f32 {
    let v = seed ^ (cell.x as u32).wrapping_mul(0x9e3779b1) ^ (cell.y as u32).wrapping_mul(0x85ebca77);
    let v = (v ^ (v >> 16)).wrapping_mul(0x7feb352d);
    let v = (v ^ (v >> 15)).wrapping_mul(0x846ca68b);
    ((v ^ (v >> 16)) >> 8) as f32 / 16_777_216.0
}

fn cloud_noise(seed: u32, origin: IVec2, at: Vec2, frequency: IVec2) -> f32 {
    let scaled = at * frequency.as_vec2();
    let cell = scaled.floor();
    let grid = origin * frequency + cell.as_ivec2();
    let frac = scaled - cell;
    let weight = frac * frac * (3.0 - 2.0 * frac);
    let corner = |x, z| cloud_hash(seed, grid + IVec2::new(x, z));
    let lerp = |a, b, t| a + (b - a) * t;
    lerp(lerp(corner(0, 0), corner(1, 0), weight.x), lerp(corner(0, 1), corner(1, 1), weight.x), weight.y)
}

// Match the field in common.wgsl. Weather bands have empty margins, so a connected patch
// can't change from one weather type to another without a break in cloud cover.
fn cloud_sample(seed: u32, origin: IVec2, at: Vec2) -> CloudColumn {
    let warp = cloud_noise(seed ^ 0xa341316c, origin, at, IVec2::ONE) - 0.5;
    let warped = at + Vec2::new(0.78, 0.54) * warp;
    let coarse = cloud_noise(seed, origin, warped, IVec2::new(1, 2));
    let detail = cloud_noise(seed ^ 0x3ca1ef53, origin, warped, IVec2::new(3, 6));
    let edge = cloud_noise(seed ^ 0x5a528247, origin, warped, IVec2::new(9, 16));
    let cover = (coarse * 0.64 + detail * 0.23 + edge * 0.13 - 0.47) * 4.7;
    if cover <= 0.0 {
        return CloudColumn::default();
    }
    // Climate has twice the wavelength of the coarse cloud noise. With the
    // margins below, even diagonally adjacent columns cannot jump a band.
    let parity = origin & IVec2::ONE;
    let climate_origin = (origin - parity) / 2;
    let climate_at = (at + parity.as_vec2()) * 0.5;
    let bend = cloud_noise(seed ^ 0xab5399d3, climate_origin, climate_at, IVec2::ONE) - 0.5;
    let weather = cloud_noise(seed ^ 0xb73102ad, climate_origin, climate_at + Vec2::new(0.18, -0.10) * bend, IVec2::ONE);
    let margin = 0.051 + (0.03 * detail + 0.07 * edge - 0.025).max(0.0);
    let gap = (weather - 0.51).abs().min((weather - 0.77).abs()) - margin;
    let blend = (gap / 0.045).clamp(0.0, 1.0);
    let kind = usize::from(weather >= 0.51) + usize::from(weather >= 0.77);
    CloudColumn { cover: cover * blend * blend * (3.0 - 2.0 * blend), kind, detail: detail * 0.65 + edge * 0.35 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moving_weather_leaves_holes_and_stays_below_the_cloud_layer() {
        let cloud = CloudField { seed: 42, drift: 24.0, height: 200.0, rain: 1, thunder: 2 };
        let field = WeatherField { clouds: Some(cloud), forced: None };
        let mut seen = [false; 3];
        for z in -32..32 {
            for x in -32..32 {
                let xz = DVec2::new(x as f64 * 72.0, z as f64 * 72.0);
                let pos = DVec3::new(xz.x, 64.0, xz.y);
                let weather = field.at_pos(pos);
                seen[weather as usize] = true;
                assert_eq!(field.at_pos(DVec3::new(xz.x, cloud.height, xz.y)), 0);
                assert_eq!(WeatherField { forced: Some(1), ..field }.at_pos(pos), 1);
                assert_eq!(WeatherField { forced: Some(0), ..field }.at_pos(pos), 0);
            }
        }
        assert_eq!(seen, [true; 3]);
    }

    #[test]
    fn weather_columns_cross_lattice_lines_and_different_types_have_gaps() {
        let mut crosses_boundary = false;
        for z in -12..12 {
            let origin = IVec2::new(-5, z);
            let mut previous = None;
            for x in 0..4000 {
                let at = DVec2::new(x as f64 / 400.0, 0.45);
                let column = cloud_column(42, origin, at, 2);
                let visible = column.fills_band(2).then_some(column.kind);
                for offset in [DVec2::Y, DVec2::ONE] {
                    let other = cloud_column(42, origin, at + offset / CLOUD_COLUMNS, 2);
                    if column.fills_band(2) && other.fills_band(2) {
                        assert_eq!(column.kind, other.kind);
                    }
                }
                let mut covered_by_kind = None;
                for band in 0..5 {
                    let column = cloud_column(42, origin, at, band);
                    if column.fills_band(band) {
                        assert_eq!(*covered_by_kind.get_or_insert(column.kind), column.kind);
                    }
                }
                if let (Some(previous), Some(current)) = (previous, visible) {
                    assert_eq!(previous, current);
                    crosses_boundary |= x % 400 == 0;
                }
                previous = visible;
            }
            let from_left = cloud_sample(42, origin, Vec2::new(1.0, 0.8));
            let from_right = cloud_sample(42, origin + IVec2::X, Vec2::new(0.0, 0.8));
            assert!((from_left.cover - from_right.cover).abs() < 0.00001);
            assert_eq!(from_left.kind, from_right.kind);
        }
        assert!(crosses_boundary);
    }
}
