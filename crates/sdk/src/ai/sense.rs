use crate::{BlockPos, EntityKind, EntityState, Ray, Vec3, World, host, look_dir};

use super::Neighbor;

/// A sphere of living bodies, filtered by kind and optionally excluding its observer.
/// During brain sampling, local queries can share candidates and their sampled positions within a pass.
/// The radius is tested at the exact origin. Use ordinary world queries to see edits made in that pass.
#[derive(Clone, Copy)]
pub struct Nearby {
    origin: Vec3,
    radius: f64,
    kind: u32,
    skip: u64,
}

impl Nearby {
    pub fn new(origin: impl Into<Vec3>, radius: f64) -> Self {
        Self { origin: origin.into(), radius, kind: u32::MAX, skip: 0 }
    }

    pub fn kind(mut self, kind: &EntityKind) -> Self {
        self.kind = kind.id();
        self
    }

    pub fn excluding(mut self, id: u64) -> Self {
        self.skip = id;
        self
    }

    fn fill(self, world: &World, out: &mut [Neighbor]) -> usize {
        unsafe { (host().nearby)(world.raw(), &self.origin, self.radius, self.kind, self.skip, out.as_mut_ptr(), out.len()) }
    }

    pub fn count(self, world: &World) -> usize {
        self.fill(world, &mut [])
    }

    pub fn visit(self, world: &World, mut visit: impl FnMut(Neighbor)) {
        let mut local = [Neighbor::default(); 32];
        let count = self.fill(world, &mut local);
        if count <= local.len() {
            local[..count].iter().copied().for_each(visit);
        } else {
            let mut all = vec![Neighbor::default(); count];
            let count = self.fill(world, &mut all);
            for &entry in all.iter().take(count) {
                visit(entry);
            }
        }
    }

    pub fn list(self, world: &World) -> Vec<Neighbor> {
        let mut list = Vec::new();
        self.visit(world, |entry| list.push(entry));
        list
    }

    pub fn nearest(self, world: &World, mut accepts: impl FnMut(Neighbor) -> bool) -> Option<Neighbor> {
        let mut nearest = None;
        let mut best = (f64::INFINITY, u64::MAX);
        self.visit(world, |entry| {
            let key = ((entry.pos - self.origin).length_squared(), entry.id);
            if key < best && accepts(entry) {
                best = key;
                nearest = Some(entry);
            }
        });
        nearest
    }
}

impl World {
    /// Block at a point sampled by a brain. Rules in one sensing or offer pass share samples.
    /// Unloaded terrain is unknown. Use `block_loaded` for live reads after an edit in that pass.
    pub fn sensed_block(&self, pos: impl Into<BlockPos>) -> Option<u32> {
        let state = unsafe { (host().sense_block)(self.raw(), &pos.into()) };
        (state != u32::MAX).then_some(state)
    }

    /// Nearest matching loaded block whose center is in the sphere. Use a short radius for routine probes.
    pub fn nearest_block(
        &self,
        origin: impl Into<Vec3>,
        radius: u8,
        mut accepts: impl FnMut(BlockPos, u32) -> bool,
    ) -> Option<(BlockPos, u32)> {
        let origin = origin.into();
        let r = f64::from(radius);
        if !origin
            .to_array()
            .iter()
            .all(|coordinate| coordinate.is_finite())
        {
            return None;
        }
        let block = |point: Vec3| BlockPos::new(point.x.floor() as i32, point.y.floor() as i32, point.z.floor() as i32);
        let lo = block(origin.offset(-r, -r, -r));
        let hi = block(origin.offset(r, r, r));
        let mut closest = None;
        let mut limit = r * r;
        for z in lo.z..=hi.z {
            for y in lo.y..=hi.y {
                for x in lo.x..=hi.x {
                    let pos = BlockPos::new(x, y, z);
                    let distance = (pos.center() - origin).length_squared();
                    if distance <= limit
                        && let Some(state) = self.sensed_block(pos)
                        && accepts(pos, state)
                    {
                        closest = Some((pos, state));
                        limit = distance;
                    }
                }
            }
        }
        closest
    }

