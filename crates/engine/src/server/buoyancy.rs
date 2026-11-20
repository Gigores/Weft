use glam::{BVec3, DQuat, DVec3, IVec2, IVec3};
use rustc_hash::{FxHashMap, FxHashSet};
use weft_abi::{self as abi, entry_kind as ek};

use super::dim::Dim;
use super::entity::{BodyMotion, Completion, Entities, Motion};
use super::space::{ColliderGroup, CubeCells, Space};
use super::structure::{MountedPart, StructurePeer, visit_structure_shapes};
use crate::content::Content;
use crate::net::codec::Wire;
use crate::physics::{Aabb, BoxContact, Cells, Cylinder, Solid, Transform};
use crate::world::{DIRS, TICK};

const LAND_DRIVE_SHARE: f64 = 0.25;
const LAND_DRAG: f64 = 2.0;
const AIR_DRAG: f64 = 0.008;
const SUPPORT_REACH: f64 = 0.08;
const CONTACT_SKIN: f64 = 0.00015;
const ROLLING_RESISTANCE: f64 = 0.04;

pub(super) struct Ship {
    pub spin: DVec3,
    forces: (DVec3, DVec3),
    pub front: u8,
    pub(super) controls: Option<abi::ShipControls>,
    geometry: Option<Hull>,
    mounted_hulls: FxHashMap<u64, Hull>,
    floater_axes: BVec3,
    contact_mass: Option<ShipMass>,
}

impl Ship {
    pub fn new(front: u8, spin: DVec3) -> Self {
        Self {
            spin,
            forces: Default::default(),
            front,
            controls: None,
            geometry: None,
            mounted_hulls: FxHashMap::default(),
            floater_axes: BVec3::FALSE,
            contact_mass: None,
        }
    }

    pub(super) fn flooded_cells(&self) -> impl Iterator<Item = IVec3> + '_ {
        self.geometry.iter().flat_map(|hull| {
            hull.air
                .iter()
                .zip(&hull.flooded)
                .filter_map(|(cell, &wet)| wet.then_some(cell.pos))
        })
    }
}

#[derive(Default)]
struct Propulsion {
    force: DVec3,
    moment: DVec3,
    powered: bool,
    floater_velocity: DVec3,
    floater_axes: BVec3,
}

struct Hull {
    version: u64,
    air: Vec<AirCell>,
    flooded: Vec<bool>,
    mass: f64,
    center: DVec3,
    inertia: DVec3,
    radius: f64,
}

pub(super) fn structure_mass(c: &Content, space: &Space) -> f64 {
    *space.mass.get_or_init(|| Hull::new(c, space).mass)
}

#[derive(Clone, Copy)]
struct ShipMass {
    weight: f64,
    center: DVec3,
    inertia: DVec3,
    radius: f64,
}

impl ShipMass {
    fn of(c: &Content, parts: &[MountedPart<'_>], root: &Hull, mounted: &FxHashMap<u64, Hull>) -> Self {
        if parts.len() == 1 {
            return Self { weight: root.mass, center: root.center, inertia: root.inertia, radius: root.radius };
        }
        let hulls = || {
            parts.iter().enumerate().map(|(i, part)| {
                let hull = if i == 0 { root } else { &mounted[&part.id] };
                (part, hull, part.relative.to_world(hull.center))
            })
        };
        let weight = hulls().map(|(_, h, _)| h.mass).sum::<f64>();
        let center = hulls().map(|(_, h, p)| p * h.mass).sum::<DVec3>() / weight;
        let mut inertia = DVec3::ZERO;
        let mut radius = 1.0_f64;
        for (part, hull, p) in hulls() {
            let r = p - center;
            let rotation = glam::DMat3::from_quat(part.relative.rotation());
            let squared = glam::DMat3::from_cols(
                rotation.x_axis * rotation.x_axis,
                rotation.y_axis * rotation.y_axis,
                rotation.z_axis * rotation.z_axis,
            );
            inertia += squared * hull.inertia + (DVec3::splat(r.length_squared()) - r * r) * hull.mass;
            if let Some(bounds) = part.space.colliders(c).bounds {
                let bounds = part.relative.box_to_world(bounds);
                radius = radius.max(
                    (bounds.min - center)
                        .abs()
                        .max((bounds.max - center).abs())
                        .length(),
                );
            }
        }
        Self { weight, center, inertia: inertia.max(DVec3::splat(0.01)), radius }
    }
}

#[derive(Clone, Copy, Default)]
struct VelocityChange {
    linear: DVec3,
    angular: DVec3,
}

#[derive(Clone, Copy)]
struct MovingSurface {
    root: u64,
    place: Transform,
    local_bounds: Aabb,
    swept: Aabb,
    center: DVec3,
    velocity: DVec3,
    spin: DVec3,
    travel: VelocityChange,
    mass: Option<ShipMass>,
    solved: Option<SolvedSurface>,
}

#[derive(Clone, Copy)]
struct SolvedSurface {
    place: Transform,
    center: DVec3,
    velocity: DVec3,
    spin: DVec3,
}

impl MovingSurface {
    fn center_at(self, time: f64) -> DVec3 {
        self.solved
            .map_or_else(|| self.center + self.travel.linear * time, |end| self.center.lerp(end.center, time / TICK))
    }

    fn place_at(self, time: f64) -> Transform {
        if time <= 0.0 || self.solved.is_none() && self.travel.linear == DVec3::ZERO && self.travel.angular == DVec3::ZERO {
            return self.place;
        }
        let rotation = self.solved.map_or_else(
            || (DQuat::from_scaled_axis(self.travel.angular * time) * self.place.rotation()).normalize(),
            |end| {
                self.place
                    .rotation()
                    .slerp(end.place.rotation(), time / TICK)
            },
        );
        let anchor = self.place.position() + DVec3::Y * 0.5;
        let anchor = self.center_at(time) + (rotation * self.place.rotation().conjugate()) * (anchor - self.center);
        Transform::rotated(anchor - DVec3::Y * 0.5, rotation)
    }

    fn patch_velocity(self, point: DVec3, time: f64) -> DVec3 {
        let (vel, spin) = self
            .solved
            .map_or((self.velocity, self.spin), |s| (s.velocity, s.spin));
        vel + spin.cross(point - self.center_at(time))
    }

    fn turns(self) -> bool {
        self.solved.map_or_else(
            || self.travel.angular.length_squared() > 1e-8,
            |s| {
                (s.place.rotation() * self.place.rotation().conjugate())
                    .to_scaled_axis()
                    .length_squared()
                    > 1e-10
            },
        )
    }
}

type ContactFrames = FxHashMap<u64, MovingSurface>;
type Reactions = FxHashMap<u64, Vec<(f64, VelocityChange)>>;

#[derive(Clone, Copy)]
struct ContactPeer {
    id: u64,
    point: DVec3,
    time: f64,
}

impl ContactPeer {
    fn velocity(self, frames: &ContactFrames, reactions: &Reactions) -> DVec3 {
        let Some(surface) = frames.get(&self.id) else { return DVec3::ZERO };
        let velocity = surface.patch_velocity(self.point, self.time);
        let Some(changes) = reactions.get(&surface.root) else { return velocity };
        let center = frames
            .get(&surface.root)
            .unwrap_or(surface)
            .center_at(self.time);
        changes
            .iter()
            .filter(|(time, _)| *time <= self.time + 1e-7)
            .fold(velocity, |v, (_, change)| v + change.linear + change.angular.cross(self.point - center))
    }

    fn inverse_mass(self, frames: &ContactFrames, direction: DVec3) -> f64 {
        let Some(root) = frames.get(&self.id).and_then(|s| frames.get(&s.root)) else { return 0.0 };
        root.mass.map_or(0.0, |mass| {
            let torque = (self.point - root.center_at(self.time)).cross(direction);
            mass.weight.recip() + torque.dot(angular_response(&mass, root.place_at(self.time).rotation(), torque))
        })
    }

