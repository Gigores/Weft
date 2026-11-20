use glam::IVec3;
use rustc_hash::FxHashSet;
use weft_abi::{self as abi, assembly_result as result};

use super::dim::Dim;
use super::signal::BODY_LIMIT;
use super::structure::{Lifted, Liveness};
use crate::content::{AIR, Content};
use crate::net::codec::Wire;
use crate::physics::Transform;
use crate::world::DIRS;

pub const BOUNDS: &str = "core:assembly_bounds";

#[derive(Clone, Copy)]
pub(super) struct Bounds {
    lo: IVec3,
    hi: IVec3,
}

impl Bounds {
    fn new(a: IVec3, b: IVec3) -> Self {
        Self { lo: a.min(b), hi: a.max(b) }
    }

    fn contains(self, p: IVec3) -> bool {
        p.cmpge(self.lo).all() && p.cmple(self.hi).all()
    }
}

struct Preview {
    controller: Option<IVec3>,
    cells: Vec<Lifted>,
    expires: u64,
}

#[derive(Clone, Copy)]
pub(super) struct Controller {
    pub block: IVec3,
    pub face: u8,
    pub helm: bool,
    pub floater: bool,
    pub slide: Option<crate::controller::Slide>,
}

impl Controller {
    pub fn at(c: &Content, dim: &Dim, space_id: u64, block: IVec3) -> Option<Self> {
        let state = &c.states[dim.get(space_id, block)? as usize];
        let signal = state.signal;
        let floater = signal.kind == abi::signal_kind::FLOATER;
        if !floater && signal.kind != abi::signal_kind::BODY_MOUNT {
            return None;
        }
        let slide = if signal.flags as u32 & abi::signal_flags::SLIDES != 0 {
            let axis = *DIRS.get(signal.front as usize)?;
            let side = state.turn.face(3) as u8;
            let length = super::structure::guide_length(c, dim.space(space_id), block, axis);
            if length == 0 {
                return None;
            }
            Some(crate::controller::Slide { side, length: f64::from(length), velocity: 0.0, parent_velocity: None })
        } else {
            None
        };
        Some(Self { block, face: signal.front, helm: signal.flags as u32 & abi::signal_flags::HELM != 0, floater, slide })
    }

    fn grip(self) -> IVec3 {
        if self.helm {
            self.block
        } else if let Some(slide) = self.slide {
            self.block + DIRS[self.face as usize] * slide.length as i32 + DIRS[slide.side as usize]
        } else {
            self.block + DIRS[self.face as usize]
        }
    }

    pub fn center(self) -> IVec3 {
        if self.floater { self.block } else { self.grip() }
    }
}

#[derive(Default)]
pub(super) struct Edit {
    dimension: u32,
    frame: u64,
    corner: Option<IVec3>,
    bounds: Option<Bounds>,
    preview: Option<Preview>,
}

impl Dim {
    pub(crate) fn set_assembly_bounds(&mut self, space_id: u64, c: &Content, at: IVec3, min: IVec3, max: IVec3) -> bool {
        if !c.dims[self.id as usize].editable || Controller::at(c, self, space_id, at).is_none() {
            return false;
        }
        let saved = self.save_assembly_bounds(space_id, c, at, Bounds::new(min, max));
        if saved {
            self.space_mut(space_id).signals.recheck_device(at);
        }
        saved
    }

    pub(super) fn assembly_bounds(&self, space_id: u64, c: &Content, p: IVec3) -> Option<Bounds> {
        let id = self.anchored(space_id, p)?;
        let data = self.entities.comps.get(&(id, c.component(BOUNDS)?))?;
        let (lo, hi) = <(IVec3, IVec3)>::from_bytes(data)?;
        Some(Bounds::new(lo + p, hi + p))
    }

    fn save_assembly_bounds(&mut self, space_id: u64, c: &Content, p: IVec3, bounds: Bounds) -> bool {
        let (Some(id), Some(comp)) = (self.anchored(space_id, p), c.component(BOUNDS)) else { return false };
        self.entities
            .comps
            .insert((id, comp), (bounds.lo - p, bounds.hi - p).to_bytes().into_boxed_slice());
        true
    }

