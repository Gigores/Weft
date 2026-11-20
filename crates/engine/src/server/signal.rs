//! Redstone. The engine runs it like liquids, blocks only say what they are (`abi::SignalDesc`).
//!
//! A connected wire is one network, its power worked out from what feeds it when that changes, so
//! a wire costs nothing per block and a circuit nothing while it isn't changing. Gates act after a
//! delay, from a timing wheel. A tick reads everything as the last one left it and then applies
//! what changed, so no order of going through blocks shows: a build behaves the same however it is
//! turned. Every loop goes through a gate, so a tick always settles. See `wire.rs` for the shape.
//!
//! Power reaches a block two ways. A gate or lever into it powers it strongly: wire touching it
//! takes that power, and so do devices. Wire on it powers it weakly, for devices only, or all
//! wires on one wall would be one wire. What is cut off by unloaded blocks keeps its state.

use std::collections::VecDeque;
use std::sync::Arc;

use glam::{DVec3, IVec3};
use rustc_hash::{FxHashMap, FxHashSet};
use weft_abi as abi;
use weft_abi::block_flags as bf;
use weft_abi::signal_kind as sk;

use super::dim::Dim;
use super::entity::{Completion, Entities, Motion};
use super::structure::{Carried, Lifted, Liveness};
use crate::content::{AIR, Content, Signal};
use crate::physics::Aabb;
use crate::wire::{self, Link, Site};
use crate::world::placement::Part;

type Placed = (IVec3, Part);
use crate::world::{Cube, DIRS, cube_of, local_index};

const NO_NET: u32 = u32::MAX;
/// Ticks ahead the wheel reaches. A gate's delay is below it.
const WHEEL: usize = 32;
const MAX_DELAY: u8 = (WHEEL - 1) as u8;
/// Vanilla's: this many flips in this many ticks burn a torch out, for this long.
const BURN_FLIPS: usize = 8;
const BURN_WINDOW: u64 = 60;
const BURN_TIME: u64 = 160;
const PUSH_LIMIT: usize = 12;
const FLOATER_IDLE: u8 = 0;
const FLOATER_GOING: u8 = 1;
const FLOATER_STUCK: u8 = 2;

#[derive(Default)]
pub(super) struct FloaterDrive {
    pub velocity: DVec3,
    pub axes: glam::BVec3,
    pub breaks: bool,
    pub hold: bool,
}
/// More than that stuck to a floater and it doesn't go.
pub(super) const BODY_LIMIT: usize = 4096;
/// Vanilla's: a piston's blocks take two ticks to get there.
const MOVE_TICKS: u8 = 2;

#[derive(Clone, Copy)]
struct WireCell {
    mask: u8,
    level: [u8; 6],
    net: [u32; 6],
}

#[derive(Clone, Default)]
struct Device {
    state: u32,
    /// The level it is going to, and the tick it gets there: the wheel may also hold an entry
    /// for what stood here before.
    pending: Option<(u8, u64)>,
    /// Power into a sink as it was last told.
    input: u8,
    /// An observer saw something, the tick it changed: its delay counts from then.
    triggered: Option<u64>,
    /// When a piston last started pushing something out.
    pushed_at: u64,
    flips: VecDeque<u64>,
    burnt_until: u64,
    stopped_mount: bool,
}

#[derive(Clone, Copy)]
enum Port {
    /// A device next to the piece, its direction toward the piece.
    Direct(Site, u8),
    /// A block the piece touches that a device may power strongly.
    Strong(Site),
    /// A cell with something of another space in it, its direction toward the piece.
    Outside(Site, u8, Option<u8>),
}

/// What a cell of one space gives another, per direction out of it: to what reads power, and to
/// wire. A grid contact can also join two coplanar wire faces with normal attenuation.
#[derive(Clone, Copy, PartialEq, Default)]
pub struct Gives {
    all: [u8; 6],
    to_wire: [u8; 6],
    wire_face: [u8; 6],
}

impl Gives {
    pub(super) fn only(self, face: usize) -> Self {
        let mut limited = Self::default();
        limited.all[face] = self.all[face];
        limited.to_wire[face] = self.to_wire[face];
        limited
    }

    fn turned(self, turn: abi::Turn) -> Self {
        if turn == abi::Turn::NONE {
            return self;
        }
        let mut out = Self::default();
        for from in 0..6 {
            let to = turn.face(from);
            out.all[to] = self.all[from];
            out.to_wire[to] = self.to_wire[from];
            out.wire_face[to] = self.wire_face[from];
        }
        out
    }
}

/// A device or wire's state on its way with its block to another place, there or in another space.
#[derive(Clone, Default)]
pub struct Moved {
    device: Option<Device>,
    levels: [u8; 6],
}

impl Moved {
    pub(super) fn is_empty(&self) -> bool {
        self.device.is_none() && self.levels == [0; 6]
    }

    pub(super) fn turned(mut self, turn: abi::Turn) -> Self {
        let old = self.levels;
        for (face, level) in old.into_iter().enumerate() {
            self.levels[turn.face(face)] = level;
        }
        self
    }
}

#[derive(Default)]
struct Net {
    pieces: Vec<(Site, u8)>,
    /// `adj[start[i]..start[i + 1]]` are the pieces next to piece `i`.
    start: Vec<u32>,
    adj: Vec<u32>,
    ports: Vec<(u32, Port)>,
    level: Vec<u8>,
    next: Vec<u8>,
    /// Pieces with power.
    lit: Vec<u32>,
    /// Part of it may lie in what isn't loaded.
    cut: bool,
}

/// One tick's outcome of a sink or piston, applied once everything has been read.
enum Effect {
    Sink {
        p: Site,
        level: u8,
    },
    Piston {
        p: Site,
        extend: bool,
    },
    /// Mods run with the rest of the world in place: what came in at each of its own faces.
    Gate {
        p: Site,
        inputs: [u8; 6],
    },
    /// A comparator reading a block that has its own idea of it, and what is at its sides.
    Compare {
        p: Site,
        behind: Site,
        side: u8,
    },
    /// What power a floater has.
    Floater {
        p: Site,
        level: u8,
    },
}

#[derive(Default)]
pub struct Signals {
    wires: FxHashMap<Site, WireCell>,
    devices: FxHashMap<Site, Device>,
    outputs: FxHashMap<Site, [u8; 6]>,
    propulsion: FxHashSet<Site>,
    nets: Vec<Option<Net>>,
    free: Vec<u32>,
    /// Block changes not yet looked at: (where, old, new).
    events: Vec<(Site, u32, u32)>,
    topology: FxHashSet<Site>,
    /// Only origins whose fractional placement touches a different grid.
    contacts: FxHashMap<Site, Vec<wire::Contact>>,

    stale: FxHashSet<u32>,
    dirty_nets: FxHashSet<u32>,
    dirty_devices: FxHashSet<Site>,
    wheel: [Vec<Site>; WHEEL],
    /// Torches burnt out, looked at until they recover, and pistons that gave way to another,
    /// which may stop wanting the block without anything changing here.
    polled: FxHashSet<Site>,
    /// Comparators reading a container: looked at when an inventory changes.
    readers: FxHashSet<Site>,
    /// Devices that couldn't act or be sure of what they read, for a cube loading or a piston's
    /// blocks landing to give them another go.
    held: FxHashSet<Site>,
    /// Wire and devices per cube, so a cube without any goes quickly.
    per_cube: FxHashMap<IVec3, u32>,
    /// Networks cut off, by the cubes they wait for.
    cut_by_cube: FxHashMap<IVec3, Vec<u32>>,
    /// Observers by the cell they watch.
    watchers: FxHashMap<Site, Vec<Site>>,
    buckets: [Vec<u32>; 16],
    /// Wire power that changed this tick, for clients: (cell, face, level).
    pub level_changes: Vec<(Site, u8, u8)>,
    /// Stands in during a tick, keeping what mods change.
    in_tick: bool,
    /// Pistons at work: their blocks are on their way as a body.
    moving: Vec<Moving>,
    /// Where those blocks are going, which nothing else may take.
    reserved: FxHashSet<IVec3>,
    /// What comes with blocks about to be here.
    pub(super) arriving: FxHashMap<Site, Moved>,
    /// Now, for pulses that came along.
    pub(super) now: u64,
    /// Blocks on a piston: circuits go on, but pistons wait to see where they end up.
    pub(super) on_the_way: bool,
    /// What other spaces give at cells of this one: by the space, then the cell, per direction out
    /// of it. A body's in the world, and the world's on a body.
    outside: FxHashMap<u64, FxHashMap<IVec3, Gives>>,
    /// Some output here changed since the other side last looked.
    pub(super) outputs_changed: bool,
    /// Where a piston just put an observer: being moved sets it off, which is what makes a
    /// flying machine go.
    pub(super) moved_observers: FxHashSet<Site>,
}

/// A piston's blocks on their way.
struct Moving {
    body: u64,
    piston: Site,
    head: IVec3,
    blocks: Vec<Carried>,
    /// It goes back: it is out until the head is in.
    retracting: bool,
    /// The head in the body and what it turns into on the way: the short one while it is inside
    /// the piston, or its rod would stick out of the back.
    head_turns: Option<(IVec3, u32)>,
}

/// The blocks signals look at. What isn't there yet holds things as they are, in the world; a
/// body's blocks are all there, and what it has no cube for is air.
#[derive(Clone, Copy)]
pub struct View<'a> {
    pub cubes: &'a FxHashMap<IVec3, Arc<Cube>>,
    pub on_body: bool,
}

impl View<'_> {
    fn cell(&self, p: IVec3) -> Option<(u32, &crate::world::placement::CellLayout)> {
        match self.cubes.get(&cube_of(p)) {
            Some(cube) => Some((cube.get(local_index(p)), cube.layout(local_index(p)))),
            None => self
                .on_body
                .then_some((AIR, &crate::world::placement::EMPTY_LAYOUT)),
        }
    }

    fn state(&self, p: Site) -> Option<u32> {
        let (main, layout) = self.cell(p.cell)?;
        Some(p.state(crate::content::content(), main, layout))
    }

    fn has(&self, cube: IVec3) -> bool {
        self.on_body || self.cubes.contains_key(&cube)
    }
}

/// What a device gives what is next to it in direction `dir`, wire and devices.
fn direct(s: &Signal, dir: u8) -> u8 {
    if s.gives & (1 << dir) != 0 { s.level } else { 0 }
}

/// What a device gives a block in direction `dir`, strongly: its power goes on through that block.
fn can_give(s: &Signal) -> bool {
    s.gives != 0
}

/// Where a comparator reads what is behind it: opposite its output.
fn rear(s: &Signal) -> u8 {
    s.gives.trailing_zeros() as u8 ^ 1
}

use wire::is_device;

/// What a comparator with `mode` makes of what is behind it and at its sides.
fn compared(flags: u8, rear: u8, side: u8) -> u8 {
    if flags as u32 & abi::signal_flags::SUBTRACT != 0 {
        rear.saturating_sub(side)
    } else if rear >= side {
        rear
    } else {
        0
    }
}