    fn impart(self, frames: &ContactFrames, reactions: &mut Reactions, impulse: DVec3) {
        let Some(root) = frames.get(&self.id).and_then(|s| frames.get(&s.root)) else { return };
        let Some(mass) = root.mass else { return };
        if impulse == DVec3::ZERO {
            return;
        }
        let entries = reactions.entry(root.root).or_default();
        if entries
            .last()
            .is_none_or(|(time, _)| (self.time - time).abs() > 1e-7)
        {
            entries.push((self.time, VelocityChange::default()));
        }
        let (_, change) = entries.last_mut().unwrap();
        change.linear -= impulse / mass.weight;
        let torque = (self.point - root.center_at(self.time)).cross(impulse);
        change.angular -= angular_response(&mass, root.place_at(self.time).rotation(), torque);
    }
}

struct AirCell {
    pos: IVec3,
    neighbors: [Option<usize>; 6],
    openings: u8,
}

fn cube(p: IVec3) -> Aabb {
    let min = p.as_dvec3();
    Aabb { min, max: min + DVec3::ONE }
}

/// Candidate air has walls in both horizontal directions and a floor below it.
fn hull_air(seals: &FxHashSet<IVec3>) -> Vec<AirCell> {
    let line = |p: IVec3, axis: usize| IVec2::new(p[(axis + 1) % 3], p[(axis + 2) % 3]);
    let mut ends: [FxHashMap<IVec2, (i32, i32)>; 3] = std::array::from_fn(|_| FxHashMap::default());
    for &p in seals {
        for (axis, lines) in ends.iter_mut().enumerate() {
            let v = p[axis];
            let (lo, hi) = lines.entry(line(p, axis)).or_insert((v, v));
            *lo = (*lo).min(v);
            *hi = (*hi).max(v);
        }
    }
    let axis = [0, 2]
        .into_iter()
        .min_by_key(|&a| {
            ends[a]
                .values()
                .map(|&(lo, hi)| i64::from(hi) - i64::from(lo))
                .sum::<i64>()
        })
        .unwrap();
    let mut candidates = Vec::new();
    for (&other, &(lo, hi)) in &ends[axis] {
        let mut p = IVec3::ZERO;
        p[(axis + 1) % 3] = other.x;
        p[(axis + 2) % 3] = other.y;
        for v in lo + 1..hi {
            p[axis] = v;
            if !seals.contains(&p)
                && ends[2 - axis]
                    .get(&line(p, 2 - axis))
                    .is_some_and(|&(lo, hi)| p[2 - axis] > lo && p[2 - axis] < hi)
                && ends[1]
                    .get(&line(p, 1))
                    .is_some_and(|&(floor, _)| p.y > floor)
            {
                candidates.push(p);
            }
        }
    }
    candidates.sort_unstable_by_key(|p| p.to_array());
    let index: FxHashMap<_, _> = candidates
        .iter()
        .copied()
        .enumerate()
        .map(|(i, p)| (p, i))
        .collect();
    candidates
        .into_iter()
        .map(|pos| {
            let mut openings = 0;
            let neighbors = std::array::from_fn(|face| {
                let adjacent = pos + DIRS[face];
                let next = index.get(&adjacent).copied();
                if next.is_none() && !seals.contains(&adjacent) {
                    openings |= 1 << face;
                }
                next
            });
            AirCell { pos, neighbors, openings }
        })
        .collect()
}

fn dry_boxes(air: &[AirCell], flooded: &[bool]) -> Vec<(IVec3, IVec3)> {
    let dry: Vec<_> = air
        .iter()
        .zip(flooded)
        .filter_map(|(a, &wet)| (!wet).then_some(a.pos))
        .collect();
    let mut left: FxHashSet<_> = dry.iter().copied().collect();
    let mut boxes = Vec::new();
    for lo in dry {
        if !left.contains(&lo) {
            continue;
        }
        let mut hi = lo + IVec3::ONE;
        while left.contains(&IVec3::new(lo.x, lo.y, hi.z)) {
            hi.z += 1;
        }
        while (lo.z..hi.z).all(|z| left.contains(&IVec3::new(lo.x, hi.y, z))) {
            hi.y += 1;
        }
        while (lo.y..hi.y).all(|y| (lo.z..hi.z).all(|z| left.contains(&IVec3::new(hi.x, y, z)))) {
            hi.x += 1;
        }
        for x in lo.x..hi.x {
            for y in lo.y..hi.y {
                for z in lo.z..hi.z {
                    left.remove(&IVec3::new(x, y, z));
                }
            }
        }
        boxes.push((lo, hi));
    }
    boxes
}

impl Hull {
    fn new(c: &Content, space: &Space) -> Self {
        let density = c.components.id(abi::comp::block::DENSITY);
        let mut masses = Vec::new();
        let mut seals = FxHashSet::default();
        for &(p, state) in space.block_snapshot().iter() {
            for part in space.layout(p).residents(state) {
                let st = part.state;
                let info = &c.states[st as usize];
                if info.liquid.is_some() {
                    continue;
                }
                let d = c
                    .block_value::<f32>(st, density)
                    .filter(|n| n.is_finite() && *n > 0.0)
                    .map_or(1.45, f64::from);
                let min = p.as_dvec3() + part.shift();
                if info.collision.is_empty() {
                    masses.push((min + DVec3::splat(0.5), d * 0.04));
                }
                for solid in crate::physics::block_solids(min, info) {
                    let shape = solid.bounds();
                    let volume = solid.volume();
                    let (lo, hi) = ((shape.min - 1e-8).ceil().as_ivec3(), (shape.max + 1e-8).floor().as_ivec3());
                    if matches!(solid, crate::physics::Solid::Box(_)) {
                        for x in lo.x..hi.x {
                            for y in lo.y..hi.y {
                                for z in lo.z..hi.z {
                                    seals.insert(IVec3::new(x, y, z));
                                }
                            }
                        }
                    }
                    masses.push(((shape.min + shape.max) * 0.5, d * volume));
                }
            }
        }
        let mass = masses.iter().map(|(_, m)| m).sum::<f64>().max(0.01);
        let center = masses.iter().map(|(p, m)| *p * *m).sum::<DVec3>() / mass;
        let inertia = masses
            .iter()
            .map(|(p, m)| {
                let r = *p - center;
                (DVec3::splat(r.length_squared() + 1.0 / 6.0) - r * r) * *m
            })
            .sum::<DVec3>()
            .max(DVec3::splat(0.01));
        let radius = space
            .colliders(c)
            .groups
            .iter()
            .flat_map(|g| g.shapes.iter().map(|s| s.bounds()))
            .map(|b| (b.min - center).abs().max((b.max - center).abs()).length())
            .fold(1.0, f64::max);
        let air = hull_air(&seals);
        let flooded = vec![false; air.len()];
        Self { version: space.geometry_version, air, flooded, mass, center, inertia, radius }
    }

    fn flood(&mut self, c: &Content, world: &impl Cells, place: Transform) -> bool {
        let mut flooded = vec![false; self.air.len()];
        let mut open = Vec::new();
        let downhill = -place.vector_to_local(DVec3::Y).signum() * 0.48;
        let touches_water = |p: IVec3, face: IVec3| {
            let f = face.as_dvec3();
            let center = p.as_dvec3() + DVec3::splat(0.5) + f * 0.501;
            let tangent = DVec3::ONE - f.abs();
            let low = downhill * tangent;
            let wet = |p| crate::physics::liquid_at(c, world, place.to_world(p)).is_some_and(|id| !c.liquids[id as usize].rises);
            wet(center + low) || wet(center)
        };
        for (i, cell) in self.air.iter().enumerate().filter(|(_, a)| a.openings != 0) {
            if DIRS
                .iter()
                .enumerate()
                .any(|(face, &d)| cell.openings & (1 << face) != 0 && touches_water(cell.pos, d))
            {
                flooded[i] = true;
                open.push(i);
            }
        }
        while let Some(i) = open.pop() {
            let cell = &self.air[i];
            for (face, next) in cell.neighbors.iter().enumerate() {
                if let Some(j) = *next
                    && !flooded[j]
                    && touches_water(cell.pos, DIRS[face])
                {
                    flooded[j] = true;
                    open.push(j);
                }
            }
        }
        let changed = self.flooded != flooded;
        self.flooded = flooded;
        changed
    }