    pub(super) fn turn_assembly_bounds(&mut self, space_id: u64, c: &Content, p: IVec3, place: Transform) {
        let Some(bounds) = self.assembly_bounds(space_id, c, p) else { return };
        let root = place.cell_at(IVec3::ZERO);
        self.save_assembly_bounds(space_id, c, p, Bounds::new(place.cell_at(bounds.lo - root), place.cell_at(bounds.hi - root)));
    }

    pub(super) fn turn_assembly_offsets(&mut self, c: &Content, entity: u64, turn: abi::Turn) {
        let Some(component) = c.component(BOUNDS) else { return };
        let Some(data) = self.entities.comps.get_mut(&(entity, component)) else { return };
        let Some((lo, hi)) = <(IVec3, IVec3)>::from_bytes(data) else { return };
        let rotate = |p: IVec3| IVec3::from_array(turn.vector(p.to_array()));
        let bounds = Bounds::new(rotate(lo), rotate(hi));
        *data = (bounds.lo, bounds.hi).to_bytes().into_boxed_slice();
    }

    pub(super) fn assembly_blocks(
        &self,
        space_id: u64,
        c: &Content,
        controller: Option<Controller>,
        bounds: Option<Bounds>,
    ) -> Result<Vec<IVec3>, u32> {
        let grip = controller.map(Controller::grip);
        let allowed = |at| controller.is_none_or(|ctrl| ctrl.helm || at != ctrl.block) && bounds.is_none_or(|b| b.contains(at));
        let connected = controller.is_some();
        let mut seen = FxHashSet::default();
        let mut pending = Vec::new();
        let mut cells = Vec::new();
        if let Some(ctrl) = controller {
            pending.push(ctrl.grip());
            if ctrl.floater {
                if self.space(space_id).signals.reserved(ctrl.block) {
                    return Err(result::UNAVAILABLE);
                }
                cells.push(ctrl.block);
            }
        } else if let Some(b) = bounds {
            let size = b.hi.as_i64vec3() - b.lo.as_i64vec3() + glam::I64Vec3::ONE;
            if size.max_element() > BODY_LIMIT as i64 || size.x * size.y * size.z > BODY_LIMIT as i64 * 16 {
                return Err(result::TOO_LARGE);
            }
            for y in b.lo.y..=b.hi.y {
                for z in b.lo.z..=b.hi.z {
                    pending.extend((b.lo.x..=b.hi.x).map(|x| IVec3::new(x, y, z)));
                }
            }
        }
        while let Some(at) = pending.pop() {
            if !allowed(at) || !seen.insert(at) {
                continue;
            }
            let state = self.get(space_id, at).ok_or(result::UNAVAILABLE)?;
            let info = &c.states[state as usize];
            if controller.is_some_and(|ctrl| ctrl.slide.is_some() && super::structure::is_guide_rail(c, state)) {
                continue;
            }
            for cover in &self.space(space_id).layout(at).covers {
                let root = cover.owner(at).0;
                if !allowed(root) {
                    return Err(result::UNAVAILABLE);
                }
                pending.push(root);
            }
            if state == AIR || info.liquid.is_some() {
                continue;
            }
            if self.space(space_id).signals.reserved(at) {
                return Err(result::UNAVAILABLE);
            }
            if connected && Some(at) != grip && !info.solid() {
                continue;
            }
            let carried_mount = info.signal.kind == abi::signal_kind::BODY_MOUNT && info.hardness >= 0.0;
            if !self.movable(c, state) && !carried_mount {
                if Some(at) == grip {
                    return Err(result::UNAVAILABLE);
                }
                continue;
            }
            cells.push(at);
            let layout = self.space(space_id).layout(at);
            for offset in layout
                .residents(state)
                .flat_map(|part| part.footprint(c).into_owned())
            {
                let extra = at + offset;
                if !allowed(extra) || self.space(space_id).signals.reserved(extra) || self.get(space_id, extra).is_none() {
                    return Err(result::UNAVAILABLE);
                }
                pending.push(extra);
                if connected {
                    pending.extend(DIRS.map(|d| extra + d));
                }
            }
            if cells.len() > BODY_LIMIT {
                return Err(result::TOO_LARGE);
            }
            if connected {
                pending.extend(DIRS.map(|d| at + d));
            }
        }
        if cells.is_empty() {
            return Err(result::UNAVAILABLE);
        }
        cells.sort_unstable_by_key(|p| p.to_array());
        cells.dedup();
        Ok(cells)
    }

