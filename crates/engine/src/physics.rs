//! AABB movement against block collision boxes and solid bodies, riding bodies, and voxel raycasts.
//! Shared by the client (local player prediction) and the server (entities).

use glam::{DMat3, DQuat, DVec3, IVec3};
use weft_abi::{EntitySize, block_flags as bf};

use crate::content::Content;
use crate::world::placement::{CellLayout, Part};

mod character;
use character::{Collider, Sweep, Travel};

const WALKABLE_Y: f64 = std::f64::consts::FRAC_1_SQRT_2;
pub(crate) const LIQUID_SOURCES: u32 = 1 << 31;

pub(crate) fn seat_at(
    c: &Content,
    kind: u32,
    pos: DVec3,
    yaw: f32,
    seat: u8,
    place: Option<Transform>,
    helm: Option<IVec3>,
) -> Option<DVec3> {
    let offset = c.game.seats(kind, helm.is_some()).get(seat as usize)?.pos;
    Some(if let Some(helm) = helm {
        place?.to_world(helm.as_dvec3() + Transform::FEET + offset)
    } else {
        pos + crate::world::turned(offset, yaw)
    })
}

/// Position and orient the placed shape against the clicked surface. Looking within the face
/// still chooses the authored slab half, hinge or orientation.
pub(crate) fn placement(
    c: &Content,
    state: u32,
    clicked: IVec3,
    surface: Part,
    face: u8,
    hit: [f32; 3],
    look: [f32; 3],
    replacing: bool,
) -> Option<(IVec3, Part, [f32; 3])> {
    let normal = *crate::world::DIRS.get(face as usize)?;
    if !hit.iter().all(|v| v.is_finite()) {
        return None;
    }
    let axis = face as usize / 2;
    let positive = face % 2 == 1;
    // Find the clicked unit of the supporting instance's grid, even in a covered cell.
    let shift = if replacing { DVec3::ZERO } else { surface.shift().rem_euclid(DVec3::ONE) };
    let local = DVec3::from_array(hit.map(f64::from)) - shift;
    let delta = (local - normal.as_dvec3() * 1e-5).floor().as_ivec3();
    let local = if replacing { DVec3::from_array(hit.map(f64::from)) } else { local - delta.as_dvec3() };
    let plane = local[axis];
    let inner = !replacing && plane > 1e-5 && plane < 1.0 - 1e-5;
    let cell = if replacing {
        clicked
    } else if inner {
        clicked + delta
    } else {
        clicked + delta + normal
    };
    let mut hit = local.as_vec3().to_array();
    hit[axis] = if inner {
        plane as f32 + if positive { 0.001 } else { -0.001 }
    } else if positive {
        0.001
    } else {
        0.999
    };
    let state = placement_state(c.block_of(state), cell, face, look, hit).ok()?;
    let mut offset = shift;
    if inner {
        let mount = Part::aligned(state)
            .bounds(c)
            .map(|b| if positive { b.min[axis] } else { b.max[axis] })
            .reduce(if positive { f64::min } else { f64::max })?;
        offset[axis] += plane - mount;
    }
    let part = Part {
        state,
        offset: (offset * f64::from(crate::world::placement::UNIT))
            .round()
            .as_ivec3(),
    };
    let (owner, part) = part.owner(cell);
    Some((owner, part, hit))
}

pub(crate) fn placement_state(def: &crate::content::BlockDef, pos: IVec3, face: u8, look: [f32; 3], hit: [f32; 3]) -> Result<u32, u32> {
    let Some((owner, cb)) = def.cb.placement else { return Ok(def.first_state) };
    let mut args = weft_abi::PlaceArgs {
        pos: pos.to_array().into(),
        face: face as u32,
        look,
        state: def.first_state,
        first: def.first_state,
        hit,
        placed: 0,
    };
    crate::host::invoke(owner, cb, &mut args);
    if (def.first_state..def.first_state + def.states).contains(&args.state) { Ok(args.state) } else { Err(args.state) }
}

/// The box of an entity in a pose (modern vanilla), on client and server
/// alike, from its width, height and eyes standing: crouching lowers them like the player's 1.8 to
/// 1.5 and 1.62 to 1.27, swimming to 0.6 and 0.4, whatever size it has.
pub fn posed(size: EntitySize, pose: crate::world::Pose) -> Dims {
    let r = |v: f64| (v * 1e6).round() / 1e6;
    let (half_width, height, eye) = (size.width as f64 / 2.0, size.height as f64, size.eye as f64);
    let d = pose.def();
    if d.height == 1.0 && d.eye == 1.0 {
        return Dims { half_width, height, eye };
    }
    Dims { half_width, height: r(height * d.height), eye: r(eye * d.eye) }
}

/// An entity's box as physics uses it, in blocks: half its width (from its middle to a side),
/// its height and where its eyes are.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dims {
    pub half_width: f64,
    pub height: f64,
    pub eye: f64,
}

impl Dims {
    /// The same box as mods see sizes.
    pub fn size(&self) -> EntitySize {
        EntitySize { width: self.half_width as f32 * 2.0, height: self.height as f32, eye: self.eye as f32 }
    }
}

/// Where a look goes, radians: yaw 0 is north (-Z), pitch up.
pub fn look_dir(yaw: f32, pitch: f32) -> DVec3 {
    let (sy, cy) = (yaw as f64).sin_cos();
    let (sp, cp) = (pitch as f64).sin_cos();
    DVec3::new(-sy * cp, sp, -cy * cp)
}

/// A walker's jump, blocks per tick.
pub const JUMP: f64 = 0.42;

/// A dimension's gravity, in blocks per second squared, per tick squared.
pub fn gravity_per_tick(gravity: f64) -> f64 {
    gravity * crate::world::TICK * crate::world::TICK
}

/// Vanilla's knockback, in blocks per tick: half the speed, `push` away (flat, its length the
/// strength, a hurt's 0.4) and on the ground as much up, never more than 0.4 up; `resist`
/// (`KNOCKBACK_RESISTANCE`) takes its share off, all of it leaving the speed alone.
pub fn knock(vel: DVec3, push: DVec3, on_ground: bool, resist: f64) -> DVec3 {
    let push = push.with_y(0.0) * (1.0 - resist.clamp(0.0, 1.0));
    let strength = push.length();
    if strength <= 0.0 {
        return vel;
    }
    let y = if on_ground { (vel.y / 2.0 + strength).min(0.4) } else { vel.y };
    DVec3::new(vel.x / 2.0 + push.x, y, vel.z / 2.0 + push.z)
}

#[derive(Clone, Copy, Debug)]
pub struct Aabb {
    pub min: DVec3,
    pub max: DVec3,
}

impl Aabb {
    pub fn around(feet: DVec3, half_width: f64, height: f64) -> Aabb {
        Aabb {
            min: DVec3::new(feet.x - half_width, feet.y, feet.z - half_width),
            max: DVec3::new(feet.x + half_width, feet.y + height, feet.z + half_width),
        }
    }

    pub fn intersects(&self, o: &Aabb) -> bool {
        self.min.cmplt(o.max).all() && self.max.cmpgt(o.min).all()
    }

    pub fn shifted(&self, by: DVec3) -> Aabb {
        Aabb { min: self.min + by, max: self.max + by }
    }

