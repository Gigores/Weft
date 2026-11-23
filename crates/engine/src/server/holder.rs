//! What any `abi::Holder` holds, reached one way: an entity's inventory and components, a
//! block's through its block entity, a stack's through its `contents` and stack components,
//! written back into the slot the stack sits in. Storages are more inventories, kept in a
//! component of whoever carries them.

use weft_abi::{self as abi, InventorySlot, ItemMove, ItemStack, slot_flags as sf};

use super::dim::Dim;
use crate::content::Content;
use crate::stack;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum At {
    Entity(u64),
    /// The stack in `slot` of `entity`'s inventory.
    Stack {
        entity: u64,
        slot: u32,
    },
}

pub fn resolve(d: &Dim, space_id: u64, h: &abi::Holder) -> Option<At> {
    match h.kind {
        abi::holder::ENTITY => d
            .entities
            .get(h.entity)
            .filter(|e| !e.removed)
            .map(|_| At::Entity(h.entity)),
        abi::holder::BLOCK => d
            .anchored(space_id, crate::world::ivec(h.pos))
            .map(At::Entity),
        abi::holder::HELD => d
            .entities
            .inventories
            .get(&h.entity)
            .filter(|i| (h.slot as usize) < i.len())
            .map(|_| At::Stack { entity: h.entity, slot: h.slot }),
        _ => None,
    }
}

/// An inventory: the one a holder has itself (`storage` 0), or one of the storages it carries.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Inv {
    pub at: At,
    pub storage: u32,
}

impl Inv {
    pub fn own(at: At) -> Inv {
        Inv { at, storage: 0 }
    }

    pub(super) fn carrier(self) -> Option<(Inv, u32)> {
        let At::Stack { entity, slot } = self.at else { return None };
        Some((Inv::own(At::Entity(entity)), slot))
    }
}

/// The inventory `h` points at, `None` when it isn't there: a storage that isn't one, or an
/// entity that doesn't carry it. Stacks can carry any storage.
pub fn inventory(d: &Dim, space_id: u64, c: &Content, h: &abi::Holder) -> Option<Inv> {
    let inv = Inv { at: resolve(d, space_id, h)?, storage: h.storage };
    let carried = match inv.at {
        _ if h.storage == 0 => true,
        At::Entity(id) => storage_slots(c, h.storage).is_some() && d.entities.comps.contains_key(&(id, h.storage)),
        At::Stack { .. } => storage_slots(c, h.storage).is_some(),
    };
    carried.then_some(inv)
}

/// Slots of storage `comp`, `None` if it isn't one.
pub fn storage_slots(c: &Content, comp: u32) -> Option<u32> {
    c.game
        .components
        .get(comp as usize)
        .filter(|d| d.flags & abi::component_flags::STORAGE != 0)
        .map(|d| d.size / STACK_BYTES as u32)
}

/// Slots each stack of `item` carries (its `inventory` component), 0 for most items.
pub fn item_slots(c: &Content, item: u32) -> u32 {
    c.item_value::<u32>(item, c.ids.inventory)
        .unwrap_or(0)
        .min(1 << 12)
}

/// Bytes of one stack in a component holding stacks: item, count, data.
const STACK_BYTES: usize = 12;

