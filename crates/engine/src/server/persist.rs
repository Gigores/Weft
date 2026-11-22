//! Entity serialization for saves. Kinds, items and components are stored by name so a save
//! survives mods being added, removed or reordered.

use glam::{DQuat, DVec3, IVec3};
use weft_abi::{ItemStack, component_flags as cf, entry_kind as ek};

use super::entity::{BodyMount, Completion, Entity, Stuck, StuckIn};
use super::space::{BlockTimer, Space};
use crate::content::Content;
use crate::net::codec::{self, R, Wire};

/// An entity read back: itself, its inventory, components and, for a moving structure, its
/// blocks.
pub type Saved = (Entity, Option<Vec<ItemStack>>, Vec<(u32, Box<[u8]>)>, Option<SavedSpace>);
pub type ToSave<'a> = (&'a Entity, Option<&'a Vec<ItemStack>>, Vec<(u32, &'a [u8])>, Option<&'a Space>);

pub struct SavedSpace {
    pub blocks: Vec<(IVec3, crate::world::placement::Part)>,
    pub timers: Vec<BlockTimer>,
}

const ENTITY_FORMAT_V2: u32 = 0x3254_4E45;
const ENTITY_FORMAT_V3: u32 = 0x3354_4E45;
const ENTITY_FORMAT: u32 = 0x3454_4E45;
const MOUNTED_BODY: u16 = 0x80;
const FLOATING_BODY: u16 = 0x40;
const FLOATER_DRIVEN: u16 = 0x20;
const NESTED_MOUNT: u16 = 0x10;
const MOUNT_DRIVE: u16 = 0x08;
const MOUNT_SLIDE: u16 = 0x04;
const ROTATED_BODY: u16 = 0x100;
const LAND_ON_SUPPORT: u16 = 0x200;
const DOCK_BODY: u16 = 0x400;
const STUCK: u16 = 0x800;
const SHIFTED_ANCHOR: u16 = 0x1000;

struct W<'a>(&'a mut Vec<u8>);

impl W<'_> {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f64(&mut self, v: f64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn str(&mut self, s: &str) {
        self.u16(s.len() as u16);
        self.0.extend_from_slice(s.as_bytes());
    }
    fn stack(&mut self, c: &Content, s: ItemStack) {
        if s.is_empty() {
            self.str("");
            return;
        }
        self.str(&c.items[s.item as usize].name);
        self.u32(s.count);
        let mut named = codec::W::default();
        crate::stack::put_named(&mut named, crate::stack::for_save(s.data));
        self.0.extend_from_slice(&named.0);
    }
}

