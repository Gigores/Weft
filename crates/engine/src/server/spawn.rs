use glam::{DVec3, I64Vec3};
use rustc_hash::FxHashMap;

pub(super) const MIN_DISTANCE: f64 = 24.0;

pub(super) struct Players(FxHashMap<I64Vec3, Vec<DVec3>>);

fn cell(pos: DVec3) -> I64Vec3 {
    (pos / MIN_DISTANCE).floor().as_i64vec3()
}

impl Players {
    pub fn new(positions: impl Iterator<Item = DVec3>) -> Self {
        let mut cells = FxHashMap::<_, Vec<_>>::default();
        for pos in positions {
            cells.entry(cell(pos)).or_default().push(pos);
        }
        Self(cells)
    }

    pub fn allows(&self, pos: DVec3) -> bool {
        let key = cell(pos);
        for x in -1..=1 {
            for y in -1..=1 {
                for z in -1..=1 {
                    if self
                        .0
                        .get(&(key + I64Vec3::new(x, y, z)))
                        .is_some_and(|players| {
                            players
                                .iter()
                                .any(|p| p.distance_squared(pos) < MIN_DISTANCE * MIN_DISTANCE)
                        })
                    {
                        return false;
                    }
                }
            }
        }
        true
    }
}