/// What a block riding on a moving one does on coming to a cell with `there` in it.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum RiderMeets {
    Nothing,
    /// That breaks, as under the moving block itself.
    Breaks,
    /// It comes off, which is a way to stop what it was feeding.
    Wall,
}

pub(super) fn rider_meets(c: &Content, there: u32) -> RiderMeets {
    let s = &c.states[there as usize];
    if there == AIR || s.liquid.is_some() || s.flags & bf::REPLACEABLE != 0 || s.signal.kind == sk::WIRE {
        RiderMeets::Nothing
    } else if s.solid() {
        RiderMeets::Wall
    } else {
        RiderMeets::Breaks
    }
}

/// The state of a block with its `power` property at `level`.
pub fn with_level(s: &Signal, state: u32, level: u8) -> u32 {
    if s.levels == 0 {
        return state;
    }
    let index = |l: u8| if s.levels == 2 { (l > 0) as u32 } else { l.min(s.levels - 1) as u32 };
    state - index(s.level) * s.stride + index(level) * s.stride
}

impl Signals {
    pub(super) fn set_outputs(&mut self, p: impl Into<Site>, levels: [u8; 6]) {
        let p = p.into();
        let previous = if levels == [0; 6] { self.outputs.remove(&p) } else { self.outputs.insert(p, levels) };
        if previous.unwrap_or_default() != levels {
            self.output_changed(p);
        }
    }

    fn output_at(&self, p: Site, dir: u8, s: &Signal, mask: u8) -> u8 {
        if mask & (1 << dir) == 0 {
            0
        } else {
            self.outputs
                .get(&p)
                .map_or(s.level, |levels| levels[dir as usize])
        }
    }