    fn submerged(&self, c: &Content, cells: &impl Cells, solid: &[ColliderGroup], place: Transform, center: DVec3) -> (f64, DVec3) {
        let mut total = 0.0;
        let mut moment = DVec3::ZERO;
        let dry = self
            .air
            .iter()
            .zip(&self.flooded)
            .filter_map(|(a, &wet)| (!wet).then_some(crate::physics::Solid::Box(cube(a.pos))));
        for shape in solid
            .iter()
            .flat_map(|g| g.shapes.iter().copied())
            .chain(dry)
        {
            let b = shape.bounds();
            let size = b.max - b.min;
            let volume = shape.volume();
            let span = shape.world_bounds(place);
            let height = (span.max.y - span.min.y).max(1e-6);
            let mid = (b.min + b.max) * 0.5;
            for (x, z) in [(-1.0, -1.0), (-1.0, 1.0), (1.0, -1.0), (1.0, 1.0)] {
                let p = place.to_world(mid + DVec3::new(x * size.x * 0.25, 0.0, z * size.z * 0.25));
                let low = p.y - height * 0.5;
                let high = p.y + height * 0.5;
                let at = p.floor().as_ivec3();
                for y in low.floor() as i32..=high.floor() as i32 {
                    let Some((liquid, surface)) = crate::physics::liquid_surface(c, cells, at.with_y(y)) else { continue };
                    if c.liquids[liquid as usize].rises {
                        continue;
                    }
                    let bottom = low.max(y as f64);
                    let top = high.min(surface);
                    let submerged = (top - bottom).max(0.0) * volume / (height * 4.0);
                    total += submerged;
                    if submerged > 0.0 {
                        moment += (p.with_y((top + bottom) * 0.5) - center).cross(DVec3::Y * submerged);
                    }
                }
            }
        }
        (total, moment)
    }
}

impl Dim {
    pub(crate) fn set_ship_controls(&mut self, id: u64, input: Option<abi::ShipControls>) -> bool {
        let Some(ship) = self
            .entities
            .get_mut(id)
            .filter(|e| !e.removed && e.motion != Motion::Parked)
            .and_then(|e| e.ship.as_mut())
        else {
            return false;
        };
        if input.is_some_and(|i| !i.throttle.is_finite() || !i.steering.is_finite()) {
            return false;
        }
        ship.controls = input.map(|i| abi::ShipControls { throttle: i.throttle.clamp(-1.0, 1.0), steering: i.steering.clamp(-1.0, 1.0) });
        true
    }

    pub(crate) fn ship_flooded_cells(&self, id: u64) -> Option<impl Iterator<Item = IVec3> + '_> {
        let e = self
            .entities
            .get(id)
            .filter(|e| !e.removed && e.motion != Motion::Parked)?;
        Some(e.ship.as_ref()?.flooded_cells())
    }

    pub(crate) fn enable_dynamics(&mut self, id: u64, front: Option<u8>) -> bool {
        let Some(space) = self.bodies.get_mut(&id) else { return false };
        let Some(e) = self
            .entities
            .get_mut(id)
            .filter(|e| !e.removed && e.mount.is_none() && e.frame.is_none() && e.motion != Motion::Parked)
        else {
            return false;
        };
        if let Some(ship) = &mut e.ship {
            if let Some(front) = front {
                ship.front = front;
            }
            return true;
        }
        e.ship = Some(Box::new(Ship::new(front.unwrap_or(4), DVec3::ZERO)));
        e.motion = Motion::Free;
        if e.completion == Completion::Dock {
            e.completion = Completion::Stay;
        }
        space.track_mounted_blocks();
        true
    }

    pub(crate) fn body_force(&mut self, id: u64, at: DVec3, force: DVec3) -> bool {
        let Some(ship) = self
            .entities
            .get_mut(id)
            .filter(|e| e.body_motion() == BodyMotion::Dynamic)
            .and_then(|e| e.ship.as_mut())
        else {
            return false;
        };
        let (linear, angular) = (ship.forces.0 + force, ship.forces.1 + at.cross(force));
        if !(linear.length_squared().is_finite() && angular.length_squared().is_finite()) {
            return false;
        }
        ship.forces = (linear, angular);
        true
    }

    pub(super) fn set_helm(&mut self, c: &Content, id: u64, at: IVec3) {
        self.entities
            .comps
            .insert((id, c.game.fields.helm), at.to_bytes().into_boxed_slice());
    }

    pub(super) fn active_helm(&self, c: &Content, id: u64) -> Option<(IVec3, u8)> {
        let e = self.entities.get(id)?;
        e.ship.as_ref()?;
        let space = self.bodies.get(&id)?;
        let at = IVec3::from_bytes(self.entities.comps.get(&(id, c.game.fields.helm))?)?;
        let signal = c.states[space.get(at)? as usize].signal;
        (signal.flags as u32 & abi::signal_flags::HELM != 0).then_some((at, signal.front))
    }

    fn contact_frames(&self, c: &Content, before: &FxHashMap<u64, Transform>, children: &FxHashMap<u64, Vec<u64>>) -> ContactFrames {
        let mut masses = FxHashMap::default();
        for (&id, space) in &self.bodies {
            let Some(e) = self.entities.get(id) else { continue };
            let Some(ship) = e
                .ship
                .as_ref()
                .filter(|_| e.body_motion() == BodyMotion::Dynamic)
            else {
                continue;
            };
            let mass = ship.contact_mass.unwrap_or_else(|| {
                let hull = Hull::new(c, space);
                let parts = self.mounted_parts(id, e.body_place(), children);
                let attached = parts
                    .iter()
                    .skip(1)
                    .map(|p| (p.id, Hull::new(c, p.space)))
                    .collect();
                ShipMass::of(c, &parts, &hull, &attached)
            });
            masses.insert(id, mass);
        }
        self.bodies
            .iter()
            .filter_map(|(&id, space)| {
                let e = self.entities.get(id)?;
                if e.motion == Motion::Parked {
                    return None;
                }
                let root = self.entities.follows(e);
                let place = e.body_place();
                let mass = masses.get(&root.id).copied();
                let root_center = root
                    .body_place()
                    .to_world(mass.map_or(DVec3::splat(0.5), |m| m.center));
                let (mut center, mut spin) = (
                    root_center,
                    root.ship
                        .as_ref()
                        .filter(|_| mass.is_some())
                        .map_or(DVec3::ZERO, |s| s.spin),
                );
                let mut velocity = match root.motion {
                    Motion::To { to, left } => (to - root.pos) / (f64::from(left.max(1)) * TICK),
                    _ => root.vel,
                };
                let mut travel = VelocityChange { linear: velocity, angular: spin };
                if e.id != root.id || e.mount.is_some() || root.grab.is_some() {
                    if e.id != root.id {
                        center = place.to_world(DVec3::splat(0.5));
                        velocity += spin.cross(center - root_center);
                        travel.linear = velocity;
                    }
                    // The joint already moved in advance_mounts. Its velocity drives friction,
                    // but its transform should not be extrapolated for a second tick.
                    if let Some(previous) = before.get(&id) {
                        velocity += (center - previous.to_world(DVec3::splat(0.5))) / TICK;
                        spin += (place.rotation() * previous.rotation().conjugate()).to_scaled_axis() / TICK;
                    }
                }
                let local_bounds = space.colliders(c).bounds?;
                let area = place.box_to_world(local_bounds);
                let far = (area.min - center)
                    .abs()
                    .max((area.max - center).abs())
                    .length();
                let padding = DVec3::splat(travel.angular.length() * far * TICK);
                let swept = Aabb { min: area.min - padding, max: area.max + padding }.swept(travel.linear * TICK);
                Some((id, MovingSurface { root: root.id, place, local_bounds, swept, center, velocity, spin, travel, mass, solved: None }))
            })
            .collect()
    }

