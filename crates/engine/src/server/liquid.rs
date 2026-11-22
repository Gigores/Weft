//! Liquids flow in the engine, following modern vanilla's FlowingFluid step by step: a liquid
//! block reacts to neighbor changes by scheduling a tick after its delay; a tick recomputes the
//! level from the neighbors, falls if it can and otherwise spreads sideways only towards the
//! nearest way down within `slope_find`. Mods describe liquids and reactions as data
//! (`abi::comp::liquid`, `abi::comp::reaction`), nothing calls into a mod per update.

use glam::{DVec3, IVec3};
use weft_abi::{ItemStack, block_flags as bf};

use super::dim::Dim;
use crate::content::{AIR, Content, LiquidDef};
use crate::physics::Cells;
use crate::world::DIRS;

/// North, east, south, west: vanilla's horizontal order, which decides ties.
const HORIZONTAL: [IVec3; 4] = [IVec3::new(0, 0, -1), IVec3::new(1, 0, 0), IVec3::new(0, 0, 1), IVec3::new(-1, 0, 0)];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Fluid {
    id: u16,
    /// 1..8, 8 for sources and falling liquid.
    amount: u32,
    source: bool,
    falling: bool,
}

fn decode(c: &Content, state: u32) -> Option<Fluid> {
    let id = c.states[state as usize].liquid?;
    let i = state - c.liquids[id as usize].first_state;
    let (source, falling) = (i == 0, i >= 8);
    Some(Fluid { id, amount: if source || falling { 8 } else { 8 - i }, source, falling })
}

fn encode(def: &LiquidDef, amount: u32, falling: bool) -> u32 {
    if falling { def.first_state + 8 } else { def.first_state + 8 - amount.clamp(1, 8) }
}

impl Dim {
    fn fluid(&self, space_id: u64, c: &Content, p: IVec3) -> Option<Fluid> {
        self.space(space_id).liquid(c, p).and_then(|s| decode(c, s))
    }

    /// The liquid at `p` reaching over to the cell in direction `d` of it: not past a solid part
    /// in between, as a bottom slab is under the water in it.
    fn fluid_towards(&self, space_id: u64, c: &Content, p: IVec3, d: IVec3) -> Option<Fluid> {
        self.fluid(space_id, c, p)
            .filter(|_| !self.sealed(space_id, c, p, d))
    }

    /// Whether what shares the cell at `p` with a liquid closes its side towards `d`: what its
    /// shape covers whole there (a stair's back), or what its block says (`liquid_walls`).
    fn sealed(&self, space_id: u64, c: &Content, p: IVec3, d: IVec3) -> bool {
        let space = self.space(space_id);
        let Some(main) = space.get(p) else { return false };
        let layout = space.layout(p);
        let Some(dir) = crate::world::DIRS.iter().position(|&v| v == d) else { return false };
        if layout.offsets.is_empty() && layout.covers.is_empty() && layout.extras.len() <= 1 {
            return std::iter::once(main)
                .chain(layout.extras.iter().copied())
                .any(|s| c.liquid_walls[s as usize] & (1 << dir) != 0);
        }
        layout.covered_faces(c, main, true) & (1 << dir) != 0
    }

    /// Room for liquid: neither solid nor a ladder (unloaded counts as solid).
    fn has_room(&self, space_id: u64, c: &Content, p: IVec3) -> bool {
        let room = |s: u32| c.states[s as usize].flags & (bf::SOLID | bf::CLIMBABLE) == 0;
        self.get(space_id, p).is_some_and(|main| {
            self.space(space_id)
                .layout(p)
                .parts(main)
                .all(|part| room(part.state))
        })
    }

    fn solid(&self, space_id: u64, c: &Content, p: IVec3) -> bool {
        self.get(space_id, p).is_none_or(|main| {
            self.space(space_id)
                .layout(p)
                .parts(main)
                .any(|part| c.solid(part.state))
        })
    }