    fn device_signal<'c>(&self, c: &'c Content, p: Site) -> Option<&'c Signal> {
        self.devices
            .get(&p)
            .map(|d| &c.states[d.state as usize].signal)
    }

    /// Power a block `b` was given strongly.
    fn neighbors(&self, p: Site, dir: u8) -> impl Iterator<Item = Site> + '_ {
        std::iter::once(p + DIRS[dir as usize]).chain(
            self.contacts
                .get(&p)
                .into_iter()
                .flatten()
                .filter(move |v| v.dir == dir)
                .map(|v| v.at),
        )
    }

    fn strong_in(&self, c: &Content, b: Site) -> u8 {
        let mut most = 0;
        for d in 0..6u8 {
            for at in self.neighbors(b, d) {
                if let Some(s) = self.device_signal(c, at) {
                    most = most.max(self.output_at(at, d ^ 1, s, s.strong));
                }
            }
        }
        most
    }

    /// Power of the wire lying on block `b`; `None`: part of it can't be told.
    fn weak_in(&self, b: Site) -> Option<u8> {
        let mut most = 0;
        for d in 0..6u8 {
            for at in self.neighbors(b, d) {
                let Some(w) = self.wires.get(&at) else { continue };
                let f = d ^ 1;
                if w.mask & (1 << f) != 0 {
                    if self.net_cut(w.net[f as usize]) {
                        return None;
                    }
                    most = most.max(w.level[f as usize]);
                }
            }
        }
        Some(most)
    }

    fn net_cut(&self, id: u32) -> bool {
        id != NO_NET
            && self
                .nets
                .get(id as usize)
                .is_some_and(|n| n.as_ref().is_some_and(|n| n.cut))
    }

    /// The most other spaces give at `n` toward `dir`, `which` picking what reads it.
    fn outside_gives(&self, n: Site, dir: u8, which: fn(&Gives) -> &[u8; 6]) -> u8 {
        self.outside
            .values()
            .filter_map(|m| m.get(&n.cell))
            .map(|g| which(g)[dir as usize])
            .max()
            .unwrap_or(0)
    }

    /// Who gives anything here from outside.
    pub(super) fn outside_from(&self) -> Vec<u64> {
        self.outside.keys().copied().collect()
    }

    /// The power a device was last found to have coming in.
    pub(super) fn level_into(&self, p: impl Into<Site>) -> Option<u8> {
        let p = p.into();
        self.devices
            .get(&p)
            .map(|d| d.input)
            .filter(|i| *i != u8::MAX)
    }

    pub(super) fn recheck_device(&mut self, p: impl Into<Site>) {
        let p = p.into();
        if let Some(d) = self.devices.get_mut(&p) {
            d.stopped_mount = false;
            self.dirty_devices.insert(p);
        }
    }

    pub(super) fn stop_mount(&mut self, p: impl Into<Site>) {
        let p = p.into();
        if let Some(d) = self.devices.get_mut(&p) {
            d.stopped_mount = true;
        }
    }

    /// Power coming into `p` from direction `dir`; `None`: that can't be told, it is cut off.
    fn read(&self, c: &Content, cubes: View, p: Site, dir: u8) -> Option<u8> {
        let from_outside = self.outside_gives(p + DIRS[dir as usize], dir ^ 1, |g| &g.all);
        self.read_here(c, cubes, p, dir)
            .map(|v| v.max(from_outside))
    }

    fn read_here(&self, c: &Content, cubes: View, p: Site, dir: u8) -> Option<u8> {
        self.neighbors(p, dir)
            .try_fold(0, |most, n| Some(most.max(self.from_neighbor(c, cubes, n, dir)?)))
    }

    fn from_neighbor(&self, c: &Content, cubes: View, n: Site, dir: u8) -> Option<u8> {
        let toward = dir ^ 1;
        if let Some(w) = self.wires.get(&n) {
            let mut most = 0;
            for f in 0..6u8 {
                if w.mask & (1 << f) != 0 && wire::touches(f, toward) {
                    if self.net_cut(w.net[f as usize]) {
                        return None;
                    }
                    most = most.max(w.level[f as usize]);
                }
            }
            return Some(most);
        }
        let given = self
            .device_signal(c, n)
            .map_or(0, |s| self.output_at(n, toward, s, s.gives));
        let state = cubes.state(n)?;
        if wire::conducts(c, state) {
            // What feeds a block may be on any side of it.
            if DIRS.iter().any(|d| cubes.state(n + *d).is_none()) {
                return None;
            }
            return Some(given.max(self.strong_in(c, n)).max(self.weak_in(n)?));
        }
        Some(given)
    }

    fn mark_cell_nets(&mut self, p: Site) {
        if let Some(w) = self.wires.get(&p) {
            for f in 0..6 {
                if w.mask & (1 << f) != 0 && w.net[f] != NO_NET {
                    self.dirty_nets.insert(w.net[f]);
                }
            }
        }
    }

    /// What `cell` gives in each direction out of it, for another space.
    pub(super) fn gives(&self, c: &Content, cubes: View, cell: impl Into<Site>) -> Gives {
        let cell = cell.into();
        let mut out = Gives::default();
        if let Some(w) = self.wires.get(&cell) {
            for f in 0..6u8 {
                if w.mask & (1 << f) != 0 {
                    for d in (0..6u8).filter(|d| wire::touches(f, *d)) {
                        out.all[d as usize] = out.all[d as usize].max(w.level[f as usize]);
                    }
                    out.wire_face[f as usize] = w.level[f as usize].saturating_sub(1);
                }
            }
            return out;
        }
        if let Some(s) = self.device_signal(c, cell) {
            for d in 0..6u8 {
                out.all[d as usize] = self.output_at(cell, d, s, s.gives);
                out.to_wire[d as usize] = out.all[d as usize];
            }
        }
        if cubes.state(cell).is_some_and(|s| wire::conducts(c, s)) {
            let strong = self.strong_in(c, cell);
            let all = strong.max(self.weak_in(cell).unwrap_or(0));
            for d in 0..6 {
                out.to_wire[d] = out.to_wire[d].max(strong);
                out.all[d] = out.all[d].max(all);
            }
        }
        out
    }

    /// Everything of this space's signals: wire and devices.
    pub(super) fn cells(&self) -> impl Iterator<Item = IVec3> + '_ {
        self.wires.keys().chain(self.devices.keys()).map(|p| p.cell)
    }

    pub(super) fn has_cells(&self) -> bool {
        !self.wires.is_empty() || !self.devices.is_empty()
    }

    /// What this space gives other spaces, where it is `offset` from them.
    pub(super) fn giving(&self, c: &Content, cubes: View, project: impl Fn(IVec3) -> IVec3, turn: abi::Turn) -> FxHashMap<IVec3, Gives> {
        let mut out = FxHashMap::default();
        let mut look = |cell: IVec3| {
            let g = self.gives(c, cubes, cell);
            if g != Gives::default() {
                out.insert(project(cell), g.turned(turn));
            }
        };
        for cell in self.cells() {
            look(cell);
            for d in DIRS {
                look(cell + d);
            }
        }
        out
    }

    /// What this space gives at `cells`, with `offset` taking them from there to here; the
    /// result is by those.
    pub(super) fn giving_at(
        &self,
        c: &Content,
        cubes: View,
        cells: impl Iterator<Item = IVec3>,
        project: impl Fn(IVec3) -> IVec3,
        turn: abi::Turn,
    ) -> FxHashMap<IVec3, Gives> {
        let mut out = FxHashMap::default();
        for cell in cells {
            let g = self.gives(c, cubes, project(cell));
            if g != Gives::default() {
                out.insert(cell, g.turned(turn));
            }
        }
        out
    }

    /// The word from space `from` on what it gives at cells here now. Only what differs wakes
    /// anything; a cell starting or stopping to give may join or leave a network.
    pub(super) fn set_outside(&mut self, from: u64, now: FxHashMap<IVec3, Gives>) {
        let news = self.outputs_changed;
        let was = self.outside.remove(&from).unwrap_or_default();
        for (cell, g) in &now {
            match was.get(cell) {
                None => {
                    self.topology.insert((*cell).into());
                    self.output_changed((*cell).into());
                }
                Some(old) if old != g => self.output_changed((*cell).into()),
                _ => {}
            }
        }
        for cell in was.keys().filter(|cell| !now.contains_key(cell)) {
            self.topology.insert((*cell).into());
            self.output_changed((*cell).into());
        }
        // Not news for the other side, which said it.
        self.outputs_changed = news;
        if !now.is_empty() {
            self.outside.insert(from, now);
        }
    }

    /// Something of another space came to `cell`, or left it.
    pub(super) fn passing(&mut self, cell: IVec3) {
        self.seen(cell.into(), self.now);
    }

    /// What is at `cell` changed at tick `when`, for observers watching it.
    fn seen(&mut self, cell: Site, when: u64) {
        if let Some(list) = self.watchers.get(&cell) {
            for o in list.clone() {
                if let Some(d) = self.devices.get_mut(&o) {
                    d.triggered.get_or_insert(when);
                    self.dirty_devices.insert(o);
                }
            }
        }
    }

    /// Observers here watching cells that are not of these blocks, which the world may show
    /// something else at now.
    pub(super) fn watched(&self) -> Vec<IVec3> {
        self.watchers.keys().map(|p| p.cell).collect()
    }

    /// What comparators read of the block at `p` may be different now.
    pub fn read_changed(&mut self, c: &Content, p: impl Into<Site>) {
        let p = p.into();
        if self.readers.is_empty() {
            return;
        }
        let behind = |s: &Self, r: Site| {
            s.device_signal(c, r)
                .and_then(|g| DIRS.get(rear(g) as usize))
                .map(|d| r + *d)
        };
        let those: Vec<Site> = self
            .readers
            .iter()
            .copied()
            .filter(|r| behind(self, *r) == Some(p))
            .collect();
        self.dirty_devices.extend(those);
    }

    /// The combined drive in body coordinates. `None` until its redstone inputs are known.
    pub(super) fn floater_drive(&self, c: &Content) -> Option<FloaterDrive> {
        let mut drive = FloaterDrive::default();
        for p in &self.propulsion {
            let d = &self.devices[p];
            let s = &c.states[d.state as usize].signal;
            if s.kind == sk::FLOATER {
                if d.input == u8::MAX {
                    return None;
                }
                let axis = DIRS[s.front as usize].as_dvec3();
                drive.axes |= axis.cmpne(DVec3::ZERO);
                drive.velocity += axis * d.input as f64 * super::structure::SPEED_PER_LEVEL;
                drive.breaks |= s.flags as u32 & abi::signal_flags::BREAKS != 0;
                drive.hold |= s.flags as u32 & abi::signal_flags::HOLD != 0;
            }
        }
        drive.velocity = drive
            .velocity
            .clamp(DVec3::splat(-super::structure::MOST_SPEED), DVec3::splat(super::structure::MOST_SPEED));
        Some(drive)
    }

    pub(super) fn thrusters<'a>(&'a self, c: &'a Content) -> impl Iterator<Item = (IVec3, DVec3)> + 'a {
        self.propulsion.iter().filter_map(move |&pos| {
            let dev = &self.devices[&pos];
            if !(1..=15).contains(&dev.input) {
                return None;
            }
            let force = c.block_value::<f32>(dev.state, Some(c.game.thrust_comp))?;
            if !force.is_finite() || force <= 0.0 {
                return None;
            }
            let dir = DIRS.get(c.states[dev.state as usize].signal.front as usize)?;
            Some((pos.cell, dir.as_dvec3() * (f64::from(force) * f64::from(dev.input) / 15.0)))
        })
    }

    /// Its devices that can't be moved just now: an anchor with power.
    pub(super) fn fixed<'a>(&'a self, c: &'a Content) -> impl Iterator<Item = IVec3> + 'a {
        self.devices
            .iter()
            .filter(|(_, d)| c.states[d.state as usize].flags & bf::IMMOVABLE != 0)
            .map(|(p, _)| p.cell)
    }

    /// What goes with the block at `p` if it moves.
    pub(super) fn carry_out(&self, p: impl Into<Site>) -> Moved {
        let p = p.into();
        Moved { device: self.devices.get(&p).cloned(), levels: self.wires.get(&p).map_or([0; 6], |w| w.level) }
    }

    /// A device's output changed: whatever could be reading it looks again.
    fn contacts_changed(&mut self, p: Site) {
        if self.contacts.is_empty() {
            return;
        }
        {
            let mut nearby: Vec<_> = self
                .contacts
                .get(&p)
                .into_iter()
                .flatten()
                .map(|v| v.at)
                .collect();
            for d in DIRS {
                nearby.extend(
                    self.contacts
                        .get(&(p + d))
                        .into_iter()
                        .flatten()
                        .map(|v| v.at),
                );
            }
            for n in nearby {
                self.mark_cell_nets(n);
                if self.devices.contains_key(&n) {
                    self.dirty_devices.insert(n);
                }
                for d in DIRS {
                    self.mark_cell_nets(n + d);
                    if self.devices.contains_key(&(n + d)) {
                        self.dirty_devices.insert(n + d);
                    }
                }
            }
        }
    }

    fn output_changed(&mut self, p: Site) {
        self.outputs_changed = true;
        self.contacts_changed(p);
        for d in DIRS {
            let n = p + d;
            if self.devices.contains_key(&n) {
                self.dirty_devices.insert(n);
            }
            self.mark_cell_nets(n);
            // On through a block, to anything touching that.
            for e in DIRS {
                let m = n + e;
                if m != p {
                    if self.devices.contains_key(&m) {
                        self.dirty_devices.insert(m);
                    }
                    self.mark_cell_nets(m);
                }
            }
        }
    }

    /// A piece's power changed: the devices it touches and those around the block it lies on.
    fn piece_changed(&mut self, cell: Site, face: u8) {
        self.outputs_changed = true;
        self.contacts_changed(cell);
        for e in 0..6u8 {
            if wire::touches(face, e) {
                let n = cell + DIRS[e as usize];
                if self.devices.contains_key(&n) {
                    self.dirty_devices.insert(n);
                }
            }
        }
        let on = cell + DIRS[face as usize];
        for d in DIRS {
            if self.devices.contains_key(&(on + d)) {
                self.dirty_devices.insert(on + d);
            }
        }
    }

    fn add_device(&mut self, c: &Content, p: Site, state: u32) {
        let s = &c.states[state as usize].signal;
        self.index_propulsion(c, p, state);
        for d in wire::dirs(s.watches) {
            self.watchers
                .entry(p + DIRS[d as usize])
                .or_default()
                .push(p);
        }
        let triggered = self.moved_observers.remove(&p).then_some(self.now);
        let device = match self.arriving.remove(&p).and_then(|m| m.device) {
            Some(mut d) => {
                d.state = state;
                d.triggered = d.triggered.or(triggered);
                // On its way, and that goes on here.
                if let Some((_, due)) = d.pending.as_mut() {
                    *due = (*due).max(self.now + 1);
                    self.wheel[*due as usize % WHEEL].push(p);
                }
                d
            }
            None => Device { state, input: u8::MAX, triggered, ..Device::default() },
        };
        if self.devices.insert(p, device).is_none() {
            *self.per_cube.entry(p.cell >> 4).or_default() += 1;
        }
        self.dirty_devices.insert(p);
    }

    fn remove_device(&mut self, c: &Content, p: Site) {
        self.set_outputs(p, [0; 6]);
        if let Some(d) = self.devices.remove(&p) {
            self.count_out(p);
            let s = &c.states[d.state as usize].signal;
            for d in wire::dirs(s.watches) {
                let at = p + DIRS[d as usize];
                if let Some(list) = self.watchers.get_mut(&at) {
                    list.retain(|o| *o != p);
                    if list.is_empty() {
                        self.watchers.remove(&at);
                    }
                }
            }
        }
        self.polled.remove(&p);
        self.readers.remove(&p);
        self.propulsion.remove(&p);
        self.held.remove(&p);
        self.dirty_devices.remove(&p);
    }

    fn index_propulsion(&mut self, c: &Content, p: Site, state: u32) {
        if c.states[state as usize].signal.kind == sk::FLOATER
            || c.block_value::<f32>(state, Some(c.game.thrust_comp))
                .is_some_and(|force| force.is_finite() && force > 0.0)
        {
            self.propulsion.insert(p);
        } else {
            self.propulsion.remove(&p);
        }
    }

    fn count_out(&mut self, p: Site) {
        if let Some(n) = self.per_cube.get_mut(&(p.cell >> 4)) {
            *n -= 1;
            if *n == 0 {
                self.per_cube.remove(&(p.cell >> 4));
            }
        }
    }

    fn set_wire(&mut self, p: Site, mask: u8) {
        if mask == 0 {
            if let Some(w) = self.wires.remove(&p) {
                self.count_out(p);
                for f in 0..6 {
                    if w.net[f] != NO_NET {
                        self.stale.insert(w.net[f]);
                    }
                    // Or a client keeps it, for when wire is back in that cell.
                    if w.level[f] > 0 {
                        self.level_changes.push((p, f as u8, 0));
                    }
                }
            }
            return;
        }
        if !self.wires.contains_key(&p) {
            *self.per_cube.entry(p.cell >> 4).or_default() += 1;
        }
        // What came with it shows at once, and clients here hear of it.
        let came = if self.wires.contains_key(&p) { None } else { self.arriving.remove(&p) };
        if let Some(m) = &came {
            for f in wire::dirs(mask).filter(|f| m.levels[*f as usize] > 0) {
                self.level_changes.push((p, f, m.levels[f as usize]));
            }
        }
        let w = self
            .wires
            .entry(p)
            .or_insert(WireCell { mask: 0, level: came.map_or([0; 6], |m| m.levels), net: [NO_NET; 6] });
        let gone = w.mask & !mask;
        for f in 0..6 {
            if gone & (1 << f) != 0 {
                if w.net[f] != NO_NET {
                    self.stale.insert(w.net[f]);
                }
                w.net[f] = NO_NET;
                if w.level[f] > 0 {
                    self.level_changes.push((p, f as u8, 0));
                }
                w.level[f] = 0;
            }
        }
        w.mask = mask;
    }

    /// A block changed. Looked at when the tick comes.
    pub fn block_changed(&mut self, c: &Content, p: Site, old: u32, new: u32) {
        let relevant = c.states[old as usize].signal.kind != sk::NONE || c.states[new as usize].signal.kind != sk::NONE;
        if relevant || self.in_tick || !self.wires.is_empty() || !self.devices.is_empty() {
            self.events.push((p, old, new));
        }
    }

    fn apply_events(&mut self, c: &Content) {
        for (p, old, new) in std::mem::take(&mut self.events) {
            self.seen(p, self.now);
            let (a, b) = (&c.states[old as usize], &c.states[new as usize]);
            // It may be what a piston couldn't move.
            if a.block != b.block || a.flags != b.flags {
                self.give_another_go();
            }
            self.read_changed(c, p);
            let (ka, kb) = (a.signal.kind, b.signal.kind);
            if ka == sk::WIRE || kb == sk::WIRE {
                self.set_wire(p, wire::mask(c, new));
                self.topology.insert(p);
                if ka == kb {
                    continue;
                }
            }
            if wire::same_device(a, b) {
                if let Some(d) = self.devices.get_mut(&p) {
                    let was = c.states[d.state as usize].signal.level;
                    d.state = new;
                    self.index_propulsion(c, p, new);
                    if was != b.signal.level {
                        self.output_changed(p);
                    }
                    self.dirty_devices.insert(p);
                    continue;
                }
            }
            if is_device(ka) {
                self.remove_device(c, p);
            }
            if is_device(kb) {
                self.add_device(c, p, new);
            }
            if is_device(ka) || is_device(kb) {
                self.topology.insert(p);
                self.output_changed(p);
            } else if wire::conducts(c, old) != wire::conducts(c, new)
                || wire::in_the_way(c, old) != wire::in_the_way(c, new)
                || a.selection != b.selection
            {
                self.topology.insert(p);
            }
        }
    }

    pub fn cube_loaded(&mut self, c: &Content, pos: IVec3, cube: &Cube) {
        // Blocks here may be what something next door was waiting to see.
        self.give_another_go();
        self.stale
            .extend(self.cut_by_cube.remove(&pos).into_iter().flatten());
        let circuit = |s: u32| c.states[s as usize].signal.kind != sk::NONE;
        if !cube.palette().iter().copied().any(circuit)
            && !cube
                .all_extras()
                .any(|(_, parts)| parts.iter().copied().any(circuit))
        {
            return;
        }
        for i in 0..4096 {
            for part in cube
                .layout(i)
                .residents(cube.get(i))
                .filter(|v| circuit(v.state))
            {
                let p = Site::of(c, pos * 16 + crate::world::cube::offset(i), part, cube.get(i), cube.layout(i));
                if c.states[part.state as usize].signal.kind == sk::WIRE {
                    self.set_wire(p, wire::mask(c, part.state));
                } else {
                    self.add_device(c, p, part.state);
                }
                self.topology.insert(p);
            }
        }
    }

    fn give_another_go(&mut self) {
        self.dirty_devices.extend(self.held.drain());
    }

    pub fn cube_unloaded(&mut self, c: &Content, pos: IVec3) {
        // The events are for blocks that are gone now, or come again with the cube.
        self.events.retain(|e| e.0.cell >> 4 != pos);
        if !self.per_cube.contains_key(&pos) {
            return;
        }
        let cells: Vec<Site> = self
            .wires
            .keys()
            .filter(|p| p.cell >> 4 == pos)
            .copied()
            .collect();
        for p in cells {
            self.set_wire(p, 0);
            self.topology.insert(p);
        }
        let gone: Vec<Site> = self
            .devices
            .keys()
            .filter(|p| p.cell >> 4 == pos)
            .copied()
            .collect();
        for p in gone {
            self.remove_device(c, p);
            self.topology.insert(p);
        }
    }

    fn contact_changed(&mut self, site: Site) {
        if let Some(wire) = self.wires.get(&site) {
            self.stale
                .extend(wire.net.into_iter().filter(|&id| id != NO_NET));
        }
        if self.devices.contains_key(&site) {
            self.dirty_devices.insert(site);
        }
    }

    fn refresh_contacts(&mut self, c: &Content, cubes: View, site: Site) {
        if let Some(old) = self.contacts.remove(&site) {
            for contact in old {
                if let Some(back) = self.contacts.get_mut(&contact.at) {
                    back.retain(|v| v.at != site);
                    if back.is_empty() {
                        self.contacts.remove(&contact.at);
                    }
                }
                self.contact_changed(contact.at);
            }
        }
        if let Some(state) = cubes.state(site).filter(|&s| s != AIR) {
            let found = wire::contacts(c, site, state, |p| cubes.cell(p));
            for v in &found {
                self.contacts.entry(v.at).or_default().push(wire::Contact {
                    at: site,
                    dir: v.dir ^ 1,
                    near_faces: v.far_faces,
                    far_faces: v.near_faces,
                });
                self.contact_changed(v.at);
            }
            if !found.is_empty() {
                self.contacts.insert(site, found);
            }
        }
    }

    /// Networks near what changed are worked out again from their pieces.
    fn rebuild(&mut self, c: &Content, cubes: View) {
        if self.topology.is_empty() && self.stale.is_empty() {
            return;
        }
        let mut again: Vec<(Site, u8)> = Vec::new();
        let changed = std::mem::take(&mut self.topology);
        for &cell in &changed {
            // Most cubes have no fractional placements. Inspect geometry only near a changed
            // fractional origin, an existing contact, or an indexed placement in a neighbor.
            let nearby = self.contacts.contains_key(&cell) || wire::needs_contacts(cell, |p| cubes.cell(p));
            if nearby {
                self.refresh_contacts(c, cubes, cell);
            }
        }
        for cell in changed {
            // A device's power reaches wire two blocks away, through the block between.
            for dx in -2i32..=2 {
                for dy in -2i32..=2 {
                    for dz in -2i32..=2 {
                        if dx.abs() + dy.abs() + dz.abs() > 2 {
                            continue;
                        }
                        let q = cell + IVec3::new(dx, dy, dz);
                        if let Some(w) = self.wires.get(&q) {
                            for f in 0..6u8 {
                                if w.mask & (1 << f) != 0 {
                                    if w.net[f as usize] == NO_NET {
                                        again.push((q, f));
                                    } else {
                                        self.stale.insert(w.net[f as usize]);
                                    }
                                }
                            }
                        }
                        if self.devices.contains_key(&q) {
                            self.dirty_devices.insert(q);
                        }
                    }
                }
            }
        }
        for id in std::mem::take(&mut self.stale) {
            let Some(net) = self.nets.get_mut(id as usize).and_then(|n| n.take()) else { continue };
            self.free.push(id);
            self.dirty_nets.remove(&id);
            for (cell, f) in net.pieces {
                if let Some(w) = self.wires.get_mut(&cell) {
                    if w.mask & (1 << f) != 0 {
                        w.net[f as usize] = NO_NET;
                        again.push((cell, f));
                    }
                }
            }
        }
        for (cell, f) in again {
            if self
                .wires
                .get(&cell)
                .is_some_and(|w| w.net[f as usize] == NO_NET && w.mask & (1 << f) != 0)
            {
                self.flood(c, cubes, cell, f);
            }
        }
    }

    fn flood(&mut self, c: &Content, cubes: View, cell: Site, face: u8) {
        let id = self.free.pop().unwrap_or_else(|| {
            self.nets.push(None);
            self.nets.len() as u32 - 1
        });
        let get = |p: Site| cubes.state(p);
        let mut net = Net::default();
        let mut index: FxHashMap<(Site, u8), u32> = FxHashMap::default();
        index.insert((cell, face), 0);
        net.pieces.push((cell, face));
        let mut links: Vec<Vec<u32>> = Vec::new();
        let mut at = 0;
        while at < net.pieces.len() {
            let (p, f) = net.pieces[at];
            at += 1;
            let mut mine = Vec::with_capacity(4);
            for e in wire::edges(f) {
                match wire::link(c, &get, p, f, e) {
                    Link::Piece(q, g) => {
                        let next = net.pieces.len() as u32;
                        let i = *index.entry((q, g)).or_insert_with(|| {
                            net.pieces.push((q, g));
                            next
                        });
                        mine.push(i);
                    }
                    Link::Unknown => net.cut = true,
                    Link::End => {}
                }
            }
            for contact in self
                .contacts
                .get(&p)
                .into_iter()
                .flatten()
                .filter(|v| v.near_faces & (1 << f) != 0)
            {
                for g in wire::dirs(contact.far_faces) {
                    if !wire::touches(f, contact.dir) || !wire::touches(g, contact.dir ^ 1) {
                        continue;
                    }
                    let next = net.pieces.len() as u32;
                    let j = *index.entry((contact.at, g)).or_insert_with(|| {
                        net.pieces.push((contact.at, g));
                        next
                    });
                    if !mine.contains(&j) {
                        mine.push(j);
                    }
                }
            }
            links.push(mine);
        }
        net.start.reserve(links.len() + 1);
        net.start.push(0);
        for l in &links {
            net.adj.extend_from_slice(l);
            net.start.push(net.adj.len() as u32);
        }
        let n = net.pieces.len();
        net.level = vec![0; n];
        net.next = vec![0; n];
        for (i, &(p, f)) in net.pieces.iter().enumerate() {
            // Blocks written without telling this (a structure being built) show up by events.
            let Some(w) = self.wires.get_mut(&p) else { continue };
            w.net[f as usize] = id;
            let level = w.level[f as usize];
            // What it shows until worked out again.
            net.level[i] = level;
            if level > 0 {
                net.lit.push(i as u32);
            }
            for e in 0..6u8 {
                if !wire::touches(f, e) {
                    continue;
                }
                let at = p + DIRS[e as usize];
                if self.outside.values().any(|m| m.contains_key(&at.cell)) {
                    let wire_face = (e != f && cubes.state(at) == Some(AIR)).then_some(f);
                    net.ports
                        .push((i as u32, Port::Outside(at, e ^ 1, wire_face)));
                }
                for at in self.neighbors(p, e) {
                    if self.device_signal(c, at).is_some_and(can_give) {
                        net.ports.push((i as u32, Port::Direct(at, e ^ 1)));
                    } else if !self.wires.contains_key(&at) {
                        match cubes.state(at) {
                            Some(state) if wire::conducts(c, state) => {
                                if DIRS.iter().any(|d| cubes.state(at + *d).is_none()) {
                                    net.cut = true;
                                }
                                if (0..6).any(|dir| {
                                    self.neighbors(at, dir)
                                        .any(|n| self.device_signal(c, n).is_some_and(can_give))
                                }) {
                                    net.ports.push((i as u32, Port::Strong(at)));
                                }
                            }
                            Some(_) => {}
                            None => net.cut = true,
                        }
                    }
                }
            }
        }
        if net.cut {
            // What it waits for: any cube around a piece that isn't there.
            let mut waits: FxHashSet<IVec3> = FxHashSet::default();
            for &(p, _) in &net.pieces {
                for dx in -2..=2 {
                    for dy in -2..=2 {
                        for dz in -2..=2 {
                            let q = p + IVec3::new(dx, dy, dz);
                            if !cubes.has(q.cell >> 4) {
                                waits.insert(q.cell >> 4);
                            }
                        }
                    }
                }
            }
            for cube in waits {
                self.cut_by_cube.entry(cube).or_default().push(id);
            }
        }
        self.nets[id as usize] = Some(net);
        self.dirty_nets.insert(id);
    }

    /// Works out a network's power from what feeds it: every piece has the most any feed leaves
    /// it after the distance, so the order of going through them doesn't show.
    fn recompute(&mut self, c: &Content, id: u32) {
        // Part of what feeds a cut one can't be seen: as it is until it can.
        if self.net_cut(id) {
            return;
        }
        let Some(mut net) = self.nets.get_mut(id as usize).and_then(|n| n.take()) else { return };
        let mut buckets = std::mem::take(&mut self.buckets);
        for &(i, port) in &net.ports {
            let v = match port {
                Port::Direct(at, dir) => self
                    .device_signal(c, at)
                    .map_or(0, |s| self.output_at(at, dir, s, s.gives)),
                Port::Strong(at) => self.strong_in(c, at),
                Port::Outside(at, dir, face) => {
                    let direct = self.outside_gives(at, dir, |g| &g.to_wire);
                    direct.max(face.map_or(0, |f| self.outside_gives(at, f, |g| &g.wire_face)))
                }
            };
            if v > net.next[i as usize] {
                net.next[i as usize] = v;
                buckets[v.min(15) as usize].push(i);
            }
        }
        let mut lit: Vec<u32> = Vec::new();
        for level in (1..=15u8).rev() {
            while let Some(i) = buckets[level as usize].pop() {
                let iu = i as usize;
                if net.next[iu] != level {
                    continue;
                }
                lit.push(i);
                if level > 1 {
                    for k in net.start[iu]..net.start[iu + 1] {
                        let j = net.adj[k as usize] as usize;
                        if net.next[j] < level - 1 {
                            net.next[j] = level - 1;
                            buckets[(level - 1) as usize].push(j as u32);
                        }
                    }
                }
            }
        }
        self.buckets = buckets;
        // Only the pieces that had power and that have it now, however long the wire.
        let old = std::mem::replace(&mut net.lit, lit);
        let changed: Vec<u32> = old
            .iter()
            .copied()
            .filter(|&i| net.next[i as usize] == 0)
            .chain(
                net.lit
                    .iter()
                    .copied()
                    .filter(|&i| net.next[i as usize] != net.level[i as usize]),
            )
            .collect();
        for i in changed {
            let iu = i as usize;
            let (cell, face) = net.pieces[iu];
            let level = net.next[iu];
            net.level[iu] = level;
            if let Some(w) = self.wires.get_mut(&cell) {
                w.level[face as usize] = level;
            }
            self.level_changes.push((cell, face, level));
            self.piece_changed(cell, face);
            // Wire's power is part of what it is, as an observer sees it.
            self.seen(cell.into(), self.now);
        }
        for &i in &net.lit {
            net.next[i as usize] = 0;
        }
        self.nets[id as usize] = Some(net);
    }

    fn schedule(&mut self, p: Site, level: u8, delay: u8, now: u64) {
        let Some(d) = self.devices.get_mut(&p) else { return };
        if d.pending.is_none() {
            let due = now + delay.clamp(1, MAX_DELAY) as u64;
            d.pending = Some((level, due));
            self.wheel[due as usize % WHEEL].push(p);
        }
    }

    fn fullness(&self, c: &Content, entities: &Entities, frame: u64, at: Site) -> Option<u8> {
        let id = entities.anchored(frame, at.cell)?;
        let items = entities.inventories.get(&id)?;
        if items.is_empty() {
            return Some(0);
        }
        let mut sum = 0.0f32;
        let mut any = false;
        for s in items.iter().filter(|s| !s.is_empty()) {
            any = true;
            sum += s.count as f32 / c.max_stack(s) as f32;
        }
        Some(if any { 1 + (sum / items.len() as f32 * 14.0) as u8 } else { 0 })
    }

    /// What a device should do, from what is around it now.
    fn evaluate(&mut self, c: &'static Content, d: &Dim, space_id: u64, p: Site, now: u64, effects: &mut Vec<Effect>) {
        let Some(dev) = self.devices.get(&p) else { return };
        let state = dev.state;
        let s = c.states[state as usize].signal;
        let view = d.signal_view(space_id);
        // What can't be told keeps things as they are.
        macro_rules! read {
            ($dir:expr) => {
                match self.read(c, view, p, $dir) {
                    Some(v) => v,
                    None => {
                        self.held.insert(p);
                        return;
                    }
                }
            };
        }
        // The most of what comes in at these faces.
        macro_rules! most {
            ($mask:expr) => {{
                let mut most = 0;
                for dir in wire::dirs($mask) {
                    most = most.max(read!(dir));
                }
                most
            }};
        }
        match s.kind {
            sk::TORCH => {
                let input = most!(s.inputs);
                let burnt = self.devices[&p].burnt_until > now;
                if burnt {
                    self.polled.insert(p);
                } else if self.devices[&p].burnt_until != 0 {
                    self.polled.remove(&p);
                    if let Some(dev) = self.devices.get_mut(&p) {
                        dev.burnt_until = 0;
                        dev.flips.clear();
                    }
                }
                let want = if input == 0 && !burnt { 15 } else { 0 };
                if want != s.level {
                    self.schedule(p, want, s.delay, now);
                }
            }
            sk::REPEATER => {
                // A gate pointing into a lock holds it where it is.
                if wire::dirs(s.locks).any(|side| view.state(p + DIRS[side as usize]).is_none()) {
                    self.held.insert(p);
                    return;
                }
                let locked = wire::dirs(s.locks).any(|side| {
                    self.device_signal(c, p + DIRS[side as usize])
                        .is_some_and(|g| matches!(g.kind, sk::REPEATER | sk::COMPARATOR) && direct(g, side ^ 1) > 0)
                });
                if !locked {
                    let want = if most!(s.inputs) > 0 { 15 } else { 0 };
                    if want != s.level {
                        self.schedule(p, want, s.delay, now);
                    }
                }
            }
            sk::COMPARATOR => {
                let behind = p + DIRS[rear(&s) as usize];
                let reads_itself = d
                    .get(space_id, behind.cell)
                    .is_some_and(|b| c.block_of(b).cb.signal_read.is_some());
                let container = self.fullness(c, &d.entities, space_id, behind);
                if reads_itself || container.is_some() {
                    self.readers.insert(p);
                } else {
                    self.readers.remove(&p);
                }
                let side = most!(s.inputs & !(1 << rear(&s)));
                if reads_itself {
                    effects.push(Effect::Compare { p, behind, side });
                    return;
                }
                let rear = match container {
                    Some(v) => v,
                    None => read!(rear(&s)),
                };
                let want = compared(s.flags, rear, side);
                if want != s.level {
                    self.schedule(p, want, s.delay, now);
                }
            }
            sk::OBSERVER => {
                let triggered = std::mem::take(&mut self.devices.get_mut(&p).expect("just read").triggered);
                if s.level > 0 {
                    self.schedule(p, 0, s.delay, now);
                } else if let Some(when) = triggered {
                    let due = (when + s.delay as u64).max(now + 1);
                    self.schedule(p, 15, (due - now) as u8, now);
                }
            }
            sk::GATE => {
                // By its own faces, as its model is written.
                let back = c.states[state as usize].turn.inverse();
                let mut inputs = [0u8; 6];
                for dir in wire::dirs(s.inputs) {
                    inputs[back.face(dir as usize)] = read!(dir);
                }
                effects.push(Effect::Gate { p, inputs });
            }
            sk::SINK => {
                let mut most = most!(s.inputs);
                // A big block takes power at any of its cells.
                for &o in p.part(state).footprint(c).iter() {
                    for dir in 0..6u8 {
                        match self.read(c, view, p + o, dir) {
                            Some(v) => most = most.max(v),
                            None => {
                                self.held.insert(p);
                                return;
                            }
                        }
                    }
                }
                if self.devices[&p].input != most {
                    effects.push(Effect::Sink { p, level: most });
                }
            }
            sk::FLOATER | sk::BODY_MOUNT => {
                self.polled.remove(&p);
                effects.push(Effect::Floater { p, level: most!(s.inputs) });
            }
            sk::PISTON => {
                self.polled.remove(&p);
                let most = most!(s.inputs);
                if (most > 0) != (s.level > 0) {
                    effects.push(Effect::Piston { p, extend: most > 0 });
                }
            }
            _ => {}
        }
    }

    /// Blocks are on their way to `p`.
    pub fn reserved(&self, p: IVec3) -> bool {
        self.reserved.contains(&p)
    }

    fn idle(&self, now: u64) -> bool {
        self.events.is_empty()
            && self.topology.is_empty()
            && self.stale.is_empty()
            && self.dirty_nets.is_empty()
            && self.dirty_devices.is_empty()
            && self.polled.is_empty()
            && self.wheel[now as usize % WHEEL].is_empty()
    }

    /// The strongest power coming into a cell from around it, for mods.
    pub fn power_at(&self, c: &Content, cubes: View, p: impl Into<Site>) -> u8 {
        let p = p.into();
        let mut most = self
            .wires
            .get(&p)
            .map_or(0, |w| w.level.iter().copied().max().unwrap_or(0));
        for dir in 0..6u8 {
            most = most.max(self.read(c, cubes, p, dir).unwrap_or(0));
        }
        most
    }

    /// Wire power in a cube, for a client getting it: (cell, face, level).
    pub fn levels_in(&self, c: &Content, pos: IVec3, cube: &Cube) -> Vec<(Site, u8, u8)> {
        let mut out = Vec::new();
        for p in wire::cells(c, pos, cube) {
            if let Some(w) = self.wires.get(&p) {
                out.extend(
                    (0..6u8)
                        .filter(|f| w.level[*f as usize] > 0)
                        .map(|f| (p, f, w.level[f as usize])),
                );
            }
        }
        out
    }
}

