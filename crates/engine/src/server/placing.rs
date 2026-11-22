//! Features and templates put into the world as it is (`Host::place`, `/structure load`), with
//! what they replaced kept to undo them, and what a generated cube's features bring (entities,
//! loot, block entities, `event::FEATURE_PLACED`) put in once it joins the world.

use std::collections::VecDeque;

use glam::{DVec3, IVec3};
use weft_abi as abi;

use super::dim::{Dim, UNTOLD};
use crate::content::{AIR, Content};
use crate::features::{Cells, Extra, Generated, Start};
use crate::template::Transform;
use crate::world::placement::Part;

/// The last placings undo can take back.
const KEPT: usize = 16;

/// A placing: its id, the dimension, and each cell it changed with all that was in it.
type Placing = (u64, u32, Vec<(IVec3, Vec<Part>)>);

static PLACINGS: parking_lot::Mutex<VecDeque<Placing>> = parking_lot::Mutex::new(VecDeque::new());
static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// The world taking a start's blocks: what each cell held before is kept.
struct Live<'a> {
    d: &'a Dim,
    space_id: u64,
    lo: IVec3,
    hi: IVec3,
    after: rustc_hash::FxHashMap<IVec3, Vec<Part>>,
}

impl Live<'_> {
    fn edit(&mut self, p: IVec3) -> &mut Vec<Part> {
        let space = self.d.space(self.space_id);
        self.after.entry(p).or_insert_with(|| {
            space
                .layout(p)
                .residents(space.get(p).unwrap_or(AIR))
                .collect()
        })
    }
}

impl Cells for Live<'_> {
    fn bounds(&self) -> (IVec3, IVec3) {
        (self.lo, self.hi)
    }
    fn get(&self, p: IVec3) -> Option<u32> {
        if p.cmplt(self.lo).any() || p.cmpgt(self.hi).any() {
            return None;
        }
        self.after
            .get(&p)
            .map_or_else(|| self.d.get(self.space_id, p), |parts| Some(parts.first().map_or(AIR, |v| v.state)))
    }
    fn set(&mut self, p: IVec3, state: u32) {
        for &cover in &self.d.space(self.space_id).layout(p).covers {
            let (owner, old) = cover.owner(p);
            self.edit(owner).retain(|part| *part != old);
        }
        self.after
            .insert(p, if state == AIR { Vec::new() } else { vec![Part::aligned(state)] });
    }
    fn add_part(&mut self, p: IVec3, part: Part) {
        let parts = self.edit(p);
        if !parts.contains(&part) {
            parts.push(part);
        }
    }
}

impl Dim {
    /// Template `t` with its lowest corner at `at` once placed with `transform`, slots filled
    /// from `palette`. The placing's id, `None` if part of it isn't loaded.
    pub fn place_template(
        &mut self,
        space_id: u64,
        c: &'static Content,
        t: &crate::template::TemplateDef,
        at: IVec3,
        transform: Transform,
        palette: Option<u32>,
    ) -> Option<u64> {
        let variant = t.variant(c, transform, palette);
        // Not a feature of the dimension: any of its own will do to hold it.
        let mut start = Start::loose(at, transform);
        let view =
            crate::generation::ReadCtx { snapshot: crate::generation::Snapshot::new(c, self.id, self.seed, 0), local: None, plan: None };
        let piece = crate::features::make_piece(&view, at, variant, 0, false);
        crate::features::add_piece(&mut start, piece);
        self.place_start(space_id, c, &start)
    }

    /// Feature `fi` of the dimension as if it started at `at`.
    pub fn place_feature(&mut self, space_id: u64, c: &'static Content, fi: usize, at: IVec3) -> Option<u64> {
        let def = &c.dims[self.id as usize];
        let f = def.features.get(fi)?;
        let random = weft_noise::hash(self.seed ^ f.salt, at.x as i64, at.y as i64, at.z as i64);
        let t = f.transforms[(random >> 40) as usize % f.transforms.len()];
        let biome = def.column(self.seed, at.x, at.z).map_or(0, |c| c.biome);
        let start = crate::features::make_start(c, def, self.id, self.seed, fi, at, biome, random, t)?;
        self.place_start(space_id, c, &start)
    }

    fn place_start(&mut self, space_id: u64, c: &'static Content, start: &Start) -> Option<u64> {
        let (lo, hi) = (start.lo, start.hi);
        if lo.cmpgt(hi).any() {
            return None;
        }
        let (clo, chi) = (crate::world::cube_of(lo), crate::world::cube_of(hi));
        for z in clo.z..=chi.z {
            for y in clo.y..=chi.y {
                for x in clo.x..=chi.x {
                    self.get(space_id, IVec3::new(x, y, z) * 16)?;
                }
            }
        }
        let def = &c.dims[self.id as usize];
        let mut live = Live { d: self, space_id, lo, hi, after: Default::default() };
        crate::features::write_start(c, def, live.d.seed, start, &mut live);
        let after = live.after;
        let before = self.place_cells(space_id, c, after)?;
        self.bring(space_id, c, &start.extras);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut kept = PLACINGS.lock();
        kept.push_back((id, self.id, before));
        while kept.len() > KEPT {
            kept.pop_front();
        }
        Some(id)
    }