    pub(super) fn advance_ships(&mut self, c: &'static Content, before: &FxHashMap<u64, Transform>, children: &FxHashMap<u64, Vec<u64>>) {
        let mut ships: Vec<_> = self
            .bodies
            .keys()
            .copied()
            .filter(|id| {
                self.entities
                    .get(*id)
                    .is_some_and(|e| e.body_motion() == BodyMotion::Dynamic)
            })
            .collect();
        ships.sort_unstable();
        if ships.is_empty() {
            return;
        }
        let mut frames = self.contact_frames(c, before, children);
        let mut reactions = Reactions::default();
        let watch_hits = c
            .game
            .event_names
            .get(abi::event::SHIP_COLLISION)
            .is_some_and(|&event| !c.game.events[event as usize].handlers.is_empty());
        for id in ships {
            if self.docking(id) {
                if children.contains_key(&id) {
                    self.stop_mounted_children(id, children);
                } else if self.try_dock(c, id) {
                    continue;
                }
            }
            let helm = self.active_helm(c, id);
            self.update_helm_outputs(c, id, helm.map(|(at, _)| at));
            if helm.is_none()
                && self
                    .entities
                    .comps
                    .get(&(id, c.game.fields.helm))
                    .is_some_and(|bytes| !bytes.is_empty())
            {
                self.entities
                    .comps
                    .insert((id, c.game.fields.helm), Box::default());
            }
            let Some(e) = self.entities.get_mut(id) else { continue };
            let old = e.body_place();
            let vel = e.vel;
            let mut incoming = reactions.remove(&id).unwrap_or_default();
            incoming.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut ship = e.ship.take().unwrap();
            let Some(space) = self.bodies.get(&id) else { continue };
            let parts = self.mounted_parts(id, old, children);
            let mounted: FxHashSet<_> = parts.iter().skip(1).map(|p| p.id).collect();
            ship.mounted_hulls.retain(|id, _| mounted.contains(id));
            let world = CubeCells(&self.space.cubes);
            let mut dry = Vec::new();
            for part in &parts {
                let mut slot = if part.id == id { ship.geometry.take() } else { ship.mounted_hulls.remove(&part.id) };
                let edited = slot
                    .as_ref()
                    .is_none_or(|h| h.version != part.space.geometry_version);
                if edited {
                    slot = Some(Hull::new(c, part.space));
                }
                let mut hull = slot.unwrap();
                if hull.flood(c, &world, part.relative.in_frame(old)) || edited {
                    dry.push((part.id, dry_boxes(&hull.air, &hull.flooded)));
                }
                if part.id == id {
                    ship.geometry = Some(hull);
                } else {
                    ship.mounted_hulls.insert(part.id, hull);
                }
            }
            let hull = ship.geometry.as_ref().unwrap();
            let mass = ShipMass::of(c, &parts, hull, &ship.mounted_hulls);
            ship.contact_mass = Some(mass);
            let (force, torque) = std::mem::take(&mut ship.forces);
            let mut propulsion = Propulsion {
                force,
                moment: torque - mass.center.cross(force),
                powered: force != DVec3::ZERO || torque != DVec3::ZERO,
                ..Propulsion::default()
            };
            for part in &parts {
                for (at, force) in part.space.signals.thrusters(c) {
                    let force = part.relative.vector_to_world(force);
                    propulsion.force += force;
                    let at = part.relative.to_world(at.as_dvec3() + DVec3::splat(0.5));
                    propulsion.moment += (at - mass.center).cross(force);
                    propulsion.powered = true;
                }
            }
            if let Some(floater) = space.signals.floater_drive(c) {
                let active = floater.velocity.cmpne(DVec3::ZERO);
                propulsion.floater_axes = if floater.hold { active | ship.floater_axes } else { active };
                propulsion.floater_velocity = floater.velocity;
                ship.floater_axes = active;
            }
            if let Some((_, front)) = helm {
                ship.front = front;
            }
            let (drive, steer) = ship.controls.map_or_else(
                || if helm.is_some() { self.ship_input(c, id) } else { (0.0, 0.0) },
                |input| (f64::from(input.throttle), -f64::from(input.steering)),
            );
            let mut collisions = Vec::new();
            let mut joint_loads = Vec::new();
            let mut rolling = Vec::new();
            let has_springs = parts.iter().skip(1).any(|p| {
                self.entities
                    .get(p.id)
                    .and_then(|e| e.mount.as_ref())
                    .is_some_and(|m| m.slide.is_some())
            });
            let (place, vel, spin, grounded) = self.step_ship(
                c,
                id,
                &mass,
                hull,
                &ship.mounted_hulls,
                &parts,
                ShipStep { old, vel, spin: ship.spin },
                ship.front,
                drive,
                steer,
                propulsion,
                before,
                &frames,
                &mut reactions,
                &incoming,
                &mut rolling,
                watch_hits.then_some(&mut collisions),
                has_springs.then_some(&mut joint_loads),
            );
            let child_order: Vec<_> = parts.iter().skip(1).map(|p| p.id).collect();
            for (part, boxes) in dry {
                if let Some(space) = self.bodies.get_mut(&part) {
                    space.dry_changed = boxes != space.dry_boxes;
                    space.dry_boxes = boxes;
                }
            }
            if let Some(e) = self.entities.get_mut(id) {
                e.set_body_pose(place);
                e.vel = vel;
                e.on_ground = grounded;
                ship.spin = spin;
                e.ship = Some(ship);
            }
            for &child in &child_order {
                self.entities.update_mount_pose(child);
                self.roll_freewheel(c, child, &rolling, children);
            }
            if let Some(root) = frames.get(&id).copied() {
                let center = place.to_world(mass.center);
                let turn = place.rotation() * root.place.rotation().conjugate();
                for part in std::iter::once(id).chain(child_order) {
                    let Some(surface) = frames.get_mut(&part) else { continue };
                    let Some(place) = self.entities.get(part).map(|e| e.body_place()) else { continue };
                    let joint_vel = surface.velocity - root.velocity - root.spin.cross(surface.center - root.center);
                    let joint_spin = surface.spin - root.spin;
                    let part_center = if id == part { center } else { place.to_world(DVec3::splat(0.5)) };
                    surface.solved = Some(SolvedSurface {
                        place,
                        center: part_center,
                        velocity: vel + spin.cross(part_center - center) + turn * joint_vel,
                        spin: spin + turn * joint_spin,
                    });
                    let bounds = place.box_to_world(surface.local_bounds);
                    surface.swept.min = surface.swept.min.min(bounds.min);
                    surface.swept.max = surface.swept.max.max(bounds.max);
                }
            }
            self.load_springs(c, id, children, joint_loads);
            for collision in collisions {
                self.emit_engine(0, c, abi::event::SHIP_COLLISION, &collision);
            }
        }
        for (id, changes) in reactions {
            if let Some(e) = self.entities.get_mut(id)
                && e.body_motion() == BodyMotion::Dynamic
                && let Some(ship) = &mut e.ship
            {
                for (_, change) in changes {
                    e.vel += change.linear;
                    ship.spin += change.angular;
                }
            }
        }
    }