fn read_u32(b: &[u8], i: usize) -> u32 {
    b.get(i..i + 4)
        .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// `n` stacks out of a component's bytes; what is missing or unknown is empty.
fn decode(c: &Content, bytes: &[u8], n: u32) -> Vec<ItemStack> {
    (0..n as usize)
        .map(|i| {
            let at = i * STACK_BYTES;
            let s = ItemStack { item: read_u32(bytes, at), count: read_u32(bytes, at + 4), data: read_u32(bytes, at + 8) };
            if s.is_empty() || s.item as usize >= c.items.len() { ItemStack::EMPTY } else { s }
        })
        .collect()
}

fn encode(list: &[ItemStack]) -> Vec<u8> {
    list.iter()
        .flat_map(|s| {
            let s = if s.is_empty() { ItemStack::EMPTY } else { *s };
            [s.item, s.count, s.data]
        })
        .flat_map(u32::to_le_bytes)
        .collect()
}

fn stack_at(d: &Dim, entity: u64, slot: u32) -> ItemStack {
    d.entities
        .inventories
        .get(&entity)
        .and_then(|i| i.get(slot as usize))
        .copied()
        .unwrap_or_default()
}

/// The component an inventory other than an entity's own is kept in: a stack's own is its
/// `contents`.
fn kept_in(c: &Content, inv: Inv) -> Option<u32> {
    if inv.storage == 0 { c.ids.contents } else { Some(inv.storage) }
}

fn stored(d: &Dim, c: &Content, inv: Inv) -> Vec<ItemStack> {
    let bytes = kept_in(c, inv).and_then(|comp| component(d, inv.at, comp));
    decode(c, bytes.as_deref().unwrap_or(&[]), len(d, c, inv))
}

/// Writes back what `stored` read; a stack holding nothing drops the component.
fn store(d: &mut Dim, c: &Content, inv: Inv, list: &[ItemStack]) {
    let Some(comp) = kept_in(c, inv) else { return };
    let bytes = encode(list);
    let empty = matches!(inv.at, At::Stack { .. }) && list.iter().all(ItemStack::is_empty);
    set_component(d, c, inv.at, comp, (!empty).then_some(&bytes[..]));
}

pub fn len(d: &Dim, c: &Content, inv: Inv) -> u32 {
    match (inv.at, inv.storage) {
        (At::Entity(id), 0) => d
            .entities
            .inventories
            .get(&id)
            .map_or(0, |i| i.len() as u32),
        (At::Stack { entity, slot }, 0) => item_slots(c, stack_at(d, entity, slot).item),
        (At::Entity(id), comp) if !d.entities.comps.contains_key(&(id, comp)) => 0,
        (At::Stack { entity, slot }, _) if stack_at(d, entity, slot).is_empty() => 0,
        (_, comp) => storage_slots(c, comp).unwrap_or(0),
    }
}

pub fn get(d: &Dim, c: &Content, inv: Inv, i: u32) -> ItemStack {
    match (inv.at, inv.storage) {
        (At::Entity(id), 0) => stack_at(d, id, i),
        _ => stored(d, c, inv)
            .get(i as usize)
            .copied()
            .unwrap_or_default(),
    }
}

pub fn set(d: &mut Dim, c: &Content, inv: Inv, i: u32, s: ItemStack) {
    match (inv.at, inv.storage) {
        (At::Entity(id), 0) => d.entities.set_slot(id, i, s),
        _ => {
            let mut list = stored(d, c, inv);
            if let Some(e) = list.get_mut(i as usize) {
                *e = s;
                store(d, c, inv, &list);
            }
        }
    }
}

/// Merges into matching stacks, then empty slots. Returns the rest.
pub fn insert(d: &mut Dim, c: &Content, inv: Inv, stack: ItemStack) -> ItemStack {
    match (inv.at, inv.storage) {
        (At::Entity(id), 0) => d.entities.insert_item(c, id, stack),
        _ => {
            let mut list = stored(d, c, inv);
            let rest = stack::insert_into(
                stack,
                0..list.len(),
                |i, value| {
                    if let Some(value) = value {
                        list[i] = value;
                    }
                    list[i]
                },
                |_, _| c.max_stack(&stack),
            );
            if rest != stack {
                store(d, c, inv, &list);
            }
            rest
        }
    }
}

type Slot = (Inv, u32);

fn resolve_slot(d: &Dim, c: &Content, slot: InventorySlot) -> Option<Slot> {
    if !d.has_space(slot.frame) {
        return None;
    }
    let inv = inventory(d, slot.frame, c, &slot.holder)?;
    (slot.slot < len(d, c, inv)).then_some((inv, slot.slot))
}

fn slot_group<'a>(d: &Dim, c: &'a Content, (inv, slot): Slot) -> Option<&'a crate::registry::GroupDef> {
    let Inv { at: At::Entity(id), storage: 0 } = inv else { return None };
    let entity = d.entities.get(id)?;
    let layout = &c.game.player_layout;
    if entity.player {
        return layout.storage_at(slot).or_else(|| {
            layout
                .equipment
                .get(slot.checked_sub(layout.worn.start)? as usize)
                .map(|part| &part.group)
        });
    }
    let frame = entity.block_frame();
    let anchor = entity.anchor.filter(|_| d.has_space(frame))?;
    let layout = d.space(frame).layout(anchor);
    let block = layout
        .residents(d.get(frame, anchor)?)
        .map(|part| (part.offset, c.states[part.state as usize].block))
        .find(|&(offset, block)| offset == entity.anchor_offset && c.game.block_entities.get(&block) == Some(&entity.kind))?
        .1;
    let menu = c.blocks[block as usize].opens?;
    c.game.menus[menu as usize]
        .groups
        .iter()
        .find(|g| g.owner == abi::slot_owner::HOLDER && slot >= g.start && slot - g.start < g.cols * g.rows)
}

