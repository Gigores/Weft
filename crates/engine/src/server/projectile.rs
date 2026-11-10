//! What rays hit, blocks and entities alike, and projectiles: each tick a ray along a
//! projectile's speed decides what it hits before it moves, so nothing is flown through however
//! fast, and where it sticks is where the ray met the block.

use glam::DVec3;
use weft_abi as abi;

use super::dim::Dim;
use crate::content::Content;
use crate::physics::{Aabb, Transform};

/// At most this many hits refused in one tick before a projectile just flies on.
const MAX_REFUSED: usize = 4;

impl Dim {
    /// What a ray from `from` (this dimension's frame) along `dir` hits first within `max`, of
    /// what `flags` say (`abi::ray`), past the entities in `skip`.
    pub fn raycast(&self, c: &Content, from: DVec3, dir: DVec3, max: f64, flags: u32, skip: &[u64]) -> Option<abi::RayHit> {
        let dir = dir.normalize_or_zero();
        if dir == DVec3::ZERO || !max.is_finite() {
            return None;
        }
        let mut best: Option<abi::RayHit> = None;
        if flags & (abi::ray::BLOCKS | abi::ray::LIQUIDS) != 0 {
            if let Some(h) = self.ray(c, from, dir, max, flags & abi::ray::LIQUIDS != 0) {
                let point = from + dir * h.dist;
                // On a body's block, where in that body's blocks.
                let on = match self.entities.get(h.frame).filter(|_| h.frame != 0) {
                    Some(b) => Transform::of(b.pos).to_local(self.place.to_world(point)),
                    None => point,
                };
                best = Some(abi::RayHit {
                    entity: 0,
                    body: h.frame,
                    pos: h.origin.to_array().into(),
                    state: h.state,
                    face: h.face as u32,
                    point: point.to_array().into(),
                    local: (on - h.origin.as_dvec3()).to_array().into(),
                    distance: h.dist,
                });
            }
        }
        if flags & abi::ray::ENTITIES != 0 {
            let reach = best.map_or(max, |b| b.distance);
            let mut near: Option<(f64, u8, u64, Aabb)> = None;
            for e in &self.entities.list {
                // A body with blocks is hit by those; projectiles fly past each other.
                let projectile = c.game.entities[e.kind as usize].projectile.is_some();
                if e.removed || !e.is_body() || projectile || skip.contains(&e.id) || self.bodies.contains_key(&e.id) {
                    continue;
                }
                let b = e.aabb(c);
                let b = Aabb { min: self.place.to_local(b.min), max: self.place.to_local(b.max) };
                if let Some((t, face)) = crate::physics::ray_aabb(from, dir, &b, reach) {
                    if near.is_none_or(|n| t < n.0) {
                        near = Some((t, face, e.id, b));
                    }
                }
            }
            if let Some((t, face, id, b)) = near {
                let point = from + dir * t;
                best = Some(abi::RayHit {
                    entity: id,
                    body: 0,
                    pos: abi::Vec3::from(point.to_array()).block(),
                    state: 0,
                    face: face as u32,
                    point: point.to_array().into(),
                    local: (point - b.min).to_array().into(),
                    distance: t,
                });
            }
        }
        best
    }

    /// Projectiles (`comp::entity::PROJECTILE`) move: a ray along this tick's speed first, what
    /// it hits asked (`action::PROJECTILE_HIT`) and told; an entity hit takes the damage
    /// (`chain::PROJECTILE_DAMAGE`) and the projectile goes, a block stops it there, stuck or
    /// gone. Then drag and gravity, as vanilla's arrow.
    pub(super) fn fly_projectiles(&mut self, c: &'static Content) {
        let flying: Vec<(u64, abi::Projectile)> = self
            .entities
            .list
            .iter()
            .filter(|e| !e.removed && e.item.is_empty())
            .filter_map(|e| {
                c.game.entities[e.kind as usize]
                    .projectile
                    .clone()
                    .map(|p| (e.id, p))
            })
            .collect();
        let gravity = c.dims[self.id as usize].gravity as f64;
        for (id, p) in flying {
            let gravity = gravity * self.attribute(c, id, crate::registry::attr::GRAVITY, false);
            let Some(e) = self.entities.get_mut(id) else { continue };
            if e.stuck > 0 {
                e.stuck += 1;
                if e.stuck > p.stuck_ticks {
                    e.gone(abi::removal::EXPIRED);
                }
                continue;
            }
            let v = e.vel * crate::world::TICK;
            let speed = v.length();
            let (pos, owner) = (self.place.to_local(e.pos), e.owner);
            let mut skip = vec![id];
            // Its shooter can't be hit until it flew clear of them.
            if owner != 0 && !e.left_owner {
                let clear = self.entities.get(owner).is_none_or(|o| {
                    !o.aabb(c)
                        .intersects(&self.entities.get(id).map_or(o.aabb(c), |e| e.aabb(c)))
                });
                if clear {
                    if let Some(e) = self.entities.get_mut(id) {
                        e.left_owner = true;
                    }
                } else {
                    skip.push(owner);
                }
            }
            let mut to = pos + v;
            let mut stopped = false;
            for _ in 0..MAX_REFUSED {
                let Some(h) = self.raycast(c, pos, v, speed, abi::ray::BLOCKS | abi::ray::ENTITIES, &skip) else { break };
                let q = abi::Query { entity: id, other: h.entity, pos: h.pos, state: h.state, amount: speed, ..abi::Query::DEFAULT };
                if !self.ask(c, abi::action::PROJECTILE_HIT, &q) {
                    if h.entity == 0 {
                        break;
                    }
                    skip.push(h.entity);
                    continue;
                }
                let point = DVec3::from_array(h.point.to_array());
                let hit = abi::ProjectileHit { projectile: id, target: h.entity, owner, pos: h.pos, face: h.face, point: h.point };
                self.emit_engine(c, abi::action::PROJECTILE_HIT, &hit);
                if h.entity != 0 {
                    let amount = self.compute(c, abi::chain::PROJECTILE_DAMAGE, p.damage as f64, &q, false) as f32;
                    let d = abi::Damage {
                        amount,
                        cause: abi::damage_cause::PROJECTILE,
                        source: owner,
                        direct: id,
                        knock: v.to_array().into(),
                        ..abi::Damage::DEFAULT
                    };
                    self.damage(c, h.entity, &d);
                    if let Some(e) = self.entities.get_mut(id) {
                        e.gone(abi::removal::HIT);
                    }
                } else if let Some(e) = self.entities.get_mut(id) {
                    if p.sticks {
                        // Just short of the face it went in through, as vanilla's arrow.
                        to = point - v.normalize_or_zero() * 0.05;
                        e.stuck = 1;
                    } else {
                        e.gone(abi::removal::HIT);
                    }
                }
                stopped = true;
                break;
            }
            let wet = crate::physics::Cells::liquid(&super::space::CubeCells(&self.space.cubes), c, to.floor().as_ivec3()).is_some();
            let Some(e) = self.entities.get_mut(id) else { continue };
            e.pos = self.place.to_world(to);
            let v = if stopped { DVec3::ZERO } else { v * if wet { p.liquid_drag } else { p.drag } as f64 - DVec3::Y * gravity };
            e.vel = v * crate::world::TPS as f64;
            e.in_liquid = wet;
            // Pointing where it flies.
            if v.length_squared() > 1e-8 {
                e.yaw = (-v.x).atan2(-v.z) as f32;
                e.pitch = v.y.atan2(v.with_y(0.0).length()) as f32;
            }
        }
    }
}