    pub fn swept(self, delta: DVec3) -> Aabb {
        Aabb { min: self.min + delta.min(DVec3::ZERO), max: self.max + delta.max(DVec3::ZERO) }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Cylinder {
    pub center: DVec3,
    pub axis: DVec3,
    pub radius: f64,
    pub half: f64,
    pub grip: f64,
}

impl Cylinder {
    pub fn bounds(self) -> Aabb {
        let axial = self.axis.abs();
        let radial = (DVec3::ONE - axial * axial).max(DVec3::ZERO).map(f64::sqrt);
        let half = axial * self.half + radial * self.radius;
        Aabb { min: self.center - half, max: self.center + half }
    }

    pub fn placed(self, place: Transform) -> Self {
        Self { center: place.to_world(self.center), axis: place.vector_to_world(self.axis), ..self }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Solid {
    Box(Aabb),
    Cylinder(Cylinder),
}

impl Solid {
    pub fn bounds(self) -> Aabb {
        match self {
            Self::Box(bounds) => bounds,
            Self::Cylinder(cylinder) => cylinder.bounds(),
        }
    }

    pub fn world_bounds(self, place: Transform) -> Aabb {
        match self {
            Self::Box(bounds) => place.box_to_world(bounds),
            Self::Cylinder(cylinder) => cylinder.placed(place).bounds(),
        }
    }

    pub(crate) fn overlaps(self, other: Self) -> bool {
        if (self.bounds().max.min(other.bounds().max) - self.bounds().min.max(other.bounds().min)).min_element() <= 1e-6 {
            return false;
        }
        matches!((self, other), (Self::Box(_), Self::Box(_))) || convex_contact(self, Transform::default(), other, DVec3::ZERO).is_some()
    }

    pub fn volume(self) -> f64 {
        match self {
            Self::Box(bounds) => (bounds.max - bounds.min).element_product(),
            Self::Cylinder(c) => std::f64::consts::PI * c.radius * c.radius * 2.0 * c.half,
        }
    }
}

pub fn block_solids(offset: DVec3, state: &crate::content::StateInfo) -> impl Iterator<Item = Solid> + '_ {
    state
        .cylinder
        .map(|s| Solid::Cylinder(Cylinder { center: s.center + offset, ..s }))
        .into_iter()
        .chain(
            state
                .collision
                .iter()
                .filter(move |_| state.cylinder.is_none())
                .map(move |b| {
                    let b = b.map(f64::from);
                    Solid::Box(Aabb { min: offset + DVec3::new(b[0], b[1], b[2]), max: offset + DVec3::new(b[3], b[4], b[5]) })
                }),
        )
}

mod convex;
use convex::convex_contact;

/// Where a body's blocks are, from its own coordinates to the world's and back.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Transform {
    origin: DVec3,
    rotation: DQuat,
}

impl Transform {
    const CENTER: DVec3 = DVec3::splat(0.5);
    pub const FEET: DVec3 = DVec3::new(0.5, 0.0, 0.5);

    pub fn of(pos: DVec3) -> Transform {
        Self { origin: pos - Self::FEET, rotation: DQuat::IDENTITY }
    }
    pub fn rotated(pos: DVec3, rotation: DQuat) -> Transform {
        if rotation == DQuat::IDENTITY {
            return Self::of(pos);
        }
        let center = pos + DVec3::Y * 0.5;
        Transform { origin: center - rotation * Self::CENTER, rotation }
    }
    pub fn origin(self) -> DVec3 {
        self.origin
    }
    pub fn rotation(self) -> DQuat {
        self.rotation
    }
    pub fn position(self) -> DVec3 {
        self.origin + self.vector_to_world(Self::CENTER) - DVec3::Y * 0.5
    }
    pub fn vector_to_world(self, v: DVec3) -> DVec3 {
        if self.rotation == DQuat::IDENTITY { v } else { self.rotation * v }
    }
    pub fn vector_to_local(self, v: DVec3) -> DVec3 {
        if self.rotation == DQuat::IDENTITY { v } else { self.rotation.conjugate() * v }
    }
    pub fn to_world(self, p: DVec3) -> DVec3 {
        self.vector_to_world(p) + self.origin
    }
    pub fn to_local(self, p: DVec3) -> DVec3 {
        self.vector_to_local(p - self.origin)
    }
    pub fn box_to_world(self, b: Aabb) -> Aabb {
        if self.rotation == DQuat::IDENTITY {
            return b.shifted(self.origin);
        }
        let m = DMat3::from_quat(self.rotation);
        let abs = DMat3::from_cols(m.x_axis.abs(), m.y_axis.abs(), m.z_axis.abs());
        let center = self.to_world((b.min + b.max) * 0.5);
        let half = abs * ((b.max - b.min) * 0.5);
        Aabb { min: center - half, max: center + half }
    }
    pub fn box_to_local(self, b: Aabb) -> Aabb {
        Self { origin: self.vector_to_local(-self.origin), rotation: self.rotation.conjugate() }.box_to_world(b)
    }
    pub fn cell_at(self, p: IVec3) -> IVec3 {
        self.to_world(p.as_dvec3() + Self::CENTER)
            .floor()
            .as_ivec3()
    }
    pub fn relative_to(self, parent: Transform) -> Transform {
        Transform { origin: parent.to_local(self.origin), rotation: parent.rotation.conjugate() * self.rotation }
    }
    pub fn in_frame(self, parent: Transform) -> Transform {
        Transform { origin: parent.to_world(self.origin), rotation: (parent.rotation * self.rotation).normalize() }
    }
    pub fn turn(self) -> weft_abi::Turn {
        let face = |v| {
            let v = self.vector_to_world(v);
            crate::world::DIRS
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| v.dot(a.as_dvec3()).total_cmp(&v.dot(b.as_dvec3())))
                .map_or(0, |(i, _)| i as u8)
        };
        weft_abi::Turn { east: face(DVec3::X), up: face(DVec3::Y) }
    }
    pub fn grid_aligned(self) -> bool {
        self.vector_to_world(DVec3::X).abs().max_element() > 1.0 - 1e-9 && self.vector_to_world(DVec3::Y).abs().max_element() > 1.0 - 1e-9
    }
    /// The whole-block place of its coordinates among a world's, which was `was`. On each axis it
    /// gets to the next only strictly past the middle: exactly half is no more, or which way
    /// something goes shows at speeds that land there.
    pub fn cell(self, was: Option<IVec3>) -> IVec3 {
        IVec3::from_array([0, 1, 2].map(|a| match was {
            Some(w) if (self.origin[a] - w[a] as f64).abs() <= 0.5 + 1e-6 => w[a],
            _ => self.origin[a].round() as i32,
        }))
    }
}

#[derive(Clone, Copy)]
struct RotatedBox {
    center: DVec3,
    half: DVec3,
    axes: DMat3,
}

struct BoxInterval {
    enter: f64,
    leave: f64,
    entry_normal: DVec3,
    depth: f64,
    overlap_normal: DVec3,
}

impl RotatedBox {
    fn new(b: Aabb, place: Transform) -> Self {
        Self { center: place.to_world((b.min + b.max) * 0.5), half: (b.max - b.min) * 0.5, axes: DMat3::from_quat(place.rotation()) }
    }

    // Project the boxes and the motion onto the separating axes.
    fn overlap_interval(self, b: Aabb, velocity: DVec3) -> Option<(f64, f64)> {
        let hit = self.project::<false>(b, velocity)?;
        Some((hit.enter, hit.leave))
    }

    fn project<const CONTACT: bool>(self, b: Aabb, velocity: DVec3) -> Option<BoxInterval> {
        let center = (b.min + b.max) * 0.5 - self.center;
        let half = (b.max - b.min) * 0.5;
        let opposite = self.axes.transpose();
        let mut hit = BoxInterval {
            enter: f64::NEG_INFINITY,
            leave: f64::INFINITY,
            entry_normal: DVec3::ZERO,
            depth: f64::INFINITY,
            overlap_normal: DVec3::ZERO,
        };
        let basis = [DVec3::X, DVec3::Y, DVec3::Z];
        let axes = [self.axes.x_axis, self.axes.y_axis, self.axes.z_axis];
        for axis in basis
            .into_iter()
            .chain(axes)
            .chain(basis.into_iter().flat_map(|a| axes.map(|b| a.cross(b))))
        {
            let length = axis.length();
            if length < 1e-8 {
                continue;
            }
            let room = (opposite * axis).abs().dot(self.half) + axis.abs().dot(half);
            let distance = center.dot(axis);
            let speed = velocity.dot(axis);
            if CONTACT {
                let depth = (room - distance.abs()) / length;
                if depth < hit.depth - 1e-7 {
                    hit.depth = depth;
                    hit.overlap_normal = axis * (-1.0f64.copysign(distance) / length);
                }
            }
            if speed.abs() < 1e-9 * length {
                if distance.abs() >= room - 1e-7 * length {
                    return None;
                }
                continue;
            }
            let mid = -distance / speed;
            let size = room / speed.abs();
            if mid - size > hit.enter {
                hit.enter = mid - size;
                if CONTACT {
                    hit.entry_normal = axis * (speed.signum() / length);
                }
            }
            hit.leave = hit.leave.min(mid + size);
            if hit.enter >= hit.leave {
                return None;
            }
        }
        Some(hit)
    }

    fn axis_aligned(b: Aabb) -> Self {
        Self { center: (b.min + b.max) * 0.5, half: (b.max - b.min) * 0.5, axes: DMat3::IDENTITY }
    }
}

pub fn rotated_collides(c: &Content, block: &impl Cells, local: Solid, place: Transform) -> bool {
    let area = local.world_bounds(place);
    let mut rotated = None;
    each_shape(c, block, area, |world, _| {
        if !world.bounds().intersects(&area) {
            return false;
        }
        match (local, world) {
            (Solid::Box(_), Solid::Box(_)) if place.rotation() == DQuat::IDENTITY => true,
            (Solid::Box(local), Solid::Box(world)) => rotated
                .get_or_insert_with(|| RotatedBox::new(local, place))
                .overlap_interval(world, DVec3::ZERO)
                .is_some(),
            _ => convex_contact(local, place, world, DVec3::ZERO).is_some(),
        }
    })
}

#[derive(Clone, Copy)]
pub struct BoxContact {
    pub fraction: f64,
    pub normal: DVec3,
    pub point: DVec3,
    pub depth: f64,
}

/// Contact of an oriented box with the cells along a linear displacement, or current overlap.
pub fn rotated_contacts(c: &Content, block: &impl Cells, local: Solid, place: Transform, delta: DVec3, mut report: impl FnMut(BoxContact)) {
    let area = local.world_bounds(place).swept(delta);
    let mut rotated = None;
    each_merged_shape(c, block, area, |target| {
        let (Solid::Box(local), Solid::Box(world)) = (local, target) else {
            if let Some(hit) = convex_contact(local, place, target, delta) {
                report(hit);
            }
            return;
        };
        let shape = rotated.get_or_insert_with(|| RotatedBox::new(local, place));
        let Some(hit) = shape.project::<true>(world, -delta) else { return };
        let (fraction, normal, depth) = if delta == DVec3::ZERO {
            (0.0, hit.overlap_normal, hit.depth)
        } else if hit.enter >= -1e-6 && hit.enter <= 1.0 && hit.leave > 1e-7 {
            (hit.enter.max(0.0), hit.entry_normal, 0.0)
        } else if hit.enter < 0.0 && hit.leave > 0.0 && hit.depth > 1e-6 {
            (0.0, hit.overlap_normal, hit.depth)
        } else {
            return;
        };
        let feature = |mut center: DVec3, axes: DMat3, half: DVec3, normal: DVec3| {
            let mut free = half;
            for i in 0..3 {
                let axis = axes.col(i);
                let n = axis.dot(normal);
                if n.abs() > 1e-5 {
                    center += axis * (n.signum() * half[i]);
                    free[i] = 0.0;
                }
            }
            (center, axes.transpose(), free)
        };
        let (a, a_axes, a_half) = feature(shape.center + delta * fraction, shape.axes, shape.half, -normal);
        let (b, b_axes, b_half) = feature((world.min + world.max) * 0.5, DMat3::IDENTITY, (world.max - world.min) * 0.5, normal);
        let (u, v) = normal.any_orthonormal_pair();
        let span = |axis: DVec3| {
            let (x, y) = (a.dot(axis), b.dot(axis));
            let (r, s) = ((a_axes * axis).abs().dot(a_half), (b_axes * axis).abs().dot(b_half));
            ((x - r).max(y - s) + (x + r).min(y + s)) * 0.5
        };
        let point = u * span(u) + v * span(v) + normal * ((a + b).dot(normal) * 0.5);
        report(BoxContact { fraction, normal, point, depth });
    });
}

/// Blocks a moving structure is built of, in its own coordinates (outside it: air).
pub trait Blocks: Sync {
    fn block(&self, p: IVec3) -> u32;
    /// The corners of its blocks, inclusive; `None` when it has none.
    fn bounds(&self) -> Option<(IVec3, IVec3)>;
    /// What else is in that cell.
    fn layout(&self, _p: IVec3) -> &CellLayout {
        &crate::world::placement::EMPTY_LAYOUT
    }
}

/// Blocks by position to physics: the one a cell stands for, `None` for what isn't loaded (to
/// count as solid), and what else is in it. A plain closure has nothing else.
pub trait Cells {
    fn main(&self, p: IVec3) -> Option<u32>;
    fn layout(&self, _p: IVec3) -> &CellLayout {
        &crate::world::placement::EMPTY_LAYOUT
    }
    /// Both at once, for what reads every cell around every mob every tick.
    #[inline]
    fn cell(&self, p: IVec3) -> Option<(u32, &CellLayout)> {
        Some((self.main(p)?, self.layout(p)))
    }
    /// The liquid in a cell, on its own or in with something.
    fn liquid(&self, c: &Content, p: IVec3) -> Option<u32> {
        self.main(p)
            .into_iter()
            .chain(self.layout(p).extras.iter().copied())
            .find(|&s| c.states[s as usize].liquid.is_some())
    }

