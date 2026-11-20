//! Moving structures: blocks that move together, a `Space` in its own coordinates owned by an
//! entity whose position places it: block (0, 0, 0) is centered on the entity. Whatever the
//! world does with blocks works in its addressed space. `Transform` takes points and vectors
//! to the world when an effect or entity crosses that boundary.

use glam::{DVec3, IVec3};
use std::f64::consts::{FRAC_PI_2, TAU};

use crate::world::placement::Part;
use weft_abi as abi;

use super::dim::Dim;
use super::entity::{BodyMotion, BodyMount, Completion, Motion};
use super::signal::{Moved, RiderMeets};
use super::space::{CubeCells, FloaterPath, Space};
use crate::content::{AIR, Content};
use crate::physics::{Aabb, Solid, Transform};
use crate::world::{cube_of, local_index};
use rustc_hash::{FxHashMap, FxHashSet};

/// What has landed is there for circuits at once, and for clients this much later, once their view
/// of the body has caught up with the server's (they show bodies 1.5 ticks behind).
const REST_TICKS: u8 = 2;
/// And the body goes only after the block has reached the client, or whoever stands on it has
/// nothing under them for a moment.
const LINGER_TICKS: u8 = 2;

/// A floater's speed: blocks a second per level of power.
pub(super) const SPEED_PER_LEVEL: f64 = 0.5;
/// However many floaters, on each axis: it looks one block ahead for each it comes to, so less
/// than one a tick.
pub(super) const MOST_SPEED: f64 = 15.0;

pub(super) fn is_guide_rail(c: &Content, state: u32) -> bool {
    c.components
        .has(crate::components::BLOCK, state, c.game.guide_rail_comp)
}

pub(super) fn guide_length(c: &Content, space: &Space, at: IVec3, axis: IVec3) -> i32 {
    (1..=crate::controller::MAX_RAILS)
        .take_while(|&n| {
            space
                .get(at + axis * n)
                .is_some_and(|s| is_guide_rail(c, s))
        })
        .count() as i32
}

pub(super) struct MountedPart<'a> {
    pub id: u64,
    pub space: &'a Space,
    pub relative: Transform,
}

#[derive(Clone, Copy)]
pub(super) struct StructurePeer<'a> {
    pub id: u64,
    pub place: Transform,
    pub bounds: Aabb,
    pub space: &'a Space,
    pub delta: DVec3,
}