    fn roll_freewheel(&mut self, c: &Content, child: u64, rolling: &[(u64, f64)], children: &FxHashMap<u64, Vec<u64>>) {
        if children.contains_key(&child) {
            return;
        }
        let Some(mount) = self
            .entities
            .get_mut(child)
            .and_then(|e| e.mount.as_deref_mut())
        else {
            return;
        };
        let Some(drive) = mount
            .drive
            .filter(|d| d.rule.motion == crate::controller::Motion::Freewheel)
        else {
            mount.coast_rate = 0.0;
            return;
        };
        let axis = DIRS[mount.face as usize].as_dvec3();
        if !self
            .bodies
            .get(&child)
            .is_some_and(|space| space.coaxial_tire(c, axis))
        {
            return;
        }
        let limit = f64::from(drive.rule.speed).to_radians();
        mount.coast_rate = rolling
            .iter()
            .find(|(id, _)| *id == child)
            .map_or(mount.coast_rate, |(_, rate)| *rate)
            .clamp(-limit, limit);
        if mount.coast_rate == 0.0 {
            return;
        }
        mount.angle = (mount.angle + mount.coast_rate * TICK).rem_euclid(std::f64::consts::TAU);
        mount.last_grid = (mount.angle / std::f64::consts::FRAC_PI_2).round() * std::f64::consts::FRAC_PI_2;
        log::trace!(target: "physics", "wheel {child}: angle={:.3} degrees, roll={:.3} degrees/s",
            mount.angle.to_degrees(), mount.coast_rate.to_degrees());
        self.entities.update_mount_pose(child);
    }

    fn load_springs(&mut self, c: &Content, root: u64, children: &FxHashMap<u64, Vec<u64>>, contacts: Vec<(u64, DVec3)>) {
        let mut impulses: FxHashMap<u64, f64> = FxHashMap::default();
        for (mut part, impulse) in contacts {
            while part != root {
                let Some(e) = self.entities.get(part) else { break };
                if let Some(m) = e.mount.as_ref().filter(|m| m.slide.is_some()) {
                    let axis = e
                        .body_place()
                        .vector_to_world(DIRS[m.face as usize].as_dvec3());
                    *impulses.entry(part).or_default() += impulse.dot(axis);
                }
                let Some(parent) = e.frame else { break };
                part = parent;
            }
        }
        for (joint, impulse) in impulses {
            let Some(e) = self.entities.get(joint) else { continue };
            let mass: f64 = self
                .mounted_parts(joint, e.body_place(), children)
                .iter()
                .map(|part| structure_mass(c, part.space))
                .sum();
            if let Some(slide) = self
                .entities
                .get_mut(joint)
                .and_then(|e| e.mount.as_mut())
                .and_then(|m| m.slide.as_mut())
            {
                slide.velocity += impulse / mass.max(0.01);
            }
        }
    }

    pub(super) fn try_dock(&mut self, c: &'static Content, id: u64) -> bool {
        self.finish_moving_here(id, c);
        let Some(e) = self.entities.get(id) else { return false };
        let Some(space) = self.bodies.get(&id) else { return false };
        let ahead = e.body_place().vector_to_world(DVec3::Z);
        let quarter = std::f64::consts::FRAC_PI_2;
        let yaw = (ahead.x.atan2(ahead.z) / quarter).round() * quarter;
        let rotation = DQuat::from_rotation_y(yaw);
        let pos = Transform::of(e.pos).cell(None).as_dvec3() + Transform::FEET;
        let place = Transform::rotated(pos, rotation);
        if !self.mount_fits(0, c, place, space) {
            return false;
        }
        if let Some((lo, hi)) = space.bounds {
            let area = place.box_to_world(Aabb { min: lo.as_dvec3(), max: (hi + IVec3::ONE).as_dvec3() });
            let other_here = self.bodies.iter().any(|(&other, b)| {
                if other == id {
                    return false;
                }
                let Some(e) = self
                    .entities
                    .get(other)
                    .filter(|e| e.motion != Motion::Parked)
                else {
                    return false;
                };
                b.bounds.is_some_and(|(lo, hi)| {
                    area.intersects(
                        &e.body_place()
                            .box_to_world(Aabb { min: lo.as_dvec3(), max: (hi + IVec3::ONE).as_dvec3() }),
                    )
                })
            });
            if other_here {
                return false;
            }
        }
        if let Some(rider) = self.rider_in(id, 0) {
            let helm = self.active_helm(c, id).map(|(at, _)| at);
            let at = crate::physics::seat_at(c, e.kind, pos, e.yaw, 0, Some(place), helm);
            if !self.dismount_at(0, c, rider, at) {
                return false;
            }
        }
        let Some(e) = self.entities.get_mut(id) else { return false };
        e.set_body_pose(place);
        if let Some(ship) = &mut e.ship {
            ship.spin = DVec3::ZERO;
        }
        self.put_down_whole(c, id);
        if let Some(space) = self.bodies.get_mut(&id) {
            space.dry_changed = !space.dry_boxes.is_empty();
            space.dry_boxes.clear();
        }
        true
    }

    fn ship_input(&self, c: &Content, ship: u64) -> (f64, f64) {
        let Some(e) = self.rider_in(ship, 0).and_then(|id| self.entities.get(id)) else { return (0.0, 0.0) };
        if e.mode != crate::world::GameMode::CREATIVE
            || !e.alive()
            || !e
                .helm_pilot
                .as_ref()
                .is_some_and(|p| p.mount == ship && p.profile.debug_drive)
        {
            return (0.0, 0.0);
        }
        let held = |name| {
            c.game
                .catalog
                .find(ek::KEY, name)
                .is_some_and(|id| e.keys.iter().any(|&(k, _)| k == id)) as u8 as f64
        };
        (held("core:forward") - held("core:back"), held("core:left") - held("core:right"))
    }