    fn liquid_block_at(&self, c: &Content, p: DVec3) -> Option<u32> {
        self.liquid(c, p.floor().as_ivec3())
    }

    fn masks_liquid(&self) -> bool {
        false
    }
}

pub type DryVolume<'a> = (Transform, &'a [(IVec3, IVec3)]);

pub fn inside_dry_volume((place, boxes): DryVolume<'_>, point: DVec3) -> bool {
    let p = place.to_local(point).floor().as_ivec3();
    boxes
        .iter()
        .any(|&(lo, hi)| p.cmpge(lo).all() && p.cmplt(hi).all())
}

pub struct ObservedCells<'a, C> {
    pub cells: C,
    pub dry: &'a [DryVolume<'a>],
}

impl<C: Cells> Cells for ObservedCells<'_, C> {
    fn main(&self, p: IVec3) -> Option<u32> {
        self.cells.main(p)
    }
    fn layout(&self, p: IVec3) -> &CellLayout {
        self.cells.layout(p)
    }
    fn cell(&self, p: IVec3) -> Option<(u32, &CellLayout)> {
        self.cells.cell(p)
    }
    fn liquid(&self, c: &Content, p: IVec3) -> Option<u32> {
        self.cells.liquid(c, p)
    }
    fn liquid_block_at(&self, c: &Content, point: DVec3) -> Option<u32> {
        let state = self.cells.liquid_block_at(c, point)?;
        let dry = self
            .dry
            .iter()
            .any(|&volume| inside_dry_volume(volume, point));
        (!dry).then_some(state)
    }
    fn masks_liquid(&self) -> bool {
        !self.dry.is_empty()
    }
}

impl<F: Fn(IVec3) -> Option<u32>> Cells for F {
    fn main(&self, p: IVec3) -> Option<u32> {
        self(p)
    }
}

/// A moving structure's blocks as `Cells`: never anything unloaded.
struct BodyCells<'a>(&'a dyn Blocks);

impl Cells for BodyCells<'_> {
    fn main(&self, p: IVec3) -> Option<u32> {
        Some(self.0.block(p))
    }
    fn layout(&self, p: IVec3) -> &CellLayout {
        self.0.layout(p)
    }
}

/// Collision boxes of a state in block-local coordinates.
pub fn boxes(c: &Content, state: u32, out: &mut Vec<[f64; 6]>) {
    out.extend(
        c.states[state as usize]
            .collision
            .iter()
            .map(|b| b.map(|v| v as f64)),
    );
}

/// A solid body this step: its box after its move (around all its blocks for a structure), how
/// far it moved, how high above it riders still move with it, what it does to what it squeezes
/// (`hazard_at::CRUSHED`, none: only pushes), and the blocks it is built of with where they are.
#[derive(Clone, Copy)]
pub struct Body<'a> {
    pub id: u64,
    pub aabb: Aabb,
    pub delta: DVec3,
    pub carry: f64,
    pub crushes: &'a [crate::content::Hazard],
    pub blocks: Option<(&'a dyn Blocks, Transform)>,
    before: Transform,
}

impl<'a> Body<'a> {
    /// A solid entity at `pos` with the box `(half width, height)`: a structure collides as its
    /// blocks, an empty one not at all.
    pub fn of(
        id: u64,
        kind: &'a crate::registry::EntityKind,
        place: Transform,
        size: (f64, f64),
        before: Transform,
        built: Option<&'a dyn Blocks>,
    ) -> Option<Body<'a>> {
        let pos = place.position();
        let aabb = match built {
            Some(s) => {
                let (lo, hi) = s.bounds()?;
                place.box_to_world(Aabb { min: lo.as_dvec3(), max: (hi + IVec3::ONE).as_dvec3() })
            }
            None => Aabb::around(pos, size.0, size.1),
        };
        let blocks = built.map(|s| (s, place));
        let delta = pos - before.position();
        Some(Body { id, aabb, delta, carry: kind.carry_height as f64, crushes: &kind.hazards, blocks, before })
    }

    fn change_at(&self, now: DVec3) -> DVec3 {
        self.blocks
            .filter(|(_, place)| place.rotation() != self.before.rotation())
            .map_or(self.delta, |(_, place)| now - self.before.to_world(place.to_local(now)))
    }

    pub fn carry_from(&self, was: DVec3) -> DVec3 {
        self.blocks
            .filter(|(_, place)| place.rotation() != self.before.rotation())
            .map_or(self.delta, |(_, place)| place.to_world(self.before.to_local(was)) - was)
    }

    /// Its solid boxes touching `area`, in world space (until `f` returns true).
    pub fn solids(&self, c: &Content, area: Aabb, mut f: impl FnMut(Aabb) -> bool) -> bool {
        if !self.aabb.intersects(&area) {
            return false;
        }
        match self.blocks {
            None => f(self.aabb),
            Some((blocks, place)) => each_shape(c, &BodyCells(blocks), place.box_to_local(area), |solid, _| match solid {
                Solid::Box(b)
                    if place.rotation() != DQuat::IDENTITY
                        && RotatedBox::new(b, place)
                            .overlap_interval(area, DVec3::ZERO)
                            .is_none() =>
                {
                    false
                }
                Solid::Cylinder(_) if convex_contact(solid, place, Solid::Box(area), DVec3::ZERO).is_none() => false,
                _ => f(solid.world_bounds(place)),
            }),
        }
    }

    /// Its solid boxes something with feet at `pos` could stand on, at most `reach` below the feet.
    fn under(&self, c: &Content, pos: DVec3, half_width: f64, below: f64, above: f64, before: bool, mut f: impl FnMut(Aabb, f64)) {
        let area = Aabb {
            min: DVec3::new(pos.x - half_width, pos.y - below - 1e-3, pos.z - half_width),
            max: DVec3::new(pos.x + half_width, pos.y + above + 1e-3, pos.z + half_width),
        };
        let mut look = |b: Aabb, slip| {
            let inside = b.min.x < area.max.x && b.max.x > area.min.x && b.min.z < area.max.z && b.max.z > area.min.z;
            if inside && b.max.y <= area.max.y && b.max.y >= area.min.y {
                f(b, slip);
            }
            false
        };
        match self.blocks {
            // Some only shove, and aren't to stand on and be taken away by.
            Some((blocks, place)) => {
                let place = if before { self.before } else { place };
                let Some((lo, hi)) = blocks.bounds() else { return };
                let bounds = Aabb { min: lo.as_dvec3(), max: (hi + IVec3::ONE).as_dvec3() };
                if !place.box_to_world(bounds).intersects(&area) {
                    return;
                }
                each_shape(c, &BodyCells(blocks), place.box_to_local(area), |solid, state| {
                    if c.states[state as usize].flags & bf::NOT_STOOD_ON != 0 {
                        return false;
                    }
                    let mut world = solid.world_bounds(place);
                    let probe = Aabb::around(pos.with_y(area.max.y), half_width, 1e-3);
                    match solid {
                        Solid::Box(b) if place.rotation() != DQuat::IDENTITY => {
                            let Some(hit) = RotatedBox::new(b, place).project::<true>(probe, -DVec3::Y) else { return false };
                            if -hit.entry_normal.y < WALKABLE_Y {
                                return false;
                            }
                            world.max.y = probe.min.y - hit.enter;
                        }
                        Solid::Cylinder(cylinder) => {
                            let delta = DVec3::NEG_Y * (area.max.y - area.min.y + 1e-3);
                            let Some(hit) =
                                convex_contact(Solid::Box(probe), Transform::default(), Solid::Cylinder(cylinder.placed(place)), delta)
                            else {
                                return false;
                            };
                            if hit.normal.y < WALKABLE_Y {
                                return false;
                            }
                            world.max.y = probe.min.y + delta.y * hit.fraction;
                        }
                        _ => {}
                    };
                    look(world, c.states[state as usize].friction as f64)
                });
            }
            _ => {
                look(
                    self.aabb
                        .shifted(if before { -self.delta } else { DVec3::ZERO }),
                    0.6,
                );
            }
        }
    }

    /// How much of it a box with feet at `pos` stands on.
    fn contact(&self, c: &Content, pos: DVec3, half_width: f64, reach: f64, before: bool) -> f64 {
        let mut area = 0.0;
        self.under(c, pos, half_width, reach, 0.0, before, |b, _| {
            area += footprint_area(b, pos, half_width);
        });
        area
    }

    pub fn deck_height(&self, c: &Content, pos: DVec3, half_width: f64) -> Option<f64> {
        let mut top = None::<f64>;
        self.under(c, pos, half_width, 0.45, 0.45, false, |b, _| {
            top = Some(top.map_or(b.max.y, |y| y.max(b.max.y)));
        });
        top
    }
}

