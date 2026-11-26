//! Shared cell search and movement-specific route probes.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use glam::{DVec3, IVec3};
use rustc_hash::FxHashMap;

use crate::content::Content;
use crate::physics::{self, Aabb, Dims, Solid, Transform};
use weft_abi::{RouteCost, RouteOptions, navigation as n};

#[derive(PartialEq)]
pub struct Profile {
    pub swim: bool,
    pub liquids: Vec<bool>,
    costs: Vec<RouteCost>,
    open_cost: f32,
}

impl Profile {
    pub fn new(opts: &RouteOptions, liquids: Vec<bool>) -> Self {
        let weight = |value: f32| if value.is_finite() { value.clamp(0.01, 1000.0) } else { 1.0 };
        let costs = unsafe { opts.costs.as_slice() }
            .iter()
            .map(|&cost| RouteCost { multiplier: weight(cost.multiplier), ..cost })
            .collect();
        Self { swim: opts.medium == n::SWIM, liquids, costs, open_cost: weight(opts.open_cost) }
    }

    fn minimum(&self) -> f32 {
        self.costs.iter().map(|r| r.multiplier).fold(1.0, f32::min) * self.open_cost.min(1.0)
    }

    fn block_cost(&self, c: &Content, block: &impl physics::Cells, p: IVec3) -> f32 {
        let mut weight = 1.0;
        if !self.costs.is_empty()
            && let Some((state, layout)) = block.cell(p)
        {
            weight = self
                .costs
                .iter()
                .rev()
                .find(|r| {
                    layout.parts(state).any(|part| match r.selector {
                        n::BLOCK => c.blocks[c.states[part.state as usize].block as usize].first_state == r.id,
                        n::TAG => c.components.has(crate::components::BLOCK, part.state, r.id),
                        _ => false,
                    })
                })
                .map_or(1.0, |r| r.multiplier);
        }
        weight
    }

    fn cost(&self, c: &Content, block: &impl physics::Cells, pos: DVec3, dims: Dims) -> f32 {
        let mut weight = self.block_cost(c, block, pos.floor().as_ivec3());
        if self.open_cost != 1.0 && !physics::collides(c, block, Aabb::around(pos, dims.half_width + 0.6, dims.height)) {
            weight *= self.open_cost;
        }
        weight
    }
}

/// Where an entity is walking to.
pub struct Nav {
    pub target: DVec3,
    pub speed: f32,
    pub profile: Option<Profile>,
    pub end: Option<DVec3>,
    pub path: Vec<IVec3>,
    pub next: usize,
    /// Earliest age the path may be looked for again.
    pub repath: u64,
    /// The target the path was found for; `None` looks again at `repath`.
    pub found_for: Option<DVec3>,
    /// Ticks without getting anywhere.
    pub stuck: u32,
    /// The body it walks on, whose coordinates the target and the path are in.
    pub frame: Option<u64>,
}

impl Nav {
    /// Worth a new search: never searched, the target moved off, or the walk got stuck. A path
    /// to a target that stays put is kept (searching every mob's path every second costs most of
    /// a tick with thousands of mobs).
    pub fn swimming(&self) -> bool {
        self.profile.as_ref().is_some_and(|profile| profile.swim)
    }

    pub fn swim_arrived(&self, pos: DVec3) -> bool {
        (self.target - pos).length_squared() < 0.12_f64.powi(2)
    }

    pub fn stale(&self, age: u64, pos: DVec3) -> bool {
        let retarget = if self.swimming() {
            if self.swim_arrived(pos) {
                return false;
            }
            0.25
        } else {
            0.04_f64.powi(2)
        };
        age >= self.repath
            && self
                .found_for
                .is_none_or(|f| (f - self.target).length_squared() > retarget || self.stuck > 10)
    }
}

const MAX_NODES: usize = 600;