    pub(super) fn assembly_lifted(
        &self,
        space_id: u64,
        c: &Content,
        controller: Option<Controller>,
        center: IVec3,
        cells: &[IVec3],
        bounds: Option<Bounds>,
    ) -> Vec<Lifted> {
        let fixed = controller
            .filter(|ctrl| !ctrl.helm && !ctrl.floater)
            .map(|ctrl| ctrl.block);
        let (riders, _) = self.who_rides(space_id, c, cells, fixed, IVec3::ZERO);
        let mut out: Vec<_> = cells
            .iter()
            .map(|&p| Lifted { from: p, local: p - center, to: p, part: None, only: None })
            .chain(
                riders
                    .into_iter()
                    .filter(|((p, _), _)| bounds.is_none_or(|b| b.contains(*p)))
                    .map(|((p, source), state)| Lifted { from: p, local: p - center, to: p, part: Some(state), only: Some(source) }),
            )
            .collect();
        out.sort_unstable_by_key(|l| (l.from.to_array(), l.part));
        out
    }

    pub(crate) fn assembly_select(&mut self, space_id: u64, c: &'static Content, player: u64, at: Option<IVec3>, flags: u8) -> u32 {
        let tool = flags & abi::assembly_flags::TOOL != 0;
        if !self.entities.get(player).is_some_and(|e| e.player) {
            return result::UNHANDLED;
        }
        if !c.dims[self.id as usize].editable {
            return result::UNAVAILABLE;
        }
        let controller = at.and_then(|p| Controller::at(c, self, space_id, p));
        if flags & abi::assembly_flags::DIRECT != 0 {
            let Some(ctrl) = controller.filter(|ctrl| space_id == 0 || !ctrl.helm) else { return result::UNAVAILABLE };
            let bounds = self.assembly_bounds(space_id, c, ctrl.block);
            return self.confirm_assembly(space_id, c, player, &mut Edit::default(), Some(ctrl), bounds, ctrl.center(), flags);
        }
        if space_id != 0
            && let Some(ctrl) = controller.filter(|ctrl| ctrl.helm)
        {
            if tool {
                if !self
                    .entities
                    .get(space_id)
                    .is_some_and(|e| e.ship.is_some())
                {
                    return result::UNAVAILABLE;
                }
                self.ask_put_down(space_id);
                return result::DOCKING;
            }
            return if self.mount_helm(c, player, space_id, ctrl.block, ctrl.face) { result::SEATED } else { result::UNAVAILABLE };
        }
        if tool
            && at.is_none()
            && space_id == 0
            && let Some(frame) = self
                .entities
                .get(player)
                .and_then(|e| e.assembly_edit.as_ref())
                .filter(|edit| edit.dimension == self.id && edit.frame != 0)
                .map(|edit| edit.frame)
        {
            return if self.active_space(frame) { self.assembly_select(frame, c, player, None, flags) } else { result::UNAVAILABLE };
        }
        if controller.is_none() && !tool && at.is_some() {
            return result::UNHANDLED;
        }
        let mut edit = self
            .entities
            .get_mut(player)
            .and_then(|e| e.assembly_edit.take())
            .filter(|d| d.dimension == self.id && d.frame == space_id)
            .unwrap_or_else(|| Box::new(Edit { dimension: self.id, frame: space_id, ..Edit::default() }));
        let answer = if let Some(ctrl) = controller {
            let bounds = edit
                .bounds
                .or_else(|| self.assembly_bounds(space_id, c, ctrl.block));
            if !ctrl.helm
                && !ctrl.floater
                && let Some(mount) = self.mount_at(space_id, ctrl.block)
            {
                if ctrl.slide.is_some() && tool {
                    self.ask_put_down(mount);
                    result::DOCKING
                } else if ctrl.slide.is_some() {
                    result::ALREADY_ASSEMBLED
                } else {
                    result::UNAVAILABLE
                }
            } else {
                self.confirm_assembly(space_id, c, player, &mut edit, Some(ctrl), bounds, ctrl.center(), 0)
            }
        } else if let Some(at) = at {
            edit.preview = None;
            if let Some(first) = edit.corner.take() {
                let bounds = Bounds::new(first, at);
                edit.bounds = Some(bounds);
                self.confirm_assembly(space_id, c, player, &mut edit, None, Some(bounds), bounds.lo, 0)
            } else {
                edit.bounds = None;
                edit.corner = Some(at);
                self.draw_assembly(space_id, player, Some(Bounds::new(at, at)), &[]);
                result::FIRST_CORNER
            }
        } else if let Some(bounds) = edit.bounds.filter(|_| tool) {
            self.confirm_assembly(space_id, c, player, &mut edit, None, Some(bounds), bounds.lo, flags)
        } else {
            self.draw_assembly(space_id, player, None, &[]);
            return result::CLEARED;
        };
        if answer != result::CONFIRMED
            && let Some(e) = self.entities.get_mut(player)
        {
            e.assembly_edit = Some(edit);
        }
        answer
    }

