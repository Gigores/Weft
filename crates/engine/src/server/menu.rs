//! Container menus: a list of slots pointing into inventories (the player's, the one it opened
//! on or the menu's own temporary grid), the cursor stack, and synced integers. The slots come from the
//! menu's groups; click handling is generic, mods only describe groups and react to changes
//! (crafting).

use weft_abi::{self as abi, ItemStack, slot_flags as sf};

use super::dim::Dim;
use super::holder::{self, At, Inv};
use crate::content::Content;
use crate::host;
use crate::net::ToClient;
use crate::registry::{GroupDef, Use};
use crate::stack::TransferTarget;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Owner {
    Temp,
    Player,
    /// The player's trash (`slot_flags::TRASH`).
    Trash,
    /// The inventory the menu opened on.
    On(Inv),
}

impl Owner {
    fn backing(self, player: u64) -> Self {
        if self == Owner::On(Inv::own(At::Entity(player))) { Owner::Player } else { self }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SlotRef {
    pub owner: Owner,
    pub index: u32,
    pub flags: u32,
    pub group: &'static GroupDef,
}

pub struct OpenMenu {
    /// Opened by using a block, so it closes when the player walks away. A mod opening it from
    /// code (a remote terminal) means it to work from anywhere.
    pub on_block: bool,
    pub kind: u32,
    pub on: Option<Inv>,
    /// Opened on a stack: the item it was, the menu closes once that stack is gone.
    pub held_item: u32,
    pub slots: Vec<SlotRef>,
    pub temp: Vec<ItemStack>,
    pub cursor: ItemStack,
    pub sent_slots: Vec<ItemStack>,
    pub sent_cursor: ItemStack,
    /// The owner's menu-synced components went out once.
    pub comps_sent: bool,
}

/// What `MenuCtx` pointers handed to mods point at.
pub struct MenuCall {
    pub menu: *mut OpenMenu,
    pub dim: *mut Dim,
    pub player: u64,
}

impl MenuCall {
    fn m(&self) -> &mut OpenMenu {
        unsafe { &mut *self.menu }
    }

    fn d(&self) -> &mut Dim {
        unsafe { &mut *self.dim }
    }

    /// First menu slot and slot count of group `name`.
    /// (first menu slot, slot count, columns) of group `name`.
    pub fn group(&self, c: &Content, name: &str) -> Option<(u32, u32, u32)> {
        let layout = &c.game.player_layout;
        c.game.menus[self.m().kind as usize]
            .group(layout, name)
            .map(|g| (g.1, g.2, g.0.span(layout).2))
    }

    pub fn get(&self, slot: u32) -> ItemStack {
        get(self.m(), self.d(), self.player, slot)
    }

    pub fn set(&mut self, slot: u32, stack: ItemStack) {
        set(self.m(), self.d(), self.player, slot, stack)
    }
}

impl OpenMenu {
    /// The entity it opened on (holding the stack it opened on), 0 for none.
    pub fn owner(&self) -> u64 {
        match self.on.map(|inv| inv.at) {
            Some(At::Entity(id) | At::Stack { entity: id, .. }) => id,
            None => 0,
        }
    }
}

fn get(m: &OpenMenu, dim: &Dim, player: u64, slot: u32) -> ItemStack {
    let Some(r) = m.slots.get(slot as usize) else { return ItemStack::EMPTY };
    let c = crate::content::content();
    match r.owner {
        Owner::Temp => m.temp.get(r.index as usize).copied().unwrap_or_default(),
        Owner::Trash => trash(dim, c, player),
        Owner::Player => holder::get(dim, c, Inv::own(At::Entity(player)), r.index),
        Owner::On(inv) => holder::get(dim, c, inv, r.index),
    }
}

fn set(m: &mut OpenMenu, dim: &mut Dim, player: u64, slot: u32, stack: ItemStack) {
    let Some(r) = m.slots.get(slot as usize).copied() else { return };
    let stack = if stack.count == 0 { ItemStack::EMPTY } else { stack };
    match r.owner {
        Owner::Temp => {
            if let Some(s) = m.temp.get_mut(r.index as usize) {
                *s = stack;
            }
        }
        Owner::Trash => {
            let comp = crate::content::content().game.fields.trash;
            let bytes: Box<[u8]> = [stack.item, stack.count, stack.data]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect();
            dim.entities.comps.insert((player, comp), bytes);
        }
        Owner::Player => dim.entities.set_slot(player, r.index, stack),
        Owner::On(inv) => holder::set(dim, crate::content::content(), inv, r.index, stack),
    }
}

/// What `player` last put in its trash.
fn trash(dim: &Dim, c: &Content, player: u64) -> ItemStack {
    let Some(b) = dim
        .entities
        .comps
        .get(&(player, c.game.fields.trash))
        .filter(|b| b.len() >= 12)
    else {
        return ItemStack::EMPTY;
    };
    let word = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    ItemStack { item: word(0), count: word(4), data: word(8) }
}

fn call(
    m: &mut OpenMenu,
    dim: &mut Dim,
    c: &Content,
    player: u64,
    slot: u32,
    pick: impl Fn(&crate::registry::MenuDef) -> Option<(abi::ModId, abi::Callback)>,
) {
    call_times(m, dim, c, player, slot, 1, pick);
}

/// `call` telling `times` (`abi::MenuArgs::times`).
fn call_times(
    m: &mut OpenMenu,
    dim: &mut Dim,
    c: &Content,
    player: u64,
    slot: u32,
    times: u32,
    pick: impl Fn(&crate::registry::MenuDef) -> Option<(abi::ModId, abi::Callback)>,
) {
    let def = &c.game.menus[m.kind as usize];
    let Some((by, cb)) = pick(def) else { return };
    let owner = match m.on {
        None => abi::Holder::NONE,
        Some(Inv { at: At::Entity(id), storage }) => abi::Holder { storage, ..abi::Holder::entity(id) },
        Some(Inv { at: At::Stack { entity, slot }, storage }) => abi::Holder { storage, ..abi::Holder::held(entity, slot) },
    };
    let mut call = MenuCall { menu: m as *mut OpenMenu, dim: dim as *mut Dim, player };
    let frame = match m.on {
        Some(Inv { at: At::Entity(id), .. }) => dim
            .entities
            .get(id)
            .map_or(0, |entity| entity.block_frame()),
        _ => 0,
    };
    let mut ctx = abi::WorldCtx { region: (dim as *mut Dim).cast(), frame };
    let mut args = abi::MenuArgs { world: &mut ctx, menu: &mut call as *mut MenuCall as *mut abi::MenuCtx, player, owner, slot, times };
    host::invoke(by, cb, &mut args);
}

/// Resolves each group against the inventories shown in this menu.
fn slots(dim: &Dim, c: &'static Content, kind: u32, on: Option<Inv>, player: u64) -> Vec<SlotRef> {
    use abi::slot_owner as so;
    let def = &c.game.menus[kind as usize];
    let layout = &c.game.player_layout;
    let mut out = Vec::new();
    let holder = Owner::On(on.unwrap_or(Inv::own(At::Entity(0))));
    let holder_len = on.map_or(0, |inv| holder::len(dim, c, inv));
    let player_len = holder::len(dim, c, Inv::own(At::Entity(player)));
    let worn = worn_on(dim, c, on);
    let mut add = |g: &'static GroupDef| {
        let (owner, first) = match (g.owner, worn) {
            _ if g.flags & sf::TRASH != 0 => (Owner::Trash, 0),
            (so::MENU, _) => (Owner::Temp, g.start),
            (so::HOLDER, _) => (holder, g.start),
            (so::WORN, Some(w)) => (holder, w + g.start),
            (so::WORN, None) => (Owner::Player, layout.worn.start + g.start),
            _ => (Owner::Player, g.start),
        };
        let len = match owner {
            Owner::On(_) => holder_len,
            Owner::Player => player_len,
            Owner::Temp => def.temp_slots,
            Owner::Trash => 1,
        };
        for index in first..first + g.cols * g.rows {
            // The stack the menu is open on can't be moved while it is.
            let locked = owner == Owner::Player && on.is_some_and(|inv| inv.at == At::Stack { entity: player, slot: index });
            let flags =
                if locked || index >= len || g.flags & sf::LOCKED != 0 { (g.flags | sf::LOCKED) & !sf::ALLOW_SORTING } else { g.flags };
            out.push(SlotRef { owner, index, flags, group: g });
        }
    };
    for g in &def.groups {
        if matches!(g.owner, so::PLAYER_MAIN | so::PLAYER_HOTBAR) {
            for storage in layout.storage.iter().filter(|s| s.owner == g.owner) {
                add(storage);
            }
        } else {
            add(g);
        }
    }
    out
}

/// Where the equipment of the entity `on` is starts in its inventory, if it wears.
fn worn_on(dim: &Dim, c: &Content, on: Option<Inv>) -> Option<u32> {
    let Some(Inv { at: At::Entity(id), storage: 0 }) = on else { return None };
    let w = &c.game.entities[dim.entities.get(id)?.kind as usize].worn;
    (!w.is_empty()).then_some(w.start)
}

pub fn open(dim: &mut Dim, c: &'static Content, player: u64, kind: u32, on: Option<Inv>) -> OpenMenu {
    let def = &c.game.menus[kind as usize];
    let held_item = match on.map(|inv| inv.at) {
        Some(At::Stack { entity, slot }) => holder::get(dim, c, Inv::own(At::Entity(entity)), slot).item,
        _ => 0,
    };
    let mut m = OpenMenu {
        on_block: false,
        kind,
        on,
        held_item,
        slots: slots(dim, c, kind, on, player),
        temp: vec![ItemStack::EMPTY; def.temp_slots as usize],
        cursor: ItemStack::EMPTY,
        sent_slots: Vec::new(),
        sent_cursor: ItemStack::EMPTY,
        comps_sent: false,
    };
    changed(&mut m, dim, c, player, 0);
    m
}

/// Whether what the menu is on is gone or too far away: modern's reach to a block plus 4. Virtual
/// inventories have no place to walk away from.
pub fn out_of_reach(m: &OpenMenu, dim: &Dim, player: u64) -> bool {
    let Some(on) = m.on else { return false };
    let Some(owner) = dim.entities.get(m.owner()).filter(|o| !o.removed) else { return true };
    if let At::Stack { entity, slot } = on.at {
        let now = holder::get(dim, crate::content::content(), Inv::own(At::Entity(entity)), slot);
        return now.item != m.held_item || now.is_empty();
    }
    if owner.anchor.is_none() || !m.on_block {
        return false;
    }
    dim.entities
        .get(player)
        .is_none_or(|p| (p.pos - owner.pos).length_squared() > 8.5 * 8.5)
}

pub fn tick(m: &mut OpenMenu, dim: &mut Dim, c: &Content, player: u64) {
    call(m, dim, c, player, 0, |d| d.on_tick);
}

/// Returns temp slots and the cursor to the player (dropping what does not fit). Output slots are
/// previews, not real items, and are discarded.
pub fn close(m: OpenMenu, dim: &mut Dim, c: &'static Content, player: u64) {
    let outputs: Vec<u32> = m
        .slots
        .iter()
        .filter(|s| s.owner == Owner::Temp && s.flags & sf::OUTPUT != 0)
        .map(|s| s.index)
        .collect();
    let mut back: Vec<ItemStack> = m
        .temp
        .iter()
        .enumerate()
        .filter(|(i, s)| !s.is_empty() && !outputs.contains(&(*i as u32)))
        .map(|(_, s)| *s)
        .collect();
    back.push(m.cursor);
    for s in back.into_iter().filter(|s| !s.is_empty()) {
        let rest = dim.entities.insert_item(c, player, s);
        if !rest.is_empty() {
            if let Some(p) = dim.entities.get(player).map(|e| e.pos) {
                dim.spawn_item(0, c, p + glam::DVec3::Y, rest);
            }
        }
    }
}

fn fits(c: &Content, s: &SlotRef, item: u32) -> bool {
    if s.flags & sf::LOCKED != 0 || (s.flags & sf::OFF_HAND != 0 && !crate::content::fits_one_hand(c, item)) {
        return false;
    }
    // A stack holding items doesn't go into another one.
    if matches!(s.owner, Owner::On(Inv { at: At::Stack { .. }, .. })) && holder::item_slots(c, item) > 0 {
        return false;
    }
    s.group.accepts(item, c.item_fluid.contains_key(&item))
}

/// Most of `stack` slot `s` holds.
fn cap(c: &Content, s: &SlotRef, stack: &ItemStack) -> u32 {
    s.group.capacity(c.max_stack(stack))
}

/// Whether a player may take items out of `s` by hand.
fn takeable(s: &SlotRef) -> bool {
    s.flags & (sf::INPUT | sf::LOCKED | sf::LIQUID) == 0
}

fn can_replace(c: &Content, s: &SlotRef, old: ItemStack, new: ItemStack) -> bool {
    (old.is_empty() || takeable(s)) && (new.is_empty() || s.flags & sf::OUTPUT == 0) && fits(c, s, new.item) && new.count <= cap(c, s, &new)
}

/// What a bucket holds.
const CONTAINER_MB: u32 = 1000;

/// A click on a liquid slot with a container on the cursor: a full one pours into it, an empty
/// one fills from it, a bucket at a time.
fn liquid_click(m: &mut OpenMenu, dim: &mut Dim, c: &'static Content, player: u64, slot: u32) {
    let (Some(s), cursor) = (m.slots.get(slot as usize).copied(), m.cursor) else { return };
    if cursor.is_empty() || s.flags & sf::LOCKED != 0 {
        return;
    }
    let here = get(m, dim, player, slot);
    let capacity = if s.group.limit > 0 { s.group.limit } else { CONTAINER_MB };
    let (new_slot, new_cursor) = if let Some(&(l, empty)) = c.item_liquid.get(&cursor.item) {
        // Full containers don't stack; one at a time.
        if cursor.count != 1 || s.flags & sf::OUTPUT != 0 {
            return;
        }
        let fluid = c.fluid_items.get(l as usize).copied().unwrap_or(0);
        if fluid == 0 || !fits(c, &s, fluid) || (!here.is_empty() && here.item != fluid) || here.count + CONTAINER_MB > capacity {
            return;
        }
        let poured = ItemStack { item: fluid, count: here.count + CONTAINER_MB, ..here };
        (poured, if empty == 0 { ItemStack::EMPTY } else { ItemStack::new(empty, 1) })
    } else {
        let Some(&l) = c.item_fluid.get(&here.item) else { return };
        let Some(&filled) = c.containers.get(&(cursor.item, l)) else { return };
        if here.count < CONTAINER_MB {
            return;
        }
        let left = here.count - CONTAINER_MB;
        let rest = if left == 0 { ItemStack::EMPTY } else { ItemStack { count: left, ..here } };
        if cursor.count == 1 {
            (rest, ItemStack::new(filled, 1))
        } else {
            // One of a stack of empty ones: the full one goes into the inventory.
            if !dim
                .entities
                .insert_item(c, player, ItemStack::new(filled, 1))
                .is_empty()
            {
                return;
            }
            (rest, ItemStack { count: cursor.count - 1, ..cursor })
        }
    };
    let taken = new_slot.count < here.count;
    set(m, dim, player, slot, new_slot);
    m.cursor = new_cursor;
    if taken && s.flags & sf::OUTPUT != 0 {
        took(m, dim, c, player, slot, 1);
    }
    changed(m, dim, c, player, slot);
}

/// `button` 2..=10 over a slot: number key 1..9, swaps it with that hotbar slot (vanilla).
pub const HOTBAR_SWAP: u8 = 2;
/// Double click: gathers matching items from every slot onto the cursor, up to a full stack.
pub const GATHER: u8 = 11;
/// Middle click in creative: a full stack of what is in the slot onto an empty cursor.
pub const CLONE: u8 = 12;
/// The drop key over a slot with nothing on the cursor: one of its items thrown out (vanilla's Q).
pub const THROW: u8 = 13;
/// The same with the control key: the whole stack.
pub const THROW_ALL: u8 = 14;
/// Swept over with the button still held after picking a stack up: what the slot has of the
/// same item joins the cursor, up to a full stack (Mouse Tweaks').
pub const PICK_SAME: u8 = 15;
/// The wheel up over a slot: an item of what it holds (or, empty, of the first that fits) comes
/// in from the other side, as a shift-click sends it; with shift a stack (Mouse Tweaks').
pub const WHEEL_UP: u8 = 19;
/// The wheel down: an item of the slot goes to the other side, with shift all of it.
pub const WHEEL_DOWN: u8 = 20;

/// Sorts each selected storage group independently, skipping locked slots inside it.
fn sort(m: &mut OpenMenu, dim: &mut Dim, c: &'static Content, player: u64, slot: u32) {
    let sortable = |s: &SlotRef| s.flags & sf::ALLOW_SORTING != 0;
    let on = m
        .slots
        .iter()
        .any(|s| matches!(s.owner, Owner::On(_)) && sortable(s));
    if slot != u32::MAX && !m.slots.get(slot as usize).is_some_and(sortable) {
        return;
    }
    let mut first = 0;
    while first < m.slots.len() as u32 {
        let r = m.slots[first as usize];
        let end = first + r.group.cols * r.group.rows;
        let selected = if slot != u32::MAX {
            (first..end).contains(&slot)
        } else if on {
            matches!(r.owner, Owner::On(_))
        } else {
            r.owner == Owner::Player && r.group.owner == abi::slot_owner::PLAYER_MAIN
        };
        if selected && r.group.flags & sf::ALLOW_SORTING != 0 {
            let picked = (first..end)
                .filter(|&i| sortable(&m.slots[i as usize]))
                .collect();
            sort_group(m, dim, c, player, picked);
        }
        first = end;
    }
}

fn sort_group(m: &mut OpenMenu, dim: &mut Dim, c: &'static Content, player: u64, mut slots: Vec<u32>) {
    let Some(&first) = slots.first() else { return };
    let mut stacks: Vec<_> = slots
        .iter()
        .map(|&i| get(m, dim, player, i))
        .filter(|s| !s.is_empty())
        .collect();
    stacks.sort_by_key(|s| (s.item, s.data));
    let mut packed = Vec::with_capacity(stacks.len());
    for same in stacks.chunk_by(|a, b| (a.item, a.data) == (b.item, b.data)) {
        let stack = same[0];
        let limit = u64::from(cap(c, &m.slots[first as usize], &stack));
        let mut count: u64 = same.iter().map(|s| u64::from(s.count)).sum();
        // A mod may have lowered the limit since these stacks were stored.
        if count.div_ceil(limit) > (slots.len() - packed.len()) as u64 {
            return;
        }
        while count != 0 {
            let n = count.min(limit);
            packed.push(ItemStack { count: n as u32, ..stack });
            count -= n;
        }
    }
    let mut packed = packed.into_iter();
    slots.retain(|&i| {
        let stack = packed.next().unwrap_or(ItemStack::EMPTY);
        if get(m, dim, player, i) == stack {
            return false;
        }
        set(m, dim, player, i, stack);
        true
    });
    // Callbacks see the finished group.
    for i in slots {
        changed(m, dim, c, player, i);
    }
}

/// A creative player's number key over an entry: a full stack of `item` in that hotbar slot,
/// whatever was there gone (vanilla's).
pub fn creative_to_hotbar(m: &mut OpenMenu, dim: &mut Dim, c: &'static Content, player: u64, hot: u32, item: u32) {
    if hot >= c.game.player_layout.hotbar {
        return;
    }
    let Some(h) = m
        .slots
        .iter()
        .position(|s| s.owner == Owner::Player && s.index == hot)
        .map(|i| i as u32)
    else {
        return;
    };
    let r = &m.slots[h as usize];
    let stack = ItemStack::new(item, cap(c, r, &ItemStack::new(item, 1)));
    if !can_replace(c, r, get(m, dim, player, h), stack) {
        return;
    }
    set(m, dim, player, h, stack);
    changed(m, dim, c, player, h);
}

/// The trash key over a slot: its stack goes to the player's trash, what was there gone.
pub const TO_TRASH: u8 = 17;
/// The other hand's key over a slot: swaps it with the other hand (vanilla's F).
pub const OFF_HAND_SWAP: u8 = 18;

/// Sorts the storage slot `slot` is in (`client_op::MENU_SORT`); `u32::MAX`: what the menu is
/// opened on, else the player's main inventory.
pub const SORT: u8 = 16;

/// `button`: 0 left, 1 right, or `HOTBAR_SWAP + n`, `GATHER`, `CLONE`, `THROW`, `THROW_ALL`,
/// `PICK_SAME`, `SORT`, `WHEEL_UP`, `WHEEL_DOWN`. `slot == u32::MAX` means outside the window
/// (drop the cursor).
pub fn click(m: &mut OpenMenu, dim: &mut Dim, c: &'static Content, player: u64, slot: u32, button: u8, shift: bool) {
    if button == SORT {
        sort(m, dim, c, player, slot);
        return;
    }
    if m.slots
        .get(slot as usize)
        .is_some_and(|s| s.flags & sf::LIQUID != 0)
    {
        if button <= 1 && !shift {
            liquid_click(m, dim, c, player, slot);
        }
        return;
    }
    if button == WHEEL_UP || button == WHEEL_DOWN {
        let Some(r) = m.slots.get(slot as usize).copied() else { return };
        let n = if shift { u32::MAX } else { 1 };
        if r.flags & (sf::OUTPUT | sf::LOCKED) != 0 || r.owner == Owner::Trash {
            return;
        }
        if button == WHEEL_UP {
            pull_in(m, dim, c, player, slot, n);
        } else if takeable(&r) {
            move_out(m, dim, c, player, slot, n);
        }
        return;
    }
    if (HOTBAR_SWAP..HOTBAR_SWAP + 9).contains(&button) || button == OFF_HAND_SWAP {
        let hot = button.wrapping_sub(HOTBAR_SWAP) as u32;
        if button != OFF_HAND_SWAP && hot >= c.game.player_layout.hotbar {
            return;
        }
        let Some(h) = m
            .slots
            .iter()
            .position(|s| match button {
                OFF_HAND_SWAP => s.flags & sf::OFF_HAND != 0,
                _ => s.owner == Owner::Player && s.index == hot,
            })
            .map(|i| i as u32)
        else {
            return;
        };
        let (Some(a), Some(b)) = (m.slots.get(slot as usize).copied(), m.slots.get(h as usize).copied()) else { return };
        if h == slot || !m.cursor.is_empty() {
            return;
        }
        let (sa, sb) = (get(m, dim, player, slot), get(m, dim, player, h));
        if !can_replace(c, &a, sa, sb) || !can_replace(c, &b, sb, sa) {
            return;
        }
        set(m, dim, player, slot, sb);
        set(m, dim, player, h, sa);
        if a.flags & sf::OUTPUT != 0 && !sa.is_empty() {
            took(m, dim, c, player, slot, 1);
        }
        if b.flags & sf::OUTPUT != 0 && !sb.is_empty() {
            took(m, dim, c, player, h, 1);
        }
        changed(m, dim, c, player, slot);
        changed(m, dim, c, player, h);
        return;
    }
    if button == CLONE {
        let creative = dim
            .entities
            .get(player)
            .is_some_and(|e| e.mode.has(crate::tag_bits::game_mode::NO_COSTS));
        let there = if (slot as usize) < m.slots.len() { get(m, dim, player, slot) } else { ItemStack::EMPTY };
        if creative && m.cursor.is_empty() && !there.is_empty() {
            m.cursor = ItemStack { count: c.max_stack(&there), ..there };
        }
        return;
    }
    if button == GATHER {
        let want = m.cursor;
        if want.is_empty() {
            return;
        }
        let max = c.max_stack(&want);
        // Partial stacks first, like vanilla, so full ones stay put when they are not needed.
        for full in [false, true] {
            for i in 0..m.slots.len() as u32 {
                if m.cursor.count >= max {
                    break;
                }
                let s = get(m, dim, player, i);
                if m.slots[i as usize].flags & sf::OUTPUT != 0
                    || m.slots[i as usize].owner == Owner::Trash
                    || !takeable(&m.slots[i as usize])
                    || s.is_empty()
                    || s.item != want.item
                    || s.data != want.data
                    || (s.count >= max) != full
                {
                    continue;
                }
                let n = s.count.min(max - m.cursor.count);
                m.cursor.count += n;
                set(m, dim, player, i, if n == s.count { ItemStack::EMPTY } else { ItemStack { count: s.count - n, ..s } });
                changed(m, dim, c, player, i);
            }
        }
        return;
    }
    // Into the trash, whole, what was there gone (Terraria's).
    let trash_at = m
        .slots
        .iter()
        .position(|s| s.owner == Owner::Trash)
        .map(|i| i as u32);
    let into_trash = |m: &mut OpenMenu, dim: &mut Dim, stack: ItemStack| match trash_at {
        Some(t) => set(m, dim, player, t, stack),
        None => {
            let bytes: Box<[u8]> = [stack.item, stack.count, stack.data]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect();
            dim.entities
                .comps
                .insert((player, c.game.fields.trash), bytes);
        }
    };
    if button == TO_TRASH {
        let Some(r) = m.slots.get(slot as usize).copied() else { return };
        let cur = get(m, dim, player, slot);
        if cur.is_empty() || r.owner == Owner::Trash || !takeable(&r) || r.flags & sf::OUTPUT != 0 {
            return;
        }
        into_trash(m, dim, cur);
        set(m, dim, player, slot, ItemStack::EMPTY);
        changed(m, dim, c, player, slot);
        return;
    }
    if m.slots
        .get(slot as usize)
        .is_some_and(|s| s.owner == Owner::Trash)
        && button <= 1
        && !shift
        && !m.cursor.is_empty()
    {
        let s = std::mem::replace(&mut m.cursor, ItemStack::EMPTY);
        into_trash(m, dim, s);
        return;
    }
    if button == THROW || button == THROW_ALL || button == PICK_SAME {
        let Some(r) = m.slots.get(slot as usize).copied() else { return };
        let cur = get(m, dim, player, slot);
        let output = r.flags & sf::OUTPUT != 0;
        if cur.is_empty() || !takeable(&r) {
            return;
        }
        let n = if button == PICK_SAME {
            let max = c.max_stack(&cur);
            if output || m.cursor.is_empty() || m.cursor.item != cur.item || m.cursor.data != cur.data || m.cursor.count >= max {
                return;
            }
            let n = cur.count.min(max - m.cursor.count);
            m.cursor.count += n;
            n
        } else {
            if !m.cursor.is_empty() {
                return;
            }
            // A result comes out whole.
            let n = if button == THROW_ALL || output { cur.count } else { 1 };
            dim.throw(0, c, player, ItemStack { count: n, ..cur });
            n
        };
        set(m, dim, player, slot, if n == cur.count { ItemStack::EMPTY } else { ItemStack { count: cur.count - n, ..cur } });
        if output {
            took(m, dim, c, player, slot, 1);
        }
        changed(m, dim, c, player, slot);
        return;
    }
    if slot == u32::MAX {
        if !m.cursor.is_empty() {
            let drop = if button == 1 { ItemStack { count: 1, ..m.cursor } } else { m.cursor };
            m.cursor.count -= drop.count;
            if m.cursor.count == 0 {
                m.cursor = ItemStack::EMPTY;
            }
            dim.throw(0, c, player, drop);
        }
        return;
    }
    let Some(r) = m.slots.get(slot as usize).copied() else { return };
    if r.flags & sf::LOCKED != 0 {
        return;
    }
    let cur = get(m, dim, player, slot);
    let output = r.flags & sf::OUTPUT != 0;
    let max = |s: &ItemStack| c.max_stack(s);

    if output {
        if cur.is_empty() {
            return;
        }
        if shift {
            // Craft into the inventory as long as the result stays the same and fits entirely;
            // a result that does not fit is not taken (no partial inserts, no lost ingredients).
            // A furnace's output is real items, and as many as fit go.
            if r.owner != Owner::Temp {
                let rest = dim.entities.insert_result(c, player, cur);
                if rest.count != cur.count {
                    dim.entities.dirty_inv.insert(player);
                    set(m, dim, player, slot, rest);
                    took(m, dim, c, player, slot, 1);
                    changed(m, dim, c, player, slot);
                }
                return;
            }
            if grid_of(c, m).is_some_and(|g| g.out == slot) {
                craft_all(m, dim, c, player, slot);
                return;
            }
            for _ in 0..max(&cur).max(64) {
                let now = get(m, dim, player, slot);
                if now.is_empty() || now.item != cur.item || now.data != cur.data {
                    break;
                }
                if dim.entities.item_room(c, player, now) < u64::from(now.count) {
                    break;
                }
                dim.entities.insert_result(c, player, now);
                dim.entities.dirty_inv.insert(player);
                set(m, dim, player, slot, ItemStack::EMPTY);
                took(m, dim, c, player, slot, 1);
                changed(m, dim, c, player, slot);
            }
            return;
        } else if m.cursor.is_empty() {
            m.cursor = cur;
        } else if m.cursor.item == cur.item && m.cursor.data == cur.data && m.cursor.count + cur.count <= max(&cur) {
            m.cursor.count += cur.count;
        } else {
            return;
        }
        set(m, dim, player, slot, ItemStack::EMPTY);
        took(m, dim, c, player, slot, 1);
        changed(m, dim, c, player, slot);
        return;
    }

    if shift {
        if !cur.is_empty() && takeable(&r) {
            move_out(m, dim, c, player, slot, cur.count);
        }
        return;
    }

    if !fits(c, &r, m.cursor.item) {
        return;
    }
    let mut cursor = m.cursor;
    let mut slot_stack = cur;
    let same = !cursor.is_empty() && !slot_stack.is_empty() && cursor.item == slot_stack.item && cursor.data == slot_stack.data;
    let room = cap(c, &r, &cursor);
    // An input slot only takes: anything that would hand items back is refused.
    if !takeable(&r) && !slot_stack.is_empty() && (cursor.is_empty() || !same) {
        return;
    }
    match (button, cursor.is_empty(), slot_stack.is_empty()) {
        (_, true, true) => return,
        (0, true, false) => {
            cursor = slot_stack;
            slot_stack = ItemStack::EMPTY;
        }
        (_, true, false) => {
            let half = slot_stack.count.div_ceil(2);
            cursor = ItemStack { count: half, ..slot_stack };
            slot_stack.count -= half;
        }
        (0, false, true) => {
            let n = cursor.count.min(room);
            slot_stack = ItemStack { count: n, ..cursor };
            cursor.count -= n;
        }
        (_, false, true) => {
            slot_stack = ItemStack { count: 1, ..cursor };
            cursor.count -= 1;
        }
        (0, false, false) if same => {
            let n = room.saturating_sub(slot_stack.count).min(cursor.count);
            slot_stack.count += n;
            cursor.count -= n;
        }
        (_, false, false) if same => {
            if slot_stack.count < room {
                slot_stack.count += 1;
                cursor.count -= 1;
            }
        }
        (_, false, false) if cursor.count <= room => std::mem::swap(&mut cursor, &mut slot_stack),
        (_, false, false) => return,
    }
    if cursor.count == 0 {
        cursor = ItemStack::EMPTY;
    }
    m.cursor = cursor;
    set(m, dim, player, slot, slot_stack);
    changed(m, dim, c, player, slot);
}

/// Destinations for a quick transfer, or sources for a wheel pull when no stack is given.
fn other_side(m: &OpenMenu, c: &Content, player: u64, slot: u32, stack: Option<ItemStack>) -> Vec<TransferTarget> {
    use abi::slot_owner as so;
    let from = m.slots[slot as usize];
    let mut container = Vec::new();
    let mut inventory = Vec::new();
    let max_stack = stack.map_or(0, |s| c.max_stack(&s));
    for (i, s) in m.slots.iter().enumerate() {
        if s.flags & (sf::OUTPUT | sf::LOCKED) != 0
            || s.owner == Owner::Trash
            || (s.owner.backing(player) == from.owner.backing(player) && (s.index == from.index || std::ptr::eq(s.group, from.group)))
            || stack.is_some_and(|stack| !fits(c, s, stack.item))
        {
            continue;
        }
        let priority = s.group.priority(stack.map(|s| s.item));
        let preferred = stack.is_some() && priority > 0;
        let targets = match (from.owner, s.owner) {
            (Owner::Player, Owner::On(_)) if stack.is_none() || s.group.owner == so::HOLDER || preferred => &mut container,
            (Owner::Player, Owner::Temp) if preferred => &mut container,
            (_, Owner::Player) => {
                let storage = matches!(s.group.owner, so::PLAYER | so::PLAYER_MAIN | so::PLAYER_HOTBAR);
                if !preferred && !(storage && (from.owner != Owner::Player || from.group.owner != s.group.owner)) {
                    continue;
                }
                &mut inventory
            }
            _ => continue,
        };
        targets.push(TransferTarget { slot: i as u32, priority, limit: s.group.capacity(max_stack) });
    }
    let mut targets = if container.is_empty() {
        inventory
    } else {
        container.extend(
            inventory
                .into_iter()
                .filter(|t| stack.is_some() && t.priority > 0),
        );
        container
    };
    targets.sort_by_key(|t| (std::cmp::Reverse(t.priority), m.slots[t.slot as usize].flags & sf::HOTBAR != 0));
    targets
}

/// Fills matching stacks, then empty slots, at each priority before trying a lower one.
fn move_out(m: &mut OpenMenu, dim: &mut Dim, c: &'static Content, player: u64, slot: u32, n: u32) {
    let cur = get(m, dim, player, slot);
    let going = n.min(cur.count);
    let targets = other_side(m, c, player, slot, Some(cur));
    let rest = crate::stack::insert_by_priority(ItemStack { count: going, ..cur }, &targets, |i, value| {
        if let Some(s) = value {
            set(m, dim, player, i, s);
        }
        get(m, dim, player, i)
    });
    let moved = going - rest.count;
    if moved > 0 {
        let left = cur.count - moved;
        set(m, dim, player, slot, if left == 0 { ItemStack::EMPTY } else { ItemStack { count: left, ..cur } });
        changed(m, dim, c, player, slot);
    }
}

/// Up to `n` items into `slot` from `other_side`, the last slots first (Mouse Tweaks'): of what
/// it holds, or when empty of the first item that fits (the wheel up).
fn pull_in(m: &mut OpenMenu, dim: &mut Dim, c: &'static Content, player: u64, slot: u32, n: u32) {
    let r = m.slots[slot as usize];
    let mut left = n;
    for target in other_side(m, c, player, slot, None).into_iter().rev() {
        let i = target.slot;
        let (here, s) = (get(m, dim, player, slot), get(m, dim, player, i));
        let same = !here.is_empty() && s.item == here.item && s.data == here.data;
        if left == 0 || s.is_empty() || !takeable(&m.slots[i as usize]) || (!here.is_empty() && !same) || !fits(c, &r, s.item) {
            continue;
        }
        let k = cap(c, &r, &s)
            .saturating_sub(here.count)
            .min(s.count)
            .min(left);
        if k == 0 {
            continue;
        }
        set(m, dim, player, i, if k == s.count { ItemStack::EMPTY } else { ItemStack { count: s.count - k, ..s } });
        set(m, dim, player, slot, ItemStack { count: here.count + k, ..s });
        left -= k;
    }
    if left < n {
        changed(m, dim, c, player, slot);
    }
}

/// A slot changed: the crafting grid's result shown again, then the menu's `on_change`.
fn changed(m: &mut OpenMenu, dim: &mut Dim, c: &'static Content, player: u64, slot: u32) {
    show_craft(m, dim, c, player);
    call(m, dim, c, player, slot, |d| d.on_change);
}

/// A result taken: from a crafting grid, `times` runs of its recipe use the grid up; then the
/// menu's `on_take`.
fn took(m: &mut OpenMenu, dim: &mut Dim, c: &'static Content, player: u64, slot: u32, times: u32) {
    if let Some(g) = grid_of(c, m).filter(|g| g.out == slot)
        && let Some((r, cells, _)) = craft_runs(m, dim, c, player, &g)
    {
        use_grid(m, dim, c, player, &g, r, &cells, times);
    }
    call_times(m, dim, c, player, slot, times, |d| d.on_take);
}

/// A menu's crafting grid (`slot_flags::GRID`): its slots, columns, result slot and recipe kind.
struct Grid {
    first: u32,
    len: u32,
    cols: u32,
    out: u32,
    kind: &'static str,
}

fn grid_of(c: &'static Content, m: &OpenMenu) -> Option<Grid> {
    let def = &c.game.menus[m.kind as usize];
    let kind = def.recipes.first()?;
    let layout = &c.game.player_layout;
    let (g, first, len) = def
        .slot_groups(layout)
        .find(|g| g.0.flags & sf::GRID != 0)?;
    let out = def
        .slot_groups(layout)
        .find(|g| g.0.flags & sf::OUTPUT != 0 && g.0.owner == abi::slot_owner::MENU)?
        .1;
    Some(Grid { first, len, cols: g.cols.max(1), out, kind })
}

/// What the grid makes, in its result slot.
fn show_craft(m: &mut OpenMenu, dim: &mut Dim, c: &'static Content, player: u64) {
    let Some(g) = grid_of(c, m) else { return };
    let made = craft_runs(m, dim, c, player, &g).map_or(ItemStack::EMPTY, |(r, ..)| r.result());
    if get(m, dim, player, g.out) != made {
        set(m, dim, player, g.out, made);
    }
}

/// The grid's recipe, the ingredient in each of its slots, and how many runs they last: until
/// one is used up or a tool wears out (it may break on the last).
fn craft_runs(
    m: &OpenMenu,
    dim: &Dim,
    c: &'static Content,
    player: u64,
    g: &Grid,
) -> Option<(&'static crate::registry::Recipe, Vec<Option<usize>>, u32)> {
    let stacks: Vec<ItemStack> = (g.first..g.first + g.len)
        .map(|i| get(m, dim, player, i))
        .collect();
    let ids: Vec<u32> = stacks
        .iter()
        .map(|s| if s.is_empty() { 0 } else { s.item })
        .collect();
    let mut cells = Vec::new();
    let r = c
        .game
        .match_grid(g.kind, &ids, g.cols, g.len / g.cols, &c.components, Some(&mut cells))?;
    let runs = cells
        .iter()
        .zip(&stacks)
        .filter_map(|(k, s)| Some((k.as_ref()?, s)))
        .map(|(&k, s)| match r.uses[k] {
            Use::Keep => u32::MAX,
            Use::Wear(w) => match c.durability(s) {
                0 => u32::MAX,
                most => most.saturating_sub(crate::stack::damage(s)).div_ceil(w),
            },
            Use::Consume | Use::Leaves(_) => s.count / per_run(r, k),
        })
        .min()
        .unwrap_or(0);
    (runs > 0).then_some((r, cells, runs))
}

/// How many of ingredient `k` a run takes: one a cell of a pattern, else as the recipe says.
fn per_run(r: &crate::registry::Recipe, k: usize) -> u32 {
    if r.shaped.is_some() { 1 } else { r.input_counts.get(k).copied().unwrap_or(1).max(1) }
}

/// `times` runs of the grid's recipe: each ingredient used as it says. What is left of one used
/// up (what it `leaves`, else its `remainder`) goes into its slot when that empties, the rest
/// to the player.
#[allow(clippy::too_many_arguments)]
fn use_grid(
    m: &mut OpenMenu,
    dim: &mut Dim,
    c: &'static Content,
    player: u64,
    g: &Grid,
    r: &crate::registry::Recipe,
    cells: &[Option<usize>],
    times: u32,
) {
    let mut spare = Vec::new();
    for (i, k) in cells.iter().enumerate() {
        let Some(k) = *k else { continue };
        let slot = g.first + i as u32;
        let s = get(m, dim, player, slot);
        let (mut left, back) = crate::registry::use_ingredient(c, s, r.uses[k], per_run(r, k), times);
        if !back.is_empty() {
            if left.is_empty() {
                let fit = c.max_stack(&back).min(back.count);
                left = ItemStack { count: fit, ..back };
                if back.count > fit {
                    spare.push(ItemStack { count: back.count - fit, ..back });
                }
            } else {
                spare.push(back);
            }
        }
        if left != s {
            set(m, dim, player, slot, left);
        }
    }
    for s in spare {
        give(dim, c, player, s, s.count as u64);
    }
    // What else runs give, rolled for each.
    for _ in 0..times.min(4096) {
        for s in dim.roll_table(c, &r.extras, &Default::default(), glam::DVec3::ZERO, 0) {
            give(dim, c, player, s, s.count as u64);
        }
    }
}

fn give(dim: &mut Dim, c: &'static Content, player: u64, stack: ItemStack, mut count: u64) {
    while count > 0 {
        let batch = count.min(u32::MAX as u64) as u32;
        let rest = dim
            .entities
            .insert_result(c, player, ItemStack { count: batch, ..stack });
        if !rest.is_empty() {
            dim.throw(0, c, player, rest);
        }
        count -= batch as u64;
    }
}

/// Shift on a crafting grid's result: as many runs as its ingredients last and their results
/// fit in the player's inventory, all at once.
fn craft_all(m: &mut OpenMenu, dim: &mut Dim, c: &'static Content, player: u64, slot: u32) {
    let Some(g) = grid_of(c, m) else { return };
    let Some((r, cells, runs)) = craft_runs(m, dim, c, player, &g) else { return };
    let out = r.result();
    if out.is_empty() {
        return;
    }
    let room = dim.entities.item_room(c, player, out) / out.count as u64;
    let times = runs.min(room.min(u32::MAX as u64) as u32);
    if times == 0 {
        return;
    }
    give(dim, c, player, out, out.count as u64 * times as u64);
    set(m, dim, player, slot, ItemStack::EMPTY);
    use_grid(m, dim, c, player, &g, r, &cells, times);
    call_times(m, dim, c, player, slot, times, |d| d.on_take);
    changed(m, dim, c, player, slot);
}

/// Dragging the cursor stack over `slots`: button 0 splits it evenly, button 1 leaves one item in
/// each, again each time a slot is passed over (Mouse Tweaks'). Slots that cannot take the item
/// are skipped; whatever is left stays on the cursor.
pub fn drag(m: &mut OpenMenu, dim: &mut Dim, c: &'static Content, player: u64, slots: &[u32], button: u8) {
    let cur = m.cursor;
    if cur.is_empty() {
        return;
    }
    let (valid, share) = crate::stack::drag_slots(slots, cur.count, button, |slot| {
        m.slots
            .get(slot as usize)
            .map_or(0, |r| drag_room(c, r, get(m, dim, player, slot), cur))
    });
    let mut left = cur.count;
    for &s in &valid {
        let t = get(m, dim, player, s);
        let n = share.min(drag_room(c, &m.slots[s as usize], t, cur));
        if n > 0 {
            set(m, dim, player, s, ItemStack { count: t.count + n, ..cur });
            left -= n;
        }
    }
    m.cursor = if left == 0 { ItemStack::EMPTY } else { ItemStack { count: left, ..cur } };
    for &s in &valid {
        changed(m, dim, c, player, s);
    }
}

fn drag_room(c: &Content, slot: &SlotRef, stack: ItemStack, cursor: ItemStack) -> u32 {
    if cursor.is_empty() || slot.flags & sf::OUTPUT != 0 || !fits(c, slot, cursor.item) {
        return 0;
    }
    crate::stack::room_in(stack, cursor, cap(c, slot, &cursor))
}

/// Slots and cursor that changed since the last call.
pub fn changes(m: &mut OpenMenu, dim: &Dim, c: &Content, player: u64) -> (Vec<(u32, ItemStack)>, Option<ToClient>) {
    let n = m.slots.len();
    m.sent_slots
        .resize(n, ItemStack { count: u32::MAX, ..ItemStack::EMPTY });
    let mut slots = Vec::new();
    for i in 0..n as u32 {
        let s = get(m, dim, player, i);
        if m.sent_slots[i as usize] != s {
            m.sent_slots[i as usize] = s;
            slots.push((i, crate::stack::for_client(s)));
        }
    }
    let cursor = (m.sent_cursor != m.cursor || (!m.cursor.is_empty() && !slots.is_empty())).then(|| {
        m.sent_cursor = m.cursor;
        // Two menu slots can show the same inventory index.
        let mut backing = rustc_hash::FxHashMap::default();
        let targets = m
            .slots
            .iter()
            .zip(&m.sent_slots)
            .enumerate()
            .take(if m.cursor.is_empty() { 0 } else { n })
            .map(|(i, (r, &s))| {
                let slot = *backing
                    .entry((r.owner.backing(player), r.index))
                    .or_insert(i as u32);
                crate::net::DragSlot { slot, room: drag_room(c, r, s, m.cursor) }
            })
            .collect();
        ToClient::Cursor { stack: crate::stack::for_client(m.cursor), targets }
    });
    (slots, cursor)
}