impl R<'_> {
    fn str(&mut self) -> Option<String> {
        let n = self.u16()? as usize;
        Some(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    fn stack(&mut self, c: &Content) -> Option<ItemStack> {
        let name = self.str()?;
        if name.is_empty() {
            return Some(ItemStack::EMPTY);
        }
        let count = self.u32()?;
        let data = crate::stack::get_named(self)?;
        // Items of removed mods are dropped rather than turned into something else.
        Some(
            c.game
                .catalog
                .ids(ek::ITEM)
                .get(&name)
                .map_or(ItemStack::EMPTY, |&i| ItemStack { item: i, count, data }),
        )
    }
}

/// `now`: the tick of the entities' dimension, what their effects' ends count from.
pub fn write(c: &Content, out: &mut Vec<u8>, list: &[ToSave<'_>], now: u64) {
    let mut w = W(out);
    let orientation = |e: &Entity, has_blocks: bool| {
        e.frame_rotation
            .as_deref()
            .copied()
            .filter(|&q| has_blocks && e.mount.is_none() && e.ship.is_none() && q != DQuat::IDENTITY)
    };
    let completion = |e: &Entity| match e.completion {
        Completion::LandOnSupport if e.kind != c.game.falling_kind => LAND_ON_SUPPORT,
        Completion::Dock => DOCK_BODY,
        _ => 0,
    };
    let format = if list
        .iter()
        .any(|(e, ..)| e.stuck.is_some() || e.anchor_offset != IVec3::ZERO)
    {
        ENTITY_FORMAT
    } else if list
        .iter()
        .any(|(e, _, _, space)| orientation(e, space.is_some()).is_some() || completion(e) != 0)
    {
        ENTITY_FORMAT_V3
    } else {
        ENTITY_FORMAT_V2
    };
    w.u32(format);
    w.u32(list.len() as u32);
    for (e, inv, comps, blocks) in list {
        w.str(&c.game.entities[e.kind as usize].name);
        w.u64(e.id);
        for v in e.pos.to_array().into_iter().chain(e.vel.to_array()) {
            w.f64(v);
        }
        w.f32(e.yaw);
        w.f32(e.pitch);
        w.f32(e.health);
        w.u32(e.selected);
        w.u8(e.flying as u8);
        w.u32(e.fire);
        w.str(e.mode.name());
        w.u64(e.age);
        w.stack(c, e.item);
        // 2: on a body, which it says.
        let mounted = if e.mount.is_some() { MOUNTED_BODY } else { 0 };
        let parent_mount = e.frame.filter(|_| e.mount.is_some());
        let rotation = orientation(e, blocks.is_some());
        let attachment = mounted
            | if e.ship.is_some() { FLOATING_BODY } else { 0 }
            | if e.floater_driven { FLOATER_DRIVEN } else { 0 }
            | if parent_mount.is_some() { NESTED_MOUNT } else { 0 }
            | if e.mount.as_ref().is_some_and(|m| m.drive.is_some()) { MOUNT_DRIVE } else { 0 }
            | if e.mount.as_ref().is_some_and(|m| m.slide.is_some()) { MOUNT_SLIDE } else { 0 }
            | if rotation.is_some() { ROTATED_BODY } else { 0 }
            | completion(e)
            | if e.stuck.is_some() { STUCK } else { 0 }
            | if e.anchor_offset != IVec3::ZERO { SHIFTED_ANCHOR } else { 0 };
        let frame = e.block_frame();
        let attachment = attachment | e.anchor.map_or(0, |_| if frame != 0 { 2 } else { 1 });
        if format != ENTITY_FORMAT_V2 {
            w.u16(attachment);
        } else {
            w.u8(attachment as u8);
        }
        if let Some(a) = e.anchor {
            for v in a.to_array() {
                w.u32(v as u32);
            }
            if attachment & SHIFTED_ANCHOR != 0 {
                for v in e.anchor_offset.to_array() {
                    w.u32(v as u32);
                }
            }
            if frame != 0 {
                w.u64(frame);
            }
        }
        if let Some(parent) = parent_mount {
            w.u64(parent);
        }
        if let Some(m) = &e.mount {
            for v in m.block.to_array() {
                w.u32(v as u32);
            }
            w.u8(m.face);
            w.u8(m.speed);
            w.f64(m.angle);
            w.f64(m.last_grid);
            w.f64(m.stop.unwrap_or(-1.0));
            if let Some(drive) = m.drive {
                w.0.extend(drive.to_bytes());
            }
            if let Some(slide) = m.slide {
                w.0.extend(slide.to_bytes());
            }
        }
        if let Some(ship) = &e.ship {
            w.u8(ship.front);
            for v in e
                .body_place()
                .rotation()
                .to_array()
                .into_iter()
                .chain(ship.spin.to_array())
            {
                w.f64(v);
            }
        }
        if let Some(rotation) = rotation {
            for v in rotation.to_array() {
                w.f64(v);
            }
        }
        if let Some(stuck) = e.stuck {
            match stuck.into {
                StuckIn::Block { body, block } => {
                    w.u8(0);
                    w.u64(body);
                    for v in block.to_array() {
                        w.u32(v as u32);
                    }
                }
                StuckIn::Entity(id) => {
                    w.u8(1);
                    w.u64(id);
                }
            }
            for v in stuck.at.to_array() {
                w.f64(v);
            }
            w.f32(stuck.yaw);
            w.u32(stuck.ticks);
        }
        match inv {
            Some(inv) => {
                w.u16(inv.len() as u16);
                for s in inv.iter() {
                    w.stack(c, *s);
                }
            }
            None => w.u16(u16::MAX),
        }
        let saved: Vec<&(u32, &[u8])> = comps
            .iter()
            .filter(|(id, _)| c.game.components[*id as usize].flags & cf::SAVE != 0)
            .collect();
        w.u16(saved.len() as u16);
        for (id, bytes) in saved {
            w.str(&c.game.components[*id as usize].name);
            let named = if crate::stack::holds_stacks(c, *id) {
                Some(crate::stack::contents_to_names(c, bytes))
            } else {
                (*id == c.game.fields.effect).then(|| effects_to_save(c, bytes, now))
            };
            let bytes = named.as_deref().unwrap_or(bytes);
            // Component values over 64 KB say so with a marker, older saves never have it.
            if bytes.len() >= u16::MAX as usize {
                w.u16(u16::MAX);
                w.u32(bytes.len() as u32);
            } else {
                w.u16(bytes.len() as u16);
            }
            w.0.extend_from_slice(bytes);
        }
        match blocks {
            Some(space) => {
                let list = space.placed_blocks();
                let shifted = list.iter().any(|(_, part)| part.offset != IVec3::ZERO);
                w.u32(list.len() as u32 | ((shifted as u32) << 31));
                for (p, part) in list {
                    for v in p.to_array() {
                        w.u32(v as u32);
                    }
                    if shifted {
                        for v in part.offset.to_array() {
                            w.u32(v as u32);
                        }
                    }
                    w.str(&c.state_name(part.state));
                }
            }
            None => w.u32(u32::MAX),
        }
    }
    let timed = list.iter().filter_map(|(entity, _, _, space)| {
        space
            .filter(|space| !space.scheduled.is_empty())
            .map(|space| (entity.id, space))
    });
    let count = timed.clone().count();
    if count != 0 {
        w.u32(BODY_TIMERS_MAGIC);
        w.u32(count as u32);
        for (frame, space) in timed {
            w.u64(frame);
            let length = w.0.len();
            w.u32(0);
            let mut timers: Vec<_> = space.scheduled.iter().map(|timer| &timer.0).collect();
            timers.sort_unstable();
            timers.dedup();
            append_timers(w.0, c, timers);
            let bytes = (w.0.len() - length - 4) as u32;
            w.0[length..length + 4].copy_from_slice(&bytes.to_le_bytes());
        }
        w.u32(BODY_TIMERS_MAGIC);
    }
}

/// Marks the trailer of a cube's saved data that holds the timers on its blocks: each the block,
/// then as `dim::push_timer` writes it; their length in bytes, this.
const TIMERS_MAGIC: u32 = 0x5352_4D54;
/// After the entity list, before any terrain timers. The trailing marker keeps a body's timer
/// record from looking like the terrain timer trailer to old readers.
const BODY_TIMERS_MAGIC: u32 = 0x5442_4D54;

pub fn append_timers<'a>(out: &mut Vec<u8>, c: &Content, timers: impl IntoIterator<Item = &'a BlockTimer>) {
    let start = out.len();
    for (at, p, event, data) in timers {
        p.iter()
            .for_each(|v| out.extend_from_slice(&v.to_le_bytes()));
        super::dim::push_timer(out, *at, &c.game.events[*event as usize].name, data);
    }
    out.extend_from_slice(&((out.len() - start) as u32).to_le_bytes());
    out.extend_from_slice(&TIMERS_MAGIC.to_le_bytes());
}

/// The timers `append_timers` wrote, but those of events no mod has now.
pub fn read_timers(c: &Content, data: &[u8]) -> Vec<BlockTimer> {
    let word = |at: usize| {
        data.get(at..at + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    };
    let Some(end) = data.len().checked_sub(8) else { return Vec::new() };
    if word(end + 4) != Some(TIMERS_MAGIC) {
        return Vec::new();
    }
    let Some(mut at) = end.checked_sub(word(end).unwrap_or(0) as usize) else { return Vec::new() };
    let mut out = Vec::new();
    while at + 12 < end {
        let p = [0, 1, 2].map(|i| word(at + i * 4).unwrap_or(0) as i32);
        let Some((len, (until, name, value))) = super::dim::read_timer(&data[at + 12..end]) else { break };
        at += 12 + len;
        match c.game.event_names.get(&name) {
            Some(&e) => out.push((until, p, e, value.into())),
            None => log::warn!("a timer at {p:?} for event {name}, which no mod has now"),
        }
    }
    out
}

/// `now`: the tick of the dimension they come back to, what their effects' ends count from.
pub fn read(c: &Content, data: &[u8], now: u64) -> Vec<Saved> {
    let mut r = R(data);
    let mut out = Vec::new();
    if data.is_empty() {
        return out;
    }
    let extended = match r.u32() {
        Some(ENTITY_FORMAT | ENTITY_FORMAT_V3) => true,
        Some(ENTITY_FORMAT_V2) => false,
        _ => {
            log::warn!("unsupported saved entity format");
            return out;
        }
    };
    let Some(n) = r.u32() else { return out };
    for _ in 0..n {
        let Some(e) = read_one(c, &mut r, now, extended) else { break };
        if let Some(e) = e {
            out.push(e);
        }
    }
    if r.u32() == Some(BODY_TIMERS_MAGIC) {
        let count = r.u32().unwrap_or(0);
        let mut timers = rustc_hash::FxHashMap::default();
        for _ in 0..count {
            let Some((id, bytes)) = r.u64().zip(r.u32()) else { break };
            let Some(data) = r.take(bytes as usize) else { break };
            timers.insert(id, read_timers(c, data));
        }
        for (entity, _, _, space) in &mut out {
            if let Some(space) = space
                && let Some(timers) = timers.remove(&entity.id)
            {
                space.timers = timers;
            }
        }
    }
    center_saved_falls(c, &mut out);
    out
}

// Older falls stored the placement offset in the body instead of its pose.
fn center_saved_falls(c: &Content, saved: &mut [Saved]) {
    let mut offsets = rustc_hash::FxHashMap::default();
    for (entity, _, _, space) in saved.iter_mut() {
        if entity.kind == c.game.falling_kind
            && let Some(space) = space
            && let [(cell, part)] = space.blocks.as_mut_slice()
            && *cell == IVec3::ZERO
            && part.offset != IVec3::ZERO
        {
            offsets.insert(entity.id, part.offset);
            entity.pos += entity.body_place().vector_to_world(part.shift());
            part.offset = IVec3::ZERO;
        }
    }
    if offsets.is_empty() {
        return;
    }
    for (entity, _, _, _) in saved {
        if entity.anchor == Some(IVec3::ZERO) && entity.frame.and_then(|id| offsets.get(&id)) == Some(&entity.anchor_offset) {
            entity.anchor_offset = IVec3::ZERO;
        }
        if let Some(stuck) = &mut entity.stuck
            && let StuckIn::Block { body, .. } = stuck.into
            && let Some(offset) = offsets.get(&body)
        {
            stuck.at -= offset.as_dvec3() / f64::from(crate::world::placement::UNIT);
        }
    }
}

fn read_one(c: &Content, r: &mut R, now: u64, extended: bool) -> Option<Option<Saved>> {
    let kind_name = r.str()?;
    let id = r.u64()?;
    let mut v = [0.0f64; 6];
    for x in v.iter_mut() {
        *x = r.f64()?;
    }
    let yaw = r.f32()?;
    let pitch = r.f32()?;
    let health = r.f32()?;
    let selected = r.u32()?;
    let flying = r.u8()? != 0;
    let fire = r.u32()?;
    let mode = crate::world::GameMode::parse(&r.str()?).unwrap_or(crate::world::GameMode::SURVIVAL);
    let age = r.u64()?;
    let item = r.stack(c)?;
    let attachment = if extended { r.u16()? } else { u16::from(r.u8()?) };
    let anchor_kind = attachment & 0x03;
    let anchor = if anchor_kind != 0 { Some(IVec3::new(r.u32()? as i32, r.u32()? as i32, r.u32()? as i32)) } else { None };
    let anchor_offset =
        if attachment & SHIFTED_ANCHOR != 0 { IVec3::new(r.u32()? as i32, r.u32()? as i32, r.u32()? as i32) } else { IVec3::ZERO };
    let frame = if anchor_kind == 2 || attachment & NESTED_MOUNT != 0 { Some(r.u64()?) } else { None };
    let mount = if attachment & MOUNTED_BODY != 0 {
        let block = IVec3::new(r.u32()? as i32, r.u32()? as i32, r.u32()? as i32);
        let face = r.u8()?;
        let speed = r.u8()?.clamp(1, 15);
        let angle = r.f64()?;
        let last_grid = r.f64()?;
        let stop = r.f64()?;
        let drive = if attachment & MOUNT_DRIVE != 0 { Some(crate::controller::Drive::get(r)?) } else { None };
        let slide = if attachment & MOUNT_SLIDE != 0 { Some(crate::controller::Slide::get(r)?) } else { None };
        (face < 6 && angle.is_finite() && last_grid.is_finite() && stop.is_finite()).then(|| {
            Box::new(BodyMount {
                block,
                face,
                speed,
                angle,
                last_grid,
                stop: (stop != -1.0).then_some(stop),
                drive,
                slide,
                coast_rate: 0.0,
            })
        })
    } else {
        None
    };
    let dynamic = if attachment & FLOATING_BODY != 0 {
        let front = r.u8()?;
        let rotation = DQuat::from_xyzw(r.f64()?, r.f64()?, r.f64()?, r.f64()?);
        let spin = DVec3::new(r.f64()?, r.f64()?, r.f64()?);
        (front < 6 && rotation.is_finite() && rotation.length_squared() > 1e-9 && spin.is_finite())
            .then(|| (Box::new(super::buoyancy::Ship::new(front, spin)), rotation.normalize()))
    } else {
        None
    };
    let rotation = if attachment & ROTATED_BODY != 0 {
        let q = DQuat::from_xyzw(r.f64()?, r.f64()?, r.f64()?, r.f64()?);
        (q.is_finite() && q.length_squared() > 1e-9).then(|| q.normalize())
    } else {
        None
    };
    let stuck = if attachment & STUCK != 0 {
        let into = match r.u8()? {
            0 => StuckIn::Block { body: r.u64()?, block: IVec3::get(r)? },
            1 => StuckIn::Entity(r.u64()?),
            _ => return None,
        };
        Some(Stuck { into, at: DVec3::get(r)?, yaw: r.f32()?, ticks: r.u32()? })
    } else {
        None
    };
    let inv_len = r.u16()?;
    let inv = if inv_len == u16::MAX {
        None
    } else {
        let mut v = Vec::with_capacity(inv_len as usize);
        for _ in 0..inv_len {
            v.push(r.stack(c)?);
        }
        Some(v)
    };
    let nc = r.u16()?;
    let mut comps = Vec::new();
    for _ in 0..nc {
        let name = r.str()?;
        let len = r.u16()? as usize;
        let len = if len == u16::MAX as usize { r.u32()? as usize } else { len };
        let bytes = r.take(len)?.to_vec();
        if let Some(id) = c.component(&name) {
            let bytes = if crate::stack::holds_stacks(c, id) {
                crate::stack::contents_from_names(c, &bytes)
            } else if id == c.game.fields.effect {
                effects_from_save(c, &bytes, now)
            } else {
                bytes
            };
            comps.push((id, c.game.fit_component(id, bytes)));
        }
    }
    let nb = r.u32()?;
    let blocks = if nb == u32::MAX {
        None
    } else {
        let shifted = nb & (1 << 31) != 0;
        let nb = nb & 0x7fff_ffff;
        if nb as usize > r.0.len() / 12 {
            return None;
        }
        let mut v = Vec::with_capacity(nb as usize);
        for _ in 0..nb {
            let p = IVec3::new(r.u32()? as i32, r.u32()? as i32, r.u32()? as i32);
            let offset = if shifted { IVec3::new(r.u32()? as i32, r.u32()? as i32, r.u32()? as i32) } else { IVec3::ZERO };
            // Blocks of removed mods are left out.
            if let Some(s) = c.state_by_name(&r.str()?) {
                v.push((p, crate::world::placement::Part { state: s, offset }));
            }
        }
        Some(SavedSpace { blocks: v, timers: Vec::new() })
    };
    let Some(kind) = c.game.catalog.find(ek::ENTITY, &kind_name) else { return Some(None) };
    if kind == c.game.ship_kind
        && !comps.iter().any(|(id, _)| *id == c.game.fields.helm)
        && blocks.as_ref().is_some_and(|space| {
            space.blocks.iter().any(|&(pos, state)| {
                pos == IVec3::ZERO && c.states[state.state as usize].signal.flags as u32 & weft_abi::signal_flags::HELM != 0
            })
        })
    {
        comps.push((c.game.fields.helm, IVec3::ZERO.to_bytes().into_boxed_slice()));
    }
    let mut e = Entity::new(id, kind, DVec3::new(v[0], v[1], v[2]), health);
    e.selected = selected;
    e.flying = flying;
    e.fire = fire;
    e.mode = mode;
    e.vel = DVec3::new(v[3], v[4], v[5]);
    e.yaw = yaw;
    e.pitch = pitch;
    e.stuck = stuck;
    e.age = age;
    e.item = item;
    e.anchor = anchor;
    e.anchor_offset = anchor_offset;
    e.frame = frame;
    e.mount = mount;
    e.completion = match attachment {
        flags if flags & LAND_ON_SUPPORT != 0 => Completion::LandOnSupport,
        flags if flags & DOCK_BODY != 0 => Completion::Dock,
        _ => Completion::Stay,
    };
    if let Some((ship, rotation)) = dynamic {
        e.ship = Some(ship);
        e.set_frame_rotation(rotation);
    } else if let Some(rotation) = rotation {
        e.set_frame_rotation(rotation);
    }
    e.floater_driven = attachment & FLOATER_DRIVEN != 0;
    if kind == c.game.item_kind && item.is_empty() {
        return Some(None);
    }
    Some(Some((e, inv, comps, blocks)))
}

/// An `EFFECT` component as saves keep it: each effect by name, with its level and the ticks it has
/// left, so a mod coming or going and time passing elsewhere change nothing.
pub fn effects_to_save(c: &Content, bytes: &[u8], now: u64) -> Vec<u8> {
    let mut out = Vec::new();
    for f in crate::world::effects_on(Some(bytes), now) {
        let Some(d) = c.game.entity_effects.get(f.id as usize) else { continue };
        out.extend((d.name.len() as u16).to_le_bytes());
        out.extend(d.name.as_bytes());
        out.extend(f.level.to_le_bytes());
        out.extend((f.until - now).min(u32::MAX as u64).to_le_bytes()[..4].iter());
    }
    out
}

/// `effects_to_save` read back for a dimension at tick `now`, but those no mod has now.
pub fn effects_from_save(c: &Content, bytes: &[u8], now: u64) -> Vec<u8> {
    let mut r = R(bytes);
    let mut list = Vec::new();
    while let Some(len) = r.u16() {
        let (Some(name), Some(level), Some(left)) = (r.take(len as usize), r.u32(), r.u32()) else { break };
        let name = String::from_utf8_lossy(name);
        if let Some(id) = c.game.catalog.find(ek::ENTITY_EFFECT, name.as_ref()) {
            let until = if left == weft_abi::INFINITE_TICKS { crate::world::FOREVER } else { now + left as u64 };
            list.push(crate::world::OnEffect { id, level, until });
        }
    }
    crate::world::encode_effects(&list).into_vec()
}