    fn confirm_assembly(
        &mut self,
        space_id: u64,
        c: &'static Content,
        player: u64,
        edit: &mut Edit,
        controller: Option<Controller>,
        bounds: Option<Bounds>,
        center: IVec3,
        flags: u8,
    ) -> u32 {
        let direct = flags & abi::assembly_flags::DIRECT != 0;
        let cells = match self.assembly_blocks(space_id, c, controller, bounds) {
            Ok(cells) => cells,
            Err(code) => {
                edit.preview = None;
                if !direct {
                    self.draw_assembly(space_id, player, bounds, &[]);
                }
                return code;
            }
        };
        let lifted = self.assembly_lifted(space_id, c, controller, center, &cells, bounds);
        if lifted.len() > BODY_LIMIT {
            return result::TOO_LARGE;
        }
        let at = controller.map(|ctrl| ctrl.block);
        if !direct
            && !edit
                .preview
                .as_ref()
                .is_some_and(|p| p.controller == at && p.expires > self.time && p.cells == lifted)
        {
            self.draw_assembly(space_id, player, bounds, &lifted);
            edit.preview = Some(Preview { controller: at, cells: lifted, expires: self.time + 160 });
            return if controller.is_some() { result::PREVIEW } else { result::BOUNDS };
        }
        if let Some(ctrl) = controller.filter(|ctrl| !ctrl.helm) {
            let fit = lifted
                .iter()
                .fold(Bounds::new(center, center), |b, l| Bounds::new(b.lo.min(l.from), b.hi.max(l.from)));
            if !self.save_assembly_bounds(space_id, c, ctrl.block, bounds.unwrap_or(fit)) {
                return result::UNAVAILABLE;
            }
            self.space_mut(space_id).signals.recheck_device(ctrl.block);
            if !direct {
                self.draw_assembly(space_id, player, None, &[]);
            }
            return result::CONFIRMED;
        }
        if space_id != 0 {
            return result::UNAVAILABLE;
        }
        let (id, _) = self.lift(space_id, c, c.game.platform_kind, center.as_dvec3() + Transform::FEET, lifted, &[], Liveness::Assembly);
        if !self.enable_dynamics(id, controller.map(|ctrl| ctrl.face)) {
            return result::UNAVAILABLE;
        }
        if controller.is_some() {
            self.set_helm(c, id, IVec3::ZERO);
        }
        if !direct {
            self.draw_assembly(space_id, player, None, &[]);
        }
        result::CONFIRMED
    }

    fn draw_assembly(&mut self, space_id: u64, player: u64, bounds: Option<Bounds>, cells: &[Lifted]) {
        let mut draw = |lo: IVec3, hi: IVec3, color| {
            let bounds = self
                .space_pose(space_id)
                .box_to_world(crate::physics::Aabb { min: lo.as_dvec3(), max: (hi + IVec3::ONE).as_dvec3() });
            self.gizmos.push((
                abi::Who::Player(player),
                crate::net::GizmoShape::shape(
                    crate::net::GizmoShape::ASSEMBLY,
                    abi::gizmo_kind::BOX as u8,
                    bounds.min,
                    bounds.max,
                    color,
                    if color == 0 { 0 } else { 160 },
                ),
            ))
        };
        draw(IVec3::ZERO, IVec3::ZERO, 0);
        if let Some(b) = bounds {
            draw(b.lo, b.hi, 0xFFCE5CFF);
        }
        let occupied: FxHashSet<_> = cells.iter().map(|l| l.from).collect();
        for &at in &occupied {
            if DIRS.iter().any(|d| !occupied.contains(&(at + *d))) {
                draw(at, at, 0x4ADFBECC);
            }
        }
    }
}