/// A piston's move, worked out before any of them happens.
struct PushPlan {
    p: IVec3,
    offset: IVec3,
    port: u32,
    extend: bool,
    dir: u8,
    /// The blocks that move, one step along `step`.
    moved: Vec<Placed>,
    step: IVec3,
    /// What is in the way and gets broken.
    broken: Vec<Placed>,
    /// What stands on those and comes along: where, and what of it (a cell of wire may have
    /// pieces on what stays).
    riding: Vec<(Placed, u32)>,
}

impl Dim {
    pub fn power_at(&self, c: &Content, id: u64, at: IVec3) -> u8 {
        let space = self.space(id);
        let view = self.signal_view(id);
        if let Some((p, part)) = space.acting.filter(|(p, _)| *p == at) {
            return space
                .signals
                .power_at(c, view, Site::of(c, p, part, space.get(p).unwrap_or(AIR), space.layout(p)));
        }
        space
            .layout(at)
            .parts(space.get(at).unwrap_or(0))
            .map(|part| {
                let (cell, part) = part.owner(at);
                space
                    .signals
                    .power_at(c, view, Site::of(c, cell, part, space.get(cell).unwrap_or(AIR), space.layout(cell)))
            })
            .max()
            .unwrap_or_else(|| space.signals.power_at(c, view, at))
    }

