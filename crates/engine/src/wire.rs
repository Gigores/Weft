//! The shape of redstone wire, the same for the server's simulation and the client's picture. A
//! wire block lies on any of a cell's six faces, a face being named by the direction of the block
//! holding it up. Nothing here knows which way is up, so a build works the same whichever way it
//! is turned.

use glam::IVec3;
use weft_abi::{block_flags as bf, signal_kind as sk};

use crate::content::{Content, StateInfo};
use crate::world::placement::{CellLayout, Part};
use crate::world::{Cube, DIRS};

/// The physical origin of a circuit part, in the same fixed point as block placement.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
pub struct Site {
    pub cell: IVec3,
    pub offset: IVec3,
    /// Unpowered state of a second circuit sharing this origin. Zero is the default grid port.
    pub port: u32,
}

impl From<IVec3> for Site {
    fn from(cell: IVec3) -> Self {
        Self { cell, ..Self::default() }
    }
}

impl std::ops::Add<IVec3> for Site {
    type Output = Self;
    fn add(self, d: IVec3) -> Self {
        Self { cell: self.cell + d, port: 0, ..self }
    }
}

impl Site {
    pub fn part(self, state: u32) -> crate::world::placement::Part {
        crate::world::placement::Part { state, offset: self.offset }
    }

    fn key(c: &Content, state: u32) -> u32 {
        let s = &c.states[state as usize].signal;
        if s.kind == sk::WIRE {
            return c.block_of(state).first_state;
        }
        let level = if s.levels == 2 { u32::from(s.level > 0) } else { u32::from(s.level) };
        state - level * s.stride
    }

    pub fn of(c: &Content, cell: IVec3, part: Part, main: u32, layout: &CellLayout) -> Self {
        let mut site = Self { cell, offset: part.offset, port: 0 };
        if !layout.extras.is_empty() && site.state(c, main, layout) != part.state {
            site.port = Self::key(c, part.state);
        }
        site
    }

    pub fn state(self, c: &Content, main: u32, layout: &CellLayout) -> u32 {
        if layout.extras.is_empty() && layout.offsets.is_empty() {
            return if self.offset == IVec3::ZERO && self.port == 0 { main } else { crate::content::AIR };
        }
        if self.port != 0 {
            let first = Self { port: 0, ..self }.state(c, main, layout);
            if Self::key(c, first) == self.port {
                return crate::content::AIR;
            }
        }
        let mut found = crate::content::AIR;
        for part in layout.residents(main).filter(|v| v.offset == self.offset) {
            if self.port != 0 {
                if Self::key(c, part.state) == self.port {
                    return part.state;
                }
            } else {
                if c.states[part.state as usize].signal.kind != sk::NONE {
                    return part.state;
                }
                if found == crate::content::AIR {
                    found = part.state;
                }
            }
        }
        found
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Contact {
    pub at: Site,
    /// From the near shape to the far shape.
    pub dir: u8,
    pub near_faces: u8,
    pub far_faces: u8,
}

pub(crate) fn wire_faces(c: &Content, part: crate::world::placement::Part, b: crate::physics::Aabb) -> u8 {
    if c.states[part.state as usize].signal.kind != sk::WIRE {
        return 0;
    }
    let axis = (b.max - b.min).min_position();
    let high = (b.min[axis] + b.max[axis]) * 0.5 - part.shift()[axis] > 0.5;
    (1 << (axis * 2 + high as usize)) & mask(c, part.state)
}

pub fn needs_contacts<'a>(site: Site, get: impl Fn(IVec3) -> Option<(u32, &'a CellLayout)>) -> bool {
    site.offset != IVec3::ZERO
        || site.port != 0
        || DIRS
            .into_iter()
            .chain(std::iter::once(IVec3::ZERO))
            .any(|dir| get(site.cell + dir).is_some_and(|(_, l)| !l.offsets.is_empty() || !l.covers.is_empty() || !l.extras.is_empty()))
}

/// Contacts between different placement origins. Aligned circuits retain their authored
/// face ports; a fractional placement meets another one at their actual surfaces.
pub fn contacts<'a>(
    c: &Content,
    site: Site,
    state: u32,
    get: impl Fn(IVec3) -> Option<(u32, &'a crate::world::placement::CellLayout)>,
) -> Vec<Contact> {
    let mut out = Vec::new();
    let part = site.part(state);
    for patch in part.bounds(c) {
        let near_faces = wire_faces(c, part, patch);
        for dir in 0..6u8 {
            let a = dir as usize / 2;
            let plane = if dir % 2 == 0 { patch.min[a] } else { patch.max[a] };
            crate::world::placement::face_contacts(c, site.cell, patch, dir as usize, plane, &get, |cell, other, bounds| {
                let (cell, resident) = other.owner(cell);
                let Some((main, layout)) = get(cell) else { return false };
                let at = Site::of(c, cell, resident, main, layout);
                if at == site || (at == site + DIRS[dir as usize] && site.port == 0) {
                    return false;
                }
                let contact = Contact { at, dir, near_faces, far_faces: wire_faces(c, other, bounds) };
                if !out.contains(&contact) {
                    out.push(contact);
                }
                false
            });
        }
    }
    out
}