    /// What liquid `l` would make of `p` from its neighbors: a state, or `None` for nothing.
    fn new_liquid(&self, space_id: u64, c: &Content, p: IVec3, l: u16, down: IVec3) -> Option<u32> {
        let def = &c.liquids[l as usize];
        let (mut max, mut sources) = (0, 0);
        for d in HORIZONTAL {
            if let Some(f) = self
                .fluid_towards(space_id, c, p + d, -d)
                .filter(|f| f.id == l)
            {
                sources += f.source as u32;
                max = max.max(f.amount);
            }
        }
        if sources >= def.renewable {
            let under = self.fluid(space_id, c, p + down);
            if self.solid(space_id, c, p + down) || under.is_some_and(|f| f.id == l && f.source) {
                return Some(def.first_state);
            }
        }
        if self
            .fluid_towards(space_id, c, p - down, down)
            .is_some_and(|f| f.id == l)
        {
            return Some(encode(def, 8, true));
        }
        let k = max as i32 - def.drop_off as i32;
        (k > 0).then(|| encode(def, k as u32, false))
    }

    fn is_hole(&self, space_id: u64, c: &Content, below: IVec3, l: u16) -> bool {
        self.fluid(space_id, c, below).is_some_and(|f| f.id == l) || self.has_room(space_id, c, below)
    }

    fn passes(&self, space_id: u64, c: &Content, p: IVec3, l: u16) -> bool {
        !self
            .fluid(space_id, c, p)
            .is_some_and(|f| f.id == l && f.source)
            && self.has_room(space_id, c, p)
    }

    /// Whether a liquid may flow into `p`: into empty room (what it meets there reacts by its
    /// rules).
    fn can_spread_to(&self, space_id: u64, c: &Content, p: IVec3) -> bool {
        if self
            .space(space_id)
            .bounds
            .is_some_and(|(lo, hi)| p.cmplt(lo).any() || p.cmpgt(hi).any())
        {
            return false;
        }
        self.fluid(space_id, c, p).is_none() && self.has_room(space_id, c, p)
    }

    fn spread_to(&mut self, space_id: u64, c: &'static Content, p: IVec3, state: u32) -> Option<()> {
        if self.fluid(space_id, c, p).is_some() {
            return None;
        }
        let was = self.parts(space_id, p);
        self.set(space_id, c, p, state)?;
        // Plants and torches wash away with their drops, like vanilla.
        for old in was {
            self.drop_block_loot(space_id, c, p, old, ItemStack::EMPTY, false, 0);
        }
        Some(())
    }

    fn slope_distance(&self, space_id: u64, c: &Content, p: IVec3, depth: u32, from: IVec3, l: u16, down: IVec3) -> u32 {
        let mut min = 1000;
        for d in HORIZONTAL {
            if d == from {
                continue;
            }
            let n = p + d;
            if self.passes(space_id, c, n, l) {
                if self.is_hole(space_id, c, n + down, l) {
                    return depth;
                }
                if depth < c.liquids[l as usize].slope_find {
                    min = min.min(self.slope_distance(space_id, c, n, depth + 1, -d, l, down));
                }
            }
        }
        min
    }

    /// Directions to spread in: the ones nearest to a way down, each with the state it gets.
    fn spread_dirs(&self, space_id: u64, c: &Content, p: IVec3, l: u16, down: IVec3) -> Vec<(IVec3, u32)> {
        let mut min = 1000;
        let mut out = Vec::new();
        for d in HORIZONTAL {
            let n = p + d;
            if !self.passes(space_id, c, n, l) {
                continue;
            }
            let Some(state) = self.new_liquid(space_id, c, n, l, down) else { continue };
            let slope = if self.is_hole(space_id, c, n + down, l) { 0 } else { self.slope_distance(space_id, c, n, 1, -d, l, down) };
            if slope < min {
                out.clear();
            }
            if slope <= min {
                out.push((d, state));
                min = slope;
            }
        }
        out
    }