pub(super) fn fits(c: &Content, flags: u32, in_stack: bool, group: Option<&crate::registry::GroupDef>, item: u32) -> bool {
    flags & sf::LOCKED == 0
        && (flags & sf::OFF_HAND == 0 || crate::content::fits_one_hand(c, item))
        && (!in_stack || item_slots(c, item) == 0)
        && group.is_none_or(|g| g.accepts(item, c.item_fluid.contains_key(&item)))
}

pub(super) fn takeable(flags: u32) -> bool {
    flags & (sf::INPUT | sf::LOCKED | sf::LIQUID) == 0
}

fn insertion_limit(d: &Dim, c: &Content, key @ (inv, _): Slot, stack: ItemStack) -> u32 {
    let group = slot_group(d, c, key);
    let flags = group.map_or(0, |g| g.flags);
    if flags & (sf::OUTPUT | sf::TRASH) != 0 || !fits(c, flags, matches!(inv.at, At::Stack { .. }), group, stack.item) {
        return 0;
    }
    let max = c.max_stack(&stack);
    group.map_or(max, |g| g.capacity(max))
}

pub fn move_into(d: &mut Dim, c: &Content, from: InventorySlot, to: abi::Holder, frame: u64) -> u32 {
    let (Some((source, i)), Some(target)) = (resolve_slot(d, c, from), inventory(d, frame, c, &to)) else { return 0 };
    let stack = get(d, c, source, i);
    if stack.is_empty() {
        return 0;
    }
    let before: Vec<_> = (0..len(d, c, target))
        .map(|k| {
            let stack = get(d, c, target, k);
            if stack.is_empty() { ItemStack::EMPTY } else { stack }
        })
        .collect();
    let mut after = before.clone();
    let rest = stack::insert_into(
        stack,
        (0..before.len()).filter(|&k| source != target || k != i as usize),
        |k, value| {
            if let Some(value) = value {
                after[k] = value;
            }
            after[k]
        },
        |k, _| insertion_limit(d, c, (target, k as u32), stack),
    );
    if rest.count == stack.count {
        return 0;
    }
    let plan: Vec<_> = before
        .iter()
        .zip(after)
        .enumerate()
        .filter_map(|(k, (old, new))| {
            (old.count < new.count).then(|| ItemMove::new(from, InventorySlot { holder: to, frame, slot: k as u32 }, new.count - old.count))
        })
        .collect();
    if move_items(d, c, &plan) { stack.count - rest.count } else { 0 }
}