    fn step_ship(
        &self,
        c: &Content,
        id: u64,
        mass: &ShipMass,
        hull: &Hull,
        mounted_hulls: &FxHashMap<u64, Hull>,
        parts: &[MountedPart<'_>],
        step: ShipStep,
        front: u8,
        drive: f64,
        steer: f64,
        propulsion: Propulsion,
        before: &FxHashMap<u64, Transform>,
        frames: &ContactFrames,
        reactions: &mut Reactions,
        incoming: &[(f64, VelocityChange)],
        rolling: &mut Vec<(u64, f64)>,
        mut collisions: Option<&mut Vec<abi::ShipCollision>>,
        mut joint_loads: Option<&mut Vec<(u64, DVec3)>>,
    ) -> (Transform, DVec3, DVec3, bool) {
        let ShipStep { old, vel, spin } = step;
        let has_springs = joint_loads.is_some();
        let grip_radius = parts
            .iter()
            .map(|part| part.space.colliders(c).grip_radius)
            .fold(0.0, f64::max);
        let has_grip = grip_radius > 0.0;
        let Some(bounds) = parts
            .iter()
            .filter_map(|p| Some(p.relative.box_to_world(p.space.colliders(c).bounds?)))
            .reduce(|a, b| Aabb { min: a.min.min(b.min), max: a.max.max(b.max) })
        else {
            return (old, DVec3::ZERO, DVec3::ZERO, false);
        };
        let world = CubeCells(&self.space.cubes);
        let center = old.to_world(mass.center);
        let gravity = c.dims[self.id as usize].gravity as f64;
        let trace = crate::log::on(crate::log::Channel::Physics, log::Level::Debug);
        let trace_contacts = crate::log::on(crate::log::Channel::Physics, log::Level::Trace);
        let water_started = trace.then(std::time::Instant::now);
        let mut wet = 0.0;
        let mut moment = DVec3::ZERO;
        for part in parts {
            let body_hull = if part.id == id { hull } else { &mounted_hulls[&part.id] };
            let (volume, torque) = body_hull.submerged(c, &world, &part.space.colliders(c).groups, part.relative.in_frame(old), center);
            wet += volume;
            moment += torque;
        }
        let water_time = water_started.map_or(0.0, |start| start.elapsed().as_secs_f64() * 1000.0);
        let contact = (wet / mass.weight).clamp(0.0, 1.0);
        let (mut grounded, mut resting) = (false, false);
        let dry = contact < LAND_DRIVE_SHARE;
        if dry && (has_grip || drive != 0.0 || steer != 0.0 || vel.with_y(0.0).length_squared() > 1e-6 || vel.y.abs() < 0.05) {
            visit_structure_shapes(c, &world, parts, &[], old, -DVec3::Y * SUPPORT_REACH, |part, b, place, delta, _| {
                crate::physics::rotated_contacts(c, &world, b, place, delta, |hit| {
                    grounded |= hit.normal.y > 0.6;
                    resting |= (!has_springs || part == id) && hit.normal.y > 0.999 && hit.fraction * SUPPORT_REACH < 0.001;
                });
                resting
            });
        }
        let resting =
            resting && !has_grip && old.vector_to_world(DVec3::Y).y > 1.0 - 1e-6 && !propulsion.powered && !propulsion.floater_axes.any();
        let drive_contact = if grounded { LAND_DRIVE_SHARE } else { contact };
        let forward = old
            .vector_to_world(DIRS[front as usize].as_dvec3())
            .with_y(0.0)
            .normalize_or_zero();
        let thrust = crate::config::get(c.game.ship_thrust);
        let water_drag = crate::config::get(c.game.ship_water_drag);
        let drive_accel = drive * drive_contact * thrust;
        // A level hull resting on a level floor is supported over its footprint.
        let (vel, gravity_accel) = if resting { (vel.with_y(vel.y.max(0.0)), 0.0) } else { (vel, gravity * (wet / mass.weight - 1.0)) };
        let acceleration = DVec3::Y * gravity_accel + forward * drive_accel;
        let impulse = old.vector_to_world(propulsion.force) * (TICK / mass.weight);
        let mut free = vel + acceleration * TICK;
        // Integrate quadratic air drag without a speed-dependent explicit step limit.
        let air_decay = 1.0 / (1.0 + TICK * AIR_DRAG * (1.0 - contact) * (free + impulse).length());
        let drag = air_decay * (-TICK * (0.05 + contact * water_drag)).exp();
        let side = DVec3::Y.cross(forward);
        let side_drag = water_drag * crate::config::get(c.game.ship_water_side_drag);
        let side_decay = air_decay * (-TICK * (0.05 + contact * side_drag)).exp();
        free = free * drag + side * (free.dot(side) * (side_decay - drag));
        if grounded && !has_grip {
            free = (free * (-TICK * LAND_DRAG).exp()).with_y(free.y);
        }
        let driven = if propulsion.floater_axes.any() {
            old.vector_to_world(DVec3::select(propulsion.floater_axes, propulsion.floater_velocity, old.vector_to_local(free)))
        } else {
            free
        };
        let vel = driven + impulse * drag;
        let torque = old.vector_to_world((old.vector_to_local(moment * gravity) + propulsion.moment) / mass.inertia)
            + DVec3::Y * steer * drive_contact * 2.7;
        let turn_travel = 0.22;
        let spin = spin + torque * TICK;
        let spin = if resting { DVec3::Y * spin.y } else { spin };
        let axis = old.vector_to_local(spin.normalize_or_zero());
        let corners: [DVec3; 8] =
            std::array::from_fn(|i| DVec3::select(BVec3::new(i & 1 != 0, i & 2 != 0, i & 4 != 0), bounds.max, bounds.min));
        let radius = corners
            .into_iter()
            .map(|point| (point - mass.center).cross(axis).length())
            .fold(1.0, f64::max)
            .min(mass.radius);
        let max_spin = (turn_travel * 4.0 / (radius * TICK)).min(2.5);
        let spin = (spin * (-TICK * (0.1 + drive_contact * 2.8)).exp()).clamp_length_max(max_spin);
        let at = |center, rotation| center - rotation * (mass.center - DVec3::splat(0.5)) - DVec3::Y * 0.5;
        let started = trace.then(std::time::Instant::now);
        let probes = std::cell::Cell::new(0);
        let envelope = old.box_to_world(bounds);
        let margin = DVec3::splat((vel.length() * TICK + radius * max_spin * TICK).max(0.5));
        let swept = Aabb { min: envelope.min - margin, max: envelope.max + margin };
        let mut others: Vec<_> = frames
            .iter()
            .filter_map(|(&other, surface)| {
                if surface.root == id
                    || !swept.intersects(&surface.swept)
                    || self
                        .entities
                        .get(other)
                        .is_none_or(|e| e.removed || e.motion == Motion::Parked)
                {
                    return None;
                }
                Some((other, *surface, self.bodies.get(&other)?.as_ref()))
            })
            .collect();
        others.sort_unstable_by_key(|entry| entry.0);

        let peers = std::cell::RefCell::new(Vec::with_capacity(others.len()));
        let query = |place, delta, elapsed, duration, report: &mut dyn FnMut(u64, Option<Cylinder>, u64, BoxContact)| {
            let mut peers = peers.borrow_mut();
            peers.clear();
            peers.extend(
                others
                    .iter()
                    .filter(|(_, surface, _)| duration == 0.0 || !surface.turns())
                    .map(|&(id, surface, space)| {
                        let place = surface.place_at(elapsed);
                        // Rotational travel is tested at the sampled poses below.
                        let delta =
                            if duration == 0.0 { DVec3::ZERO } else { surface.place_at(elapsed + duration).position() - place.position() };
                        StructurePeer { id, place, bounds: place.box_to_world(surface.local_bounds), space, delta }
                    }),
            );
            visit_structure_shapes(c, &world, parts, &peers, place, delta, |part, shape, relative, delta, peer| {
                if trace {
                    probes.set(probes.get() + 1);
                }
                let wheel = match shape {
                    Solid::Cylinder(wheel) if wheel.grip > 0.0 => Some(wheel),
                    _ => None,
                };
                if let Some(peer) = peer {
                    crate::physics::rotated_contacts(c, peer.space, shape, relative, delta, |mut hit| {
                        hit.normal = peer.place.vector_to_world(hit.normal);
                        hit.point = peer.place.to_world(hit.point) + peer.delta * hit.fraction;
                        report(part, wheel, peer.id, hit);
                    });
                } else {
                    crate::physics::rotated_contacts(c, &world, shape, relative, delta, |hit| report(part, wheel, 0, hit));
                }
                false
            });
        };
        let tire_at = |part, wheel, peer, rotation, hit| {
            TireFriction::at(c, &self.entities, &world, &self.bodies, parts, before, part, wheel, old, rotation, peer, frames, hit)
        };
        let tire_contacts = |place, center, time, rotation| {
            let mut contacts: Vec<(ImpactPoint, ContactConstraint)> = Vec::new();
            if !has_grip {
                return contacts;
            }
            let reach = 0.05;
            query(place, DVec3::NEG_Y * reach, time, 0.0, &mut |part, wheel, other, hit| {
                let Some(wheel) = wheel else { return };
                if hit.normal.y <= 0.5 {
                    return;
                }
                let point = ImpactPoint::new(other, part, Some(wheel), hit);
                let previous = contacts.iter_mut().find(|(old, _)| old.same_patch(&point));
                if previous
                    .as_ref()
                    .is_some_and(|entry| hit.fraction >= entry.0.hit.fraction)
                {
                    return;
                }
                let peer = ContactPeer { id: other, point: hit.point, time };
                let tire = tire_at(part, wheel, peer, rotation, hit);
                let contact = (point, ContactConstraint::support(center, peer, hit, reach, tire));
                if let Some(previous) = previous {
                    *previous = contact;
                } else {
                    contacts.push(contact);
                }
            });
            contacts
        };
        let mut moved = center;
        let mut vel = vel;
        let mut spin = spin;
        let mut rotation = old.rotation();
        let mut incoming = incoming.iter().peekable();
        while incoming.peek().is_some_and(|(time, _)| *time <= 1e-7) {
            let (_, change) = incoming.next().unwrap();
            vel += change.linear;
            spin += change.angular;
        }
        let mut supports = tire_contacts(old, center, 0.0, rotation);
        if !supports.is_empty() {
            for _ in 0..12 {
                for (_, support) in &mut supports {
                    support.solve(mass, rotation, &mut vel, &mut spin, frames, reactions);
                }
            }
            if let Some(loads) = &mut joint_loads {
                loads.extend(
                    supports
                        .iter()
                        .filter(|(p, s)| p.part != id && s.load > 0.0)
                        .map(|(p, s)| (p.part, s.normal * s.load + s.friction)),
                );
            }
        }
        let mut elapsed = 0.0_f64;
        let mut impacts = 0;
        while elapsed < TICK - 1e-6 && impacts < 8 {
            while incoming
                .peek()
                .is_some_and(|(time, _)| *time <= elapsed + 1e-7)
            {
                let (_, change) = incoming.next().unwrap();
                vel += change.linear;
                spin += change.angular;
            }
            let end = incoming.peek().map_or(TICK, |(time, _)| time.min(TICK));
            let time = end - elapsed;
            let delta = vel * time;
            let angle = spin * time;
            let pose = |f| (DQuat::from_scaled_axis(angle * f) * rotation).normalize();
            let start = Transform::rotated(at(moved, rotation), rotation);
            let mut contact = Impact::default();
            query(start, delta, elapsed, time, &mut |part, wheel, other, hit| {
                let peer = ContactPeer { id: other, point: hit.point, time: elapsed + hit.fraction * time };
                let arm = hit.point - (moved + delta * hit.fraction);
                let closing = (vel + spin.cross(arm) - peer.velocity(frames, reactions)).dot(hit.normal);
                if closing < -1e-6 || (hit.depth > 1e-4 && closing <= 1e-6) {
                    contact.add(other, if has_springs || wheel.is_some() { part } else { id }, wheel, hit);
                }
            });
            let mut fraction = contact.fraction();
            let mut witness = fraction;
            let mut sample_distance = radius * angle.length();
            for (_, surface, _) in others.iter().filter(|(_, surface, _)| surface.turns()) {
                let from = surface.place_at(elapsed);
                let to = surface.place_at(end);
                let peer_angle = (to.rotation() * from.rotation().conjugate())
                    .to_scaled_axis()
                    .length();
                let arm = surface.local_bounds.max - surface.local_bounds.min;
                let relative = delta - (to.position() - from.position());
                sample_distance = sample_distance.max(radius * angle.length() + relative.length() + peer_angle * arm.length());
            }
            if fraction > 0.0 && sample_distance > 1e-6 {
                // Detect contacts caused by the swept orientation. Existing overlap against a
                // resting floor must not win the search unless this rotation worsens it.
                let angular_contacts = |f| {
                    let rot = pose(f);
                    let sample = Transform::rotated(at(moved + delta * f, rot), rot);
                    let mut hits = Impact::default();
                    query(sample, DVec3::ZERO, elapsed + time * f, 0.0, &mut |part, wheel, other, mut hit| {
                        let own_travel = hit.point - start.to_world(sample.to_local(hit.point));
                        let peer_travel = frames.get(&other).map_or(DVec3::ZERO, |surface| {
                            let from = surface.place_at(elapsed);
                            let to = surface.place_at(elapsed + time * f);
                            hit.point - from.to_world(to.to_local(hit.point))
                        });
                        if (own_travel - peer_travel).dot(hit.normal) < -1e-6 {
                            hit.depth = 0.0; // we advance only to the last clear pose
                            hits.add(other, if has_springs || wheel.is_some() { part } else { id }, wheel, hit);
                        }
                    });
                    hits
                };
                let mut free = 0.0;
                let samples = (2.0 * sample_distance * fraction / turn_travel)
                    .ceil()
                    .clamp(2.0, 24.0) as u32;
                for sample in 1..=samples {
                    let t = fraction * f64::from(sample) / f64::from(samples);
                    let mut hits = angular_contacts(t);
                    if hits.points.is_empty() {
                        free = t;
                        continue;
                    }
                    let mut blocked = t;
                    for _ in 0..7 {
                        let mid = (free + blocked) * 0.5;
                        let at_mid = angular_contacts(mid);
                        if at_mid.points.is_empty() {
                            free = mid;
                        } else {
                            blocked = mid;
                            hits = at_mid;
                        }
                    }
                    fraction = free;
                    witness = blocked;
                    contact = hits;
                    break;
                }
            }
            moved += delta * fraction;
            rotation = pose(fraction);
            let started_at = elapsed;
            elapsed += time * fraction;
            if contact.points.is_empty() {
                continue;
            }
            impacts += 1;
            let mut manifold: Vec<_> = contact
                .points
                .iter()
                .map(|point| {
                    let hit = point.hit;
                    let peer = ContactPeer { id: point.other, point: hit.point, time: started_at + time * witness };
                    let tire = point
                        .wheel
                        .map(|wheel| tire_at(point.part, wheel, peer, rotation, hit));
                    ContactConstraint::new(hit.point - moved, hit.normal, 1e-4, peer, tire)
                })
                .collect();
            // Re-solve the floor and the obstacle together. A wheel hitting a wall can
            // drive the other wheels into the floor through the chassis.
            let pose = Transform::rotated(at(moved, rotation), rotation);
            let mut support = tire_contacts(pose, moved, elapsed, rotation);
            support.retain(|(point, _)| !contact.points.iter().any(|hit| hit.same_patch(point)));
            let speed = |s: &ContactConstraint| -s.patch_velocity(vel, spin, frames, reactions).dot(s.normal);
            let speeds: Vec<_> = manifold.iter().map(speed).collect();
            let coupled = !support.is_empty() || manifold.len() > 1 || manifold[0].tire.is_some();
            for _ in 0..if coupled { 16 } else { 1 } {
                for point in &mut manifold {
                    point.solve(mass, rotation, &mut vel, &mut spin, frames, reactions);
                }
                for (_, point) in &mut support {
                    point.solve(mass, rotation, &mut vel, &mut spin, frames, reactions);
                }
            }
            if let Some(loads) = &mut joint_loads {
                loads.extend(
                    support
                        .iter()
                        .filter(|(p, s)| p.part != id && s.load > 0.0)
                        .map(|(p, s)| (p.part, s.normal * s.load + s.friction)),
                );
            }
            let mut correction = DVec3::ZERO;
            for ((point, constraint), speed) in contact.points.into_iter().zip(manifold).zip(speeds) {
                let ImpactPoint { other, part, wheel, hit, .. } = point;
                let impulse = constraint.load;
                grounded |= hit.normal.y > 0.6;
                if trace_contacts {
                    log::trace!(target: "physics", "ship {id} contact: part={part} other={other} tread={} fraction={fraction:.5} depth={:.5} normal={:?} point={:?} remaining={time:.5} impulse={impulse:.5}",
                        wheel.is_some(), hit.depth, hit.normal, hit.point);
                }
                if impulse > 0.0
                    && let Some(collisions) = collisions.as_mut()
                {
                    let event = abi::ShipCollision {
                        ship: id,
                        other,
                        point: hit.point.to_array().into(),
                        normal: hit.normal.to_array().into(),
                        speed: speed.max(0.0),
                        impulse,
                    };
                    if let Some(last) = collisions.iter_mut().find(|e| e.other == other) {
                        if impulse > last.impulse {
                            *last = event;
                        }
                    } else {
                        collisions.push(event);
                    }
                }
                if impulse > 0.0
                    && part != id
                    && let Some(loads) = &mut joint_loads
                {
                    loads.push((part, hit.normal * impulse + constraint.friction));
                }
                // Coplanar contacts share a correction. A row of voxels must not amplify it.
                let separation = hit.depth.clamp(0.0, 0.025) + CONTACT_SKIN;
                correction += hit.normal * (separation - correction.dot(hit.normal)).max(0.0);
            }
            moved += correction;
        }
        for (_, change) in incoming {
            vel += change.linear;
            spin += change.angular;
        }
        supports.sort_by(|a, b| b.1.load.total_cmp(&a.1.load));
        for (point, support) in supports {
            if support.load > 0.0
                && let Some(tire) = support.tire
                && let Some(coast) = tire.coast
                && !rolling.iter().any(|&(id, _)| id == point.part)
            {
                let rate = support
                    .patch_velocity(vel, spin, frames, reactions)
                    .dot(tire.roll)
                    * coast;
                rolling.push((point.part, rate));
            }
        }
        if let Some(started) = started {
            log::debug!(target: "physics", "ship {id}: collision {:.3} ms, buoyancy {:.3} ms, {} box queries, {} nearby bodies; pos={:?}, mass={:.2}, displaced={:.2}, air={}, up={:?}, front={:?}, impacts={impacts}, vel={vel:?}, spin={spin:?}",
                started.elapsed().as_secs_f64() * 1000.0, water_time, probes.get(), others.len(), at(moved, rotation), mass.weight, wet, hull.flooded.iter().filter(|&&f| !f).count(),
                rotation * DVec3::Y, rotation * DIRS[front as usize].as_dvec3());
        }
        (Transform::rotated(at(moved, rotation), rotation), vel, spin, grounded)
    }
}

fn angular_response(mass: &ShipMass, rotation: DQuat, torque: DVec3) -> DVec3 {
    rotation * ((rotation.conjugate() * torque) / mass.inertia)
}

#[derive(Clone, Copy)]
struct TireFriction {
    side: DVec3,
    roll: DVec3,
    tread_speed: f64,
    grip: f64,
    coast: Option<f64>,
}

impl TireFriction {
    fn at(
        c: &Content,
        entities: &Entities,
        world: &impl Cells,
        bodies: &FxHashMap<u64, Box<Space>>,
        parts: &[MountedPart<'_>],
        before: &FxHashMap<u64, Transform>,
        part: u64,
        wheel: Cylinder,
        old: Transform,
        rotation: DQuat,
        peer: ContactPeer,
        frames: &ContactFrames,
        hit: BoxContact,
    ) -> Self {
        let rel = parts.iter().find(|p| p.id == part).unwrap().relative;
        let axle = rotation * rel.vector_to_world(wheel.axis);
        let across = axle - hit.normal * axle.dot(hit.normal);
        let side = across.normalize_or_zero();
        let roll = side.cross(hit.normal);
        let coast = entities
            .get(part)
            .and_then(|e| e.mount.as_deref())
            .and_then(|mount| {
                let drive = mount.drive?;
                (drive.rule.motion == crate::controller::Motion::Freewheel)
                    .then(|| wheel.axis.dot(DIRS[mount.face as usize].as_dvec3()) / wheel.radius)
            });
        let tread_speed = if coast.is_some() {
            0.0
        } else {
            before.get(&part).map_or(0.0, |prev| {
                let current = rel.in_frame(old);
                let change = (current.rotation() * prev.rotation().conjugate()).to_scaled_axis();
                change.dot(current.vector_to_world(wheel.axis)) * wheel.radius / TICK
            })
        };
        let inside = hit.point - hit.normal * 0.002;
        let ground = if let Some(surface) = frames.get(&peer.id) {
            let at = surface.place_at(peer.time).to_local(inside);
            bodies
                .get(&peer.id)
                .and_then(|space| space.get(at.floor().as_ivec3()))
        } else {
            world.main(inside.floor().as_ivec3())
        };
        let ground_grip = ground.map_or(1.0, |s| ((1.0 - f64::from(c.states[s as usize].friction)) / 0.4).clamp(0.05, 1.5));
        // The sidewall should not transmit the same traction as the tread.
        let tread_fraction = across.length_squared();
        Self { side, roll, tread_speed, grip: wheel.grip * ground_grip * tread_fraction * tread_fraction, coast }
    }

    fn change(self, patch_velocity: DVec3, effective_mass: impl Fn(DVec3) -> f64) -> DVec3 {
        let lateral = self.side * (-patch_velocity.dot(self.side) * effective_mass(self.side));
        lateral + self.roll * ((self.tread_speed - patch_velocity.dot(self.roll)) * effective_mass(self.roll))
    }
}

struct ContactConstraint {
    arm: DVec3,
    normal: DVec3,
    min_speed: f64,
    peer: ContactPeer,
    tire: Option<TireFriction>,
    load: f64,
    friction: DVec3,
}

impl ContactConstraint {
    fn new(arm: DVec3, normal: DVec3, min_speed: f64, peer: ContactPeer, tire: Option<TireFriction>) -> Self {
        Self { arm, normal, min_speed, peer, tire, load: 0.0, friction: DVec3::ZERO }
    }

