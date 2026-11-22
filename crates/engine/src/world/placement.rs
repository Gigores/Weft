use std::borrow::Cow;

use glam::{DVec3, IVec3};

use crate::content::{AIR, Content};
use crate::physics::{Aabb, Cylinder, Solid};

/// Local positions use fixed point so an attachment stays on the same plane after a save,
/// a network round trip or a quarter turn. World cell coordinates keep their full range.
pub const UNIT: i32 = 65536;

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Part {
    pub state: u32,
    /// From the corner of the cell being queried to the origin of the placed shape.
    pub offset: IVec3,
}

impl Part {
    pub fn aligned(state: u32) -> Self {
        Self { state, offset: IVec3::ZERO }
    }

    pub fn shift(self) -> DVec3 {
        self.offset.as_dvec3() / f64::from(UNIT)
    }

    pub fn owner(self, cell: IVec3) -> (IVec3, Self) {
        let delta = self.offset.div_euclid(IVec3::splat(UNIT));
        (cell + delta, self.in_cell(delta))
    }

    pub fn turned(self, c: &Content, turn: weft_abi::Turn) -> Self {
        if turn == weft_abi::Turn::NONE {
            return self;
        }
        Self { state: c.turned_state(self.state, turn), offset: turn.vector(self.offset.to_array()).into() }
    }

    pub fn in_cell(self, delta: IVec3) -> Self {
        Self { offset: self.offset - delta * UNIT, ..self }
    }

    pub fn footprint(self, c: &Content) -> Cow<'_, [IVec3]> {
        let s = &c.states[self.state as usize];
        if self.offset == IVec3::ZERO { Cow::Borrowed(&s.cells) } else { Cow::Owned(covered_cells(&s.selection, s.cylinder, self.offset)) }
    }

    fn shapes(self, c: &Content) -> impl Iterator<Item = Solid> + '_ {
        let s = &c.states[self.state as usize];
        shapes(&s.selection, s.cylinder, self.shift())
    }

    pub fn bounds(self, c: &Content) -> impl Iterator<Item = Aabb> + '_ {
        self.shapes(c).map(Solid::bounds)
    }

    pub fn intersects(self, c: &Content, area: Aabb) -> bool {
        self.shapes(c).any(|shape| shape.overlaps(Solid::Box(area)))
    }

    pub fn overlaps(self, other: Self, c: &Content) -> bool {
        self.shapes(c)
            .any(|a| other.shapes(c).any(|b| a.overlaps(b)))
    }
}

fn shapes(boxes: &[[f32; 6]], cylinder: Option<Cylinder>, shift: DVec3) -> impl Iterator<Item = Solid> + '_ {
    boxes
        .iter()
        .filter(move |_| cylinder.is_none())
        .map(move |b| {
            Solid::Box(Aabb {
                min: DVec3::new(b[0].into(), b[1].into(), b[2].into()) + shift,
                max: DVec3::new(b[3].into(), b[4].into(), b[5].into()) + shift,
            })
        })
        .chain(cylinder.map(|s| Solid::Cylinder(Cylinder { center: s.center + shift, ..s })))
}

/// Index the union of the individual boxes. A face on an integer plane belongs to the
/// cell on its inside. Empty gaps between separate boxes have no entries.
pub fn covered_cells(boxes: &[[f32; 6]], cylinder: Option<Cylinder>, offset: IVec3) -> Vec<IVec3> {
    let mut cells = Vec::new();
    for shape in shapes(boxes, cylinder, offset.as_dvec3() / f64::from(UNIT)) {
        let b = shape.bounds();
        let lo = b.min.floor().as_ivec3();
        let hi = b.max.ceil().as_ivec3();
        for y in lo.y..hi.y {
            for z in lo.z..hi.z {
                for x in lo.x..hi.x {
                    let cell = IVec3::new(x, y, z);
                    if cell != IVec3::ZERO
                        && (cylinder.is_none()
                            || shape.overlaps(Solid::Box(Aabb { min: cell.as_dvec3(), max: cell.as_dvec3() + DVec3::ONE })))
                    {
                        cells.push(cell);
                    }
                }
            }
        }
    }
    cells.sort_unstable_by_key(|p| p.to_array());
    cells.dedup();
    cells
}

