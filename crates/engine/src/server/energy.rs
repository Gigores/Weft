//! Resource channels share topology and balancing. Each channel has a conduit tag and a
//! persistent storage component. A cable or pipe needs no tick or block data of its own.

use glam::IVec3;
use rustc_hash::{FxHashMap, FxHashSet};
use weft_abi::Energy;

use super::entity::Entities;
use crate::content::Content;
use crate::world::{Cube, DIRS};

#[derive(Default)]
pub struct Grid {
    channels: Vec<Network>,
}

#[derive(Default)]
struct Network {
    conductors: FxHashSet<IVec3>,
    nets: Vec<Vec<u64>>,
    dirty: bool,
}

pub struct Channel {
    conduit: u32,
    storage: u32,
}

pub fn freeze(game: &mut crate::registry::Game, c: &crate::components::Components, states: usize, errors: &mut Vec<String>) {
    let kind = weft_abi::entry_kind::RECORD;
    let mut records: Vec<_> = game.catalog.ids(kind).iter().collect();
    records.sort_by_key(|(_, id)| **id);
    let text = |id, name| c.id(name).and_then(|comp| c.text(kind as usize, id, comp));
    if let Some(conduit) = c.id(weft_abi::comp::CONDUCTOR) {
        game.resource_networks
            .push(Channel { conduit, storage: game.energy });
    }
    let mut storages = FxHashSet::default();
    storages.insert(game.energy);
    for (name, &id) in records {
        let Some(conduit) = text(id, "core:resource_conduit") else { continue };
        let storage = text(id, "core:resource_storage").and_then(|s| c.id(s));
        let Some(storage) =
            storage.filter(|&s| game.components[s as usize].runtime && c.schemas[s as usize] == <Energy as weft_abi::Schema>::SCHEMA)
        else {
            errors.push(format!("{name}: a resource network needs a storage instance component"));
            continue;
        };
        if !storages.insert(storage) {
            errors.push(format!("{name}: resource storage is already part of another network"));
            continue;
        }
        let Some(conduit) = c.id(conduit) else {
            // A channel can be declared by a library and only get conduits when a mod adds them.
            continue;
        };
        game.resource_networks.push(Channel { conduit, storage });
    }
    for (i, channel) in game.resource_networks.iter().enumerate() {
        for state in 0..states as u32 {
            if c.has(weft_abi::entry_kind::BLOCK as usize, state, channel.conduit) {
                game.resource_conduits.entry(state).or_default().push(i);
            }
        }
    }
}

fn read(e: &Entities, comp: u32, id: u64) -> Option<Energy> {
    let b = e.comps.get(&(id, comp))?;
    if b.len() != std::mem::size_of::<Energy>() {
        return None;
    }
    Some(unsafe { std::ptr::read_unaligned(b.as_ptr() as *const Energy) })
}

fn write(e: &mut Entities, comp: u32, id: u64, v: Energy) {
    if let Some(b) = e.comps.get_mut(&(id, comp)) {
        unsafe { std::ptr::write_unaligned(b.as_mut_ptr() as *mut Energy, v) };
    }
}

impl Grid {
    pub fn cube_loaded(&mut self, c: &Content, pos: IVec3, cube: &Cube) {
        // A machine arriving next to cables that are here already belongs to their network.
        if !self.channels.is_empty()
            && cube
                .palette()
                .iter()
                .any(|&s| c.game.has_block_data(c.states[s as usize].block))
        {
            for net in &mut self.channels {
                net.dirty |= !net.conductors.is_empty();
            }
        }
        if !cube
            .palette()
            .iter()
            .any(|s| c.game.resource_conduits.contains_key(s))
        {
            return;
        }
        self.channels
            .resize_with(c.game.resource_networks.len(), Network::default);
        for i in 0..4096usize {
            if let Some(channels) = c.game.resource_conduits.get(&cube.get(i)) {
                for &channel in channels {
                    let net = &mut self.channels[channel];
                    net.conductors
                        .insert(pos * 16 + crate::world::cube::offset(i));
                    net.dirty = true;
                }
            }
        }
    }

    pub fn cube_unloaded(&mut self, pos: IVec3) {
        for net in &mut self.channels {
            let before = net.conductors.len();
            net.conductors.retain(|p| *p >> 4 != pos);
            net.dirty |= before != net.conductors.len();
        }
    }

