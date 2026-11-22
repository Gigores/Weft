use glam::{DVec2, DVec3, IVec2, IVec3};
use rustc_hash::FxHashSet;
use weft_abi::block_flags;
use weft_noise::Rng;

use super::{DimSet, Server, TPS};
use crate::content::Content;
use crate::net::{Lightning, ToClient};
use crate::weather::LIGHTNING_RANGE;
use crate::world;

const CELL: f64 = 896.0;
const PERIOD: u64 = 16;

fn horizontal(pos: DVec3) -> DVec2 {
    DVec2::new(pos.x, pos.z)
}

impl Server {
    pub(super) fn tick_lightning(&mut self, c: &Content) {
        for dim in self.dims.iter().filter(|dim| dim.time % TPS as u64 == 0) {
            let field = dim.weather_field(c, dim.time);
            let Some(cloud) = field.clouds else { continue };
            if !c.game.weathers[field.forced.unwrap_or(cloud.thunder) as usize].lightning() {
                continue;
            }
            let players: Vec<_> = self
                .players
                .iter()
                .enumerate()
                .filter(|(_, p)| p.joined && p.dim == dim.id && !p.conn.is_closed())
                .filter_map(|(i, p)| Some((i, horizontal(dim.entity(p.entity)?.pos))))
                .collect();
            let mut cells = FxHashSet::default();
            for &(_, pos) in &players {
                let low = ((pos - DVec2::splat(LIGHTNING_RANGE)) / CELL)
                    .floor()
                    .as_ivec2();
                let high = ((pos + DVec2::splat(LIGHTNING_RANGE)) / CELL)
                    .floor()
                    .as_ivec2();
                for z in low.y..=high.y {
                    for x in low.x..=high.x {
                        cells.insert(IVec2::new(x, z));
                    }
                }
            }
            let second = dim.time / TPS as u64;
            let seed = dim.seed() ^ u64::from(cloud.seed);
            for cell in cells {
                let salt = weft_noise::hash(seed, i64::from(cell.x), 0, i64::from(cell.y));
                let offset = salt % PERIOD;
                let time = second.wrapping_add(offset);
                if time % PERIOD != 0 {
                    continue;
                }
                let mut rng = Rng(weft_noise::hash(salt, (time / PERIOD) as i64, 0, 0));
                if rng.float() >= 0.47 {
                    continue;
                }
                let pos = (cell.as_dvec2() + DVec2::new(f64::from(rng.float()), f64::from(rng.float()))) * CELL;
                let Some(origin) = cloud.discharge(pos, field.forced.is_some()) else { continue };
                let nearby = |p: DVec2| p.distance_squared(horizontal(origin)) < LIGHTNING_RANGE * LIGHTNING_RANGE;
                if !players.iter().any(|&(_, p)| nearby(p)) {
                    continue;
                }
                let angle = f64::from(rng.float()) * std::f64::consts::TAU;
                let direction = DVec2::from_angle(angle);
                let strike_at = horizontal(origin) + direction * (8.0 + f64::from(rng.float()) * 36.0);
                let impact = (rng.float() < 0.28)
                    .then(|| ground_below(dim, c, strike_at, origin.y).or_else(|| distant_ground(dim, c, strike_at, origin.y)))
                    .flatten();
                let end = impact.unwrap_or_else(|| {
                    let length = 28.0 + f64::from(rng.float()) * 62.0;
                    origin + DVec3::new(direction.x * length, (f64::from(rng.float()) - 0.5) * 13.0, direction.y * length)
                });
                let strike = Lightning {
                    dim: dim.id,
                    tick: dim.time,
                    origin,
                    end,
                    ground: impact.is_some(),
                    seed: rng.next(),
                    strength: 0.8 + rng.float() * 0.38,
                };
                log::debug!(
                    "strike {} at tick {} from {} to {} (ground: {})",
                    strike.seed,
                    strike.tick,
                    strike.origin,
                    strike.end,
                    strike.ground
                );
                for &(i, pos) in &players {
                    if nearby(pos) {
                        self.players[i].conn.send(ToClient::Lightning(strike));
                    }
                }
            }
        }
    }
}

fn ground_below(dim: &DimSet, c: &Content, pos: DVec2, height: f64) -> Option<DVec3> {
    let start = IVec3::new(pos.x.floor() as i32, height.floor() as i32 - 1, pos.y.floor() as i32);
    let cube = world::cube_of(start);
    let mut loaded = false;
    for cy in (cube.y - 48..=cube.y).rev() {
        let Some(local) = dim.snapshot(IVec3::new(cube.x, cy, cube.z)) else {
            if loaded {
                return None;
            }
            continue;
        };
        loaded = true;
        let blocks = |state: u32| c.states[state as usize].flags & (block_flags::SOLID | block_flags::LIQUID) != 0;
        if local.uniform_state().is_some_and(|s| !blocks(s)) {
            continue;
        }
        let (x, z) = ((start.x & 15) as usize, (start.z & 15) as usize);
        let top = (start.y - 16 * cy).min(15) as usize;
        for y in (0..=top).rev() {
            if blocks(local.get(world::cube::index(x, y, z))) {
                return Some(DVec3::new(pos.x, f64::from(cy * 16 + y as i32 + 1) + 0.015, pos.y));
            }
        }
    }
    None
}

fn distant_ground(dim: &DimSet, c: &Content, pos: DVec2, height: f64) -> Option<DVec3> {
    let x = pos.x.floor() as i32;
    let z = pos.y.floor() as i32;
    let terrain = &c.dims[dim.id as usize];
    let ground = terrain.column(dim.seed(), x, z)?.height;
    let top = ground.max(terrain.sea_level().unwrap_or(ground));
    if top == i32::MIN || f64::from(top) + 2.0 >= height || dim.has(world::cube_of(IVec3::new(x, top, z))) {
        return None;
    }
    if top == ground && terrain.carved(dim.seed(), x, z) {
        return None;
    }
    Some(DVec3::new(pos.x, f64::from(top) + 1.0, pos.y))
}