    /// Validate the complete selection before clearing space. This also handles a template
    /// whose next row contains the far end of an earlier block.
    fn place_cells(
        &mut self,
        space_id: u64,
        c: &'static Content,
        after: rustc_hash::FxHashMap<IVec3, Vec<Part>>,
    ) -> Option<Vec<(IVec3, Vec<Part>)>> {
        if !c.dims[self.id as usize].editable {
            return None;
        }
        let space = self.space(space_id);
        let mut before = Vec::new();
        let mut occupying: rustc_hash::FxHashMap<IVec3, Vec<(IVec3, Part)>> = Default::default();
        for (&p, parts) in &after {
            let old: Vec<_> = space.layout(p).residents(space.get(p)?).collect();
            if old != *parts {
                before.push((p, old.clone()));
            }
            for part in old.iter().filter(|v| !parts.contains(v)) {
                for &d in part.footprint(c).iter() {
                    space.get(p + d)?;
                }
            }
            for &part in parts {
                for d in std::iter::once(IVec3::ZERO).chain(part.footprint(c).iter().copied()) {
                    let at = p + d;
                    let shape = part.in_cell(d);
                    let collides = |v: Part| !c.may_share(v.state, part.state) && v.overlaps(shape, c);
                    if space
                        .layout(at)
                        .parts(space.get(at)?)
                        .any(|v| !after.contains_key(&v.owner(at).0) && collides(v))
                    {
                        return None;
                    }
                    let others = occupying.entry(at).or_default();
                    if others.iter().any(|&(owner, v)| owner != p && collides(v)) {
                        return None;
                    }
                    others.push((p, shape));
                }
            }
        }
        for (p, old) in &before {
            let mut kept: Vec<_> = old
                .iter()
                .filter(|v| after[p].contains(v))
                .copied()
                .collect();
            if kept.len() != old.len() {
                self.write_residents(space_id, c, *p, &mut kept, false)?;
            }
        }
        for (p, mut parts) in after {
            self.write_residents(space_id, c, p, &mut parts, false)?;
        }
        Some(before)
    }

    /// Puts back what placing `id` replaced; whether it was still kept.
    pub fn unplace(&mut self, space_id: u64, c: &'static Content, id: u64) -> bool {
        let taken = {
            let mut kept = PLACINGS.lock();
            let Some(i) = kept.iter().position(|p| p.0 == id && p.1 == self.id) else { return false };
            kept.remove(i)
        };
        let Some((_, _, cells)) = taken else { return false };
        self.place_cells(space_id, c, cells.into_iter().collect())
            .is_some()
    }

    /// The last placing in this dimension still kept, for `/structure undo`.
    pub fn last_placing(&self) -> Option<u64> {
        PLACINGS
            .lock()
            .iter()
            .rev()
            .find(|p| p.1 == self.id)
            .map(|p| p.0)
    }

    /// The box `lo..=hi` as a template's parts: every block, what block entities hold and the
    /// entities in it but players; `None` if part of it isn't here.
    pub fn capture(&self, space_id: u64, c: &'static Content, lo: IVec3, hi: IVec3) -> Option<crate::template::Parts> {
        let mut parts = crate::template::Parts::default();
        let inside = |p: IVec3| p.cmpge(lo).all() && p.cmple(hi).all();
        let mut outside = rustc_hash::FxHashSet::default();
        for y in lo.y..=hi.y {
            for z in lo.z..=hi.z {
                for x in lo.x..=hi.x {
                    let p = IVec3::new(x, y, z);
                    let state = self.get(space_id, p)?;
                    let layout = self.space(space_id).layout(p);
                    for cover in &layout.covers {
                        let (owner, part) = cover.owner(p);
                        if !inside(owner) && outside.insert((owner, part)) {
                            self.get(space_id, owner)?;
                            let block = abi::TemplateBlock { pos: (owner - lo).to_array(), block: c.state_name(part.state) };
                            parts.positioned.push((block, part.offset));
                        }
                    }
                    let mut residents = layout.residents(state);
                    let main = residents.next();
                    let aligned = main.filter(|v| v.offset == IVec3::ZERO);
                    let entry = |state| abi::TemplateBlock { pos: (p - lo).to_array(), block: c.state_name(state) };
                    parts.blocks.push(entry(aligned.map_or(AIR, |v| v.state)));
                    for part in main
                        .filter(|_| aligned.is_none())
                        .into_iter()
                        .chain(residents)
                    {
                        parts.positioned.push((entry(part.state), part.offset));
                    }
                }
            }
        }
        for e in self
            .entities
            .list
            .iter()
            .filter(|e| !e.removed && e.frame.unwrap_or(0) == space_id && e.vehicle.is_none())
        {
            if e.kind == c.game.player_kind {
                continue;
            }
            let data = super::serialize(c, self, &[e.id]);
            match e.anchor {
                Some(a)
                    if inside(a)
                        || outside.iter().any(|&(owner, part)| {
                            owner == a
                                && part.offset == e.anchor_offset
                                && c.game
                                    .block_entities
                                    .get(&c.states[part.state as usize].block)
                                    == Some(&e.kind)
                        }) =>
                {
                    parts
                        .data
                        .push(abi::TemplateData { pos: (a - lo).to_array(), data })
                }
                Some(_) => {}
                None => {
                    let at = self.space_pose(space_id).to_local(e.pos);
                    if inside(at.floor().as_ivec3()) {
                        let rel = (at - lo.as_dvec3()).as_vec3();
                        let kind = c.game.entities[e.kind as usize].name.clone();
                        parts
                            .entities
                            .push(abi::TemplateEntity { pos: rel.to_array(), kind, data });
                    }
                }
            }
        }
        Some(parts)
    }