    fn spread_sides(&mut self, space_id: u64, c: &'static Content, p: IVec3, f: Fluid, down: IVec3) {
        let def = &c.liquids[f.id as usize];
        let amount = if f.falling { 7 } else { f.amount as i32 - def.drop_off as i32 };
        if amount <= 0 {
            return;
        }
        for (d, state) in self.spread_dirs(space_id, c, p, f.id, down) {
            if !self.sealed(space_id, c, p, d) && self.can_spread_to(space_id, c, p + d) {
                self.spread_to(space_id, c, p + d, state);
            }
        }
    }

    /// The scheduled tick of a liquid block.
    pub(super) fn liquid_tick(&mut self, space_id: u64, c: &'static Content, p: IVec3, state: u32) {
        let Some(mut f) = decode(c, state) else { return };
        let def = &c.liquids[f.id as usize];
        let down = if def.rises { IVec3::Y } else { IVec3::NEG_Y };
        if !f.source {
            match self.new_liquid(space_id, c, p, f.id, down) {
                None => {
                    self.replace_part(space_id, c, p, state, AIR);
                    return;
                }
                Some(s) if s != state => {
                    self.replace_part(space_id, c, p, state, s);
                    f = decode(c, s).unwrap_or(f);
                }
                _ => {}
            }
        }
        let below = p + down;
        if !self.sealed(space_id, c, p, down) && self.can_spread_to(space_id, c, below) {
            let state = self
                .new_liquid(space_id, c, below, f.id, down)
                .unwrap_or(encode(def, 8, true));
            self.spread_to(space_id, c, below, state);
            let sources = HORIZONTAL
                .iter()
                .filter(|&&d| {
                    self.fluid(space_id, c, p + d)
                        .is_some_and(|n| n.id == f.id && n.source)
                })
                .count();
            if sources >= 3 {
                self.spread_sides(space_id, c, p, f, down);
            }
        } else if f.source || !self.is_hole(space_id, c, below, f.id) {
            self.spread_sides(space_id, c, p, f, down);
        }
    }

    /// A neighbor of a liquid changed: react with what touches it, else flow after the delay.
    pub(super) fn liquid_neighbor(&mut self, space_id: u64, c: &'static Content, p: IVec3, state: u32) {
        let Some(f) = decode(c, state) else { return };
        let def = &c.liquids[f.id as usize];
        self.schedule(space_id, p, def.delay);
    }

    /// Bucket logic for `player` holding `held`: pour a liquid item, or fill an empty container
    /// from a source it looks at. The new held stack, or `None` if nothing happened.
    pub fn use_container(&mut self, space_id: u64, c: &'static Content, player: u64, held: ItemStack) -> Option<ItemStack> {
        if held.is_empty() {
            return None;
        }
        let liquid = c.item_liquid.get(&held.item).copied();
        let reach = self.block_reach(space_id, c, player);
        let flags = if liquid.is_none() { crate::physics::LIQUID_SOURCES } else { weft_abi::ray::BLOCKS };
        let hit = self.look_ray(space_id, c, player, reach, flags)?;
        self.with_space(c, hit.frame, |region, _, _| region.use_container_at(c, player, held, hit, liquid))?
    }