    pub fn signal_view(&self, space_id: u64) -> View<'_> {
        View { cubes: &self.space(space_id).cubes, on_body: space_id != 0 }
    }

    /// A change of a gate's look that nothing around it needs to hear: no lighting, no neighbors.
    fn set_signal_state(&mut self, space_id: u64, c: &'static Content, p: Site, old: u32, new: u32) {
        if p.offset != IVec3::ZERO
            || *self.space(space_id).layout(p.cell) != crate::world::placement::CellLayout::EMPTY
            || !c.states[old as usize].cells.is_empty()
            || c.states[old as usize].light != c.states[new as usize].light
        {
            self.replace_placed(space_id, c, p.cell, p.part(old), new);
            return;
        }
        let p = p.cell;
        let cp = cube_of(p);
        let Some(cube) = self.space_mut(space_id).cubes.get_mut(&cp) else { return };
        Arc::make_mut(cube).set(local_index(p), new);
        self.space_mut(space_id).changes.push((p, new));
        self.space_mut(space_id).modified.insert(cp);
        self.space_mut(space_id).edited.insert(cp);
    }

    pub fn signal_tick(&mut self, space_id: u64, c: &'static Content) {
        let now = self.time;
        self.space_mut(space_id).signals.now = now;
        if !self.space(space_id).signals.readers.is_empty() && !self.entities.dirty_inv.is_empty() {
            let frame = space_id;
            let changed: Vec<IVec3> = self
                .entities
                .dirty_inv
                .iter()
                .filter_map(|id| {
                    self.entities
                        .get(*id)
                        .filter(|e| e.block_frame() == frame)
                        .and_then(|e| e.anchor)
                })
                .collect();
            for p in changed {
                self.space_mut(space_id).signals.read_changed(c, p);
            }
        }
        if self.space(space_id).signals.idle(now) {
            return;
        }
        let mut s = std::mem::take(&mut self.space_mut(space_id).signals);
        self.space_mut(space_id).signals.in_tick = true;
        s.apply_events(c);
        s.rebuild(c, self.signal_view(space_id));
        // What was on its way comes out now.
        for p in std::mem::take(&mut s.wheel[now as usize % WHEEL]) {
            let Some(dev) = s.devices.get_mut(&p) else { continue };
            let Some((level, _)) = dev.pending.filter(|(_, due)| *due == now) else { continue };
            dev.pending = None;
            let old = dev.state;
            let info = c.states[old as usize].signal;
            let new = with_level(&info, old, level);
            dev.state = new;
            if info.kind == sk::TORCH {
                dev.flips.push_back(now);
                while dev
                    .flips
                    .front()
                    .is_some_and(|t| now.saturating_sub(*t) > BURN_WINDOW)
                {
                    dev.flips.pop_front();
                }
                if dev.flips.len() >= BURN_FLIPS {
                    dev.burnt_until = now + BURN_TIME;
                }
            }
            if new != old {
                self.set_signal_state(space_id, c, p, old, new);
                s.events.push((p, old, new));
                s.output_changed(p);
            }
            s.dirty_devices.insert(p);
        }
        if now % 2 == 0 {
            s.dirty_devices.extend(s.polled.iter().copied());
        }
        // Wires, from gates as they are now. Nothing here acts back on them in this tick.
        let nets: Vec<u32> = s.dirty_nets.drain().collect();
        for id in nets {
            s.recompute(c, id);
        }
        // Then everything reads what that came to. Sorted for mods' callbacks to run the same way
        // each time; no outcome depends on it.
        let mut dirty: Vec<Site> = s.dirty_devices.drain().collect();
        dirty.sort_unstable_by_key(|p| (p.cell.to_array(), p.offset.to_array(), p.port));
        let mut effects = Vec::new();
        for p in dirty {
            s.evaluate(c, self, space_id, p, now, &mut effects);
        }
        // What mods changed while the tick was going.
        let during = std::mem::replace(&mut self.space_mut(space_id).signals, s);
        self.space_mut(space_id)
            .signals
            .events
            .extend(during.events);
        self.signal_effects(space_id, c, effects);
        // What changed in this tick is looked at in the next, but observers count from now.
        let s = &mut self.space_mut(space_id).signals;
        for i in 0..s.events.len() {
            s.seen(s.events[i].0, now);
        }
    }

    fn signal_effects(&mut self, space_id: u64, c: &'static Content, effects: Vec<Effect>) {
        let mut pistons = Vec::new();
        let mut floaters = Vec::new();
        for e in effects {
            match e {
                Effect::Sink { p, level } => {
                    let Some(dev) = self.space_mut(space_id).signals.devices.get_mut(&p) else { continue };
                    // One just placed or loaded has nothing to tell yet.
                    let was = std::mem::replace(&mut dev.input, level);
                    let state = dev.state;
                    let info = c.states[state as usize].signal;
                    let new = with_level(&info, state, level);
                    if new != state {
                        self.replace_placed(space_id, c, p.cell, p.part(state), new);
                    }
                    let block = c.block_of(state);
                    if let (Some((owner, cb)), true) = (block.cb.on_signal, was != u8::MAX) {
                        self.call(space_id, owner, cb, |w| abi::SignalArgs {
                            world: w,
                            pos: p.cell.to_array().into(),
                            state: new,
                            level: level as u32,
                            before: was as u32,
                        });
                    }
                }
                Effect::Piston { p, extend } => pistons.push((p, extend)),
                Effect::Compare { p, behind, side } => {
                    let (Some(state), Some(there)) = (
                        self.space(space_id)
                            .signals
                            .devices
                            .get(&p)
                            .map(|d| d.state),
                        self.get(space_id, behind.cell),
                    ) else {
                        continue;
                    };
                    let info = c.states[state as usize].signal;
                    let block = c.block_of(there);
                    let Some((owner, cb)) = block.cb.signal_read else { continue };
                    let out = self.call(space_id, owner, cb, |w| abi::SignalReadArgs {
                        world: w,
                        pos: behind.cell.to_array().into(),
                        state: there,
                        output: 0,
                    });
                    let want = compared(info.flags, out.output.min(15) as u8, side);
                    if want != info.level {
                        let now = self.time;
                        self.space_mut(space_id)
                            .signals
                            .schedule(p, want, info.delay, now);
                    }
                }
                Effect::Floater { p, level } => {
                    let program = self.rotor_program(c, space_id, p.cell);
                    let Some(dev) = self.space_mut(space_id).signals.devices.get_mut(&p) else { continue };
                    let previous = dev.input;
                    dev.input = level;
                    let info = c.states[dev.state as usize].signal;
                    if info.kind == sk::BODY_MOUNT {
                        if info.flags as u32 & abi::signal_flags::HELM != 0 {
                            continue;
                        }
                        dev.stopped_mount &= program
                            .as_ref()
                            .map_or(level != 0, |p| p.rule(level) == p.rule(previous));
                        let stopped = dev.stopped_mount;
                        if !self.has_mount(space_id, p.cell) {
                            let active = program
                                .as_ref()
                                .map_or(level != 0, |p| p.rule(level).is_some());
                            if !active {
                                self.floater_shows(space_id, c, p.cell, FLOATER_IDLE);
                            } else if !stopped && !self.docking(space_id) {
                                floaters.push(p.cell);
                            }
                        }
                        continue;
                    }
                    if space_id != 0 {
                        self.floater_shows(space_id, c, p.cell, FLOATER_GOING * (level > 0) as u8);
                    } else if level > 0 {
                        floaters.push(p.cell);
                    } else {
                        self.floater_shows(space_id, c, p.cell, FLOATER_IDLE);
                    }
                }
                Effect::Gate { p, inputs } => {
                    let Some(state) = self
                        .space(space_id)
                        .signals
                        .devices
                        .get(&p)
                        .map(|d| d.state)
                    else {
                        continue;
                    };
                    let info = c.states[state as usize].signal;
                    let block = c.block_of(state);
                    let Some((owner, cb)) = block.cb.signal_gate else { continue };
                    let out = self.call(space_id, owner, cb, |w| abi::GateArgs {
                        world: w,
                        pos: p.cell.to_array().into(),
                        state,
                        inputs: inputs.map(|v| v as u32),
                        output: info.level as u32,
                    });
                    let want = out.output.min(15) as u8;
                    if want != info.level {
                        let now = self.time;
                        self.space_mut(space_id)
                            .signals
                            .schedule(p, want, info.delay, now);
                    }
                }
            }
        }
        // A lamp can change state in this same tick. Capture the exact parts to lift after
        // all devices have applied their outputs, whichever way the build faces.
        let mut plans = Vec::new();
        for (p, extend) in pistons {
            let signals = &self.space(space_id).signals;
            if signals.on_the_way || signals.moving.iter().any(|m| m.piston == p) {
                continue;
            }
            if let Some(plan) = self.push_plan(space_id, c, p, extend) {
                plans.push(plan);
            } else {
                self.space_mut(space_id).signals.held.insert(p);
            }
        }
        // Worked out from who moves whom and who wants what, never from where they are, for
        // a build to behave the same whichever way it is turned. A floater moving a piston is
        // like a piston moving one; what floaters and pistons both want, neither gets.
        floaters.sort_unstable_by_key(|p| p.to_array());
        let (mut floater_plans, held) = self.floater_plans(space_id, c, floaters);
        for p in held {
            self.space_mut(space_id).signals.held.insert(p.into());
            self.floater_shows(space_id, c, p, FLOATER_STUCK);
        }
        plans.retain(|plan: &PushPlan| {
            let moved = floater_plans.iter().any(|m| m.cells.contains(&plan.p));
            if moved {
                self.space_mut(space_id).signals.held.insert(plan.p.into());
            }
            !moved
        });
        let mut lost = vec![false; plans.len()];
        let mut lost_floaters: Vec<_> = floater_plans
            .iter()
            .map(|child| {
                // An enclosing assembly carries the controller and its selected blocks.
                // The child can assemble in its new frame on the next signal pass.
                child.mount.is_some_and(|(block, _)| {
                    floater_plans.iter().any(|parent| {
                        parent.cells.len() > child.cells.len() && parent.cells.contains(&block) && child.cells.is_subset(&parent.cells)
                    })
                })
            })
            .collect();
        for (plan, &deferred) in floater_plans.iter().zip(&lost_floaters) {
            if deferred {
                self.space_mut(space_id)
                    .signals
                    .polled
                    .extend(plan.floaters.iter().copied().map(Site::from));
            }
        }
        let waiting = loop {
            let waiting = being_moved(&plans, &lost);
            let mut claims: FxHashMap<IVec3, u32> = FxHashMap::default();
            let going = |i: &usize| !lost[*i] && !waiting[*i];
            for i in (0..plans.len()).filter(going) {
                let cells: FxHashSet<IVec3> = plan_cells(&plans[i]).collect();
                for cell in cells {
                    *claims.entry(cell).or_default() += 1;
                }
            }
            for (m, _) in floater_plans
                .iter()
                .enumerate()
                .filter(|(m, _)| !lost_floaters[*m])
            {
                for cell in floater_plans[m]
                    .cells
                    .iter()
                    .chain(floater_plans[m].broken.iter())
                {
                    *claims.entry(*cell).or_default() += 1;
                }
            }
            let losers: Vec<usize> = (0..plans.len())
                .filter(going)
                .filter(|i| plan_cells(&plans[*i]).any(|cell| claims[&cell] > 1))
                .collect();
            let floater_losers: Vec<usize> = (0..floater_plans.len())
                .filter(|m| !lost_floaters[*m])
                .filter(|m| {
                    floater_plans[*m]
                        .cells
                        .iter()
                        .chain(floater_plans[*m].broken.iter())
                        .any(|cell| claims[cell] > 1)
                })
                .collect();
            if losers.is_empty() && floater_losers.is_empty() {
                break waiting;
            }
            for i in losers {
                // Two bringing back one block both come back without it.
                if !plans[i].extend && !plans[i].moved.is_empty() {
                    let plan = &mut plans[i];
                    (plan.moved, plan.riding, plan.broken) = Default::default();
                    continue;
                }
                log::debug!(
                    "piston at {} gives way: {:?}",
                    plans[i].p,
                    plan_cells(&plans[i])
                        .filter(|cell| claims[cell] > 1)
                        .collect::<Vec<_>>()
                );
                lost[i] = true;
                self.space_mut(space_id)
                    .signals
                    .polled
                    .insert(plans[i].p.into());
            }
            for m in floater_losers {
                lost_floaters[m] = true;
                self.space_mut(space_id)
                    .signals
                    .polled
                    .extend(floater_plans[m].floaters.iter().copied().map(Site::from));
                for &p in &floater_plans[m].floaters {
                    self.floater_shows(space_id, c, p, FLOATER_STUCK);
                }
            }
        };
        for (i, plan) in plans.into_iter().enumerate() {
            if waiting[i] {
                self.space_mut(space_id).signals.held.insert(plan.p.into());
            } else if !lost[i] {
                self.run_plan(space_id, c, plan);
            }
        }
        for (m, plan) in std::mem::take(&mut floater_plans).into_iter().enumerate() {
            if !lost_floaters[m] {
                self.run_floater_plan(space_id, c, plan);
            }
        }
    }

    /// What a floater shows and a comparator reads of it (`power`: 0 idle, 1 going, 2 stuck).
    pub(super) fn floater_shows(&mut self, space_id: u64, c: &'static Content, p: IVec3, level: u8) {
        let Some(state) = self.get(space_id, p) else { return };
        let info = c.states[state as usize].signal;
        if matches!(info.kind, sk::FLOATER | sk::BODY_MOUNT) && info.level != level {
            self.set(space_id, c, p, with_level(&info, state, level));
        }
    }

    pub(super) fn movable(&self, c: &Content, state: u32) -> bool {
        let s = &c.states[state as usize];
        let extended = s.signal.kind == sk::PISTON && s.signal.level > 0;
        s.hardness >= 0.0 && s.flags & bf::IMMOVABLE == 0 && s.signal.kind != sk::PISTON_HEAD && !extended
    }

    /// What moving the block at `start` along `step` takes: blocks ahead of what moves go too, and
    /// what touches a sticky block. `None` if something can't.
    fn gather_move(
        &self,
        space_id: u64,
        c: &Content,
        base: Site,
        free: Option<IVec3>,
        start: IVec3,
        step: IVec3,
    ) -> Option<(Vec<Placed>, Vec<Placed>)> {
        let space = self.space(space_id);
        let mut moved = Vec::new();
        let mut broken = Vec::new();
        let mut todo = Vec::new();
        let collect = |origin: IVec3, area: Aabb, todo: &mut Vec<Placed>| -> Option<()> {
            let lo = area.min.floor().as_ivec3();
            let hi = area.max.ceil().as_ivec3();
            for x in lo.x..hi.x {
                for y in lo.y..hi.y {
                    for z in lo.z..hi.z {
                        let delta = IVec3::new(x, y, z);
                        let at = origin + delta;
                        if space.signals.reserved(at) {
                            return None;
                        }
                        for part in space.layout(at).parts(space.get(at)?) {
                            let owner = part.owner(at);
                            if Some(owner.0) == free || (owner.0 == base.cell && owner.1.offset == base.offset) {
                                continue;
                            }
                            let shift = delta.as_dvec3();
                            let local = Aabb { min: area.min - shift, max: area.max - shift };
                            if part.intersects(c, local) && !todo.contains(&owner) {
                                todo.push(owner);
                            }
                        }
                    }
                }
            }
            Some(())
        };
        let shift = base.part(AIR).shift();
        collect(start, Aabb { min: shift, max: shift + DVec3::ONE }, &mut todo)?;
        while let Some((p, part)) = todo.pop() {
            if moved.contains(&(p, part)) || broken.contains(&(p, part)) {
                continue;
            }
            let info = &c.states[part.state as usize];
            if info.liquid.is_some() {
                broken.push((p, part));
                continue;
            }
            if !self.movable(c, part.state) {
                return None;
            }
            if !info.solid() {
                broken.push((p, part));
                continue;
            }
            if moved.len() == PUSH_LIMIT {
                return None;
            }
            moved.push((p, part));
            for patch in part.bounds(c) {
                collect(p, patch.swept(step.as_dvec3()), &mut todo)?;
                if info.flags & bf::STICKY != 0 {
                    for dir in 0..6 {
                        let axis = dir / 2;
                        let plane = if dir % 2 == 0 { patch.min[axis] } else { patch.max[axis] };
                        crate::world::placement::face_contacts(
                            c,
                            p,
                            patch,
                            dir,
                            plane,
                            &|at| Some((space.get(at)?, space.layout(at))),
                            |at, other, _| {
                                let other = other.owner(at);
                                if other.0 != base.cell
                                    && c.solid(other.1.state)
                                    && self.movable(c, other.1.state)
                                    && !todo.contains(&other)
                                    && !moved.contains(&other)
                                {
                                    todo.push(other);
                                }
                                false
                            },
                        );
                    }
                }
            }
        }
        Some((moved, broken))
    }

    fn break_pushed(&mut self, space_id: u64, c: &'static Content, broken: &[Placed]) {
        for &(p, part) in broken {
            if self.replace_placed(space_id, c, p, part, AIR) && c.states[part.state as usize].liquid.is_none() {
                self.drop_block_loot(space_id, c, p, part.state, abi::ItemStack::EMPTY, false, 0);
            }
        }
    }

    /// What a piston in front of `from` facing `dir` would push, moved one block that way as a
    /// body over a piston's time, then put down: for a mod's own movers. False if it can't go.
    pub fn push_blocks(&mut self, space_id: u64, c: &'static Content, from: IVec3, dir: u8) -> bool {
        let Some(&step) = DIRS.get(dir as usize) else { return false };
        let Some((moved, broken)) = self.gather_move(space_id, c, (from - step).into(), None, from, step) else { return false };
        if moved.is_empty() {
            return false;
        }
        let (riders, mut cut) = self.riders_on(space_id, c, &moved, None, step);
        cut.extend(
            broken
                .into_iter()
                .filter(|b| !riders.iter().any(|r| r.0 == *b)),
        );
        self.break_pushed(space_id, c, &cut);
        let cells = moved
            .iter()
            .map(|&(p, part)| Lifted { from: p, local: p - from, to: p, part: None, only: Some(part) })
            .chain(riders.into_iter().map(|((p, source), part)| Lifted {
                from: p,
                local: p - from,
                to: p,
                part: Some(part),
                only: Some(source),
            }));
        let (id, _) =
            self.lift(space_id, c, c.game.platform_kind, from.as_dvec3() + crate::physics::Transform::FEET, cells, &[], Liveness::Free);
        let Some(e) = self.entities.get_mut(id) else { return false };
        e.motion = super::entity::Motion::To { to: e.pos + step.as_dvec3(), left: MOVE_TICKS as u16 };
        e.completion = Completion::Dock;
        true
    }

    fn push_plan(&mut self, space_id: u64, c: &Content, site: Site, extend: bool) -> Option<PushPlan> {
        let p = site.cell;
        let state = self.signal_view(space_id).state(site)?;
        let s = c.states[state as usize].signal;
        let dir = s.front;
        let step = *DIRS.get(dir as usize)?;
        let head = p + step;
        let mut plan = PushPlan {
            p,
            offset: site.offset,
            port: site.port,
            extend,
            dir,
            moved: Vec::new(),
            step,
            broken: Vec::new(),
            riding: Vec::new(),
        };
        if extend {
            (plan.moved, plan.broken) = self.gather_move(space_id, c, site, None, head, step)?;
        } else {
            // A sticky one brings back what is against its head, if it can; if not it comes back
            // without it. So it does when the power is gone before what it pushed has got there:
            // vanilla's way to leave a block behind. With nothing pushed, it brings back what it
            // reached however short the power was, which is how a flying machine's back half comes.
            plan.step = -step;
            let held_on = self
                .space(space_id)
                .signals
                .devices
                .get(&site)
                .is_some_and(|d| self.time > d.pushed_at + MOVE_TICKS as u64);
            if s.flags as u32 & abi::signal_flags::STICKY != 0 && held_on {
                let beyond = head + step;
                let here = self.block_at(space_id, c, beyond)?.1;
                if here != AIR && c.solid(here) && self.movable(c, here) {
                    if let Some((moved, broken)) = self.gather_move(space_id, c, site, Some(head), beyond, -step) {
                        (plan.moved, plan.broken) = (moved, broken);
                    }
                }
            }
        }
        let (riding, broken) = self.riders_on(space_id, c, &plan.moved, Some(p), plan.step);
        plan.broken.retain(|v| !riding.iter().any(|r| r.0 == *v));
        for v in broken {
            if !plan.broken.contains(&v) {
                plan.broken.push(v);
            }
        }
        plan.riding = riding;
        Some(plan)
    }

    /// `moved` with what stands on it, as cells of a new body around `center`.
    pub(super) fn with_riders(&self, space_id: u64, c: &Content, center: IVec3, moved: Vec<IVec3>) -> Vec<Lifted> {
        let (riding, _) = self.who_rides(space_id, c, &moved, None, IVec3::ZERO);
        moved
            .into_iter()
            .map(|p| Lifted { from: p, local: p - center, to: p, part: None, only: None })
            .chain(riding.into_iter().map(|((p, source), state)| Lifted {
                from: p,
                local: p - center,
                to: p,
                part: Some(state),
                only: Some(source),
            }))
            .collect()
    }

    pub(super) fn who_rides(
        &self,
        space_id: u64,
        c: &Content,
        list: &[IVec3],
        not: Option<IVec3>,
        step: IVec3,
    ) -> (Vec<(Placed, u32)>, Vec<Placed>) {
        let space = self.space(space_id);
        let moved: Vec<_> = list
            .iter()
            .flat_map(|&p| {
                space
                    .layout(p)
                    .residents(space.get(p).unwrap_or(AIR))
                    .map(move |part| (p, part))
            })
            .collect();
        self.riders_on(space_id, c, &moved, not, step)
    }

    /// A mounted part rides the surfaces carrying it. A wire with several faces can leave
    /// some behind. The destination is checked against the actual pieces that stay there.
    fn riders_on(&self, space_id: u64, c: &Content, list: &[Placed], not: Option<IVec3>, step: IVec3) -> (Vec<(Placed, u32)>, Vec<Placed>) {
        let space = self.space(space_id);
        let moved: FxHashSet<_> = list.iter().copied().collect();
        let mut masks: FxHashMap<Placed, u8> = Default::default();
        for &(cell, support) in list {
            for patch in support.bounds(c) {
                for dir in 0..6 {
                    let axis = dir / 2;
                    let plane = if dir % 2 == 0 { patch.min[axis] } else { patch.max[axis] };
                    crate::world::placement::face_contacts(
                        c,
                        cell,
                        patch,
                        dir,
                        plane,
                        &|p| Some((space.get(p)?, space.layout(p))),
                        |at, part, bounds| {
                            let owner = part.owner(at);
                            let info = &c.states[part.state as usize];
                            if Some(owner.0) == not || moved.contains(&owner) || info.solid() || info.liquid.is_some() {
                                return false;
                            }
                            let on = dir as u8 ^ 1;
                            let wire = info.signal.kind == sk::WIRE;
                            let holds =
                                if wire { wire::wire_faces(c, part, bounds) & (1 << on) != 0 } else { c.held_by(part.state) == Some(on) };
                            if holds {
                                *masks.entry(owner).or_default() |= 1 << on;
                            }
                            false
                        },
                    );
                }
            }
        }
        let mut riding: FxHashMap<_, _> = masks
            .into_iter()
            .map(|(placed, mask)| {
                let state = placed.1.state;
                let state =
                    if c.states[state as usize].signal.kind == sk::WIRE { c.block_of(state).with_sides(state, mask) } else { state };
                (placed, state)
            })
            .collect();
        let mut broken = Vec::new();
        if step != IVec3::ZERO {
            loop {
                let mut stuck = Vec::new();
                let mut knocked = Vec::new();
                for (&(p, source), &state) in &riding {
                    let part = Part { state, ..source };
                    let mut wall = false;
                    for d in std::iter::once(IVec3::ZERO).chain(part.footprint(c).iter().copied()) {
                        let at = p + step + d;
                        let Some(main) = space.get(at) else {
                            wall = true;
                            break;
                        };
                        wall |= space.signals.reserved(at);
                        for other in space.layout(at).parts(main) {
                            let owner = other.owner(at);
                            if moved.contains(&owner)
                                || riding.contains_key(&owner)
                                || c.may_share(state, other.state)
                                || !part.in_cell(d).overlaps(other, c)
                            {
                                continue;
                            }
                            match rider_meets(c, other.state) {
                                RiderMeets::Wall => wall = true,
                                RiderMeets::Breaks => knocked.push(owner),
                                RiderMeets::Nothing => {}
                            }
                        }
                    }
                    if wall {
                        stuck.push((p, source));
                    }
                }
                if stuck.is_empty() {
                    broken.extend(knocked);
                    break;
                }
                for v in stuck {
                    riding.remove(&v);
                    broken.push(v);
                }
            }
        }
        let mut riding: Vec<_> = riding.into_iter().collect();
        riding.sort_unstable_by_key(|((p, part), _)| (p.to_array(), part.offset.to_array(), part.state));
        (riding, broken)
    }

    /// Takes the plan's blocks out of the world and sends them on their way as one body, which
    /// carries and shoves what is around it like any other. They land as blocks.
    fn run_plan(&mut self, space_id: u64, c: &'static Content, plan: PushPlan) {
        let site = Site { cell: plan.p, offset: plan.offset, port: plan.port };
        let Some(state) = self.signal_view(space_id).state(site) else { return };
        let info = c.states[state as usize].signal;
        let head = plan.p + DIRS[plan.dir as usize];
        self.break_pushed(space_id, c, &plan.broken);
        let short = |h: u32| c.state_with(h, "short", "true").unwrap_or(h);
        // The head on its way is the body's: out, it is the piston's own cell, and in, the piston
        // is a cell again. Meanwhile the piston is moving, its short base.
        let mut also = Vec::new();
        let mut head_turns = None;
        let h = info
            .head
            .and_then(|b| c.state_with(c.blocks[b as usize].first_state, "facing", wire::dir_name(plan.dir)));
        if let Some(h) = h {
            if plan.extend {
                // From inside the piston, out of its short base.
                also.push((IVec3::ZERO, site.part(short(h))));
                head_turns = Some((IVec3::ZERO, h));
            } else {
                also.push((head - plan.p, site.part(h)));
                head_turns = Some((head - plan.p, short(h)));
            }
        }
        self.replace_placed(space_id, c, plan.p, site.part(state), with_level(&info, state, 1));
        // Where they are going is taken from now on.
        self.space_mut(space_id).signals.reserved.insert(head);
        for &(m, _) in &plan.moved {
            self.space_mut(space_id)
                .signals
                .reserved
                .insert(m + plan.step);
        }
        if plan.extend {
            let now = self.time;
            if let Some(d) = self
                .space_mut(space_id)
                .signals
                .devices
                .get_mut(&site)
                .filter(|_| !plan.moved.is_empty())
            {
                d.pushed_at = now;
            }
        }
        // In the body, relative to the piston, as they start; and where they end up.
        for &((n, _), _) in &plan.riding {
            self.space_mut(space_id)
                .signals
                .reserved
                .insert(n + plan.step);
        }
        let cells = plan
            .moved
            .iter()
            .map(|&(p, part)| Lifted { from: p, local: p - plan.p, to: p + plan.step, part: None, only: Some(part) })
            .chain(plan.riding.iter().map(|&((p, source), part)| Lifted {
                from: p,
                local: p - plan.p,
                to: p + plan.step,
                part: Some(part),
                only: Some(source),
            }));
        let (body, blocks) =
            self.lift(space_id, c, c.game.moving_kind, plan.p.as_dvec3() + DVec3::new(0.5, 0.0, 0.5), cells, &also, Liveness::OnTheWay);
        if let Some(e) = self.entities.get_mut(body) {
            e.motion = Motion::To { to: e.pos + plan.step.as_dvec3(), left: MOVE_TICKS as u16 };
        }
        self.space_mut(space_id)
            .signals
            .moving
            .push(Moving { body, piston: site, head, blocks, retracting: !plan.extend, head_turns });
        self.sound(
            space_id,
            c.states[state as usize].effects[crate::content::block_event::PLACE].sound,
            plan.p.as_dvec3() + DVec3::splat(0.5),
            0.5,
            0.0,
        );
    }

    /// Bodies of pistons that got there turn back into blocks.
    pub(super) fn land_moving(&mut self, space_id: u64, c: &'static Content) {
        self.tick_landings(space_id, c);
        if self.space(space_id).signals.moving.is_empty() {
            return;
        }
        let (space, entities) = self.space_entities(space_id);
        let (done, going): (Vec<Moving>, Vec<Moving>) = std::mem::take(&mut space.signals.moving)
            .into_iter()
            .partition(|m| {
                entities
                    .get(m.body)
                    .is_none_or(|e| !matches!(e.motion, Motion::To { .. }))
            });
        space.signals.moving = going;
        for mut m in done {
            // Clients show this a tick and a half later, a quarter of the way out: past that a
            // long rod no longer shows behind the piston.
            if !m.retracting
                && self.has_space(m.body)
                && let Some((at, state)) = m.head_turns.take()
            {
                self.carry_block(m.body, c, at, state);
            }
            self.land_piston(space_id, c, m);
        }
    }

    fn land_piston(&mut self, space_id: u64, c: &'static Content, m: Moving) {
        let base = self.signal_view(space_id).state(m.piston);
        let out = base.is_some_and(|s| c.states[s as usize].signal.kind == sk::PISTON && c.states[s as usize].signal.level > 0);
        self.space_mut(space_id).signals.reserved.remove(&m.head);
        for b in &m.blocks {
            self.space_mut(space_id).signals.reserved.remove(&b.to);
        }
        // In, or out with its head: a piston gone meanwhile leaves the head with the body.
        let mut also = Vec::new();
        if let Some(s) = base.filter(|_| out) {
            let info = c.states[s as usize].signal;
            self.replace_placed(space_id, c, m.piston.cell, m.piston.part(s), with_level(&info, s, if m.retracting { 0 } else { 15 }));
            also.push((m.piston.cell, s));
        }
        self.space_mut(space_id)
            .signals
            .dirty_devices
            .insert(m.piston.into());
        self.space_mut(space_id).signals.give_another_go();
        self.put_down(space_id, c, m.body, m.blocks, also, m.head_turns);
    }

    /// All that is on its way gets there now: before a save, or before the blocks around it go
    /// to another region or out of memory. It isn't kept anywhere else.
    pub fn finish_moving(&mut self, c: &'static Content) {
        self.finish_moving_here(0, c);
        self.out_of_touch(c);
        let ids: Vec<u64> = self.bodies.keys().copied().collect();
        for id in ids {
            self.finish_moving_here(id, c);
        }
    }

    pub(super) fn finish_moving_here(&mut self, space_id: u64, c: &'static Content) {
        if !self.has_space(space_id) {
            return;
        }
        for m in std::mem::take(&mut self.space_mut(space_id).signals.moving) {
            self.land_piston(space_id, c, m);
        }
        self.finish_landings(space_id);
    }
}