/// A direction's name as `facing` properties have it.
pub fn dir_name(d: u8) -> &'static str {
    weft_abi::FACE_NAMES[d as usize % 6]
}

/// The faces a wire state lies on.
pub fn mask(c: &Content, state: u32) -> u8 {
    c.states[state as usize].sides
}

/// Whether wire can't pass through a block on its way past an edge.
pub fn in_the_way(c: &Content, state: u32) -> bool {
    let s = &c.states[state as usize];
    s.solid() || !matches!(s.signal.kind, sk::NONE | sk::WIRE)
}

/// Whether a block carries power from what feeds it to what touches it. A sink (a lamp) does
/// while it takes that power itself.
pub fn conducts(c: &Content, state: u32) -> bool {
    let s = &c.states[state as usize];
    s.solid() && s.flags & bf::SIGNAL_BLOCKER == 0 && matches!(s.signal.kind, sk::NONE | sk::SINK)
}

/// Whether a kind of signal block is a device, something other than wire with a circuit.
pub fn is_device(kind: u32) -> bool {
    !matches!(kind, sk::NONE | sk::WIRE)
}

/// Whether going from one state to the other leaves wire around it as it is: the same block turned
/// the same way (a gate going on or off, water falling a level). Only wire itself changes.
pub fn same_shape(a: &StateInfo, b: &StateInfo) -> bool {
    a.block == b.block && a.signal.kind != sk::WIRE && a.turn == b.turn
}

/// `same_shape` of a device: it keeps its circuit, changing its level.
pub fn same_device(a: &StateInfo, b: &StateInfo) -> bool {
    is_device(a.signal.kind) && is_device(b.signal.kind) && same_shape(a, b)
}

/// The directions in a face mask.
pub fn dirs(mask: u8) -> impl Iterator<Item = u8> {
    (0..6u8).filter(move |d| mask & (1 << d) != 0)
}

/// Whether a cube has wire.
pub fn has_wire(c: &Content, cube: &Cube) -> bool {
    let wire = |s: u32| c.states[s as usize].signal.kind == sk::WIRE;
    cube.palette().iter().copied().any(wire)
        || cube
            .all_extras()
            .any(|(_, states)| states.iter().copied().any(wire))
}

/// Wire origins in this cube; foreign coverage is indexed by its owner.
pub fn cells(c: &Content, pos: IVec3, cube: &Cube) -> Vec<Site> {
    if !has_wire(c, cube) {
        return Vec::new();
    }
    (0..4096)
        .flat_map(|i| {
            let cell = pos * 16 + crate::world::cube::offset(i);
            cube.layout(i).residents(cube.get(i)).filter_map(move |p| {
                (c.states[p.state as usize].signal.kind == sk::WIRE).then(|| Site::of(c, cell, p, cube.get(i), cube.layout(i)))
            })
        })
        .collect()
}

/// The four directions along a face, which are its edges.
pub fn edges(face: u8) -> impl Iterator<Item = u8> {
    (0..6u8).filter(move |d| d / 2 != face / 2)
}

/// What lies past an edge of a piece of wire.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Link {
    /// Another piece of the same wire: `(cell, face)`.
    Piece(Site, u8),
    /// No wire goes on from here.
    End,
    /// Past it isn't loaded.
    Unknown,
}

/// Follows edge `e` of the piece on `face` of `cell`: into the corner on the wall at that edge,
/// on along the same plane, or around the edge of the block it lies on, in that order.
pub fn link(c: &Content, get: &impl Fn(Site) -> Option<u32>, cell: Site, face: u8, e: u8) -> Link {
    let Some(here) = get(cell) else { return Link::Unknown };
    let block = c.states[here as usize].block;
    if mask(c, here) & (1 << e) != 0 {
        return Link::Piece(cell, e);
    }
    let same = |s: u32, f: u8| c.states[s as usize].block == block && mask(c, s) & (1 << f) != 0;
    let next = cell + DIRS[e as usize];
    let Some(ahead) = get(next) else { return Link::Unknown };
    if same(ahead, face) {
        return Link::Piece(next, face);
    }
    if in_the_way(c, ahead) {
        return Link::End;
    }
    let around = next + DIRS[face as usize];
    let Some(below) = get(around) else { return Link::Unknown };
    if same(below, e ^ 1) {
        return Link::Piece(around, e ^ 1);
    }
    Link::End
}

/// Whether the piece on `face` of a cell touches what is next to that cell in direction `e`: not
/// from the far side of the cell.
pub fn touches(face: u8, e: u8) -> bool {
    e != face ^ 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_are_along_the_face() {
        for f in 0..6u8 {
            let e: Vec<u8> = edges(f).collect();
            assert_eq!(e.len(), 4);
            assert!(!e.contains(&f) && !e.contains(&(f ^ 1)));
        }
    }

    #[test]
    fn a_piece_reaches_all_but_the_far_side() {
        assert!(touches(2, 2), "what holds it up");
        assert!(touches(2, 0), "at an edge");
        assert!(!touches(2, 3), "the cell's far side");
    }
}