/// Visit the shapes touching a patch from across its plane. Both axes along the patch
/// must overlap. A shared corner alone cannot support a block or transmit a signal.
pub fn face_contacts<'a>(
    c: &Content,
    cell: IVec3,
    patch: Aabb,
    face: usize,
    plane: f64,
    get: &impl Fn(IVec3) -> Option<(u32, &'a CellLayout)>,
    mut visit: impl FnMut(IVec3, Part, Aabb) -> bool,
) -> Option<bool> {
    let axis = face / 2;
    let positive = face % 2 == 1;
    let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
    let mut delta = IVec3::ZERO;
    delta[axis] = (plane + if positive { 1e-5 } else { -1e-5 }).floor() as i32;
    let mut unknown = false;
    for a in patch.min[u].floor() as i32..patch.max[u].ceil() as i32 {
        delta[u] = a;
        for b in patch.min[v].floor() as i32..patch.max[v].ceil() as i32 {
            delta[v] = b;
            let at = cell + delta;
            let Some((main, layout)) = get(at) else {
                unknown = true;
                continue;
            };
            for other in layout.parts(main) {
                for bounds in other.bounds(c) {
                    let edge = if positive { bounds.min[axis] } else { bounds.max[axis] };
                    if (edge + f64::from(delta[axis]) - plane).abs() <= 1.0 / f64::from(UNIT)
                        && [u, v].iter().all(|&i| {
                            bounds.min[i] + f64::from(delta[i]) < patch.max[i] - 1e-6
                                && bounds.max[i] + f64::from(delta[i]) > patch.min[i] + 1e-6
                        })
                        && visit(at, other, bounds)
                    {
                        return Some(true);
                    }
                }
            }
        }
    }
    (!unknown).then_some(false)
}

pub fn supported<'a>(
    c: &Content,
    cell: IVec3,
    part: Part,
    face: usize,
    get: impl Fn(IVec3) -> Option<(u32, &'a CellLayout)>,
    accepts: impl Fn(u32) -> bool,
) -> Option<bool> {
    let edge = |b: Aabb| if face % 2 == 1 { b.max[face / 2] } else { b.min[face / 2] };
    let plane = part
        .bounds(c)
        .map(edge)
        .reduce(if face % 2 == 1 { f64::max } else { f64::min })?;
    let mut unknown = false;
    for patch in part.bounds(c).filter(|&b| (edge(b) - plane).abs() < 1e-5) {
        let found =
            face_contacts(c, cell, patch, face, plane, &get, |at, other, _| other.owner(at) != (cell, part) && accepts(other.state));
        if found == Some(true) {
            return found;
        }
        unknown |= found.is_none();
    }
    (!unknown).then_some(false)
}

/// Sparse supplement to the palette word. Aligned single blocks need no entry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CellLayout {
    pub extras: Vec<u32>,
    /// Offsets of the palette block followed by its extras. Trailing zeroes are omitted.
    pub offsets: Vec<IVec3>,
    /// Shapes rooted in other cells, in this cell's coordinates.
    pub covers: Vec<Part>,
}

pub static EMPTY_LAYOUT: CellLayout = CellLayout::EMPTY;

impl CellLayout {
    pub const EMPTY: Self = Self { extras: Vec::new(), offsets: Vec::new(), covers: Vec::new() };