    fn use_container_at(
        &mut self,
        c: &'static Content,
        player: u64,
        held: ItemStack,
        hit: crate::physics::Hit,
        liquid: Option<(u16, u32)>,
    ) -> Option<ItemStack> {
        let space_id = hit.frame;
        let creative = self
            .entities
            .get(player)
            .is_some_and(|e| e.mode.has(crate::tag_bits::game_mode::NO_COSTS));
        if let Some((l, empty)) = liquid {
            let source = c.liquids[l as usize].first_state;
            // Into what holds liquid, with the same say as placing a block.
            if c.states[hit.state as usize].liquid.is_none()
                && self.fluid(space_id, c, hit.block).is_none()
                && self.can_hold_liquid(space_id, c, hit.block, source)
            {
                let mut q = weft_abi::Query {
                    entity: player,
                    pos: hit.block.to_array().into(),
                    state: source,
                    item: held.item,
                    ..weft_abi::Query::DEFAULT
                };
                if !self.ask(space_id, c, weft_abi::action::PLACE_BLOCK, &mut q) || !self.add_part(space_id, c, hit.block, source) {
                    return None;
                }
                return Some(if creative || empty == 0 { held } else { ItemStack::new(empty, 1) });
            }
            let replaceable = |s: u32| c.states[s as usize].flags & bf::REPLACEABLE != 0;
            let target = if replaceable(hit.state) { hit.block } else { hit.block + DIRS[hit.face as usize] };
            let old = self.get(space_id, target).filter(|&s| replaceable(s))?;
            // Only into what would become a source, and with the same say as placing a block.
            if old == source {
                return None;
            }
            let mut q = weft_abi::Query {
                entity: player,
                pos: target.to_array().into(),
                state: source,
                item: held.item,
                ..weft_abi::Query::DEFAULT
            };
            if !self.ask(space_id, c, weft_abi::action::PLACE_BLOCK, &mut q) {
                return None;
            }
            if c.states[old as usize].liquid.is_some() {
                self.set(space_id, c, target, source)?;
            } else {
                self.spread_to(space_id, c, target, source)?;
            }
            return Some(if creative || empty == 0 { held } else { ItemStack::new(empty, 1) });
        }
        let f = decode(c, hit.state).filter(|f| f.source)?;
        let filled = *c.containers.get(&(held.item, f.id))?;
        let mut q = weft_abi::Query {
            entity: player,
            pos: hit.block.to_array().into(),
            state: hit.state,
            item: held.item,
            ..weft_abi::Query::DEFAULT
        };
        if !self.ask(space_id, c, weft_abi::action::BREAK_BLOCK, &mut q) || !self.replace_part(space_id, c, hit.block, hit.state, AIR) {
            return None;
        }
        if creative {
            return Some(held);
        }
        if held.count == 1 {
            return Some(ItemStack::new(filled, 1));
        }
        let rest = self
            .entities
            .insert_item(c, player, ItemStack::new(filled, 1));
        if !rest.is_empty() {
            let pos = self.entities.get(player).map_or(DVec3::ZERO, |e| e.pos);
            self.spawn_item(0, c, pos, rest);
        }
        Some(ItemStack { count: held.count - 1, ..held })
    }

    /// The block an entity looks at from its eyes, in the world or a moving structure.
    fn look_ray(&self, space_id: u64, c: &Content, id: u64, max: f64, flags: u32) -> Option<crate::physics::Hit> {
        let e = self.entities.get(id)?;
        let eye = e.pos + DVec3::Y * e.eye(c);
        let place = self.space_pose(space_id);
        self.ray(space_id, c, place.to_local(eye), place.vector_to_local(crate::physics::look_dir(e.yaw, e.pitch)), max, flags, &[])
    }

    /// The first block along a ray from `from` (in this dimension's frame: a body's blocks when
    /// asked from them), bodies included when asked from the world; `flags` as `physics::pick`'s.
    pub fn ray(
        &self,
        space_id: u64,
        c: &Content,
        from: DVec3,
        dir: DVec3,
        max: f64,
        flags: u32,
        skip: &[u64],
    ) -> Option<crate::physics::Hit> {
        // Asked from a body's blocks, it looks along those.
        if space_id != 0 {
            return crate::physics::pick(c, self.space(space_id), &[], from, dir, max, flags)
                .map(|hit| crate::physics::Hit { frame: space_id, ..hit });
        }
        let structures: Vec<(u64, crate::physics::Transform, &dyn crate::physics::Blocks)> = self
            .bodies
            .iter()
            .filter(|(sid, _)| !skip.contains(sid))
            .filter_map(|(&sid, s)| {
                self.entities
                    .get(sid)
                    .map(|b| (sid, b.body_place(), &**s as &dyn crate::physics::Blocks))
            })
            .collect();
        crate::physics::pick(c, self.space(space_id), &structures, from, dir, max, flags)
    }
}