    pub fn block_changed(&mut self, c: &Content, p: IVec3, old: u32, new: u32) {
        let before = c.game.resource_conduits.get(&old);
        let after = c.game.resource_conduits.get(&new);
        if after.is_some() {
            self.channels
                .resize_with(c.game.resource_networks.len(), Network::default);
        }
        for (i, net) in self.channels.iter_mut().enumerate() {
            let a = before.is_some_and(|s| s.contains(&i));
            let b = after.is_some_and(|s| s.contains(&i));
            if b {
                net.conductors.insert(p);
            } else if a {
                net.conductors.remove(&p);
            }
            if a || b || DIRS.iter().any(|d| net.conductors.contains(&(p + *d))) {
                net.dirty = true;
            }
        }
    }

    pub fn tick(&mut self, e: &mut Entities, frame: u64, c: &Content, catch_up: Option<&FxHashMap<u64, u32>>) {
        for (net, channel) in self.channels.iter_mut().zip(&c.game.resource_networks) {
            net.tick(e, frame, channel.storage, catch_up);
        }
    }
}

impl Network {
    fn rebuild(&mut self, e: &Entities, frame: u64, comp: u32) {
        self.dirty = false;
        self.nets.clear();
        let mut seen: FxHashSet<IVec3> = FxHashSet::default();
        let mut order: Vec<IVec3> = self.conductors.iter().copied().collect();
        order.sort_by_key(|p| (p.x, p.y, p.z));
        for start in order {
            if !seen.insert(start) {
                continue;
            }
            let mut stack = vec![start];
            let mut ends: Vec<u64> = Vec::new();
            while let Some(p) = stack.pop() {
                let powered = |cell| {
                    e.block_ids(frame, cell)
                        .filter(|id| e.comps.contains_key(&(*id, comp)))
                };
                ends.extend(powered(p));
                for d in DIRS {
                    let n = p + d;
                    if self.conductors.contains(&n) {
                        if seen.insert(n) {
                            stack.push(n);
                        }
                    } else {
                        ends.extend(powered(n));
                    }
                }
            }
            ends.sort();
            ends.dedup();
            if ends.len() > 1 {
                self.nets.push(ends);
            }
        }
    }

    /// `catch_up`: a part of a catch-up, the machines making up ticks and how many (their rates
    /// count that many times); the others sit it out.
    fn tick(&mut self, e: &mut Entities, frame: u64, comp: u32, catch_up: Option<&FxHashMap<u64, u32>>) {
        if self.dirty {
            self.rebuild(e, frame, comp);
        }
        for net in &self.nets {
            let mut offer: Vec<(u64, i64)> = Vec::new();
            let mut want: Vec<(u64, i64)> = Vec::new();
            for &id in net {
                let Some(v) = read(e, comp, id) else { continue };
                let n = catch_up.map_or(1, |m| m.get(&id).copied().unwrap_or(0)) as i64;
                if v.max_out > 0 && v.stored > 0 {
                    offer.push((id, v.max_out.saturating_mul(n).min(v.stored)));
                }
                if v.max_in > 0 && v.stored < v.capacity {
                    want.push((
                        id,
                        v.max_in
                            .saturating_mul(n)
                            .min(v.capacity.saturating_sub(v.stored)),
                    ));
                }
            }
            let total_offer: i128 = offer.iter().map(|o| i128::from(o.1)).sum();
            let total_want: i128 = want.iter().map(|w| i128::from(w.1)).sum();
            let moved = total_offer.min(total_want).min(i128::from(i64::MAX));
            if moved <= 0 {
                continue;
            }
            for (list, sign) in [(&offer, -1i64), (&want, 1i64)] {
                let total: i128 = list.iter().map(|x| i128::from(x.1)).sum();
                let mut left = moved;
                let mut shares: Vec<(u64, i64)> = list
                    .iter()
                    .map(|&(id, amount)| {
                        let s = (i128::from(amount) * moved / total) as i64;
                        left -= i128::from(s);
                        (id, s)
                    })
                    .collect();
                for ((_, amount), (_, share)) in list.iter().zip(&mut shares) {
                    if left == 0 {
                        break;
                    }
                    if *share < *amount {
                        *share += 1;
                        left -= 1;
                    }
                }
                for (id, share) in shares {
                    if let Some(mut v) = read(e, comp, id) {
                        v.stored += sign * share;
                        write(e, comp, id, v);
                    }
                }
            }
        }
    }
}