pub fn move_items(d: &mut Dim, c: &Content, moves: &[ItemMove<InventorySlot>]) -> bool {
    let mut resolved = Vec::with_capacity(moves.len());
    let mut touched = rustc_hash::FxHashSet::default();
    for m in moves {
        let (Some(from), Some(to)) = (resolve_slot(d, c, m.from), resolve_slot(d, c, m.to)) else { return false };
        if m.count == 0 || from == to {
            continue;
        }
        if slot_group(d, c, from).is_some_and(|g| !takeable(g.flags) || g.flags & sf::TRASH != 0) {
            return false;
        }
        touched.extend([from, to]);
        resolved.push(ItemMove::new(from, to, m.count));
    }
    // Applying a change to the carrier would change the identity of the inventory it holds.
    if touched.iter().any(|&(inv, _)| {
        inv.carrier()
            .is_some_and(|parent| touched.contains(&parent))
    }) {
        return false;
    }
    let Some(changes) = stack::plan_moves(&resolved, |(inv, i)| get(d, c, inv, i), |key, stack| insertion_limit(d, c, key, stack)) else {
        return false;
    };
    for ((inv, i), stack) in changes {
        set(d, c, inv, i, stack);
    }
    true
}

/// The holder's `comp`, as bytes.
pub fn component(d: &Dim, at: At, comp: u32) -> Option<Box<[u8]>> {
    match at {
        At::Entity(id) => d.entities.comps.get(&(id, comp)).cloned(),
        At::Stack { entity, slot } => stack::value(stack_at(d, entity, slot).data, comp).map(Into::into),
    }
}

/// Sets the holder's `comp` (`None` takes it off a stack). False if it can't: an entity
/// without it, or bytes of another size than its kind's.
pub fn set_component(d: &mut Dim, c: &Content, at: At, comp: u32, v: Option<&[u8]>) -> bool {
    match at {
        At::Entity(id) => {
            let size = c
                .game
                .components
                .get(comp as usize)
                .map_or(0, |def| def.size as usize);
            match (d.entities.comps.get_mut(&(id, comp)), v) {
                (Some(old), Some(v)) if size == 0 || v.len() == size => {
                    *old = v.into();
                    true
                }
                _ => false,
            }
        }
        At::Stack { entity, slot } => {
            let s = stack_at(d, entity, slot);
            if s.is_empty() {
                return false;
            }
            d.entities
                .set_slot(entity, slot, ItemStack { data: stack::with(s.data, comp, v), ..s });
            true
        }
    }
}

/// What of `amount` (in positive, out negative) a store of `stored` out of `capacity` takes or
/// gives within its rate limits, with the same sign.
fn bounded(amount: i64, stored: i64, capacity: i64, max_in: i64, max_out: i64) -> i64 {
    if amount >= 0 {
        amount.min(max_in.max(0)).min((capacity - stored).max(0))
    } else {
        -(amount.saturating_neg())
            .min(max_out.max(0))
            .min(stored.max(0))
    }
}

fn read<T: Copy>(d: &Dim, at: At, comp: u32) -> Option<T> {
    let b = component(d, at, comp).filter(|b| b.len() >= size_of::<T>())?;
    Some(unsafe { std::ptr::read_unaligned(b.as_ptr() as *const T) })
}

fn write<T: Copy>(d: &mut Dim, c: &Content, at: At, comp: u32, v: &T) {
    let bytes = unsafe { std::slice::from_raw_parts(v as *const T as *const u8, size_of::<T>()) };
    set_component(d, c, at, comp, Some(bytes));
}

/// Moves `amount` in (positive) or out (negative) within the holder's limits; returns what
/// moved, with the same sign.
pub fn move_energy(d: &mut Dim, c: &Content, at: At, amount: i64, simulate: bool) -> i64 {
    let Some(comp) = c.ids.energy else { return 0 };
    let Some(mut v) = read::<abi::Energy>(d, at, comp) else { return 0 };
    let n = bounded(amount, v.stored, v.capacity, v.max_in, v.max_out);
    if !simulate && n != 0 {
        v.stored += n;
        write(d, c, at, comp, &v);
    }
    n
}