#[derive(Default)]
struct Scratch {
    came: FxHashMap<IVec3, (IVec3, f32)>,
    queue: BinaryHeap<Reverse<(u32, i32, i32, i32)>>,
    edges: Vec<(IVec3, f32)>,
}

thread_local! {
    /// The search's map and queue, kept between searches (thousands of mobs walk about).
    static SCRATCH: std::cell::RefCell<Scratch> = std::cell::RefCell::new(Scratch::default());
}

// Center even-sized footprints between cells so they fit even-width passages.
fn offset(dims: Dims) -> DVec3 {
    let half = (dims.half_width * 2.0).ceil() * 0.5;
    DVec3::new(half, 0.0, half)
}

pub(super) fn walk_point(cell: IVec3, dims: Dims) -> DVec3 {
    cell.as_dvec3() + offset(dims)
}

/// Positions a body can stand in, from `from` towards `to`: the whole way, or as far as
/// it gets when the target is out of reach or more than `MAX_NODES` nodes away.
/// `drop`: the most blocks it goes down at once (what it falls without harm).
pub fn find(
    c: &Content,
    block: &impl physics::Cells,
    from: DVec3,
    to: DVec3,
    dims: Dims,
    drop: i32,
    profile: Option<&Profile>,
) -> Vec<IVec3> {
    let offset = offset(dims);
    let cell = |p: DVec3| (p - offset + DVec3::new(0.5, 0.0, 0.5)).floor().as_ivec3();
    let target = to;
    let (from, to) = (cell(from), cell(to));
    let area = |p: IVec3| Aabb::around(p.as_dvec3() + offset, dims.half_width, dims.height);
    let burns = |s: u32| {
        profile.is_none()
            && c.states[s as usize]
                .liquid
                .is_some_and(|l| c.liquids[l as usize].burns())
    };
    let room = |p: IVec3| {
        let area = area(p);
        if physics::collides(c, block, area) {
            return false;
        }
        let lo = area.min.floor().as_ivec3();
        let hi = area.max.ceil().as_ivec3() - IVec3::ONE;
        for y in lo.y..=hi.y {
            for z in lo.z..=hi.z {
                for x in lo.x..=hi.x {
                    let p = IVec3::new(x, y, z);
                    if block.main(p).is_none_or(burns) || block.layout(p).extras.iter().any(|&s| burns(s)) {
                        return false;
                    }
                }
            }
        }
        true
    };
    let ground = |p: IVec3| {
        let area = area(p);
        let lo = area.min.floor().as_ivec3();
        let hi = area.max.ceil().as_ivec3() - IVec3::ONE;
        let floor = Aabb { min: area.min - DVec3::Y * 0.50001, max: DVec3::new(area.max.x, area.min.y, area.max.z) };
        let mut supported = physics::collides(c, block, floor);
        for z in lo.z..=hi.z {
            for x in lo.x..=hi.x {
                let below = IVec3::new(x, p.y - 1, z);
                if let Some(main) = block.main(below) {
                    for part in block.layout(below).parts(main) {
                        if burns(part.state) {
                            return false;
                        }
                        supported |= c.states[part.state as usize].liquid.is_some();
                    }
                }
            }
        }
        supported
    };
    let standable = |p: IVec3| room(p) && ground(p);
    let wet = |p: IVec3| block.liquid(c, p).is_some();
    let distance = |p: IVec3| (p.as_dvec3() + offset - target).length() as f32;
    let minimum = profile.map_or(1.0, Profile::minimum);
    let h = |p| distance(p) * minimum;
    search(
        from,
        h,
        |p| p == to,
        |p, edges| {
            for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)] {
                let side = IVec3::new(dx, 0, dz);
                // Diagonals only where both straight neighbors leave room (no corner cutting).
                if dx != 0 && dz != 0 && !(room(p + IVec3::new(dx, 0, 0)) && room(p + IVec3::new(0, 0, dz))) {
                    continue;
                }
                let base = if dx != 0 && dz != 0 { 1.414 } else { 1.0 };
                let q = p + side;
                let mut next = None;
                if standable(q) {
                    next = Some((q, base));
                } else if !room(q) {
                    // A step up: jump, with room above the head first.
                    let up = q + IVec3::Y;
                    if standable(up) && room(p + IVec3::Y) {
                        next = Some((up, base + 1.0));
                    }
                } else {
                    // Open but nothing to stand on: drop down to the first ground.
                    for k in 1..=drop {
                        let down = q - IVec3::Y * k;
                        if !room(down) {
                            break;
                        }
                        if ground(down) {
                            next = Some((down, base + 0.5 * k as f32));
                            break;
                        }
                    }
                }
                if let Some((n, cost)) = next {
                    let cost = profile.map_or_else(
                        || cost + if wet(n) { 2.0 } else { 0.0 },
                        |profile| cost * profile.cost(c, block, n.as_dvec3() + offset, dims),
                    );
                    edges.push((n, cost));
                }
            }
        },
    )
}