fn footprint_area(b: Aabb, pos: DVec3, half_width: f64) -> f64 {
    let width = |lo: f64, hi: f64, at: f64| (hi.min(at + half_width) - lo.max(at - half_width)).max(0.0);
    width(b.min.x, b.max.x, pos.x) * width(b.min.z, b.max.z, pos.z)
}

fn ground_material(c: &Content, block: &impl Cells, bodies: &[Body<'_>], skip: u64, pos: DVec3, half_width: f64) -> f64 {
    let mut top = None::<(f64, f64, f64)>;
    let mut consider = |shape: Aabb, slip| {
        let y = shape.max.y;
        let area = footprint_area(shape, pos, half_width);
        if area > 0.0 && top.is_none_or(|(height, coverage, _)| y > height + 1e-7 || (y - height).abs() <= 1e-7 && area > coverage) {
            top = Some((y, area, slip));
        }
    };
    let mut body_underfoot = false;
    for b in bodies.iter().filter(|b| b.id != skip) {
        b.under(c, pos, half_width, 0.0, 0.0, false, |shape, slip| {
            body_underfoot = true;
            consider(shape, slip);
        });
    }
    if body_underfoot {
        // At the junction with a deck the world can still be bearing most of the footprint.
        let probe = Aabb::around(pos - DVec3::Y * 1e-3, half_width, 2e-3);
        each_solid_with_state(c, block, probe, |shape, state| {
            if shape.max.y >= probe.min.y && shape.max.y <= probe.max.y {
                consider(shape, c.states[state as usize].friction as f64);
            }
            false
        });
    }
    top.map_or_else(
        || {
            block
                .main((pos - DVec3::Y * 0.5).floor().as_ivec3())
                .map_or(0.6, |s| c.states[s as usize].friction as f64)
        },
        |(_, _, slip)| slip,
    )
}

/// What a box stands on, given where its feet are against each body: the one it has most under it.
/// Two moving differently with as much under it carry it nowhere, for no list order to decide.
fn most_under<'a>(
    c: &Content,
    bodies: &'a [Body<'a>],
    skip: u64,
    half_width: f64,
    reach: f64,
    pos: DVec3,
    before: bool,
) -> Option<&'a Body<'a>> {
    let mut best: Option<(&Body, f64)> = None;
    let mut tied = false;
    for b in bodies.iter().filter(|b| b.id != skip) {
        let area = b.contact(c, pos, half_width, reach, before);
        if area <= 1e-9 {
            continue;
        }
        match best {
            Some((_, most)) if area > most + 1e-9 => (best, tied) = (Some((b, area)), false),
            Some((other, most)) if area >= most - 1e-9 => tied |= other.carry_from(pos) != b.carry_from(pos),
            Some(_) => {}
            None => best = Some((b, area)),
        }
    }
    best.filter(|_| !tied).map(|b| b.0)
}

/// What a rider's support does to it before its own move: carries it by `carry`, or lets go
/// (`released`: the support's last move, for keeping its momentum).
#[derive(Default)]
pub struct Ride {
    pub carry: DVec3,
    pub released: Option<DVec3>,
}