    /// Can the entity's body travel straight to this feet position without hitting blocks in this space?
    /// Unloaded terrain is solid. Intended for local probes, with a limit of 1024 blocks per axis.
    pub fn block_path_clear(&self, id: u64, target: impl Into<Vec3>) -> bool {
        unsafe { (host().block_path_clear)(self.raw(), id, &target.into()) != 0 }
    }

    /// Whether the other body's collision box overlaps this entity's body or a named hitbox part.
    pub fn touches_hitbox(&self, entity: u64, part: Option<&str>, other: u64) -> bool {
        let part = part.map_or(0, crate::abi::hitbox_part_id);
        unsafe { (host().hitbox_touches)(self.raw(), entity, part, other) != 0 }
    }
}

/// One field of view, in degrees. Combine several profiles for multiple eyes or distinct zones.
#[derive(Clone, Copy, Debug)]
pub struct Vision {
    pub range: f64,
    pub blind_near: f64,
    pub horizontal: f64,
    pub vertical: f64,
    pub yaw_offset: f32,
    pub pitch_offset: f32,
    pub occlusion: bool,
}

impl Vision {
    pub fn new(range: f64, horizontal: f64, vertical: f64) -> Self {
        Self { range, blind_near: 0.0, horizontal, vertical, yaw_offset: 0.0, pitch_offset: 0.0, occlusion: true }
    }

    /// Test a point from the entity's eyes in world coordinates, as in brain callbacks.
    /// Range and angles are checked before casting a block ray.
    pub fn sees(self, world: &World, observer: &EntityState, point: impl Into<Vec3>) -> bool {
        let point = point.into();
        let origin = observer.eye();
        let delta = point - origin;
        let squared = delta.length_squared();
        if squared <= 0.0 || squared < self.blind_near.powi(2) || squared > self.range.powi(2) {
            return false;
        }
        let yaw = observer.yaw + self.yaw_offset.to_radians();
        let pitch = observer.pitch + self.pitch_offset.to_radians();
        let forward = dot(delta, look_dir(yaw, pitch));
        let side = dot(delta, look_dir(yaw + std::f32::consts::FRAC_PI_2, 0.0));
        let up = dot(delta, look_dir(yaw, pitch + std::f32::consts::FRAC_PI_2));
        if side.atan2(forward).to_degrees().abs() > self.horizontal * 0.5
            || up.atan2(side.hypot(forward)).to_degrees().abs() > self.vertical * 0.5
        {
            return false;
        }
        !self.occlusion
            || world
                .raycast(origin, delta, squared.sqrt(), Ray::blocks())
                .is_none()
    }
}

fn dot(a: Vec3, b: Vec3) -> f64 {
    a.x * b.x + a.y * b.y + a.z * b.z
}

/// Contact with the collision box or a specified part. Range bounds the search for other bodies'
/// origins, so callers should allow for the span of the part and the size of possible contacts.
#[derive(Clone, Copy, Debug)]
pub struct Touch<'a> {
    pub range: f64,
    pub part: Option<&'a str>,
}

impl Touch<'_> {
    pub fn nearest(self, world: &World, id: u64, state: &EntityState, mut accepts: impl FnMut(Neighbor) -> bool) -> Option<Neighbor> {
        Nearby::new(state.pos, self.range)
            .excluding(id)
            .nearest(world, |other| accepts(other) && world.touches_hitbox(id, self.part, other.id))
    }
}

/// A local smell profile. A signal fades with distance and can be sensed around an obstacle.
/// A mod supplies the origin and strength from its entities, blocks or events.
#[derive(Clone, Copy, Debug)]
pub struct Scent {
    pub range: f64,
    pub threshold: f64,
}

impl Scent {
    pub fn detect(self, observer: impl Into<Vec3>, source: impl Into<Vec3>, strength: f64) -> Option<f64> {
        if self.range <= 0.0 {
            return None;
        }
        let distance = (source.into() - observer.into()).length_squared().sqrt();
        let received = strength * (1.0 - distance / self.range);
        (distance < self.range && received > 0.0 && received >= self.threshold).then_some(received)
    }
}