    fn support(center: DVec3, peer: ContactPeer, hit: BoxContact, reach: f64, tire: TireFriction) -> Self {
        let distance = (hit.fraction * reach * hit.normal.y - 2e-5).max(0.0);
        let min_speed = (hit.depth.min(0.02) * 0.2 - distance) / TICK + 1e-4;
        Self::new(hit.point - center, hit.normal, min_speed, peer, Some(tire))
    }

    fn patch_velocity(&self, vel: DVec3, spin: DVec3, frames: &ContactFrames, reactions: &Reactions) -> DVec3 {
        vel + spin.cross(self.arm) - self.peer.velocity(frames, reactions)
    }

    fn solve(
        &mut self,
        mass: &ShipMass,
        rotation: DQuat,
        vel: &mut DVec3,
        spin: &mut DVec3,
        frames: &ContactFrames,
        reactions: &mut Reactions,
    ) {
        let response = |dir| {
            let torque = self.arm.cross(dir);
            (mass.weight.recip() + torque.dot(angular_response(mass, rotation, torque)) + self.peer.inverse_mass(frames, dir)).recip()
        };
        let patch_speed = self
            .patch_velocity(*vel, *spin, frames, reactions)
            .dot(self.normal);
        let loaded = (self.load + (self.min_speed - patch_speed) * response(self.normal)).max(0.0);
        let normal = self.normal * (loaded - self.load);
        self.load = loaded;
        *vel += normal / mass.weight;
        *spin += angular_response(mass, rotation, self.arm.cross(normal));
        self.peer.impart(frames, reactions, normal);
        if let Some(tire) = self.tire {
            let limit = loaded * tire.grip;
            let mut friction = self.friction + tire.change(self.patch_velocity(*vel, *spin, frames, reactions), response);
            if tire.coast.is_some() {
                let along = friction.dot(tire.roll);
                let resistance = limit * ROLLING_RESISTANCE;
                friction += tire.roll * (along.clamp(-resistance, resistance) - along);
            }
            let friction = friction.clamp_length_max(limit);
            let tangent = friction - self.friction;
            self.friction = friction;
            *vel += tangent / mass.weight;
            *spin += angular_response(mass, rotation, self.arm.cross(tangent));
            self.peer.impart(frames, reactions, tangent);
        }
    }
}

struct ImpactPoint {
    other: u64,
    part: u64,
    wheel: Option<Cylinder>,
    hit: BoxContact,
    count: u32,
}

impl ImpactPoint {
    fn new(other: u64, part: u64, wheel: Option<Cylinder>, hit: BoxContact) -> Self {
        Self { other, part, wheel, hit, count: 1 }
    }