/// A rider moves with the body it stands on, and keeps doing so while in the air above it up to
/// the body's carry height (its own motion stays relative to the body, like inside a moving
/// ship); further away it lets go. What stood on a body before its move rides from that move.
pub fn ride(c: &Content, pos: DVec3, half_width: f64, on_ground: bool, support: &mut Option<u64>, bodies: &[Body<'_>], skip: u64) -> Ride {
    if support.is_none() && on_ground {
        *support = most_under(c, bodies, skip, half_width, 0.0, pos, true).map(|b| b.id);
    }
    let Some(id) = *support else { return Ride::default() };
    let Some(b) = bodies.iter().find(|b| b.id == id) else {
        *support = None;
        return Ride::default();
    };
    // Where the body was before its move, which is where the rider stood on it.
    let past = b
        .blocks
        .filter(|(_, place)| place.rotation() != b.before.rotation())
        .and_then(|(s, _)| s.bounds())
        .map_or_else(
            || b.aabb.shifted(-b.delta),
            |(lo, hi)| {
                b.before
                    .box_to_world(Aabb { min: lo.as_dvec3(), max: (hi + IVec3::ONE).as_dvec3() })
            },
        );
    let (lo, hi) = (past.min, past.max);
    let mut carry = b.carry_from(pos);
    if on_ground
        && b.blocks
            .is_some_and(|(_, place)| place.rotation() != b.before.rotation())
    {
        let at = pos + carry;
        if let Some(y) = b.deck_height(c, at, half_width) {
            carry.y = y - pos.y;
        }
    }
    // A rider right at the edge stays on (exact edges come and go with rounding).
    let e = half_width + 1e-6;
    let over = pos.x + e > lo.x && pos.x - e < hi.x && pos.z + e > lo.z && pos.z - e < hi.z;
    // Anywhere from its bottom (inside a structure's cabin, say) up to its carry height.
    if over && pos.y > lo.y - 0.25 && pos.y - hi.y <= b.carry.max(0.01) {
        Ride { carry, released: None }
    } else {
        *support = None;
        Ride { carry: DVec3::ZERO, released: Some(carry) }
    }
}

/// The body under a box with feet at `pos`, its top at most `reach` below the feet: the frame
/// a position put there belongs to.
pub fn body_below(c: &Content, pos: DVec3, half_width: f64, bodies: &[Body<'_>], reach: f64) -> Option<u64> {
    most_under(c, bodies, 0, half_width, reach, pos, false).map(|b| b.id)
}

/// What bodies did to a box before its own move: the last move of one it let go of (for keeping
/// its momentum), and the hazards of one that squeezed it into blocks (`hazard_at::CRUSHED`).
pub struct Carried {
    pub released: Option<DVec3>,
    pub crushed: Vec<crate::content::Hazard>,
}

/// The body a box rides moves it first (blocks still stop it), then bodies that ran into it push.
/// The order and the rules are the same for a player's prediction and for the server.
#[allow(clippy::too_many_arguments)]
pub fn carried(
    c: &Content,
    block: &impl Cells,
    pos: &mut DVec3,
    half_width: f64,
    height: f64,
    on_ground: bool,
    support: &mut Option<u64>,
    bodies: &[Body<'_>],
    skip: u64,
) -> Carried {
    let rode = ride(c, *pos, half_width, on_ground, support, bodies, skip);
    if rode.carry != DVec3::ZERO {
        *pos = move_box_excluding(c, block, bodies, [skip, support.unwrap_or(0)], *pos, half_width, height, rode.carry, 0.0, 0.0).pos;
    }
    let (shift, crushes) = push(c, *pos, half_width, height, bodies, skip, *support);
    let mut crushed = Vec::new();
    if shift != DVec3::ZERO {
        let to = move_box(c, block, *pos, half_width, height, shift, 0.0).pos;
        // It hurts what it cannot push away.
        if (to - *pos - shift).length_squared() > 1e-6 {
            crushed = crushes
                .iter()
                .filter(|h| h.at == weft_abi::hazard_at::CRUSHED)
                .copied()
                .collect();
        }
        *pos = to;
    }
    let crushes_rotated = |b: &&Body<'_>| {
        b.id != skip
            && Some(b.id) != *support
            && b.blocks
                .is_some_and(|(_, place)| place.rotation() != DQuat::IDENTITY && place != b.before)
            && b.crushes
                .iter()
                .any(|h| h.at == weft_abi::hazard_at::CRUSHED)
    };
    let overlaps = |b: &Body<'_>, pos| {
        let area = Aabb::around(pos, half_width, height);
        b.solids(c, area, |solid| solid.intersects(&area))
    };
    // Settle into the current frame before the caller chooses a new pose. Otherwise
    // a wall that moved a little into the standing box can make a walker crawl under it.
    if bodies
        .iter()
        .any(|b| b.id != skip && b.blocks.is_some() && overlaps(b, *pos))
    {
        *pos = move_box_with(c, block, bodies, skip, *pos, half_width, height, DVec3::ZERO, 0.0).pos;
        for b in bodies
            .iter()
            .filter(crushes_rotated)
            .filter(|b| overlaps(b, *pos))
        {
            crushed.extend(
                b.crushes
                    .iter()
                    .filter(|h| h.at == weft_abi::hazard_at::CRUSHED)
                    .copied(),
            );
        }
    }
    Carried { released: rode.released, crushed }
}

/// The body whose top a box with feet at `pos` rests on.
pub fn standing_on(c: &Content, pos: DVec3, half_width: f64, bodies: &[Body<'_>], skip: u64) -> Option<u64> {
    most_under(c, bodies, skip, half_width, 0.0, pos, false).map(|b| b.id)
}

/// How far bodies that moved this step and overlap a box push it out of them, along each body's
/// main direction of travel, and the hazards of those that did. A box already inside goes out too
/// (like vanilla's shulker boxes): a body that showed up around a rider, or a rider put on a
/// deck the client draws ahead of the server, must not stay stuck in it. The body it rides and
/// `skip` (itself) do not push.
pub fn push<'a>(
    c: &Content,
    pos: DVec3,
    half_width: f64,
    height: f64,
    bodies: &[Body<'a>],
    skip: u64,
    support: Option<u64>,
) -> (DVec3, &'a [crate::content::Hazard]) {
    let me = Aabb::around(pos, half_width, height);
    let (mut shift, mut crushes): (DVec3, &[crate::content::Hazard]) = (DVec3::ZERO, &[]);
    for b in bodies
        .iter()
        .filter(|b| b.id != skip && Some(b.id) != support && b.aabb.intersects(&me))
    {
        let change = b.change_at((me.min + me.max) * 0.5);
        if change == DVec3::ZERO {
            continue;
        }
        // Rotated contacts, including overlap with the support, go through the character sweep.
        if b.blocks
            .is_some_and(|(_, p)| p.rotation() != DQuat::IDENTITY)
        {
            continue;
        }
        let d = change.abs();
        let axis = if d.y >= d.x && d.y >= d.z {
            1
        } else if d.x >= d.z {
            0
        } else {
            2
        };
        let up = change[axis] > 0.0;
        b.solids(c, me, |s| {
            if s.intersects(&me) {
                let need = if up { s.max[axis] - me.min[axis] } else { s.min[axis] - me.max[axis] };
                if need.abs() > shift[axis].abs() {
                    shift[axis] = need;
                }
                if b.crushes
                    .iter()
                    .any(|h| h.at == weft_abi::hazard_at::CRUSHED)
                {
                    crushes = b.crushes;
                }
            }
            false
        });
    }
    (shift, crushes)
}

fn each_solid_with_state(c: &Content, block: &impl Cells, area: Aabb, mut f: impl FnMut(Aabb, u32) -> bool) -> bool {
    each_shape(c, block, area, |shape, state| f(shape.bounds(), state))
}

fn each_shape(c: &Content, block: &impl Cells, area: Aabb, mut f: impl FnMut(Solid, u32) -> bool) -> bool {
    let lo = area.min.floor().as_ivec3();
    let hi = area.max.floor().as_ivec3();
    let mut visited = Vec::new();
    for y in lo.y..=hi.y {
        for z in lo.z..=hi.z {
            for x in lo.x..=hi.x {
                let p = IVec3::new(x, y, z);
                let b = p.as_dvec3();
                let full = Aabb { min: b, max: b + DVec3::ONE };
                let Some((s, extras)) = block.cell(p) else {
                    if f(Solid::Box(full), crate::content::AIR) {
                        return true;
                    }
                    continue;
                };
                for part in extras.parts(s) {
                    let s = part.state;
                    let (owner, resident) = part.owner(p);
                    if owner != p || !c.states[s as usize].cells.is_empty() || resident.offset != IVec3::ZERO {
                        if visited.contains(&(owner, resident)) {
                            continue;
                        }
                        visited.push((owner, resident));
                    }
                    for shape in block_solids(b + part.shift(), &c.states[s as usize]) {
                        if f(shape, s) {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

fn each_merged_shape(c: &Content, block: &impl Cells, area: Aabb, mut visit: impl FnMut(Solid)) {
    let mut boxes = CONTACT_SOLIDS.with_borrow_mut(std::mem::take);
    boxes.clear();
    each_shape(c, block, area, |shape, _| {
        if shape.bounds().intersects(&area) {
            match shape {
                Solid::Box(b) => boxes.push(b),
                Solid::Cylinder(_) => visit(shape),
            }
        }
        false
    });
    merge_adjacent_boxes(&mut boxes);
    for &b in &boxes {
        visit(Solid::Box(b));
    }
    CONTACT_SOLIDS.with_borrow_mut(|saved| *saved = boxes);
}

// Adjacent boxes with the same cross section have one continuous surface for contact normals.
fn merge_adjacent_boxes(boxes: &mut Vec<Aabb>) {
    for axis in [0, 1, 2] {
        if boxes.len() < 2 {
            break;
        }
        let (a, b) = ((axis + 1) % 3, (axis + 2) % 3);
        boxes.sort_unstable_by(|p, q| {
            p.min[a]
                .total_cmp(&q.min[a])
                .then_with(|| p.max[a].total_cmp(&q.max[a]))
                .then_with(|| p.min[b].total_cmp(&q.min[b]))
                .then_with(|| p.max[b].total_cmp(&q.max[b]))
                .then_with(|| p.min[axis].total_cmp(&q.min[axis]))
        });
        let mut kept = 0;
        for i in 0..boxes.len() {
            let next = boxes[i];
            if kept > 0 {
                let last = &mut boxes[kept - 1];
                if last.min[a] == next.min[a]
                    && last.max[a] == next.max[a]
                    && last.min[b] == next.min[b]
                    && last.max[b] == next.max[b]
                    && last.max[axis] == next.min[axis]
                {
                    last.max[axis] = next.max[axis];
                    continue;
                }
            }
            boxes[kept] = next;
            kept += 1;
        }
        boxes.truncate(kept);
    }
}

/// How far along `from`..`to` (0..1) a cube `half` each way around `from` goes before something
/// solid stops it; what it starts inside of doesn't.
pub fn sweep(c: &Content, block: &impl Cells, from: DVec3, to: DVec3, half: f64) -> f64 {
    let d = to - from;
    if d.length_squared() < 1e-12 {
        return 1.0;
    }
    let h = DVec3::splat(half);
    let area = Aabb { min: from.min(to) - h, max: from.max(to) + h };
    let mut t = 1.0f64;
    each_shape(c, block, area, |solid, _| {
        let Solid::Box(b) = solid else {
            let moving = Solid::Box(Aabb { min: from - h, max: from + h });
            if let Some(hit) = convex_contact(moving, Transform::default(), solid, d).filter(|h| h.depth <= 1e-7) {
                t = t.min(hit.fraction);
            }
            return false;
        };
        let grown = Aabb { min: b.min - h, max: b.max + h };
        let inside = from.cmpgt(grown.min).all() && from.cmplt(grown.max).all();
        if !inside {
            if let Some((hit, _)) = ray_aabb(from, d, &grown, 1.0) {
                t = t.min(hit);
            }
        }
        false
    });
    t
}

/// Whether `area` overlaps anything solid.
pub fn collides(c: &Content, block: &impl Cells, area: Aabb) -> bool {
    rotated_collides(c, block, Solid::Box(area), Transform::of(Transform::FEET))
}

pub fn occupied(c: &Content, block: &impl Cells, bodies: &[Body<'_>], skip: u64, area: Aabb) -> bool {
    collides(c, block, area)
        || bodies
            .iter()
            .any(|b| b.id != skip && b.solids(c, area, |s| s.intersects(&area)))
}

thread_local! {
    /// `move_box_with`'s obstacles, kept between calls.
    static SOLIDS: std::cell::RefCell<Vec<Aabb>> = const { std::cell::RefCell::new(Vec::new()) };
    static BODY_SOLIDS: std::cell::RefCell<Vec<Collider>> = const { std::cell::RefCell::new(Vec::new()) };
    static CONTACT_SOLIDS: std::cell::RefCell<Vec<Aabb>> = const { std::cell::RefCell::new(Vec::new()) };
}

pub struct MoveResult {
    pub pos: DVec3,
    pub velocity: DVec3,
    pub hit: [bool; 3],
    pub on_ground: bool,
}

/// Moves a box with feet at `pos` by `delta`. `block(p)` returns the state at p, or `None` for
/// unloaded positions which are treated as solid so nothing falls out of the world while loading.
pub fn move_box(c: &Content, block: &impl Cells, pos: DVec3, half_width: f64, height: f64, delta: DVec3, step_height: f64) -> MoveResult {
    move_box_with(c, block, &[], 0, pos, half_width, height, delta, step_height)
}

/// `move_box` that also bumps into solid bodies (all but `skip`).
#[allow(clippy::too_many_arguments)]
pub fn move_box_with(
    c: &Content,
    block: &impl Cells,
    bodies: &[Body<'_>],
    skip: u64,
    pos: DVec3,
    half_width: f64,
    height: f64,
    delta: DVec3,
    step_height: f64,
) -> MoveResult {
    move_box_excluding(c, block, bodies, [skip, 0], pos, half_width, height, delta, step_height, 0.0)
}

#[allow(clippy::too_many_arguments)]
fn move_box_excluding(
    c: &Content,
    block: &impl Cells,
    bodies: &[Body<'_>],
    skip: [u64; 2],
    pos: DVec3,
    half_width: f64,
    height: f64,
    delta: DVec3,
    step_height: f64,
    grip: f64,
) -> MoveResult {
    let start = Aabb::around(pos, half_width, height);
    let bodies = || bodies.iter().filter(|b| !skip.contains(&b.id));
    let mut body_shapes = BODY_SOLIDS.with_borrow_mut(std::mem::take);
    body_shapes.clear();
    // Sliding can introduce movement on an axis absent from the original displacement.
    let radius = DVec3::splat(delta.length() + character::RECOVERY);
    let area = Aabb { min: start.min - radius - DVec3::Y * step_height, max: start.max + radius + DVec3::Y * step_height };
    for b in bodies().filter(|b| b.aabb.intersects(&area)) {
        if let Some((blocks, place)) = b.blocks {
            each_merged_shape(c, &BodyCells(blocks), place.box_to_local(area), |s| {
                if s.world_bounds(place).intersects(&area) {
                    body_shapes.push(Collider::new(s, place));
                }
            });
        }
    }
    let mut has_structure = !body_shapes.is_empty();
    let mut bounds = if has_structure {
        area
    } else {
        let mut bounds = start.swept(delta);
        bounds.max.y += step_height;
        bounds
    };
    let mut solids = SOLIDS.with_borrow_mut(std::mem::take);
    solids.clear();
    let collect = |bounds: Aabb, wide: bool, solids: &mut Vec<Aabb>, body_shapes: &mut Vec<Collider>| {
        each_shape(c, block, bounds, |solid, _| {
            let b = solid.bounds();
            match solid {
                Solid::Box(_) if !wide || b.intersects(&bounds) => solids.push(b),
                Solid::Cylinder(_) if b.intersects(&bounds) => body_shapes.push(Collider::new(solid, Transform::default())),
                _ => {}
            }
            false
        })
    };
    collect(bounds, has_structure, &mut solids, &mut body_shapes);
    if !has_structure && !body_shapes.is_empty() {
        bounds = area;
        has_structure = true;
        solids.clear();
        body_shapes.clear();
        collect(bounds, true, &mut solids, &mut body_shapes);
    }
    if has_structure {
        merge_adjacent_boxes(&mut solids);
    }
    for b in bodies().filter(|b| b.blocks.is_none() && b.aabb.intersects(&bounds)) {
        solids.push(b.aabb);
    }
    let sweep = Sweep { aligned: &solids, structures: &body_shapes };
    let start = if has_structure { sweep.recover(start) } else { start };
    let travel = |from, wish| {
        if has_structure { sweep.travel(from, wish, grip) } else { axis_travel(&solids, from, wish) }
    };
    let mut path = travel(start, delta);
    if step_height > 0.0 && (path.hit[0] || path.hit[2]) && delta.y <= 0.0 {
        let vertical = |at, dy| {
            if has_structure { sweep.vertical(at, dy) } else { axis_travel(&solids, at, DVec3::Y * dy) }
        };
        let up = vertical(start, step_height);
        let lifted = up.area.min.y - start.min.y;
        if lifted > 1e-7 {
            let side = travel(up.area, delta.with_y(0.0));
            let down = vertical(side.area, delta.y - lifted);
            let gain = |a: Aabb| (a.min - start.min).with_y(0.0).length_squared();
            if (down.floor || !has_structure) && gain(down.area) > gain(path.area) + 1e-6 {
                path = Travel {
                    area: down.area,
                    velocity: side.velocity.with_y(if down.floor { 0.0 } else { delta.y }),
                    hit: [side.hit[0], down.floor, side.hit[2]],
                    floor: down.floor,
                };
            }
        }
    }
    if has_structure && delta.y <= 0.0 && (path.floor || step_height > 0.0) {
        let reach = (path.area.min - start.min)
            .with_y(0.0)
            .length()
            .min(character::RECOVERY);
        let floor = sweep.floor(path.area, reach);
        path.floor = floor.is_some();
        if let Some(at) = floor {
            path.area = at;
            path.velocity.y = 0.0;
            path.hit[1] = true;
        }
    }
    let pos = path.area.min + DVec3::new(half_width, 0.0, half_width);
    SOLIDS.with_borrow_mut(|s| *s = solids);
    BODY_SOLIDS.with_borrow_mut(|s| *s = body_shapes);
    MoveResult { pos, hit: path.hit, velocity: path.velocity, on_ground: path.floor }
}

fn axis_travel(solids: &[Aabb], mut area: Aabb, delta: DVec3) -> Travel {
    let mut hit = [false; 3];
    let mut velocity = delta;
    for axis in [1usize, 0, 2] {
        let mut d = delta[axis];
        if d == 0.0 {
            continue;
        }
        for s in solids {
            let overlap = (0..3)
                .filter(|&a| a != axis)
                .all(|a| area.min[a] < s.max[a] - 1e-7 && area.max[a] > s.min[a] + 1e-7);
            if !overlap {
                continue;
            }
            if d > 0.0 && area.max[axis] <= s.min[axis] + 1e-7 {
                d = d.min(s.min[axis] - area.max[axis]);
            } else if d < 0.0 && area.min[axis] >= s.max[axis] - 1e-7 {
                d = d.max(s.max[axis] - area.min[axis]);
            }
        }
        if d != delta[axis] {
            hit[axis] = true;
            velocity[axis] = 0.0;
        }
        area.min[axis] += d;
        area.max[axis] += d;
    }
    Travel { area, velocity, hit, floor: hit[1] && delta.y <= 0.0 }
}

#[derive(Clone, Copy, Debug)]
pub struct Hit {
    /// In the coordinates of `frame`: the moving structure (entity) the block is part of, 0 for
    /// the world.
    pub block: IVec3,
    /// The cell of the block hit: `block` itself, or a big block's own cell of the instance.
    pub origin: IVec3,
    pub offset: IVec3,
    pub frame: u64,
    /// Face index (-X, +X, -Y, +Y, -Z, +Z) that was entered.
    pub face: u8,
    pub state: u32,
    /// From the ray's start to where it entered the block's selection.
    pub dist: f64,
}

impl Hit {
    pub fn part(self) -> Part {
        Part { state: self.state, offset: self.offset }
    }
}

/// The one way to find what a ray from `eye` hits within `reach`: the nearest selection box of
/// the world's blocks and of moving structures (`(entity, where its blocks are, its blocks)`), by
/// where the ray enters it. The client's crosshair, the server's and mods'
/// `look_at` all ask this. `flags`: `abi::ray::LIQUIDS`, `SOLID`.
pub fn pick(
    c: &Content,
    world: &impl Cells,
    structures: &[(u64, Transform, &dyn Blocks)],
    eye: DVec3,
    dir: DVec3,
    reach: f64,
    flags: u32,
) -> Option<Hit> {
    let mut best = raycast(c, world, eye, dir, reach, flags);
    for &(id, place, blocks) in structures {
        if let Some(mut h) = raycast(c, &BodyCells(blocks), place.to_local(eye), place.vector_to_local(dir), reach, flags) {
            if best.is_none_or(|b| h.dist < b.dist) {
                h.frame = id;
                best = Some(h);
            }
        }
    }
    best
}

/// Where a ray enters box `b` (0 when it starts inside), within `max`, and the face it went in
/// through (-X, +X, -Y, +Y, -Z, +Z; inside: the one it looks most toward).
pub fn ray_aabb(o: DVec3, d: DVec3, b: &Aabb, max: f64) -> Option<(f64, u8)> {
    let (t0, t1, face) = slab(o, d, b.min, b.max, max)?;
    if t0 < 0.0 {
        let axis = (0..3)
            .max_by(|&a, &b| d[a].abs().total_cmp(&d[b].abs()))
            .unwrap_or(0);
        return (t1 >= 0.0).then_some((0.0, (axis * 2) as u8 + (d[axis] < 0.0) as u8));
    }
    Some((t0, face))
}

/// The slab test: along `d`, where the ray enters and leaves the box, and the face it enters by.
fn slab(o: DVec3, d: DVec3, lo: DVec3, hi: DVec3, max: f64) -> Option<(f64, f64, u8)> {
    let (mut t0, mut t1, mut face) = (f64::NEG_INFINITY, max, 0u8);
    for i in 0..3 {
        if d[i].abs() < 1e-12 {
            if o[i] < lo[i] || o[i] > hi[i] {
                return None;
            }
            continue;
        }
        let a = (lo[i] - o[i]) / d[i];
        let c = (hi[i] - o[i]) / d[i];
        let (near, far) = if a < c { (a, c) } else { (c, a) };
        if near > t0 {
            t0 = near;
            // Entering through the low side means the -axis face.
            face = (i * 2) as u8 + if d[i] > 0.0 { 0 } else { 1 };
        }
        t1 = t1.min(far);
    }
    (t0 <= t1).then_some((t0, t1, face))
}

/// Amanatides-Woo traversal. Blocks are hit where the ray crosses their selection boxes; of
/// blocks the ray starts inside, only what is solid where it starts (a head stuck in a block).
/// First targetable block along the ray; with `abi::ray::LIQUIDS` liquids stop it too, with
/// `SOLID` blocks are hit by their collision instead, what stops a body.
pub fn raycast(c: &Content, block: &impl Cells, origin: DVec3, dir: DVec3, max: f64, flags: u32) -> Option<Hit> {
    let sources = flags & LIQUID_SOURCES != 0;
    let (liquids, solid) = (sources || flags & weft_abi::ray::LIQUIDS != 0, flags & weft_abi::ray::SOLID != 0);
    let dir = dir.normalize();
    let mut p = origin.floor().as_ivec3();
    let step = dir.signum().as_ivec3();
    let inv = DVec3::ONE / dir.abs().max(DVec3::splat(1e-12));
    let mut t_max = DVec3::ZERO;
    for i in 0..3 {
        let o = origin[i] - p[i] as f64;
        t_max[i] = if dir[i] > 0.0 { (1.0 - o) * inv[i] } else { o * inv[i] };
    }
    let mut t = 0.0;
    while t <= max {
        if let Some((main, layout)) = block.cell(p) {
            let mut best: Option<Hit> = None;
            for part in layout.parts(main) {
                let state = part.state;
                let (at, resident) = part.owner(p);
                let local = origin - p.as_dvec3() - part.shift();
                let s = &c.states[state as usize];
                if sources
                    && s.liquid
                        .is_some_and(|liquid| state != c.liquids[liquid as usize].first_state)
                {
                    continue;
                }
                let wanted = liquids && s.liquid.is_some();
                if s.flags & bf::NOT_TARGETABLE != 0 && !wanted && !solid {
                    continue;
                }
                let stuck = s.cylinder.is_none()
                    && t == 0.0
                    && s.collision
                        .iter()
                        .any(|b| (0..3).all(|i| (b[i] as f64..b[i + 3] as f64).contains(&local[i])));
                let near = if let Some(cylinder) = s.cylinder {
                    convex::cylinder_ray(cylinder, local, dir, max)
                } else if stuck {
                    // Entered, as it were, through the face it looks most toward.
                    let axis = (0..3)
                        .max_by(|&a, &b| dir[a].abs().total_cmp(&dir[b].abs()))
                        .unwrap_or(0);
                    Some((0.0, (axis * 2) as u8 + (dir[axis] < 0.0) as u8))
                } else {
                    let boxes = if solid && !wanted { &s.collision } else { &s.selection };
                    boxes
                        .iter()
                        .filter_map(|b| ray_box(local, dir, b, max))
                        .min_by(|a, b| a.0.total_cmp(&b.0))
                };
                if let Some((dist, face)) = near.filter(|(dist, _)| *dist + 1e-8 >= t && *dist <= t_max.min_element() + 1e-8) {
                    if wanted {
                        return Some(Hit { frame: 0, block: p, origin: at, offset: resident.offset, face, state, dist });
                    }
                    if best.is_none_or(|b| dist < b.dist) {
                        best = Some(Hit { frame: 0, block: p, origin: at, offset: resident.offset, face, state, dist });
                    }
                }
            }
            if best.is_some() {
                return best;
            }
        }
        let axis = if t_max.x < t_max.y && t_max.x < t_max.z {
            0
        } else if t_max.y < t_max.z {
            1
        } else {
            2
        };
        t = t_max[axis];
        t_max[axis] += inv[axis];
        p[axis] += step[axis];
    }
    None
}

/// Entry distance and entered face, `None` if missed or if the ray starts inside.
fn ray_box(o: DVec3, d: DVec3, b: &[f32; 6], max: f64) -> Option<(f64, u8)> {
    let lo = DVec3::new(b[0] as f64, b[1] as f64, b[2] as f64);
    let hi = DVec3::new(b[3] as f64, b[4] as f64, b[5] as f64);
    slab(o, d, lo, hi, max)
        .filter(|s| s.0 >= 0.0)
        .map(|s| (s.0, s.2))
}

/// Whether liquid fills the block `level` above the feet at `pos` (0 the feet, the eye height for
/// eyes): the one question of "in water", asked at the height the caller means.
pub fn in_liquid(c: &Content, block: &impl Cells, pos: DVec3, level: f64) -> bool {
    block.liquid_block_at(c, pos + DVec3::Y * level).is_some()
}

/// What the cells a box of that height stands in do to it, by all that is in them: the one answer
/// for whoever asks about a player or a mob, here or on a client.
#[derive(Clone, Copy, Default)]
pub struct Around {
    /// Liquid at its feet or around the middle of its body.
    pub in_liquid: bool,
    /// Something at its feet holds it up, like a ladder.
    pub climbing: bool,
}

/// How far above its feet "the middle of its body" is, for liquids: a giant's isn't wading.
pub fn body_level(height: f64) -> f64 {
    (height * 0.5).min(1.0)
}

pub fn around(c: &Content, block: &impl Cells, pos: DVec3, height: f64, half_width: f64) -> Around {
    let area = Aabb::around(pos, half_width + 1e-4, height);
    let lo = area.min.floor().as_ivec3();
    let hi = area.max.floor().as_ivec3();
    let climbing = (lo.y..=hi.y).any(|y| {
        (lo.z..=hi.z).any(|z| {
            (lo.x..=hi.x).any(|x| {
                let cell = IVec3::new(x, y, z);
                block.cell(cell).is_some_and(|(main, layout)| {
                    layout.parts(main).any(|part| {
                        c.states[part.state as usize].flags & bf::CLIMBABLE != 0
                            && part
                                .bounds(c)
                                .any(|b| area.intersects(&Aabb { min: cell.as_dvec3() + b.min, max: cell.as_dvec3() + b.max }))
                    })
                })
            })
        })
    });
    Around { in_liquid: in_liquid(c, block, pos, 0.0) || in_liquid(c, block, pos, body_level(height)), climbing }
}

/// Which liquid a point is under, its surface considered: water in a block only fills it up to
/// its level, unless more of the same is on top.
pub fn liquid_at(c: &Content, block: &impl Cells, p: DVec3) -> Option<u16> {
    let at = p.floor().as_ivec3();
    let (liquid, surface) = surface_of(c, block, at, block.liquid_block_at(c, p)?)?;
    (p.y <= surface).then_some(liquid)
}

pub fn liquid_surface(c: &Content, block: &impl Cells, at: IVec3) -> Option<(u16, f64)> {
    surface_of(c, block, at, block.liquid(c, at)?)
}

fn surface_of(c: &Content, block: &impl Cells, at: IVec3, state: u32) -> Option<(u16, f64)> {
    let st = &c.states[state as usize];
    let liquid = st.liquid?;
    let covered = block
        .liquid(c, at + IVec3::Y)
        .is_some_and(|s| c.states[s as usize].liquid == Some(liquid));
    let surface = if covered { 1.0 } else { (8.0 - st.level as f64) / 9.0 };
    Some((liquid, at.y as f64 + surface))
}

/// How a walker moves itself this tick, in blocks per tick like vanilla.
#[derive(Clone, Copy, Default)]
pub struct Control {
    /// Where it wants to go, horizontal, length up to 1.
    pub wish: DVec3,
    /// Acceleration on ground of normal friction (a player 0.1, 0.13 sprinting); slippery ground
    /// changes it the way vanilla does, so ice is fast and slides.
    pub ground: f64,
    /// Acceleration in the air.
    pub air: f64,
    /// Acceleration in liquids (vanilla's 0.02, times `SWIM_SPEED`).
    pub liquid: f64,
    /// Jump speed when it jumps off the ground, 0 = not jumping.
    pub jump: f64,
    /// Jump speed to use when a reachable ledge stops this step.
    pub auto_jump: f64,
    /// Added to the velocity with a jump (a sprinting player's leap).
    pub jump_boost: DVec3,
    /// Holds itself up in liquids (a swimming mob, a player holding jump).
    pub swim: bool,
    /// Faster and without sinking in liquids (a sprinting player).
    pub sprint: bool,
    /// Swimming: goes where it looks, this being the look's up component.
    pub swim_look: Option<f64>,
    /// Never steps off an edge, and holds on to a ladder (sneaking).
    pub careful: bool,
}

/// Vanilla's acceleration in liquids.
pub const SWIM: f64 = 0.02;

/// The walker's box, the world's pull, and what its attributes say of how it moves.
#[derive(Clone, Copy)]
pub struct Frame {
    pub half_width: f64,
    pub height: f64,
    /// Blocks per tick squared.
    pub gravity: f64,
    pub moves: Moves,
}

/// What a body's attributes say of how it moves, the same on its client and the server.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Moves {
    /// Blocks it walks up without jumping (0.6).
    pub step: f64,
    /// Multipliers (1): on its dimension's gravity, its own push in liquids, its flight, its push
    /// in the air, how slippery the ground is under it, its climbing.
    pub gravity: f64,
    pub swim: f64,
    pub fly: f64,
    pub air: f64,
    pub slip: f64,
    pub climb: f64,
}

impl Default for Moves {
    fn default() -> Self {
        Moves { step: 0.6, gravity: 1.0, swim: 1.0, fly: 1.0, air: 1.0, slip: 1.0, climb: 1.0 }
    }
}

impl Moves {
    /// From its attributes, by id.
    pub fn of(mut attr: impl FnMut(u32) -> f64) -> Moves {
        use crate::registry::attr as a;
        Moves {
            step: attr(a::STEP_HEIGHT),
            gravity: attr(a::GRAVITY),
            swim: attr(a::SWIM_SPEED),
            fly: attr(a::FLYING_SPEED),
            air: attr(a::AIR_CONTROL),
            slip: attr(a::SLIPPERINESS),
            climb: attr(a::CLIMB_SPEED),
        }
    }
}

pub struct Walked {
    pub hit: [bool; 3],
    pub on_ground: bool,
    pub jumped: bool,
    /// `around` where it started.
    pub started_in: Around,
}

/// One tick of walking, the same for players and mobs: friction (ice), drag (lava, oil),
/// ladders, liquids and steps from the blocks it is in, with vanilla's numbers. `vel` is blocks
/// per tick; a player differs only in where `ctl` comes from.
#[allow(clippy::too_many_arguments)]
pub fn walk(
    c: &Content,
    block: &impl Cells,
    bodies: &[Body<'_>],
    skip: u64,
    pos: &mut DVec3,
    vel: &mut DVec3,
    on_ground: bool,
    ctl: &Control,
    f: &Frame,
) -> Walked {
    let feet = pos.floor().as_ivec3();
    let started_in = around(c, block, *pos, f.height, f.half_width);
    let Around { in_liquid: liquid, climbing } = started_in;
    let drag = block
        .liquid_block_at(c, *pos)
        .or_else(|| {
            block
                .main(feet)
                .filter(|&s| c.states[s as usize].liquid.is_none())
        })
        .map_or(1.0, |s| c.states[s as usize].drag as f64);
    // Never quite all of the speed kept, however slippery it is made.
    let slip = if on_ground { (ground_material(c, block, bodies, skip, *pos, f.half_width) * f.moves.slip).clamp(0.0, 0.999) } else { 1.0 };
    let accelerate = |vel: &mut DVec3, k: f64| {
        let len2 = ctl.wish.length_squared();
        if len2 > 1e-7 {
            *vel += ctl.wish * (k / len2.sqrt().max(1.0));
        }
    };
    let mut jumped = false;
    if liquid {
        accelerate(vel, ctl.liquid * drag);
        if ctl.swim {
            vel.y += 0.04;
        }
        if ctl.careful {
            vel.y -= 0.04;
        }
        if let Some(look) = ctl.swim_look {
            let head_under = in_liquid(c, block, *pos, 0.9);
            if look <= 0.0 || ctl.swim || head_under {
                vel.y += (look - vel.y) * if look < -0.2 { 0.085 } else { 0.06 };
            }
        }
    } else {
        let accel = if on_ground { ctl.ground * (0.216 / slip.max(0.1).powi(3)) } else { ctl.air * f.moves.air };
        if ctl.jump > 0.0 && on_ground {
            vel.y = ctl.jump;
            *vel += ctl.jump_boost;
            jumped = true;
        }
        accelerate(vel, accel * drag);
        if climbing {
            let most = 0.15 * f.moves.climb;
            vel.x = vel.x.clamp(-most, most);
            vel.z = vel.z.clamp(-most, most);
            vel.y = vel.y.max(if ctl.careful { 0.0 } else { -most });
        }
    }
    // Careful at an edge (vanilla's sneaking): each horizontal component is cut back in 0.05
    // steps while the moved box would have nothing under it, so it slides along the edge.
    if ctl.careful && on_ground {
        let ground = |dx: f64, dz: f64| {
            let area = Aabb::around(*pos + DVec3::new(dx, -0.6, dz), f.half_width, f.height);
            occupied(c, block, bodies, skip, area)
        };
        let shrink = |v: f64| if v.abs() < 0.05 { 0.0 } else { v - 0.05 * v.signum() };
        while vel.x != 0.0 && !ground(vel.x, 0.0) {
            vel.x = shrink(vel.x);
        }
        while vel.z != 0.0 && !ground(0.0, vel.z) {
            vel.z = shrink(vel.z);
        }
        while vel.x != 0.0 && vel.z != 0.0 && !ground(vel.x, vel.z) {
            vel.x = shrink(vel.x);
            vel.z = shrink(vel.z);
        }
    }
    let before = *pos;
    // Slipperiness is the retained speed. A quadratic conversion gives ordinary blocks
    // static grip on a gentle slope while leaving nearly all of the slope force on ice.
    let grip = 2.5 * (1.0 - slip).powi(2);
    let move_by = |delta, step| move_box_excluding(c, block, bodies, [skip, 0], before, f.half_width, f.height, delta, step, grip);
    let step = if on_ground { f.moves.step } else { 0.0 };
    let travel = *vel;
    let mut r = move_by(travel, step);
    if ctl.auto_jump > 0.0 && on_ground && !jumped && !liquid && !climbing && !ctl.careful && (r.hit[0] || r.hit[2]) {
        let max_rise = jump_rise(ctl.auto_jump, f.gravity);
        if max_rise > step {
            let landing = move_by(*vel, max_rise).pos;
            if landing.y > before.y + step + 1e-6 && (landing - r.pos).dot(ctl.wish) > 1e-6 {
                vel.y = ctl.auto_jump;
                *vel += ctl.jump_boost;
                r = move_by(*vel, 0.0);
                jumped = true;
            }
        }
    }
    *pos = r.pos;
    *vel = r.velocity;
    if (r.hit[0] || r.hit[2] || ctl.jump > 0.0) && climbing {
        vel.y = 0.2 * f.moves.climb;
    }
    if liquid {
        let keep = if ctl.sprint { 0.9 } else { 0.8 };
        vel.x *= keep;
        vel.z *= keep;
        vel.y *= 0.8;
        if !ctl.sprint {
            vel.y -= f.gravity / 16.0;
        }
        // Up a ledge out of the water, vanilla's little boost at a wall with room above it.
        if r.hit[0] || r.hit[2] {
            let at = DVec3::new(before.x + travel.x, pos.y + vel.y + 0.6, before.z + travel.z);
            let above = Aabb::around(at, f.half_width, f.height);
            if !occupied(c, block, bodies, skip, above) {
                vel.y = 0.3;
            }
        }
    } else {
        vel.y = falling_speed(vel.y, f.gravity);
        vel.x *= slip * 0.91;
        vel.z *= slip * 0.91;
    }
    Walked { hit: r.hit, on_ground: r.on_ground, jumped, started_in }
}

fn falling_speed(speed: f64, gravity: f64) -> f64 {
    (speed - gravity) * 0.98
}

/// Limit the ledge probe to a normal jump's reach, even on worlds with unusual gravity.
fn jump_rise(mut speed: f64, gravity: f64) -> f64 {
    const MAX_RISE: f64 = 1.25;
    let mut rise = 0.0;
    for _ in 0..crate::world::TPS {
        if speed <= 0.0 || rise >= MAX_RISE {
            break;
        }
        rise += speed;
        speed = falling_speed(speed, gravity);
    }
    rise.min(MAX_RISE)
}

/// What flying is told: horizontal `wish` (length up to 1), going up or down, and how fast.
pub struct Flight {
    pub wish: DVec3,
    pub up: bool,
    pub down: bool,
    /// The flight speed (1: vanilla's creative), doubled while sprinting.
    pub speed: f64,
}

/// One tick of flight, the same for players and anything else that may fly: vanilla's creative
/// numbers. `vel` is blocks per tick; `noclip` goes through everything. True when it came down
/// onto the ground, which ends a player's flight.
#[allow(clippy::too_many_arguments)]
pub fn fly(
    c: &Content,
    block: &impl Cells,
    bodies: &[Body<'_>],
    skip: u64,
    pos: &mut DVec3,
    vel: &mut DVec3,
    on_ground: &mut bool,
    half_width: f64,
    height: f64,
    f: &Flight,
    noclip: bool,
) -> bool {
    let push = 0.05 * f.speed;
    vel.y += push * 3.0 * (f.up as i32 - f.down as i32) as f64;
    let len2 = f.wish.length_squared();
    if len2 > 1e-7 {
        *vel += f.wish * (push / len2.sqrt().max(1.0));
    }
    let mut landed = false;
    if noclip {
        *pos += *vel;
        *on_ground = false;
    } else {
        let r = move_box_with(c, block, bodies, skip, *pos, half_width, height, *vel, 0.0);
        *pos = r.pos;
        *vel = r.velocity;
        *on_ground = r.on_ground;
        landed = r.on_ground && r.hit[1];
    }
    vel.y *= 0.6;
    vel.x *= 0.91;
    vel.z *= 0.91;
    landed
}

/// The push two overlapping soft bodies give each other per tick (vanilla's
/// applyEntityCollision), for an offset `d` from the other one: along the larger axis, 0.05.
pub fn nudge(d: DVec3) -> DVec3 {
    let dist = d.x.abs().max(d.z.abs()).max(0.01).sqrt();
    DVec3::new(d.x, 0.0, d.z) * ((1.0 / dist).min(1.0) / dist * 0.05)
}

#[cfg(test)]
mod knock_tests {
    use super::*;

    #[test]
    fn knock_is_vanillas() {
        let away = DVec3::X * 0.4;
        // On the ground: half the speed, away and up, never more than that up.
        assert_eq!(knock(DVec3::new(0.2, 0.0, 0.1), away, true, 0.0), DVec3::new(0.5, 0.4, 0.05));
        assert_eq!(knock(DVec3::new(0.0, 0.3, 0.0), away, true, 0.0).y, 0.4);
        // In the air it goes on up or down as it was.
        assert_eq!(knock(DVec3::new(0.0, -0.5, 0.0), away, false, 0.0).y, -0.5);
    }

    #[test]
    fn looks_where_vanilla_does() {
        assert!((look_dir(0.0, 0.0) - DVec3::NEG_Z).length() < 1e-9);
        assert!((look_dir(std::f32::consts::FRAC_PI_2, 0.0) - DVec3::NEG_X).length() < 1e-6);
        assert!((look_dir(0.0, std::f32::consts::FRAC_PI_2) - DVec3::Y).length() < 1e-6);
    }

    #[test]
    fn a_pose_scales_with_the_size() {
        use crate::world::Pose;
        let player = EntitySize { width: 0.6, height: 1.8, eye: 1.62 };
        let d = posed(player, Pose::STAND);
        assert!((d.half_width - 0.3).abs() < 1e-5 && (d.height - 1.8).abs() < 1e-5 && (d.eye - 1.62).abs() < 1e-5);
        let d = posed(player, Pose::CROUCH);
        assert!((d.height - 1.5).abs() < 1e-5 && (d.eye - 1.27).abs() < 1e-5);
        let d = posed(EntitySize { width: 1.2, height: 3.6, eye: 3.24 }, Pose::CROUCH);
        assert!((d.height - 3.0).abs() < 1e-5);
    }
}

/// `a - b` as the shortest turn, radians in -PI..PI.
pub fn angle_diff(a: f32, b: f32) -> f32 {
    (a - b + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI
}

/// One tick of vanilla's tickHeadTurn: the body turns toward where it walked this tick (`moved`,
/// in the frame of what it stands on) or stays put, and the head drags it along only once it is
/// more than 50 degrees away. The server and every client run it on the same ticks of the same
/// moves, so all of them have the same body without sending it. Radians.
pub fn turn_body(body: f32, head: f32, moved: DVec3) -> f32 {
    let mut body = body;
    let flat = moved.with_y(0.0);
    if flat.length_squared() > 0.0025 * 0.0025 {
        // Backing up, the body faces the way the head does.
        let mut toward = (-flat.x as f32).atan2(-flat.z as f32);
        if angle_diff(toward, head).abs() > 95f32.to_radians() {
            toward += std::f32::consts::PI;
        }
        body += angle_diff(toward, body) * 0.3;
    }
    let off = angle_diff(head, body);
    let limit = 50f32.to_radians();
    if off.abs() > limit {
        body += off - limit * off.signum();
    }
    // Kept near the head, so their difference never needs wrapping where it is used.
    head - angle_diff(head, body)
}