/// Plan on the ground, keeping exact coordinates for clear legs and final approaches.
pub fn walk(
    c: &Content,
    block: &impl physics::Cells,
    from: DVec3,
    to: DVec3,
    dims: Dims,
    drop: i32,
    profile: Option<&Profile>,
) -> (Option<DVec3>, Vec<IVec3>) {
    if walk_segment(c, block, from, to, dims, profile, true) {
        return (Some(to), Vec::new());
    }
    let path = find(c, block, from, to, dims, drop, profile);
    let last = path
        .last()
        .filter(|_| path.len() > 1)
        .map_or(from, |&p| walk_point(p, dims));
    let end = (last.distance_squared(to) <= 1.0 && walk_segment(c, block, last, to, dims, profile, false)).then_some(to);
    (end, path)
}

fn walk_segment(
    c: &Content,
    block: &impl physics::Cells,
    from: DVec3,
    to: DVec3,
    dims: Dims,
    profile: Option<&Profile>,
    cheapest: bool,
) -> bool {
    let delta = to - from;
    if delta.y.abs() > 0.08 || delta.length_squared() > 12.0_f64.powi(2) {
        return false;
    }
    let segments = (delta.length() * 4.0).ceil().max(1.0) as u32;
    let step = delta / f64::from(segments);
    let priced = profile.filter(|_| cheapest).map(|p| (p, p.minimum()));
    for i in 0..segments {
        let pos = from + step * f64::from(i);
        let next = pos + step;
        let body = Aabb::around(pos, dims.half_width, dims.height);
        let foot = Aabb::around(next - DVec3::Y * 0.1, dims.half_width, 0.095);
        if !physics::collides(c, block, foot) || block.cell(foot.min.floor().as_ivec3()).is_none() {
            return false;
        }
        if priced.is_some_and(|(p, minimum)| {
            (p.open_cost != 1.0 && p.cost(c, block, next, dims) > minimum) || !cheapest_segment(c, block, pos, step, p, minimum)
        }) || (profile.is_none() && block.liquid(c, next.floor().as_ivec3()).is_some())
        {
            return false;
        }
        let mut clear = !physics::collides(c, block, Aabb::around(next, dims.half_width, dims.height));
        if clear {
            physics::rotated_contacts(c, block, Solid::Box(body), Transform::default(), step, |hit| {
                clear &= hit.depth <= 1e-6 && (hit.fraction >= 1.0 || hit.normal.dot(step) >= -1e-7);
            });
        }
        if !clear {
            return false;
        }
    }
    true
}

pub fn swim_point(p: IVec3, dims: Dims) -> DVec3 {
    p.as_dvec3() + offset(dims).with_y(((1.0 - dims.height) * 0.5).max(0.0))
}