/// Which plans are of a piston that another piston is moving: it waits to see where it ends up,
/// and goes from there if it still has power. Plans that lost don't move anything, and pistons
/// moving each other in a ring all wait.
fn being_moved(plans: &[PushPlan], lost: &[bool]) -> Vec<bool> {
    let pistons: FxHashMap<IVec3, usize> = plans
        .iter()
        .enumerate()
        .filter(|(i, _)| !lost[*i])
        .map(|(i, plan)| (plan.p, i))
        .collect();
    let mut movers = vec![0u32; plans.len()];
    let mut moves: Vec<Vec<usize>> = vec![Vec::new(); plans.len()];
    for (i, plan) in plans.iter().enumerate().filter(|(i, _)| !lost[*i]) {
        for other in plan.moved.iter().filter_map(|m| pistons.get(&m.0)) {
            movers[*other] += 1;
            moves[i].push(*other);
        }
    }
    let mut goes = vec![false; plans.len()];
    let mut waits = vec![false; plans.len()];
    let mut todo: Vec<usize> = (0..plans.len())
        .filter(|i| !lost[*i] && movers[*i] == 0)
        .collect();
    while let Some(i) = todo.pop() {
        goes[i] = true;
        for &moved in &moves[i] {
            if std::mem::replace(&mut waits[moved], true) {
                continue;
            }
            // Whatever only this one would have moved stays free to go.
            for &next in &moves[moved] {
                movers[next] -= 1;
                if movers[next] == 0 && !waits[next] {
                    todo.push(next);
                }
            }
        }
    }
    (0..plans.len()).map(|i| !lost[i] && !goes[i]).collect()
}

fn plan_cells(plan: &PushPlan) -> impl Iterator<Item = IVec3> + '_ {
    let head = plan.p + DIRS[plan.dir as usize];
    let riding = plan.riding.iter().map(|r| r.0.0);
    plan.moved
        .iter()
        .map(|v| v.0)
        .chain(riding.clone())
        .chain(
            plan.moved
                .iter()
                .map(|v| v.0)
                .chain(riding)
                .map(|m| m + plan.step),
        )
        .chain(plan.broken.iter().map(|v| v.0))
        .chain([head, plan.p])
}