    /// Entities and what block entities hold, as features bring them.
    pub fn bring(&mut self, space_id: u64, c: &'static Content, extras: &[Extra]) {
        for e in extras {
            match (e.anchor, &e.data) {
                (Some(p), Some(data)) => {
                    let Some((mut saved, inv, comps, _)) = super::persist::read(c, data, self.time).into_iter().next() else { continue };
                    let (p, part) = Part { state: AIR, offset: e.transform.offset(saved.anchor_offset) }.owner(p);
                    saved.anchor_offset = part.offset;
                    if let Some(old) = self
                        .entities
                        .anchored_kind(space_id, p, saved.kind, saved.anchor_offset)
                    {
                        self.entities.remove(old, UNTOLD);
                    }
                    saved.id = self.new_id();
                    saved.anchor = Some(p);
                    saved.frame = (space_id != 0).then_some(space_id);
                    saved.pos = self.space_pose(space_id).to_world(
                        p.as_dvec3() + saved.anchor_offset.as_dvec3() / f64::from(crate::world::placement::UNIT) + DVec3::splat(0.5),
                    );
                    self.entities.insert_with(c, saved, inv, comps);
                }
                (Some(p), None) => {
                    let (Some(table), Some(owner)) = (&e.loot, self.anchored(space_id, p)) else { continue };
                    let ctx = crate::loot::Ctx { tool: abi::ItemStack::EMPTY, by_player: false, on_fire: false, mined: 0, state: 0 };
                    let stacks = self.roll_loot(c, table, &ctx, p.as_dvec3() + DVec3::splat(0.5), owner);
                    self.fill_slots(owner, stacks);
                }
                (None, Some(data)) => {
                    let Some((mut saved, inv, comps, _)) = super::persist::read(c, data, self.time).into_iter().next() else { continue };
                    saved.id = self.new_id();
                    saved.pos = self.space_pose(space_id).to_world(e.at);
                    saved.anchor = None;
                    saved.frame = None;
                    self.entities.insert_with(c, saved, inv, comps);
                }
                (None, None) if e.kind != u32::MAX => {
                    self.spawn(space_id, c, e.kind, e.at);
                }
                _ => {}
            }
        }
    }

    /// Stacks into random empty slots of `owner`'s inventory, as vanilla fills a chest.
    fn fill_slots(&mut self, owner: u64, stacks: Vec<abi::ItemStack>) {
        let slots = self.entities.inventory_len(owner);
        let mut empty: Vec<u32> = (0..slots as u32)
            .filter(|&s| self.entities.inventory_slot(owner, s).is_empty())
            .collect();
        for stack in stacks {
            if empty.is_empty() {
                break;
            }
            let i = (self.random_unit() * empty.len() as f32) as usize % empty.len();
            let slot = empty.swap_remove(i);
            self.entities.set_slot(owner, slot, stack);
        }
    }

    /// What a cube's features bring, now it is in the world.
    pub fn joined(&mut self, space_id: u64, c: &'static Content, g: Generated) {
        self.bring(space_id, c, &g.extras);
        for (feature, pos, transform) in g.placed {
            let v = abi::FeaturePlaced { feature, dim: self.id, pos: pos.to_array().into(), transform: transform as u32 };
            self.emit_engine(space_id, c, abi::event::FEATURE_PLACED, &v);
        }
    }
}