    fn same_patch(&self, point: &Self) -> bool {
        self.other == point.other
            && self.part == point.part
            && self.hit.normal.dot(point.hit.normal) > 0.999
            && match (self.wheel, point.wheel) {
                (None, None) => self.hit.point.distance_squared(point.hit.point) <= 1.0,
                (Some(a), Some(b)) => a.center == b.center,
                _ => false,
            }
    }
}

#[derive(Default)]
struct Impact {
    points: Vec<ImpactPoint>,
}

impl Impact {
    fn fraction(&self) -> f64 {
        self.points.first().map_or(1.0, |p| p.hit.fraction)
    }

    fn add(&mut self, other: u64, part: u64, wheel: Option<Cylinder>, hit: BoxContact) {
        let first = self.fraction();
        if hit.fraction < first - 1e-5 {
            self.points.clear();
        } else if hit.fraction > first + 1e-5 {
            return;
        }
        let point = ImpactPoint::new(other, part, wheel, hit);
        if let Some(patch) = self.points.iter_mut().find(|old| old.same_patch(&point)) {
            patch.count += 1;
            patch.hit.point += (hit.point - patch.hit.point) / f64::from(patch.count);
            patch.hit.depth = patch.hit.depth.max(hit.depth);
        } else if self.points.len() < 12 {
            self.points.push(point);
        }
    }
}

struct ShipStep {
    old: Transform,
    vel: DVec3,
    spin: DVec3,
}