    /// Light is still stored per voxel. Only the faces actually sealed in this voxel
    /// stop light, even when the opaque material belongs to a block in another voxel.
    pub fn light(&self, c: &Content, main: u32) -> crate::content::Tight {
        if *self == Self::EMPTY {
            return c.tight[main as usize];
        }
        let mut light = c.tight[AIR as usize];
        let mut opaque = false;
        for part in self.parts(main) {
            let t = c.tight[part.state as usize];
            light.light = std::array::from_fn(|i| light.light[i].max(t.light[i]));
            opaque |= t.dark();
            if !t.dark() {
                light.opacity = light.opacity.max(t.opacity);
                light.absorb = std::array::from_fn(|i| light.absorb[i].max(t.absorb[i]));
            }
        }
        light.seals = self.covered_faces(c, main, false);
        if opaque && light.seals == 63 {
            light.opacity = 15;
            light.absorb = [15; 3];
        }
        light
    }

    /// Complete voxel faces covered by the union of the positioned collision boxes.
    /// Liquids can also have explicit containment faces, including an empty override.
    pub fn covered_faces(&self, c: &Content, main: u32, liquid: bool) -> u8 {
        let mut faces = [[0u16; 16]; 6];
        for part in self.parts(main) {
            let s = &c.states[part.state as usize];
            if !liquid && s.layer != weft_abi::RenderLayer::Opaque && !c.tight[part.state as usize].dark() {
                continue;
            }
            let override_walls = liquid && s.liquid_walls != u8::MAX;
            let boxes: &[[f32; 6]] = if override_walls { &[[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]] } else { &s.collision };
            let sides = if override_walls { s.liquid_walls } else { 63 };
            let shift = part.shift();
            for b in boxes {
                let lo = shift + DVec3::new(b[0].into(), b[1].into(), b[2].into());
                let hi = shift + DVec3::new(b[3].into(), b[4].into(), b[5].into());
                for (face, mask) in faces.iter_mut().enumerate() {
                    let axis = face / 2;
                    let plane = (face % 2) as f64;
                    if sides & (1 << face) == 0 || lo[axis] > plane + 1e-6 || hi[axis] < plane - 1e-6 || hi[axis] <= 0.0 || lo[axis] >= 1.0
                    {
                        continue;
                    }
                    let span =
                        |a: usize| ((lo[a] * 16.0).round().clamp(0.0, 16.0) as usize, (hi[a] * 16.0).round().clamp(0.0, 16.0) as usize);
                    let ((u0, u1), (v0, v1)) = (span((axis + 1) % 3), span((axis + 2) % 3));
                    let bits = ((1u32 << v1) - (1u32 << v0)) as u16;
                    for row in &mut mask[u0..u1] {
                        *row |= bits;
                    }
                }
            }
        }
        faces
            .iter()
            .enumerate()
            .fold(0, |m, (f, rows)| m | (u8::from(rows.iter().all(|&r| r == u16::MAX)) << f))
    }

    pub fn map_states(&mut self, map: impl Fn(u32) -> u32) {
        for state in &mut self.extras {
            *state = map(*state);
        }
        for part in &mut self.covers {
            part.state = map(part.state);
        }
    }

    pub fn residents(&self, main: u32) -> impl Iterator<Item = Part> + '_ {
        std::iter::once(main)
            .chain(self.extras.iter().copied())
            .enumerate()
            .filter_map(|(i, state)| (state != AIR).then(|| Part { state, offset: self.offsets.get(i).copied().unwrap_or_default() }))
    }

    pub fn parts(&self, main: u32) -> impl Iterator<Item = Part> + '_ {
        self.residents(main).chain(self.covers.iter().copied())
    }

    pub fn set_residents(&mut self, parts: &[Part]) -> u32 {
        self.extras = parts.iter().skip(1).map(|p| p.state).collect();
        let count = parts
            .iter()
            .rposition(|p| p.offset != IVec3::ZERO)
            .map_or(0, |i| i + 1);
        self.offsets = parts[..count].iter().map(|p| p.offset).collect();
        parts.first().map_or(AIR, |p| p.state)
    }
}