pub(super) fn visit_structure_shapes(
    c: &Content,
    world: &CubeCells<'_>,
    parts: &[MountedPart<'_>],
    others: &[StructurePeer<'_>],
    place: Transform,
    delta: DVec3,
    mut visit: impl FnMut(u64, Solid, Transform, DVec3, Option<StructurePeer<'_>>) -> bool,
) -> bool {
    for part in parts {
        let place = part.relative.in_frame(place);
        for group in &part.space.colliders(c).groups {
            let area = place.box_to_world(group.area).swept(delta);
            if world.may_collide(c, area)
                && crate::physics::collides(c, world, area)
                && group
                    .shapes
                    .iter()
                    .any(|&b| visit(part.id, b, place, delta, None))
            {
                return true;
            }
            for &peer in others
                .iter()
                .filter(|peer| peer.bounds.swept(peer.delta).intersects(&area))
            {
                let relative = place.relative_to(peer.place);
                let relative_delta = delta - peer.delta;
                let moved = peer.place.vector_to_local(relative_delta);
                if group.shapes.iter().any(|&b| {
                    peer.bounds
                        .intersects(&b.world_bounds(place).swept(relative_delta))
                        && visit(part.id, b, relative, moved, Some(peer))
                }) {
                    return true;
                }
            }
        }
    }
    false
}
/// Radians per tick per signal level. Long arms also have a limit on tip travel.
const TURN_PER_LEVEL: f64 = TAU / (8.0 * crate::world::TPS as f64 * 15.0);
pub(super) const TURN_PROBE_TRAVEL: f64 = 0.125;

pub(super) fn structure_pose_clear(
    c: &Content,
    world: &CubeCells<'_>,
    parts: &[MountedPart<'_>],
    peers: &[StructurePeer<'_>],
    place: Transform,
) -> bool {
    !visit_structure_shapes(c, world, parts, peers, place, DVec3::ZERO, |_, shape, place, _, other| match other {
        None => crate::physics::rotated_collides(c, world, shape, place),
        Some(peer) => crate::physics::rotated_collides(c, peer.space, shape, place),
    })
}

/// A block out of its place on its way to another: where in the body and where it ends up,
/// with the entity it has, which stays the same one.
#[derive(Clone, Copy)]
pub struct Carried {
    pub local: IVec3,
    pub to: IVec3,
    pub state: u32,
}

/// Floaters going together, and what that takes.
pub struct FloaterPlan {
    pub floaters: Vec<IVec3>,
    pub cells: FxHashSet<IVec3>,
    pub broken: Vec<IVec3>,
    pub(super) mount: Option<(IVec3, u8)>,
    lifted: Vec<Lifted>,
}

/// What `Dim::lift` takes: from where, to where in the body and where it is meant to end up, and
/// what of it (a cell of wire may have pieces that stay), if not all.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Lifted {
    pub from: IVec3,
    pub local: IVec3,
    pub to: IVec3,
    pub part: Option<u32>,
    pub only: Option<Part>,
}

/// How a body's circuits go. They always go; only that a piston on a piston's blocks can't act.
#[derive(Clone, Copy, PartialEq)]
pub enum Liveness {
    Free,
    Assembly,
    OnTheWay,
}

/// A body whose blocks have landed, waiting for clients to catch up with them.
pub struct Landing {
    body: u64,
    ticks: u8,
    /// A block in the body that turns into another a tick in: (where, into).
    turns: Option<(IVec3, u32)>,
}

pub(super) struct Transfer {
    pub(super) destination: u64,
    turn: abi::Turn,
    pub(super) parts: FxHashMap<(IVec3, Part), (IVec3, Part)>,
    pub(super) shown_at: Option<u64>,
}

/// The box a structure's blocks fill around its entity: (width, height). Block (0, 0, 0) is
/// centered on the entity, so the box reaches as far as the farthest block.
/// Each step a body going `sign` has to clear: along each axis it goes and across the corners
/// between them, once.
fn ways(sign: IVec3) -> impl Iterator<Item = IVec3> {
    (1..8)
        .filter(move |m| (m & 1 == 0 || sign.x != 0) && (m & 2 == 0 || sign.y != 0) && (m & 4 == 0 || sign.z != 0))
        .map(move |m| IVec3::new(sign.x * (m & 1), sign.y * ((m >> 1) & 1), sign.z * ((m >> 2) & 1)))
}

pub fn extent(blocks: &[(IVec3, u32)]) -> Option<(f32, f32)> {
    let mut hw: f32 = 0.0;
    let mut top = i32::MIN;
    for (p, _) in blocks {
        hw = hw
            .max((p.x as f32 - 0.5).abs())
            .max((p.x as f32 + 0.5).abs())
            .max((p.z as f32 - 0.5).abs())
            .max((p.z as f32 + 0.5).abs());
        top = top.max(p.y + 1);
    }
    (top > i32::MIN).then_some((hw * 2.0, top as f32))
}

/// The blocks a kind is built of (its template).
pub fn template(c: &Content, kind: u32) -> Option<&[(IVec3, u32)]> {
    let t = c.game.entities.get(kind as usize)?.template?;
    Some(&c.templates[t as usize].blocks)
}

impl Dim {
    /// Takes `cells` out of here into a new body of `kind` with its entity at `at`.
    pub fn lift(
        &mut self,
        space_id: u64,
        c: &'static Content,
        kind: u32,
        at: DVec3,
        cells: impl IntoIterator<Item = Lifted>,
        also: &[(IVec3, Part)],
        live: Liveness,
    ) -> (u64, Vec<Carried>) {
        if !c.dims[self.id as usize].editable {
            return (0, Vec::new());
        }
        let mut carried = Vec::new();
        let mut blocks = also.to_vec();
        let mut arriving = FxHashMap::default();
        let mut anchors = Vec::new();
        let mut moved = FxHashMap::default();
        for l in cells {
            let Some(whole) = self.get(space_id, l.from).filter(|&s| s != AIR) else { continue };
            let original = self.space(space_id).layout(l.from).clone();
            let (mut wet, mut moving): (Vec<_>, Vec<_>) = self
                .space(space_id)
                .layout(l.from)
                .residents(whole)
                .partition(|part| {
                    l.only.is_some_and(|v| v != *part) || (live == Liveness::Assembly && c.states[part.state as usize].liquid.is_some())
                });
            let Some(main) = moving.first_mut() else { continue };
            let state = l.part.unwrap_or(main.state);
            if state != main.state {
                let first = c.block_of(main.state).first_state;
                wet.push(Part { state: first + ((main.state - first) & !(state - first)), ..*main });
                main.state = state;
            }
            if self
                .write_residents(space_id, c, l.from, &mut wet, true)
                .is_none()
            {
                continue;
            }
            for part in moving {
                if let Some(id) = c
                    .game
                    .block_entities
                    .get(&c.states[part.state as usize].block)
                    .and_then(|&kind| {
                        self.entities
                            .anchored_kind(space_id, l.from, kind, part.offset)
                    })
                {
                    self.entities.unanchor((space_id, l.from), id);
                    anchors.push((l.local, id));
                }
                let site = crate::wire::Site::of(c, l.from, part, whole, &original);
                let signal = self.space(space_id).signals.carry_out(site);
                if !signal.is_empty() {
                    arriving.insert(crate::wire::Site { cell: l.local, ..site }, signal);
                }
                blocks.push((l.local, part));
            }
            if moved.insert(l.from, l.local).is_none() {
                carried.push(Carried { local: l.local, to: l.to, state });
            }
        }
        let body = self.spawn(space_id, c, kind, at);
        let mut space = Space::finite();
        space.signals.arriving = arriving;
        space.signals.now = self.time;
        space.signals.on_the_way = live == Liveness::OnTheWay;
        // Where it starts: it moves before anything looks, and half a block on it could count as
        // either cell.
        let cell = Transform::of(self.space_pose(space_id).to_world(at)).cell(None);
        space.cell = Some(cell);
        space.build(c, blocks);
        self.bodies.insert(body, Box::new(space));
        // What it lit with doesn't go off for a moment. Its light is in the world's, from there on
        // a body too; that gets to it when it looks.
        if space_id == 0 {
            self.light_from_body(c, body, cell);
            self.see_world_light(body);
        }
        // On a body itself, it goes with that.
        let on = (space_id != 0).then_some(space_id);
        let place = Transform::of(at).in_frame(self.space_pose(space_id));
        if let Some(e) = self.entities.get_mut(body) {
            e.frame = on;
            e.set_body_pose(place);
        }
        self.reparent_mounts(space_id, body, abi::Turn::NONE, |at| moved.get(&at).copied());
        for (local, id) in anchors {
            let offset = self
                .entities
                .get(id)
                .map_or(IVec3::ZERO, |e| e.anchor_offset);
            let shift = offset.as_dvec3() / f64::from(crate::world::placement::UNIT);
            self.entities
                .attach(id, body, local, place.to_world(local.as_dvec3() + shift + DVec3::splat(0.5)));
        }
        (body, carried)
    }

    fn reparent_mounts(&mut self, source: u64, destination: u64, turn: abi::Turn, moved: impl Fn(IVec3) -> Option<IVec3>) {
        let mut reparented = Vec::new();
        let (space, entities) = self.space_entities(source);
        for e in &mut entities.list {
            if e.frame.unwrap_or(0) == source
                && e.motion != Motion::Parked
                && let Some(mount) = e.mount.as_mut()
                && let Some(local) = moved(mount.block)
            {
                space.signals.set_outside(e.id, Default::default());
                e.frame = (destination != 0).then_some(destination);
                mount.block = local;
                mount.face = turn.face(mount.face as usize) as u8;
                if let Some(slide) = &mut mount.slide {
                    slide.side = turn.face(slide.side as usize) as u8;
                }
                reparented.push(e.id);
            }
        }
        for child in reparented {
            self.entities.update_mount_pose(child);
            if let Some(space) = self.bodies.get_mut(&child) {
                space.junction_pose = None;
            }
        }
    }

    /// `cells` out of here into a platform, for a mod.
    pub fn lift_for_mod(&mut self, space_id: u64, c: &'static Content, center: IVec3, cells: &[IVec3]) -> u64 {
        let cells: Vec<IVec3> = cells
            .iter()
            .copied()
            .filter(|p| self.get(space_id, *p).is_some_and(|s| s != AIR))
            .collect();
        if cells.is_empty() {
            return 0;
        }
        let lifted = self.with_riders(space_id, c, center, cells);
        self.lift(space_id, c, c.game.platform_kind, center.as_dvec3() + DVec3::new(0.5, 0.0, 0.5), lifted, &[], Liveness::Free)
            .0
    }

    /// Floaters with power in the world, to go together: those stuck to each other are one body, and
    /// can't go if their ways add up to nothing or something is in that way.
    pub(super) fn floater_plans(&self, space_id: u64, c: &Content, floaters: Vec<IVec3>) -> (Vec<FloaterPlan>, Vec<IVec3>) {
        let mut plans: Vec<FloaterPlan> = Vec::new();
        let mut held = Vec::new();
        let mut mounted = Vec::new();
        for p in floaters {
            let Some(controller) = super::assembly::Controller::at(c, self, space_id, p) else { continue };
            let bounds = self.assembly_bounds(space_id, c, p);
            let Ok(cells) = self.assembly_blocks(space_id, c, Some(controller), bounds) else {
                held.push(p);
                continue;
            };
            let lifted = self.assembly_lifted(space_id, c, Some(controller), controller.center(), &cells, bounds);
            if lifted.len() > super::signal::BODY_LIMIT {
                held.push(p);
                continue;
            }
            if !controller.floater {
                let claims = lifted.iter().map(|l| l.from).collect();
                mounted.push(FloaterPlan {
                    floaters: vec![p],
                    cells: claims,
                    broken: Vec::new(),
                    mount: Some((p, controller.face)),
                    lifted,
                });
                continue;
            }
            let mut plan = FloaterPlan { floaters: vec![p], cells: cells.into_iter().collect(), broken: Vec::new(), mount: None, lifted };
            // It and those it shares a block with are one, whichever came first.
            let (joined, others): (Vec<FloaterPlan>, Vec<FloaterPlan>) = plans
                .into_iter()
                .partition(|o| !o.cells.is_disjoint(&plan.cells));
            for o in joined {
                plan.floaters.extend(o.floaters);
                plan.cells.extend(o.cells);
                plan.lifted.extend(o.lifted);
            }
            plans = others;
            plans.push(plan);
        }
        let mut going = Vec::new();
        for mut plan in plans {
            plan.lifted
                .sort_unstable_by_key(|l| (l.from.to_array(), l.part));
            plan.lifted.dedup_by(|duplicate, first| {
                if duplicate.from != first.from || duplicate.only != first.only {
                    return false;
                }
                if let (Some(a), Some(b)) = (first.part, duplicate.part) {
                    if c.states[a as usize].signal.kind == abi::signal_kind::WIRE {
                        first.part = Some(
                            c.block_of(a)
                                .with_sides(a, crate::wire::mask(c, a) | crate::wire::mask(c, b)),
                        );
                    }
                }
                true
            });
            if plan.lifted.len() > super::signal::BODY_LIMIT {
                held.extend(plan.floaters);
                continue;
            }
            let steps = self.floaters_way(space_id, c, &plan.floaters);
            let ahead = (steps.0 != IVec3::ZERO)
                .then(|| {
                    self.ahead_every_way(space_id, c, plan.cells.iter().copied(), &plan.cells, steps.0, steps.1)
                        .ok()
                })
                .flatten();
            match ahead {
                Some(mut broken) => {
                    // What is fixed to them comes along, unless it can't.
                    let list: Vec<IVec3> = plan.cells.iter().copied().collect();
                    let riders: FxHashSet<IVec3> = plan
                        .lifted
                        .iter()
                        .filter(|l| l.part.is_some())
                        .map(|l| l.from)
                        .collect();
                    broken.retain(|b| !riders.contains(b));
                    broken.extend(ways(steps.0).flat_map(|step| {
                        self.who_rides(space_id, c, &list, None, step)
                            .1
                            .into_iter()
                            .map(|v| v.0)
                    }));
                    broken.sort_unstable_by_key(|b| b.to_array());
                    broken.dedup();
                    plan.lifted
                        .retain(|l| l.part.is_none() || !broken.contains(&l.from));
                    plan.cells
                        .extend(riders.into_iter().filter(|p| !broken.contains(p)));
                    plan.broken = broken;
                    going.push(plan);
                }
                None => {
                    log::debug!("floaters at {:?} can't go", plan.floaters);
                    held.extend(plan.floaters);
                }
            }
        }
        going.extend(mounted);
        (going, held)
    }

    /// Which way these floaters in the world add up to, by axis, and whether they break through.
    fn floaters_way(&self, space_id: u64, c: &Content, floaters: &[IVec3]) -> (IVec3, bool) {
        let mut sum = IVec3::ZERO;
        let mut breaks = false;
        for p in floaters {
            let Some(s) = self.get(space_id, *p).map(|s| c.states[s as usize].signal) else { continue };
            let Some(level) = self.space(space_id).signals.level_into(*p) else { continue };
            sum += crate::world::DIRS
                .get(s.front as usize)
                .copied()
                .unwrap_or(IVec3::ZERO)
                * level as i32;
            breaks |= s.flags as u32 & weft_abi::signal_flags::BREAKS != 0;
        }
        (sum.signum(), breaks)
    }

    /// Off with them.
    pub(super) fn run_floater_plan(&mut self, space_id: u64, c: &'static Content, plan: FloaterPlan) {
        if let Some((block, face)) = plan.mount {
            let Some(ctrl) = super::assembly::Controller::at(c, self, space_id, block) else { return };
            let center = ctrl.center();
            if ctrl.face != face || plan.lifted.iter().any(|l| l.from - l.local != center) {
                return;
            }
            let (body, _) =
                self.lift(space_id, c, c.game.platform_kind, center.as_dvec3() + Transform::FEET, plan.lifted, &[], Liveness::Assembly);
            if let Some(e) = self.entities.get_mut(body) {
                e.mount = Some(Box::new(BodyMount {
                    coast_rate: 0.0,
                    block,
                    face,
                    speed: 1,
                    angle: 0.0,
                    last_grid: 0.0,
                    stop: None,
                    drive: None,
                    slide: ctrl.slide,
                }));
            }
            if let Some(space) = self.bodies.get_mut(&body) {
                space.track_mounted_blocks();
            }
            self.entities.update_mount_pose(body);
            self.floater_shows(space_id, c, block, 1);
            return;
        }
        self.break_for_body(space_id, c, plan.broken);
        let center = plan
            .floaters
            .iter()
            .copied()
            .min_by_key(|p| p.to_array())
            .unwrap_or(IVec3::ZERO);
        log::debug!("floaters at {:?} set off with {} blocks", plan.floaters, plan.cells.len());
        let cells = plan
            .lifted
            .into_iter()
            .map(|l| Lifted { local: l.from - center, ..l });
        let (id, _) = self.lift(space_id, c, c.game.platform_kind, center.as_dvec3() + Transform::FEET, cells, &[], Liveness::Assembly);
        if let Some(e) = self.entities.get_mut(id) {
            e.floater_driven = true;
        }
    }

    /// `what_is_ahead` along each axis `sign` has and across the corners between them.
    fn ahead_every_way(
        &self,
        space_id: u64,
        c: &Content,
        cells: impl Iterator<Item = IVec3> + Clone,
        own: &FxHashSet<IVec3>,
        sign: IVec3,
        breaks: bool,
    ) -> Result<Vec<IVec3>, IVec3> {
        let mut out = Vec::new();
        for step in ways(sign) {
            out.extend(self.what_is_ahead(space_id, c, cells.clone(), own, step, breaks)?);
        }
        Ok(out)
    }

    /// What is in the way of `cells`, whichever of them `own` has not one step along `step`:
    /// What they break, or the first blocked or unloaded cell.
    fn what_is_ahead(
        &self,
        space_id: u64,
        c: &Content,
        cells: impl Iterator<Item = IVec3>,
        own: &FxHashSet<IVec3>,
        step: IVec3,
        breaks: bool,
    ) -> Result<Vec<IVec3>, IVec3> {
        let mut out = Vec::new();
        for at in cells {
            let n = at + step;
            if own.contains(&n) {
                continue;
            }
            let s = self.get(space_id, n).ok_or(n)?;
            let info = &c.states[s as usize];
            if s == AIR || info.liquid.is_some() {
                continue;
            }
            if info.solid() && (!breaks || info.hardness < 0.0) {
                return Err(n);
            }
            out.push(n);
        }
        Ok(out)
    }

    pub(super) fn break_for_body(&mut self, space_id: u64, c: &'static Content, cells: Vec<IVec3>) {
        for p in cells {
            if let Some(s) = self.get(space_id, p).filter(|s| *s != AIR) {
                self.drop_block_loot(space_id, c, p, s, abi::ItemStack::EMPTY, false, 0);
                self.set(space_id, c, p, AIR);
            }
        }
    }

    pub(super) fn has_mount(&self, space_id: u64, at: IVec3) -> bool {
        self.mount_at(space_id, at).is_some()
    }

    pub(super) fn mount_at(&self, space_id: u64, at: IVec3) -> Option<u64> {
        self.bodies.keys().copied().find(|&id| {
            self.entities.get(id).is_some_and(|e| {
                e.motion != Motion::Parked && e.frame.unwrap_or(0) == space_id && e.mount.as_ref().is_some_and(|m| m.block == at)
            })
        })
    }

    pub(super) fn mount_children(&self) -> FxHashMap<u64, Vec<u64>> {
        let mut children: FxHashMap<u64, Vec<u64>> = FxHashMap::default();
        for e in &self.entities.list {
            if !e.removed && e.body_motion() == BodyMotion::Joint {
                children.entry(e.frame.unwrap_or(0)).or_default().push(e.id);
            }
        }
        children
    }

    pub(super) fn mount_order(&self, children: &FxHashMap<u64, Vec<u64>>) -> Vec<u64> {
        let mut order = children.get(&0).cloned().unwrap_or_default();
        if children.is_empty() {
            return order;
        }
        for e in &self.entities.list {
            if !e.removed && e.mount.is_none() && self.bodies.contains_key(&e.id) {
                order.extend(children.get(&e.id).into_iter().flatten().copied());
            }
        }
        let mut i = 0;
        while let Some(&id) = order.get(i) {
            order.extend(children.get(&id).into_iter().flatten().copied());
            i += 1;
        }
        order
    }

    pub(super) fn mounted_parts<'a>(&'a self, id: u64, pose: Transform, children: &FxHashMap<u64, Vec<u64>>) -> Vec<MountedPart<'a>> {
        let mut parts = Vec::new();
        let root = id;
        let mut pending = vec![id];
        while let Some(id) = pending.pop() {
            let (Some(space), Some(e)) = (self.bodies.get(&id), self.entities.get(id)) else { continue };
            let relative = if id == root { Transform::default() } else { e.body_place().relative_to(pose) };
            parts.push(MountedPart { id, space, relative });
            pending.extend(children.get(&id).into_iter().flatten().rev().copied());
        }
        parts
    }

    pub(super) fn advance_mounts(&mut self, c: &Content, children: &FxHashMap<u64, Vec<u64>>, order: &[u64]) -> Vec<u64> {
        for &id in order {
            self.entities.update_mount_pose(id);
        }
        let mut landing = Vec::new();
        for &id in order {
            let Some(e) = self.entities.get(id) else { continue };
            let Some(mut mount) = e.mount.as_deref().copied() else { continue };
            let parent_id = e.frame.unwrap_or(0);
            let parent = if parent_id == 0 { None } else { self.bodies.get(&parent_id) };
            let pose = self
                .entities
                .get(parent_id)
                .map_or_else(Transform::default, |e| e.body_place());
            let old_place = mount.place_at(mount.angle);
            if self.turn_body_mount(c, id, parent.map_or(&self.space, |s| s), parent_id != 0, pose, &mut mount, children) {
                if children.contains_key(&id) {
                    self.stop_mounted_children(id, children);
                } else {
                    landing.push(id);
                }
            }
            if let Some(saved) = self
                .entities
                .get_mut(id)
                .and_then(|e| e.mount.as_deref_mut())
            {
                *saved = mount;
            }
            if old_place != mount.place_at(mount.angle) {
                self.entities.update_mount_pose(id);
                let mut pending = children.get(&id).cloned().unwrap_or_default();
                while let Some(child) = pending.pop() {
                    self.entities.update_mount_pose(child);
                    pending.extend(children.get(&child).into_iter().flatten().copied());
                }
            }
        }
        landing
    }

    pub(super) fn stop_mounted_children(&mut self, id: u64, children: &FxHashMap<u64, Vec<u64>>) {
        let mut pending = children.get(&id).cloned().unwrap_or_default();
        while let Some(child) = pending.pop() {
            self.ask_put_down(child);
            pending.extend(children.get(&child).into_iter().flatten().copied());
        }
    }

    /// Advance before entity physics so riders see both poses of this tick.
    fn turn_body_mount(
        &self,
        c: &Content,
        id: u64,
        parent: &Space,
        nested: bool,
        pose: Transform,
        mount: &mut BodyMount,
        children: &FxHashMap<u64, Vec<u64>>,
    ) -> bool {
        let Some(body) = self.bodies.get(&id) else { return false };
        if body.bounds.is_none() {
            return true;
        }
        let Some(state) = parent.get(mount.block).or(nested.then_some(AIR)) else { return false };
        let info = c.states[state as usize].signal;
        let valid = info.kind == abi::signal_kind::BODY_MOUNT && info.front == mount.face;
        if mount.slide.is_some() {
            let valid = valid && info.flags as u32 & abi::signal_flags::SLIDES != 0;
            return self.step_slide(c, id, parent, pose, mount, children, valid);
        }
        let power = if valid {
            let Some(power) = parent.signals.level_into(mount.block) else { return false };
            power
        } else {
            0
        };
        let rotates = valid && info.flags as u32 & abi::signal_flags::ROTATES != 0;
        let frame = self.entities.get(id).and_then(|e| e.frame).unwrap_or(0);
        let program = rotates
            .then(|| self.rotor_program(c, frame, mount.block))
            .flatten();
        let programmable = program.is_some();
        let docking = self.docking(id);
        let command = program.and_then(|p| p.rule(power)).filter(|_| !docking);
        let drive = command.map(|rule| {
            mount
                .drive
                .filter(|d| d.rule == rule)
                .unwrap_or_else(|| crate::controller::Drive::new(rule, mount.angle))
        });
        let releasing_drive = mount.drive.is_some() && drive.is_none();
        mount.drive = drive;
        let powered = if programmable { command.is_some() } else { power != 0 };
        let hold = rotates && info.flags as u32 & abi::signal_flags::HOLD != 0 && !docking;
        if hold || drive.is_some() {
            mount.stop = None;
            if !powered {
                return false;
            }
        }
        let stopping = docking || !powered;
        if !rotates && !stopping && mount.stop.is_none() {
            return false;
        }
        let next_grid = mount.last_grid + FRAC_PI_2;
        if stopping && mount.stop.is_none() {
            mount.stop = Some(if releasing_drive || programmable {
                let turns = mount.angle / FRAC_PI_2;
                (if mount.angle < mount.last_grid { turns.floor() } else { turns.ceil() }) * FRAC_PI_2
            } else if mount.angle == mount.last_grid {
                mount.last_grid
            } else {
                next_grid
            });
        }
        if powered && mount.stop.is_none() {
            mount.speed = power;
        }
        let goal = match drive {
            Some(d) => match d.rule.motion {
                crate::controller::Motion::Absolute | crate::controller::Motion::Relative => d.target,
                crate::controller::Motion::Clockwise => mount.angle + FRAC_PI_2,
                crate::controller::Motion::Counterclockwise => mount.angle - FRAC_PI_2,
                crate::controller::Motion::Freewheel => return false,
            },
            None => mount.stop.unwrap_or(next_grid),
        };
        if drive.is_some() && mount.angle == goal {
            return false;
        }
        let initial = *mount;
        let Some(current) = self.entities.get(id).map(|e| e.body_place()) else { return false };
        let parts = self.mounted_parts(id, current, children);
        let fits = |turn: f64| Self::mount_fits_in(c, parent, initial.place_at(turn), body);
        let axis = crate::world::DIRS[mount.face as usize].as_dvec3().abs();
        let coaxial = drive.is_some() && parts.len() == 1 && body.coaxial_tire(c, axis);
        let radius = parts
            .iter()
            .filter_map(|part| {
                let bounds = part.relative.box_to_world(part.space.colliders(c).bounds?);
                let center = DVec3::splat(0.5);
                let far = (bounds.min - center).abs().max((bounds.max - center).abs());
                Some((far * (DVec3::ONE - axis)).length())
            })
            .fold(1.0, f64::max);
        let speed = drive.map_or(TURN_PER_LEVEL * mount.speed.max(1) as f64, |d| f64::from(d.rule.speed).to_radians() * crate::world::TICK);
        let step = if coaxial { speed } else { speed.min(2.0 * TURN_PROBE_TRAVEL / radius) };
        if step == 0.0 {
            return false;
        }
        let diff = goal - mount.angle;
        let mut next = if diff.abs() <= step { goal } else { mount.angle + step.copysign(diff) };
        let clear = |angle| coaxial || self.mount_clear(c, initial.place_at(angle).in_frame(pose), &parts);
        if mount.angle == goal && mount.stop.is_some() {
            if clear(goal) && fits(goal) {
                return true;
            }
            mount.stop = Some(mount.last_grid);
            return false;
        }
        if !coaxial && next != mount.angle {
            let start = initial.place_at(mount.angle).in_frame(pose);
            let world = super::space::CubeCells(&self.space.cubes);
            let others = self.mount_obstacles(c, start, &parts, DVec3::ZERO, radius * (next - mount.angle).abs());
            let closes = |angle| {
                let sample = initial.place_at(angle).in_frame(pose);
                visit_structure_shapes(c, &world, &parts, &others, sample, DVec3::ZERO, |_, shape, place, _, peer| {
                    let mut closing = false;
                    let mut contact = |point, normal| {
                        let travel: DVec3 = point - start.to_world(sample.to_local(point));
                        closing |= travel.dot(normal) < -1e-8;
                    };
                    if let Some(peer) = peer {
                        crate::physics::rotated_contacts(c, peer.space, shape, place, DVec3::ZERO, |hit| {
                            contact(peer.place.to_world(hit.point), peer.place.vector_to_world(hit.normal));
                        });
                    } else {
                        crate::physics::rotated_contacts(c, &world, shape, place, DVec3::ZERO, |hit| contact(hit.point, hit.normal));
                    }
                    closing
                })
            };
            let mut free = mount.angle;
            for sample in [(mount.angle + next) * 0.5, next] {
                if !closes(sample) {
                    free = sample;
                    continue;
                }
                let mut blocked = sample;
                for _ in 0..7 {
                    let mid = (free + blocked) * 0.5;
                    if closes(mid) {
                        blocked = mid;
                    } else {
                        free = mid;
                    }
                }
                next = free;
                break;
            }
        }
        if next == mount.angle || drive.is_none() && next == goal && !fits(goal) {
            if hold || drive.is_some() {
                return false;
            }
            mount.stop = Some(mount.last_grid);
            return mount.angle == mount.last_grid && clear(mount.last_grid) && fits(mount.last_grid);
        }
        if let Some(mut d) = drive {
            let grid = if next >= mount.angle { (next / FRAC_PI_2).floor() } else { (next / FRAC_PI_2).ceil() } * FRAC_PI_2;
            if grid >= next.min(mount.angle) && grid <= next.max(mount.angle) && fits(grid) {
                mount.last_grid = grid;
            }
            let shift = (next / TAU).trunc() * TAU;
            d.target -= shift;
            if matches!(d.rule.motion, crate::controller::Motion::Clockwise | crate::controller::Motion::Counterclockwise) {
                d.target = next - shift;
            }
            mount.drive = Some(d);
            mount.angle = next - shift;
            mount.last_grid -= shift;
            return false;
        }
        mount.angle = next;
        if next != goal {
            return false;
        }
        if mount.stop.is_some() {
            return true;
        }
        mount.last_grid = next % TAU;
        mount.angle = mount.last_grid;
        false
    }

    fn step_slide(
        &self,
        c: &Content,
        id: u64,
        parent: &Space,
        pose: Transform,
        mount: &mut BodyMount,
        children: &FxHashMap<u64, Vec<u64>>,
        valid: bool,
    ) -> bool {
        let Some(mut slide) = mount.slide else { return false };
        let Some(e) = self.entities.get(id) else { return false };
        let Some(space) = self.bodies.get(&id) else { return false };
        let axis = crate::world::DIRS[mount.face as usize];
        let rail_end = if valid { guide_length(c, parent, mount.block, axis) } else { 0 };
        let config = self.guide_settings(c, e.frame.unwrap_or(0), mount.block);
        let stopping = e.completion == Completion::Dock || rail_end == 0;
        let parts = self.mounted_parts(id, e.body_place(), children);
        let max = f64::from(config.max).min(f64::from(rail_end.max(1)));
        let min = f64::from(config.min).min(max);
        let place = mount.slide_place(slide.length).in_frame(pose);
        let direction = pose.vector_to_world(axis.as_dvec3());
        let current = slide.length;
        let step = if stopping {
            let goal = if rail_end > 0 { f64::from(rail_end) } else { slide.length.round() };
            if current == goal {
                let local = mount.slide_place(goal);
                return Self::mount_fits_in(c, parent, local, space) && self.mount_clear(c, place, &parts);
            }
            (goal - current).clamp(-0.1, 0.1)
        } else {
            let dt = crate::world::TICK;
            let mass: f64 = parts
                .iter()
                .map(|part| super::buoyancy::structure_mass(c, part.space))
                .sum();
            let parent_velocity = e
                .frame
                .and_then(|id| self.entities.get(id))
                .map_or(DVec3::ZERO, |p| self.entities.follows(p).vel);
            let acceleration = slide
                .parent_velocity
                .map_or(DVec3::ZERO, |old| (parent_velocity - old) / dt);
            slide.parent_velocity = Some(parent_velocity);
            let projected = (DVec3::NEG_Y * f64::from(c.dims[self.id as usize].gravity) - acceleration).dot(direction);
            let rest = f64::from(config.rest).clamp(min, max);
            let spring = f64::from(config.stiffness) / mass.max(0.01);
            let damping = f64::from(config.damping) / mass.max(0.01);
            // Integrate the spring implicitly so stiff settings remain stable at the tick rate.
            slide.velocity = (slide.velocity + dt * (projected + spring * (rest - current))) / (1.0 + dt * damping + dt * dt * spring);
            let limit = (current + slide.velocity * dt).clamp(min, max);
            if limit != current + slide.velocity * dt {
                slide.velocity = 0.0;
            }
            (limit - current).clamp(-TURN_PROBE_TRAVEL * 2.0, TURN_PROBE_TRAVEL * 2.0)
        };
        if step.abs() > 1e-8 {
            let delta = direction * step;
            let others = self.mount_obstacles(c, place, &parts, delta, 0.0);
            let world = CubeCells(&self.space.cubes);
            let mut fraction = 1.0_f64;
            visit_structure_shapes(c, &world, &parts, &others, place, delta, |_, shape, place, delta, peer| {
                let mut contact = |hit: crate::physics::BoxContact| {
                    if delta.dot(hit.normal) < -1e-9 {
                        fraction = fraction.min(hit.fraction);
                    }
                };
                if let Some(peer) = peer {
                    crate::physics::rotated_contacts(c, peer.space, shape, place, delta, &mut contact);
                } else {
                    crate::physics::rotated_contacts(c, &world, shape, place, delta, &mut contact);
                }
                false
            });
            slide.length += step * fraction;
            if fraction < 1.0 {
                slide.velocity = 0.0;
            }
        }
        mount.slide = Some(slide);
        false
    }

    pub(super) fn mount_obstacles<'a>(
        &'a self,
        c: &Content,
        place: Transform,
        parts: &[MountedPart<'_>],
        delta: DVec3,
        margin: f64,
    ) -> Vec<StructurePeer<'a>> {
        let moving: FxHashSet<_> = parts.iter().map(|p| p.id).collect();
        let Some(envelope) = parts
            .iter()
            .filter_map(|p| {
                Some(
                    p.relative
                        .in_frame(place)
                        .box_to_world(p.space.colliders(c).bounds?),
                )
            })
            .reduce(|a, b| Aabb { min: a.min.min(b.min), max: a.max.max(b.max) })
        else {
            return Vec::new();
        };
        let padding = DVec3::splat(margin);
        let envelope = Aabb { min: envelope.min - padding, max: envelope.max + padding }.swept(delta);
        self.bodies
            .iter()
            .filter_map(|(&other, space)| {
                let e = self.entities.get(other)?;
                if moving.contains(&other) || e.motion == Motion::Parked || e.removed {
                    return None;
                }
                let other = e.body_place();
                let area = other.box_to_world(space.colliders(c).bounds?);
                envelope
                    .intersects(&area)
                    .then_some(StructurePeer { id: e.id, place: other, bounds: area, space, delta: DVec3::ZERO })
            })
            .collect()
    }

    fn mount_clear(&self, c: &Content, place: Transform, parts: &[MountedPart<'_>]) -> bool {
        let other_bodies = self.mount_obstacles(c, place, parts, DVec3::ZERO, 0.0);
        let world = super::space::CubeCells(&self.space.cubes);
        structure_pose_clear(c, &world, parts, &other_bodies, place)
    }

    pub(super) fn mount_fits(&self, space_id: u64, c: &Content, place: Transform, body: &Space) -> bool {
        Self::mount_fits_in(c, self.space(space_id), place, body)
    }

    fn mount_fits_in(c: &Content, space: &Space, place: Transform, body: &Space) -> bool {
        let turn = place.turn();
        body.block_snapshot().iter().all(|&(p, state)| {
            body.layout(p).residents(state).all(|part| {
                let (owner, part) = part.turned(c, turn).owner(place.cell_at(p));
                std::iter::once(IVec3::ZERO)
                    .chain(part.footprint(c).iter().copied())
                    .all(|delta| {
                        let cell = owner + delta;
                        !space.signals.reserved(cell)
                            && space.get(cell).is_some_and(|main| {
                                space.layout(cell).parts(main).all(|other| {
                                    c.states[other.state as usize].flags & abi::block_flags::REPLACEABLE != 0
                                        || c.may_share(part.state, other.state)
                                        || !part.in_cell(delta).overlaps(other, c)
                                })
                            })
                    })
            })
        })
    }

    pub(super) fn land_mount(&mut self, c: &'static Content, id: u64) {
        self.finish_moving_here(id, c);
        let Some(e) = self.entities.get(id) else { return };
        let Some(m) = e.mount.as_ref() else { return };
        let Some(space) = self.bodies.get(&id) else { return };
        let parent = e.frame.unwrap_or(0);
        let local = m.place_at(m.angle);
        let parent_space = self.bodies.get(&parent).map_or(&self.space, |s| s);
        let parts = [MountedPart { id, space, relative: Transform::default() }];
        if !Self::mount_fits_in(c, parent_space, local, space) || !self.mount_clear(c, e.body_place(), &parts) {
            if let Some(m) = self
                .entities
                .get_mut(id)
                .and_then(|e| e.mount.as_deref_mut())
            {
                m.stop = Some(m.last_grid);
            }
            return;
        }
        let at = m.block;
        self.put_down_whole(c, id);
        if self.active_space(parent) {
            self.turn_assembly_bounds(parent, c, at, local);
            let power = self.space(parent).signals.level_into(at).unwrap_or(0);
            if self
                .get(parent, at)
                .is_some_and(|s| c.states[s as usize].signal.kind == abi::signal_kind::BODY_MOUNT)
            {
                self.floater_shows(parent, c, at, if power == 0 { 0 } else { 2 });
                self.space_mut(parent).signals.stop_mount(at);
            }
        }
    }

    /// Platforms: their floaters say how they go. Coming to a stop, they go on to a whole block.
    pub(super) fn drive_bodies(&mut self, c: &'static Content) {
        let children = self.mount_children();
        let ids: Vec<u64> = self
            .bodies
            .keys()
            .copied()
            .filter(|&id| {
                self.entities
                    .get(id)
                    .is_some_and(|e| e.body_motion() == BodyMotion::Kinematic)
            })
            .collect();
        for id in ids {
            self.drive(c, id, &children);
        }
    }

    fn drive(&mut self, c: &'static Content, id: u64, children: &FxHashMap<u64, Vec<u64>>) {
        let Some(b) = self.bodies.get(&id) else { return };
        let Some(e) = self.entities.get(id) else { return };
        if e.completion == Completion::Dock {
            if !matches!(e.motion, Motion::To { .. }) {
                if e.oriented_place().is_some() && e.frame.is_none() && !children.contains_key(&id) {
                    self.try_dock(c, id);
                } else {
                    self.put_down_whole(c, id);
                }
            }
            return;
        }
        if matches!(e.motion, Motion::To { .. }) {
            return;
        }
        let Some(cell) = b.cell else { return };
        let Some(drive) = b.signals.floater_drive(c) else { return };
        let (v, breaks) = (drive.velocity, drive.breaks);
        let pos = e.pos;
        let vel = e.vel;
        if v == DVec3::ZERO {
            if e.floater_driven || drive.axes.any() && vel != DVec3::ZERO {
                self.stop_floater(id, pos, vel, None, drive.hold);
            }
            return;
        }
        if e.frame.is_none() && e.body_place().rotation() != glam::DQuat::IDENTITY {
            self.enable_dynamics(id, None);
            return;
        }
        if children.contains_key(&id) {
            let place = e.body_place();
            let parts = self.mounted_parts(id, place, children);
            let world = CubeCells(&self.space.cubes);
            let blocked =
                visit_structure_shapes(c, &world, &parts[1..], &[], place, v * crate::world::TICK, |_, shape, place, delta, _| {
                    let mut hit = false;
                    crate::physics::rotated_contacts(c, &world, shape, place, delta, |contact| {
                        hit |= contact.depth > 1e-6 || delta.dot(contact.normal) < -1e-9;
                    });
                    hit
                });
            if blocked {
                self.stop_floater(id, pos, vel, (!drive.hold).then_some(cell), drive.hold);
                return;
            }
        }
        // On coming to a new cell, a look at the next one.
        let sign = IVec3::from_array([0, 1, 2].map(|a| if v[a].abs() > 1e-9 { v[a].signum() as i32 } else { 0 }));
        // What it has that can't be moved now, with more of it pushing that along: it stops as it
        // would have stayed. What nothing pushes rides on, a body doesn't come apart on its way.
        let solid = |p: IVec3| b.get(p).is_some_and(|s| c.solid(s));
        if b.signals
            .fixed(c)
            .any(|p| ways(sign).any(|step| solid(p - step)))
        {
            log::debug!("a platform at {cell} going {sign} is held by what it carries");
            self.stop_floater(id, pos, vel, None, drive.hold);
            return;
        }
        let scan = FloaterPath { cell, direction: sign, geometry: b.geometry_version, breaks, blocked: None };
        let previous = b
            .floater_path
            .filter(|p| p.cell == cell && p.direction == sign && p.geometry == scan.geometry && p.breaks == breaks);
        if let Some(blocked) = previous.and_then(|p| p.blocked) {
            if self.get(0, blocked).is_none_or(|s| {
                let info = &c.states[s as usize];
                info.solid() && info.liquid.is_none() && (!breaks || info.hardness < 0.0)
            }) {
                if !drive.hold || vel != DVec3::ZERO {
                    self.stop_floater(id, pos, vel, Some(cell), drive.hold);
                }
                return;
            }
        }
        if previous.is_none_or(|p| p.blocked.is_some()) {
            let all = b.all_blocks();
            let own: FxHashSet<IVec3> = all.iter().map(|(p, _)| *p + cell).collect();
            let at: Vec<IVec3> = all
                .iter()
                .filter(|(_, s)| c.solid(*s))
                .map(|(p, _)| *p + cell)
                .collect();
            let mut broken = match self.ahead_every_way(0, c, at.iter().copied(), &own, sign, breaks) {
                Ok(broken) => broken,
                Err(blocked) => {
                    if let Some(b) = self.bodies.get_mut(&id) {
                        b.floater_path = Some(FloaterPath { blocked: Some(blocked), ..scan });
                    }
                    self.stop_floater(id, pos, vel, Some(cell), drive.hold);
                    return;
                }
            };
            // What it carries that isn't solid doesn't hold it up.
            let mut off = Vec::new();
            for (p, _) in all.iter().filter(|(_, s)| !c.solid(*s)) {
                for to in ways(sign)
                    .map(|step| *p + cell + step)
                    .filter(|to| !own.contains(to))
                {
                    match self
                        .get(0, to)
                        .map(|there| super::signal::rider_meets(c, there))
                    {
                        Some(RiderMeets::Wall) => off.push(*p),
                        Some(RiderMeets::Breaks) => broken.push(to),
                        _ => {}
                    }
                }
            }
            off.dedup();
            if !off.is_empty() {
                self.with_space(c, id, |d, space_id, _| {
                    for p in off {
                        if let Some(s) = d.get(space_id, p).filter(|s| *s != AIR) {
                            log::debug!("{} riding on a platform at {p} came off", c.state_name(s));
                            d.drop_block_loot(space_id, c, p, s, abi::ItemStack::EMPTY, false, 0);
                            d.set(space_id, c, p, AIR);
                        }
                    }
                });
            }
            self.break_for_body(0, c, broken);
            if let Some(b) = self.bodies.get_mut(&id) {
                b.floater_path = Some(scan);
            }
        }
        if let Some(e) = self.entities.get_mut(id) {
            e.vel = v;
            e.floater_driven = true;
        }
    }

    fn stop_floater(&mut self, id: u64, pos: DVec3, vel: DVec3, at: Option<IVec3>, hold: bool) {
        if hold {
            if at.is_some() {
                self.bring_to_stop(id, pos, vel, at);
            } else if let Some(e) = self.entities.get_mut(id) {
                e.vel = DVec3::ZERO;
            }
            if let Some(entity) = self.entities.get_mut(id) {
                entity.completion = Completion::Stay;
            }
            if at.is_none()
                && let Some(body) = self.bodies.get_mut(&id)
            {
                body.floater_path = None;
            }
        } else {
            self.bring_to_stop(id, pos, vel, at);
        }
    }

    /// Goes on to the next whole block along the way it was going, or to `at` if it is stopped.
    fn bring_to_stop(&mut self, id: u64, pos: DVec3, vel: DVec3, at: Option<IVec3>) {
        let o = Transform::of(pos).origin();
        let cell = at.unwrap_or_else(|| {
            IVec3::from_array([0, 1, 2].map(|a| {
                (if vel[a] > 1e-9 {
                    (o[a] - 1e-6).ceil()
                } else if vel[a] < -1e-9 {
                    (o[a] + 1e-6).floor()
                } else {
                    o[a].round()
                }) as i32
            }))
        });
        let to = cell.as_dvec3() + DVec3::new(0.5, 0.0, 0.5);
        let speed = vel.length().max(2.0);
        let ticks = ((to - pos).length() / speed * crate::world::TPS as f64)
            .ceil()
            .max(1.0) as u16;
        if let Some(e) = self.entities.get_mut(id) {
            e.vel = DVec3::ZERO;
            e.motion = Motion::To { to, left: ticks };
            e.completion = Completion::Dock;
        }
    }

    /// A mod wants a body back as blocks, which happens when its tick comes: it may be the one
    /// whose block is asking.
    pub fn ask_put_down(&mut self, id: u64) {
        if self.bodies.contains_key(&id)
            && let Some(entity) = self
                .entities
                .get_mut(id)
                .filter(|entity| entity.motion != Motion::Parked)
        {
            entity.completion = Completion::Dock;
        }
    }

    pub(super) fn docking(&self, id: u64) -> bool {
        self.entities
            .get(id)
            .is_some_and(|entity| entity.completion == Completion::Dock)
    }

    /// All a body has goes back to where it is, as blocks of what it is on: at the whole block it
    /// is at, which it may have been on its way to. Anything else moving on it lands first.
    pub(super) fn put_down_whole(&mut self, c: &'static Content, id: u64) {
        let children = self.mount_children();
        if children.contains_key(&id) {
            self.stop_mounted_children(id, &children);
            return;
        }
        self.finish_moving_here(id, c);
        let Some(e) = self.entities.get(id).filter(|e| e.motion != Motion::Parked) else { return };
        let space_id = e.frame.unwrap_or(0);
        let parent = self.space_pose(space_id);
        let pos = parent.to_local(e.pos);
        let oriented = e
            .mount
            .as_ref()
            .map(|m| m.place_at(m.angle))
            .or_else(|| e.oriented_place().map(|pose| pose.relative_to(parent)));
        let Some(b) = self.bodies.get(&id) else { return };
        let cell = Transform::of(pos).cell(if space_id == 0 { b.cell } else { None });
        let blocks = b
            .block_snapshot()
            .iter()
            .map(|&(local, state)| {
                let to = oriented.map_or(local + cell, |place| place.cell_at(local));
                Carried { local, to, state }
            })
            .collect();
        self.put_down(space_id, c, id, blocks, Vec::new(), None);
    }

    /// Puts what a body carried where it is going: there for circuits now, and for clients when
    /// they see the body there. It stays until then and a little more, not moving. What found
    /// its place taken drops as it would broken. `also`: other cells that changed with it, and
    /// what they were.
    pub fn put_down(
        &mut self,
        space_id: u64,
        c: &'static Content,
        body: u64,
        blocks: Vec<Carried>,
        also: Vec<(IVec3, u32)>,
        turns: Option<(IVec3, u32)>,
    ) {
        if !self.active_space(body) || !self.active_space(space_id) {
            return;
        }
        let turn = self
            .entities
            .get(body)
            .and_then(|e| {
                e.mount.as_ref().map(|m| m.place_at(m.angle)).or_else(|| {
                    e.oriented_place()
                        .map(|pose| pose.relative_to(self.space_pose(space_id)))
                })
            })
            .map_or(abi::Turn::NONE, Transform::turn);
        let landing_offset = self
            .entities
            .get(body)
            .filter(|e| e.completion == super::entity::Completion::LandOnSupport)
            .map_or(IVec3::ZERO, |e| {
                let pose = Transform::of(self.space_pose(space_id).to_local(e.pos));
                ((pose.origin() - pose.cell(None).as_dvec3()) * f64::from(crate::world::placement::UNIT))
                    .round()
                    .as_ivec3()
            });
        if let Some(e) = self.entities.get_mut(body) {
            e.motion = Motion::Parked;
            e.vel = DVec3::ZERO;
        }
        let mut cells: FxHashMap<_, _> = also
            .into_iter()
            .map(|(p, state)| (p, (state, self.space(space_id).layout(p).clone())))
            .collect();
        let mut transferred = FxHashMap::default();
        let mut landed = FxHashMap::default();
        for b in blocks {
            let source = self.space(body);
            let Some(main) = source.get(b.local).filter(|&s| s != AIR) else { continue };
            let residents: Vec<_> = source.layout(b.local).residents(main).collect();
            for (i, part) in residents.into_iter().enumerate() {
                let entity = c
                    .game
                    .block_entities
                    .get(&c.states[part.state as usize].block)
                    .and_then(|&kind| {
                        self.entities
                            .anchored_kind(body, b.local, kind, part.offset)
                    });
                let signal = self
                    .space(body)
                    .signals
                    .carry_out(crate::wire::Site::of(c, b.local, part, main, self.space(body).layout(b.local)))
                    .turned(turn);
                let original = part;
                let mut part = part;
                if i == 0 && c.states[b.state as usize].signal.kind == abi::signal_kind::PISTON_HEAD {
                    part.state = b.state;
                }
                let mut part = part.turned(c, turn);
                part.offset += landing_offset;
                let (to, part) = part.owner(b.to);
                if self
                    .entities
                    .get(body)
                    .is_some_and(|e| e.completion == super::entity::Completion::LandOnSupport)
                    && !self.held_at(space_id, c, to, part)
                {
                    self.broke_landing(space_id, c, to, part.state, entity);
                    continue;
                }
                for delta in std::iter::once(IVec3::ZERO).chain(part.footprint(c).iter().copied()) {
                    let p = to + delta;
                    if let Some(state) = self.get(space_id, p) {
                        cells
                            .entry(p)
                            .or_insert_with(|| (state, self.space(space_id).layout(p).clone()));
                    }
                }
                if self.land_block(space_id, c, to, part, entity, signal) {
                    if let Some(entity) = entity {
                        self.turn_assembly_offsets(c, entity, turn);
                    }
                    transferred.insert((b.local, original), (to, part));
                    if i == 0 {
                        landed.insert(b.local, (to, part.state));
                    }
                }
            }
        }
        cells.retain(|&p, (s, layout)| self.get(space_id, p) != Some(*s) || self.space(space_id).layout(p) != layout);
        self.reparent_mounts(body, space_id, turn, |pos| landed.get(&pos).map(|&(at, _)| at));
        {
            let source = self.space_mut(body);
            source.signals = Default::default();
            source.transfer = Some(Box::new(Transfer { destination: space_id, turn, parts: transferred, shown_at: None }));
        }
        // Its circuits and light are the world's again: at once here, in a moment from on a body.
        self.space_mut(space_id)
            .signals
            .set_outside(body, Default::default());
        if space_id == 0 {
            self.set_body_lights(c, body, Vec::new());
        }
        let destination = self.space_mut(space_id);
        destination
            .changes
            .retain(|(pos, _)| !cells.contains_key(pos));
        destination.not_shown.extend(
            cells
                .into_iter()
                .map(|(pos, (state, parts))| (pos, (state, parts, body))),
        );
        destination
            .landings
            .push(Landing { body, ticks: REST_TICKS + LINGER_TICKS, turns });
    }

    /// Goes where it was going, if that is free: otherwise it breaks there, what it holds with it.
    fn land_block(&mut self, space_id: u64, c: &'static Content, at: IVec3, part: Part, entity: Option<u64>, signal: Moved) -> bool {
        let state = part.state;
        if let Some(main) = self.get(space_id, at) {
            let info = &c.states[state as usize];
            let mut parts: Vec<_> = self.space(space_id).layout(at).residents(main).collect();
            if let Some(other) = parts
                .iter_mut()
                .find(|v| v.offset == part.offset && info.sides != 0 && c.states[v.state as usize].block == info.block)
            {
                other.state = c
                    .block_of(state)
                    .with_sides(state, info.sides | c.states[other.state as usize].sides);
            } else {
                parts.retain(|v| c.states[v.state as usize].flags & abi::block_flags::REPLACEABLE == 0 || !v.overlaps(part, c));
                if parts
                    .iter()
                    .any(|&v| v == part || (!c.may_share(state, v.state) && v.overlaps(part, c)))
                {
                    self.broke_landing(space_id, c, at, state, entity);
                    return false;
                }
                parts.push(part);
            }
            let mut layout = self.space(space_id).layout(at).clone();
            let main = layout.set_residents(&parts);
            let site = crate::wire::Site::of(c, at, part, main, &layout);
            if info.signal.kind == abi::signal_kind::OBSERVER {
                self.space_mut(space_id)
                    .signals
                    .moved_observers
                    .insert(site);
            }
            if !signal.is_empty() {
                self.space_mut(space_id)
                    .signals
                    .arriving
                    .insert(site, signal);
            }
            if self
                .write_residents(space_id, c, at, &mut parts, true)
                .is_some()
            {
                if let Some(id) = entity {
                    if let Some(e) = self.entities.get_mut(id) {
                        e.anchor_offset = part.offset;
                    }
                    let pos = self
                        .space_pose(space_id)
                        .to_world(at.as_dvec3() + part.shift() + DVec3::splat(0.5));
                    self.entities.attach(id, space_id, at, pos);
                }
                return true;
            }
            self.space_mut(space_id).signals.arriving.remove(&site);
        }
        self.broke_landing(space_id, c, at, state, entity);
        false
    }

    /// A block that couldn't land drops what it does, and what it held.
    pub(super) fn broke_landing(&mut self, space_id: u64, c: &'static Content, at: IVec3, state: u32, entity: Option<u64>) {
        if let Some(id) = entity {
            self.drop_inventory(space_id, c, id);
            self.entities.remove(id, super::dim::UNTOLD);
        }
        self.drop_block_loot(space_id, c, at, state, abi::ItemStack::EMPTY, false, 0);
    }

    /// Landed bodies, a tick on: they get shown, and go.
    pub(super) fn tick_landings(&mut self, space_id: u64, c: &'static Content) {
        let mut landings = std::mem::take(&mut self.space_mut(space_id).landings);
        landings.retain_mut(|landing| {
            landing.ticks -= 1;
            match landing.ticks {
                0 => {
                    self.entities.remove(landing.body, super::dim::UNTOLD);
                    self.bodies.remove(&landing.body);
                }
                LINGER_TICKS => self.show_landed(space_id, c, landing.body),
                _ => {
                    if let Some((at, state)) = landing
                        .turns
                        .take()
                        .filter(|_| self.has_space(landing.body))
                    {
                        self.carry_block(landing.body, c, at, state);
                    }
                }
            }
            landing.ticks != 0
        });
        self.space_mut(space_id).landings = landings;
    }

    /// Bodies' places against the world's cells: one moved past the middle of a cell, by more than
    /// half a block from where it counted as being (exactly half is no more, or which way it goes
    /// would show at speeds that land there), gets to the next. Then their circuits and the
    /// world's hear what is next to what. Only the world's blocks, not other bodies'.
    pub(super) fn cross_cells(&mut self, c: &'static Content) {
        let ids: Vec<u64> = self.bodies.keys().copied().collect();
        for id in ids {
            let Some(e) = self.entities.get(id).filter(|e| e.motion != Motion::Parked) else { continue };
            let place = Transform::of(e.pos);
            let pose = e.oriented_place();
            let junction = e
                .mount
                .as_ref()
                .filter(|m| m.slide.is_some() || e.frame.is_some())
                .map(|m| m.wire_place());
            let Some(b) = self.bodies.get_mut(&id) else { continue };
            let was = b.cell;
            let now = place.cell(was);
            let old = std::mem::replace(&mut b.projected_pose, pose);
            let in_touch = std::mem::replace(&mut b.in_touch, true);
            let junction_changed = std::mem::replace(&mut b.junction_pose, junction) != junction;
            if in_touch && was == Some(now) && old == pose && !junction_changed {
                continue;
            }
            b.cell = Some(now);
            let project = |at: IVec3| pose.map_or(at + now, |place| place.cell_at(at));
            let previous = |at: IVec3| was.map(|w| old.map_or(at + w, |place| place.cell_at(at)));
            let moved_cells = was != Some(now)
                || b.block_snapshot()
                    .iter()
                    .any(|(p, _)| previous(*p) != Some(project(*p)));
            if moved_cells {
                self.crossed(c, id, previous, project);
            }
            if !in_touch || moved_cells {
                self.light_from_body(c, id, now);
                self.see_world_light(id);
            }
            if junction.is_none() || !in_touch || junction_changed {
                self.exchange_signals(c, id);
            }
        }
    }

    /// Body `id` came to be `now` from the world's cells (and was `was`).
    fn crossed(&mut self, c: &'static Content, id: u64, was: impl Fn(IVec3) -> Option<IVec3>, now: impl Fn(IVec3) -> IVec3) {
        let Some(b) = self.bodies.get(&id) else { return };
        let blocks: Vec<(IVec3, u32)> = b.all_blocks();
        let watched = b.signals.watched();
        for &(at, _) in &blocks {
            let (from, to) = (was(at), now(at));
            if from == Some(to) {
                continue;
            }
            self.space.signals.passing(to);
            if let Some(w) = from {
                self.space.signals.passing(w);
            }
        }
        // Its own observers, looking at what isn't its own: the world moves past them.
        if let Some(b) = self.bodies.get_mut(&id) {
            for at in watched {
                if b.get(at).is_none_or(|s| s == AIR) && was(at).is_some_and(|p| self.space.get(p) != self.space.get(now(at))) {
                    b.signals.passing(at);
                }
            }
        }
        for (at, state) in blocks {
            let cell = now(at);
            if let Some((owner, cb)) = c
                .block_of(state)
                .cb
                .on_carried
                .filter(|_| was(at) != Some(cell))
            {
                self.call(0, owner, cb, |w| abi::BlockEventArgs {
                    world: w,
                    pos: cell.to_array().into(),
                    state,
                    other: at.to_array().into(),
                    entity: id,
                });
            }
        }
    }

    /// What the world's light is where a body's blocks are, worked out here and now, not left to
    /// whatever clients have heard so far: it is shown with the body from the first. The most
    /// over each of its cubes, per channel.
    fn see_world_light(&mut self, id: u64) {
        let Some(b) = self.bodies.get(&id) else { return };
        let Some(cell) = b.cell else { return };
        let turned = self.entities.get(id).and_then(|e| e.oriented_place());
        let mut lights: FxHashMap<IVec3, u32> = FxHashMap::default();
        for &(at, _) in b.block_snapshot().iter() {
            let w = turned.map_or(at + cell, |t| t.cell_at(at));
            let here = self
                .space
                .cubes
                .get(&cube_of(w))
                .map_or(crate::world::light::SKY_FULL as u32, |cube| cube.light(local_index(w)));
            let most = lights.entry(at >> 4).or_insert(0);
            *most = u32::from_le_bytes(std::array::from_fn(|i| here.to_le_bytes()[i].max(most.to_le_bytes()[i])));
        }
        let Some(b) = self.bodies.get_mut(&id) else { return };
        if b.world_light != lights {
            (b.world_light, b.world_light_changed) = (lights, true);
        }
    }

    /// The light changed somewhere: bodies there look again. Ones landed are the world's blocks and
    /// show as they were.
    pub(super) fn bodies_see_light(&mut self) {
        let ids: Vec<u64> = self
            .bodies
            .iter()
            .filter(|(id, b)| {
                let (Some(cell), Some((lo, hi))) = (b.cell, b.bounds) else { return false };
                if self
                    .entities
                    .get(**id)
                    .is_none_or(|e| e.motion == Motion::Parked)
                {
                    return false;
                }
                let (lo, hi) = self
                    .entities
                    .get(**id)
                    .and_then(|e| e.oriented_place())
                    .map_or((lo + cell, hi + cell), |pose| {
                        let b = pose.box_to_world(Aabb { min: lo.as_dvec3(), max: (hi + IVec3::ONE).as_dvec3() });
                        (b.min.floor().as_ivec3(), b.max.floor().as_ivec3())
                    });
                let (lo, hi) = (cube_of(lo), cube_of(hi));
                self.space
                    .light_dirty
                    .iter()
                    .any(|d| d.cmpge(lo).all() && d.cmple(hi).all())
            })
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.see_world_light(id);
        }
    }

    /// The world's light from body `id`, which is `cell` from the world's cells now.
    fn light_from_body(&mut self, c: &'static Content, id: u64, cell: IVec3) {
        let turned = self.entities.get(id).and_then(|e| e.oriented_place());
        let lights: Vec<(IVec3, [u8; 3])> = self.bodies.get(&id).map_or(Vec::new(), |b| {
            b.block_snapshot()
                .iter()
                .copied()
                .flat_map(|(at, state)| {
                    std::iter::once(state)
                        .chain(b.extras(at).iter().copied())
                        .map(move |part| (at, part))
                })
                .filter(|&(_, state)| c.states[state as usize].light != [0; 3])
                .map(|(at, s)| (turned.map_or(at + cell, |t| t.cell_at(at)), c.states[s as usize].light))
                .collect()
        });
        self.set_body_lights(c, id, lights);
    }

    fn set_body_lights(&mut self, c: &'static Content, id: u64, lights: Vec<(IVec3, [u8; 3])>) {
        let was = self.space.body_lights.remove(&id).unwrap_or_default();
        if was == lights {
            if !was.is_empty() {
                self.space.body_lights.insert(id, was);
            }
            return;
        }
        let touched: Vec<IVec3> = was.iter().chain(lights.iter()).map(|l| l.0).collect();
        if !lights.is_empty() {
            self.space.body_lights.insert(id, lights);
        }
        let Space { cubes, body_light, body_lights, body_light_changes, light_dirty, .. } = &mut *self.space;
        let mut changed = Vec::new();
        for &cell in &touched {
            let all = body_lights
                .values()
                .flatten()
                .filter(|l| l.0 == cell)
                .fold([0u8; 3], |m, l| [m[0].max(l.1[0]), m[1].max(l.1[1]), m[2].max(l.1[2])]);
            let now = (all != [0; 3]).then_some(all);
            if now != body_light.get(&cell).copied() {
                match now {
                    Some(v) => body_light.insert(cell, v),
                    None => body_light.remove(&cell),
                };
                changed.push(cell);
                body_light_changes.push((cell, all));
            }
        }
        let def = &c.dims[self.id as usize];
        let mut map = super::dim::Cubes(cubes);
        let mut lighter = crate::world::light::Lighter::new(&mut map, c, def.sky_light).with_extra(body_light);
        for cell in changed {
            lighter.source_changed(cell);
        }
        let changed = std::mem::take(&mut lighter.changed);
        light_dirty.extend(changed);
    }

    /// What bodies gone gave the world goes with them, however they went; ones whose lamps changed
    /// say so.
    pub(super) fn forget_gone_bodies(&mut self, c: &'static Content) {
        let gone: Vec<u64> = self
            .space
            .body_lights
            .keys()
            .copied()
            .filter(|id| !self.bodies.contains_key(id))
            .collect();
        for id in gone {
            self.set_body_lights(c, id, Vec::new());
        }
        for id in self
            .space
            .signals
            .outside_from()
            .into_iter()
            .filter(|id| !self.bodies.contains_key(id))
        {
            self.space.signals.set_outside(id, Default::default());
        }
        let changed: Vec<(u64, IVec3)> = self
            .bodies
            .iter_mut()
            .filter_map(|(id, b)| {
                std::mem::take(&mut b.emitters_changed)
                    .then_some(())
                    .and(b.cell)
                    .map(|cell| (*id, cell))
            })
            .collect();
        for (id, cell) in changed {
            self.light_from_body(c, id, cell);
        }
    }

    /// What body `id` and the world give each other, as they are next to each other now.
    pub(super) fn exchange_signals(&mut self, c: &'static Content, id: u64) {
        let parent = self
            .entities
            .get(id)
            .and_then(|e| e.mount.as_ref().and(e.frame))
            .unwrap_or(0);
        let shaft = self
            .entities
            .get(id)
            .and_then(|e| e.mount.as_deref().copied());
        let mount = self.entities.get(id).and_then(|e| {
            let place = e.body_place();
            if let Some(m) = &e.mount { Some(m.wire_place()) } else { e.oriented_place().map(|_| (place, place.grid_aligned())) }
        });
        let (outer, child) = if parent == 0 {
            (&mut *self.space, self.bodies.get_mut(&id))
        } else {
            let [Some(outer), child] = self.bodies.get_disjoint_mut([&parent, &id]) else { return };
            (&mut **outer, child)
        };
        let Some(b) = child else { return };
        let Some(cell) = b.cell else { return };
        if mount.is_some_and(|(_, aligned)| !aligned) {
            let (mut into, mut out) = (FxHashMap::default(), FxHashMap::default());
            if parent != 0
                && let Some(shaft) = shaft
            {
                use super::signal::{Gives, View};
                let world = View { cubes: &outer.cubes, on_body: true };
                let child = View { cubes: &b.cubes, on_body: true };
                let face = shaft.face as usize;
                let axis = crate::world::DIRS[face];
                let from_parent = outer.signals.gives(c, world, shaft.block).only(face);
                let from_child = b.signals.gives(c, child, IVec3::ZERO).only(face ^ 1);
                if from_parent != Gives::default() {
                    into.insert(-axis, from_parent);
                }
                if from_child != Gives::default() {
                    out.insert(shaft.block + axis, from_child);
                }
            }
            b.signals.set_outside(0, into);
            outer.signals.set_outside(id, out);
            return;
        }
        let place = mount.map(|(place, _)| place);
        let project = |at: IVec3| place.map_or(at + cell, |t| t.cell_at(at));
        let turn = place.map_or(abi::Turn::NONE, Transform::turn);
        let (giving, around) = {
            let view = super::signal::View { cubes: &b.cubes, on_body: true };
            let around: FxHashSet<IVec3> = b
                .signals
                .cells()
                .flat_map(|p| crate::world::DIRS.map(|d| p + d))
                .filter(|p| b.get(*p).is_none_or(|s| s == AIR))
                .collect();
            (b.signals.giving(c, view, project, turn), around)
        };
        let world = super::signal::View { cubes: &outer.cubes, on_body: parent != 0 };
        let got = outer
            .signals
            .giving_at(c, world, around.into_iter(), project, turn.inverse());
        b.signals.set_outside(0, got);
        outer.signals.set_outside(id, giving);
    }

    /// Those that have anything to say, after circuits have gone.
    pub(super) fn exchange_all_signals(&mut self, c: &'static Content) {
        let world_changed = std::mem::take(&mut self.space.signals.outputs_changed);
        let changed: FxHashSet<_> = self
            .bodies
            .iter()
            .filter_map(|(&id, b)| b.signals.outputs_changed.then_some(id))
            .collect();
        let ids: Vec<u64> = self
            .bodies
            .iter()
            .filter(|&(id, b)| {
                let parent = self
                    .entities
                    .get(*id)
                    .and_then(|e| e.mount.as_ref().and(e.frame));
                b.signals.has_cells() && (changed.contains(id) || parent.map_or(world_changed, |p| changed.contains(&p)))
            })
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.exchange_signals(c, id);
            if let Some(b) = self.bodies.get_mut(&id) {
                b.signals.outputs_changed = false;
            }
        }
    }

    /// Cubes and bodies are about to go elsewhere or be written: light from bodies comes out of
    /// them, and bodies say it all again, wherever they turn out to be.
    pub(super) fn out_of_touch(&mut self, c: &'static Content) {
        let lit: Vec<u64> = self.space.body_lights.keys().copied().collect();
        for id in lit {
            self.set_body_lights(c, id, Vec::new());
        }
        for id in self.space.signals.outside_from() {
            self.space.signals.set_outside(id, Default::default());
        }
        for b in self.bodies.values_mut() {
            b.in_touch = false;
        }
    }

    fn show_landed(&mut self, space_id: u64, c: &'static Content, frame: u64) {
        let hidden = &mut self.space_mut(space_id).not_shown;
        let mut shown = Vec::new();
        hidden.retain(|&pos, (_, _, body)| {
            if *body != frame {
                return true;
            }
            shown.push(pos);
            false
        });
        for pos in shown {
            self.resend_cell(space_id, pos);
        }
        if let Some(source) = self.bodies.get(&frame)
            && let Some(transfer) = &source.transfer
        {
            let edited: Vec<_> = source
                .block_snapshot()
                .iter()
                .filter_map(|&(p, main)| {
                    let before: Vec<_> = source.layout(p).residents(main).collect();
                    let kept: Vec<_> = before
                        .iter()
                        .copied()
                        .filter(|part| {
                            transfer
                                .parts
                                .get(&(p, *part))
                                .is_some_and(|&(at, placed)| self.has_placed(space_id, at, placed))
                        })
                        .collect();
                    (before.len() != kept.len()).then_some((p, kept))
                })
                .collect();
            for (p, mut kept) in edited {
                self.write_residents(frame, c, p, &mut kept, true);
            }
        }
        if let Some(transfer) = self
            .bodies
            .get_mut(&frame)
            .and_then(|space| space.transfer.as_mut())
        {
            transfer.shown_at = Some(self.time);
        }
    }

    /// Bodies waiting to be shown go now.
    pub(super) fn finish_landings(&mut self, space_id: u64) {
        for s in std::mem::take(&mut self.space_mut(space_id).landings) {
            self.entities.remove(s.body, super::dim::UNTOLD);
            self.bodies.remove(&s.body);
        }
        for (at, _) in std::mem::take(&mut self.space_mut(space_id).not_shown) {
            self.resend_cell(space_id, at);
        }
    }

    /// A replacement at the destination ends the old cell's route from its source.
    pub(super) fn invalidate_arrival(&mut self, space_id: u64, pos: IVec3) {
        let keep: Vec<_> = self
            .space(space_id)
            .layout(pos)
            .residents(self.get(space_id, pos).unwrap_or(AIR))
            .collect();
        let frames: Vec<_> = self
            .space(space_id)
            .landings
            .iter()
            .map(|landing| landing.body)
            .collect();
        for frame in frames {
            if let Some(transfer) = self
                .bodies
                .get_mut(&frame)
                .and_then(|space| space.transfer.as_mut())
            {
                transfer
                    .parts
                    .retain(|_, (destination, part)| *destination != pos || keep.contains(part));
            }
        }
    }

    /// Resolves a late click through cells that were committed to another space.
    pub fn landed_as(&self, c: &Content, mut frame: u64, pos: IVec3, part: Part) -> Option<(u64, IVec3, Part, abi::Turn)> {
        c.states.get(part.state as usize)?;
        let (mut owner, mut part) = part.owner(pos);
        let mut turn = abi::Turn::NONE;
        while !self.active_space(frame) {
            let transfer = self.bodies.get(&frame)?.transfer.as_ref()?;
            (owner, part) = *transfer.parts.get(&(owner, part))?;
            frame = transfer.destination;
            turn = turn.then(transfer.turn);
        }
        self.has_placed(frame, owner, part)
            .then_some((frame, owner, part, turn))
    }

    /// Runs `f` on the space blocks at `frame` are in: this region (0) or the blocks of body
    /// `frame`, with where that is in the world. It settles after; the world's with the rest of
    /// the action (`Server::settle`).
    pub fn with_space<R>(&mut self, c: &'static Content, frame: u64, f: impl FnOnce(&mut Dim, u64, Transform) -> R) -> Option<R> {
        if !self.active_space(frame) {
            return None;
        }
        let out = f(self, frame, self.space_pose(frame));
        if frame != 0 {
            self.settle(frame, c);
        }
        Some(out)
    }
}