// A swept body must stay below a permitted liquid surface. Full, empty water
// cells need no solid-shape probe.
fn swim_space(
    c: &Content,
    block: &impl physics::Cells,
    pos: DVec3,
    body: Aabb,
    delta: DVec3,
    dims: Dims,
    profile: &Profile,
) -> Option<bool> {
    let area = body.swept(delta);
    let lo = area.min.floor().as_ivec3();
    let hi = area.max.ceil().as_ivec3() - IVec3::ONE;
    let padding = DVec3::new(dims.half_width, 0.0, dims.half_width);
    let epsilon = DVec3::splat(1e-8);
    let mut solids = false;
    for z in lo.z..=hi.z {
        for y in lo.y..=hi.y {
            for x in lo.x..=hi.x {
                let p = IVec3::new(x, y, z);
                let cell = p.as_dvec3();
                let top = cell.y + 1.0;
                let surface = physics::liquid_surface(c, block, p)
                    .filter(|&(liquid, _)| profile.liquids.get(liquid as usize) == Some(&true))
                    .map_or(cell.y, |(_, surface)| surface);
                if surface + 1e-8 < area.max.y.min(top) {
                    let dry = Aabb {
                        min: cell.with_y(surface) - padding - DVec3::Y * dims.height + epsilon,
                        max: cell + DVec3::ONE + padding - epsilon,
                    };
                    if physics::ray_aabb(pos, delta, &dry, 1.0).is_some() {
                        return None;
                    }
                }
                solids |= block.cell(p).is_none_or(|(s, layout)| {
                    layout.parts(s).any(|part| {
                        let state = &c.states[part.state as usize];
                        state.cylinder.is_some() || !state.collision.is_empty()
                    })
                });
            }
        }
    }
    Some(solids)
}

fn cheapest_segment(c: &Content, block: &impl physics::Cells, from: DVec3, delta: DVec3, profile: &Profile, minimum: f32) -> bool {
    if profile.costs.is_empty() {
        return true;
    }
    let to = from + delta;
    let lo = from.min(to).floor().as_ivec3();
    let hi = from.max(to).floor().as_ivec3();
    for z in lo.z..=hi.z {
        for y in lo.y..=hi.y {
            for x in lo.x..=hi.x {
                let p = IVec3::new(x, y, z);
                let min = p.as_dvec3();
                if profile.block_cost(c, block, p) > minimum
                    && physics::ray_aabb(from, delta, &Aabb { min, max: min + DVec3::ONE }, 1.0).is_some()
                {
                    return false;
                }
            }
        }
    }
    true
}

fn swim_segment(c: &Content, block: &impl physics::Cells, from: DVec3, to: DVec3, dims: Dims, profile: &Profile, cheapest: bool) -> bool {
    let delta = to - from;
    let length = delta.length();
    // Broad swept intervals are limited to local pieces to keep diagonal probes cheap.
    let segments = (length * 2.0).ceil().max(1.0) as u32;
    let step = delta / f64::from(segments);
    let minimum = cheapest.then(|| profile.minimum());
    for i in 0..segments {
        let pos = from + step * f64::from(i);
        let bounds = Aabb::around(pos, dims.half_width, dims.height);
        let Some(solids) = swim_space(c, block, pos, bounds, step, dims, profile) else { return false };
        if minimum.is_some_and(|minimum| !cheapest_segment(c, block, pos, step, profile, minimum)) {
            return false;
        }
        if !solids {
            continue;
        }
        let mut clear = true;
        physics::rotated_contacts(c, block, Solid::Box(bounds), Transform::default(), step, |hit| {
            clear &= hit.depth <= 1e-6 && (hit.fraction >= 1.0 || hit.normal.dot(step) >= -1e-7);
        });
        if !clear {
            return false;
        }
    }
    true
}

pub fn swim(
    c: &Content,
    block: &impl physics::Cells,
    from: DVec3,
    to: DVec3,
    dims: Dims,
    profile: &Profile,
) -> (Option<DVec3>, Vec<IVec3>) {
    let straight = from + (to - from).clamp_length_max(32.0);
    if profile.open_cost == 1.0 && swim_segment(c, block, from, straight, dims, profile, true) {
        return (Some(straight), Vec::new());
    }
    let from_cell = (from - offset(dims) + DVec3::new(0.5, 0.0, 0.5))
        .floor()
        .as_ivec3();
    let goal = (to - offset(dims) + DVec3::new(0.5, 0.0, 0.5))
        .floor()
        .as_ivec3();
    let minimum = profile.minimum();
    let goal_point = swim_point(goal, dims);
    let estimate = |p| (swim_point(p, dims) - goal_point).length() as f32 * minimum;
    let path = search(
        from_cell,
        estimate,
        |p| p == goal,
        |p, edges| {
            let pos = if p == from_cell { from } else { swim_point(p, dims) };
            for axis in [IVec3::X, -IVec3::X, IVec3::Y, -IVec3::Y, IVec3::Z, -IVec3::Z] {
                let next = p.saturating_add(axis);
                let point = swim_point(next, dims);
                if next != p && swim_segment(c, block, pos, point, dims, profile, false) {
                    edges.push((next, pos.distance(point) as f32 * profile.cost(c, block, point, dims)));
                }
            }
        },
    );
    let last = *path.last().unwrap();
    let last_pos = if last == from_cell { from } else { swim_point(last, dims) };
    let end = (last == goal && swim_segment(c, block, last_pos, to, dims, profile, false)).then_some(to);
    (end, path)
}

fn search(
    from: IVec3,
    h: impl Fn(IVec3) -> f32,
    arrived: impl Fn(IVec3) -> bool,
    mut neighbors: impl FnMut(IVec3, &mut Vec<(IVec3, f32)>),
) -> Vec<IVec3> {
    let Scratch { mut came, mut queue, mut edges } = SCRATCH.with_borrow_mut(std::mem::take);
    came.clear();
    queue.clear();
    came.insert(from, (from, 0.0));
    queue.push(Reverse((h(from).to_bits(), from.x, from.y, from.z)));
    let mut best = (h(from), from);
    let mut visited = 0;
    while let Some(Reverse((priority, x, y, z))) = queue.pop() {
        let p = IVec3::new(x, y, z);
        let g = came[&p].1;
        let d = h(p);
        if priority != (g + d).to_bits() {
            continue;
        }
        if d < best.0 {
            best = (d, p);
        }
        if arrived(p) {
            best = (0.0, p);
            break;
        }
        visited += 1;
        if visited > MAX_NODES {
            break;
        }
        neighbors(p, &mut edges);
        for (n, cost) in edges.drain(..) {
            let g2 = g + cost;
            if came.get(&n).is_none_or(|&(_, old)| g2 < old) {
                came.insert(n, (p, g2));
                queue.push(Reverse(((g2 + h(n)).to_bits(), n.x, n.y, n.z)));
            }
        }
    }
    let mut path = vec![best.1];
    while let Some(&(prev, _)) = came.get(path.last().unwrap()) {
        if prev == *path.last().unwrap() {
            break;
        }
        path.push(prev);
    }
    path.reverse();
    SCRATCH.with_borrow_mut(|s| *s = Scratch { came, queue, edges });
    path
}

/// Steering for this tick: walk direction, jump, and whether the target is reached.
pub fn steer(nav: &mut Nav, pos: DVec3, dims: Dims, blocked: bool) -> Option<(DVec3, bool, bool)> {
    let feet = pos.floor().as_ivec3();
    while let Some(&w) = nav.path.get(nav.next) {
        let center = walk_point(w, dims);
        let flat = (center - pos).with_y(0.0);
        if flat.length() < 0.35 && (w.y - feet.y).abs() <= 1 {
            nav.next += 1;
            continue;
        }
        return Some((flat, w.y > feet.y || blocked, false));
    }
    nav.end.map(|p| ((p - pos).with_y(0.0), false, true))
}
