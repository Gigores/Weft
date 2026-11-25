use crate::content::Content;
use rustc_hash::{FxHashMap, FxHashSet};
use weft_abi::{self as abi, BrainActor, BrainOffer, brain as b};

use super::dim::Dim;

pub(crate) const EDITS: &str = "core:brain_edits";

pub(crate) struct Rule {
    pub name: String,
    pub call: (abi::ModId, abi::Callback),
    pub settings: abi::BrainSettings,
}

struct Slot {
    rule: (u32, usize),
    due: u64,
    step: u64,
    enabled: bool,
    sampled: bool,
    wanted: BrainOffer,
    current: BrainOffer,
}

impl Slot {
    fn new(rule: (u32, usize)) -> Self {
        Self { rule, due: 0, step: ASLEEP, enabled: true, sampled: false, wanted: BrainOffer::default(), current: BrainOffer::default() }
    }

    fn definition<'a>(&self, c: &'a Content) -> &'a Rule {
        &c.game.entities[self.rule.0 as usize].brain[self.rule.1]
    }

    fn batch(&self, phase: u32) -> (u32, usize, u32) {
        (self.rule.0, self.rule.1, phase)
    }
}

pub(super) struct State {
    slots: Vec<Slot>,
    next: u64,
    controls: u32,
}

const ASLEEP: u64 = u64::MAX;
pub(super) const DECISIONS_PER_TICK: usize = 512;

impl State {
    pub fn new(kind: u32, count: usize) -> Self {
        Self {
            slots: (0..count).map(|i| Slot::new((kind, i))).collect(),
            next: if count == 0 { ASLEEP } else { 0 },
            controls: 0,
        }
    }

    pub fn restore(c: &Content, kind: u32, bytes: &[u8]) -> Option<Box<Self>> {
        let edits: Vec<(String, String, bool)> = serde_json::from_slice(bytes).ok()?;
        let mut brain = Self::new(kind, c.game.entities[kind as usize].brain.len());
        for (source, name, enabled) in edits {
            let Some(&source) = c.game.catalog.ids(abi::entry_kind::ENTITY).get(&source) else { continue };
            let Some(i) = rule_index(c, source, &name) else { continue };
            let mut slot = Slot::new((source, i));
            slot.enabled = enabled;
            slot.due = if enabled { 0 } else { ASLEEP };
            match brain.slots.iter().position(|old| old.definition(c).name == name) {
                Some(i) if brain.slots[i].rule != slot.rule => continue,
                Some(i) => brain.slots[i] = slot,
                None => brain.slots.push(slot),
            }
        }
        brain.refresh();
        (!brain.slots.is_empty()).then(|| Box::new(brain))
    }

    fn saved_edits(&self, c: &Content, kind: u32) -> Option<Box<[u8]>> {
        let edits: Vec<_> = self.slots.iter().filter(|s| s.rule.0 != kind || !s.enabled).map(|s| {
            (&c.game.entities[s.rule.0 as usize].name, &s.definition(c).name, s.enabled)
        }).collect();
        if edits.is_empty() { None } else { serde_json::to_vec(&edits).ok().map(Vec::into_boxed_slice) }
    }

    fn refresh(&mut self) {
        self.next = self.slots.iter().map(|s| s.due.min(s.step)).min().unwrap_or(ASLEEP);
    }

    pub fn controls(&self, mask: u32) -> bool {
        self.controls & mask != 0
    }

    pub fn wake_controls(&mut self, c: &Content, controls: u32, now: u64) {
        for i in 0..self.slots.len() {
            if self.slots[i].definition(c).settings.controls & controls != 0 && valid(self.slots[i].current) {
                self.wake(i, now);
            }
        }
    }

    fn wake(&mut self, i: usize, now: u64) {
        if self.slots[i].enabled {
            self.slots[i].due = self.slots[i].due.min(now);
            self.next = self.next.min(now);
        }
    }
}

fn rule_index(c: &Content, kind: u32, name: &str) -> Option<usize> {
    c.game.entities.get(kind as usize)?.brain.iter().position(|r| r.name == name)
}

type Batch = FxHashMap<(u32, usize, u32), Vec<BrainActor>>;

fn deadline(now: u64, ticks: u32) -> u64 {
    if ticks == 0 { ASLEEP } else { now.saturating_add(u64::from(ticks)) }
}

fn actor(id: u64, current: BrainOffer, offer: BrainOffer, next: u32) -> BrainActor {
    BrainActor { id, current, offer, next }
}

fn valid(offer: BrainOffer) -> bool {
    offer.valid != 0 && offer.priority.is_finite() && offer.keep.is_finite()
        && offer.speed.is_finite() && offer.point.to_array().iter().all(|v| v.is_finite())
}

fn same_target(a: BrainOffer, b: BrainOffer) -> bool {
    valid(a) && valid(b) && a.target == b.target
}

impl Dim {
    pub(super) fn observe_hands(&mut self, c: &Content) {
        let Some(&event) = c.game.event_names.get(b::HELD_CHANGED) else { return };
        if c.game.events[event as usize].handlers.is_empty() {
            return;
        }
        let mut changes = Vec::new();
        for e in &self.entities.list {
            let kind = &c.game.entities[e.kind as usize];
            if e.removed || kind.hand_slots == 0 && kind.off.is_none() {
                continue;
            }
            let Some(inv) = self.entities.inventories.get(&e.id) else { continue };
            let main = crate::content::in_main_hand(c, e.kind, inv, e.selected).item;
            let off = crate::content::in_off_hand(c, e.kind, inv).item;
            if e.seen_hands != [main, off] {
                changes.push(abi::HeldChange { entity: e.id, main, off });
            }
        }
        for change in changes {
            if let Some(e) = self.entities.get_mut(change.entity) {
                e.seen_hands = [change.main, change.off];
            }
            self.emit_engine(0, c, b::HELD_CHANGED, &change);
        }
    }

    pub(crate) fn wake_brain(&mut self, c: &Content, id: u64, name: &str) -> bool {
        let Some(e) = self.entities.get_mut(id) else { return false };
        let Some(brain) = &mut e.brain else { return false };
        let mut found = false;
        for i in 0..brain.slots.len() {
            if name.is_empty() || brain.slots[i].definition(c).name == name {
                brain.wake(i, self.time);
                found = true;
            }
        }
        found
    }

    pub(crate) fn enable_brain(&mut self, c: &Content, id: u64, name: &str, enabled: bool) -> bool {
        let Some(e) = self.entities.get_mut(id) else { return false };
        let Some(brain) = &mut e.brain else { return false };
        let mut found = false;
        for slot in &mut brain.slots {
            if name.is_empty() || slot.definition(c).name == name {
                found = true;
                if slot.enabled != enabled {
                    slot.enabled = enabled;
                    slot.due = slot.due.min(self.time);
                }
            }
        }
        brain.refresh();
        let edits = brain.saved_edits(c, e.kind);
        self.store_brain_edits(c, id, edits);
        found
    }

    pub(crate) fn install_brain_rule(&mut self, c: &Content, id: u64, source: u32, name: &str) -> bool {
        let Some(i) = rule_index(c, source, name) else { return false };
        let Some(e) = self.entities.get_mut(id) else { return false };
        let kind = e.kind;
        let brain = e.brain.get_or_insert_with(|| Box::new(State::new(kind, c.game.entities[kind as usize].brain.len())));
        if let Some(slot) = brain.slots.iter().find(|slot| slot.definition(c).name == name) {
            return slot.rule == (source, i) && self.enable_brain(c, id, name, true);
        }
        brain.slots.push(Slot::new((source, i)));
        brain.next = brain.next.min(self.time);
        let edits = brain.saved_edits(c, kind);
        self.store_brain_edits(c, id, edits);
        true
    }

    fn store_brain_edits(&mut self, c: &Content, id: u64, edits: Option<Box<[u8]>>) {
        let key = (id, c.game.brain_edits_comp);
        if let Some(edits) = edits {
            self.entities.comps.insert(key, edits);
        } else {
            self.entities.comps.remove(&key);
        }
    }

    pub(crate) fn brain_status(&self, c: &Content, id: u64, name: &str) -> Option<BrainOffer> {
        let e = self.entities.get(id)?;
        let brain = e.brain.as_ref()?;
        let slot = brain.slots.iter().find(|slot| slot.definition(c).name == name)?;
        Some(slot.current).filter(|offer| valid(*offer))
    }

    pub(super) fn brain_component_changed(&mut self, c: &Content, id: u64, name: &str) {
        let Some(e) = self.entities.get_mut(id) else { return };
        let Some(brain) = &mut e.brain else { return };
        for i in 0..brain.slots.len() {
            if brain.slots[i].definition(c).settings.watched.iter().any(|watch| watch == name) {
                brain.wake(i, self.time);
            }
        }
    }

    pub(crate) fn track_brain_write(&mut self, c: &Content, id: u64, component: u32) {
        let Some(brain) = self.entities.get(id).and_then(|e| e.brain.as_ref()) else { return };
        let Some(def) = c.game.components.get(component as usize) else { return };
        if brain.slots.iter().any(|slot| slot.definition(c).settings.watched.contains(&def.name))
            && let Some(value) = self.entities.comps.get(&(id, component))
        {
            self.brain_writes.entry((id, component)).or_insert_with(|| value.clone());
        }
    }

    pub(super) fn flush_brain_writes(&mut self, c: &Content) {
        for ((id, comp), before) in std::mem::take(&mut self.brain_writes) {
            if self.entities.comps.get(&(id, comp)) != Some(&before) {
                self.brain_component_changed(c, id, &c.game.components[comp as usize].name);
            }
        }
    }

    fn brain_callbacks(&mut self, c: &Content, batch: Batch, now: u64, decisions: &mut FxHashSet<u64>) {
        let mut batch: Vec<_> = batch.into_iter().collect();
        batch.sort_unstable_by_key(|&(key, _)| key);
        for ((kind, i, phase), mut actors) in batch {
            let (owner, callback) = c.game.entities[kind as usize].brain[i].call;
            self.call(0, owner, callback, |world| abi::BrainArgs {
                world, actors: actors.as_mut_ptr(), count: actors.len(), phase,
            });
            if !matches!(phase, b::OFFER | b::SENSE | b::START | b::STEP) {
                continue;
            }
            for entry in actors {
                let Some(brain) = self.entities.get_mut(entry.id).and_then(|e| e.brain.as_mut()) else { continue };
                let key = (kind, i);
                let index = if brain.slots.get(i).is_some_and(|s| s.rule == key) { Some(i) }
                    else { brain.slots.iter().position(|s| s.rule == key) };
                let Some(slot) = index.map(|i| &mut brain.slots[i]) else { continue };
                if matches!(phase, b::START | b::STEP) {
                    if valid(slot.current) {
                        slot.step = deadline(now, entry.next);
                    }
                } else {
                    // A callback can signal fresh work while running.
                    if slot.due == ASLEEP {
                        slot.due = deadline(now, entry.next);
                    }
                    if phase == b::OFFER {
                        let offer = if slot.enabled && valid(entry.offer) { entry.offer } else { BrainOffer::default() };
                        if slot.wanted != offer {
                            slot.wanted = offer;
                            decisions.insert(entry.id);
                        }
                    }
                }
                brain.refresh();
            }
        }
    }

    pub(super) fn tick_brains(&mut self, c: &Content, ready: &[(u32, u64)]) {
        if ready.is_empty() {
            return;
        }
        let now = self.time;
        let mut decisions = FxHashSet::default();
        for phase in [b::SENSE, b::OFFER] {
            let mut batch = Batch::default();
            for &(_, id) in ready {
                let Some(brain) = self.entities.get_mut(id).and_then(|e| e.brain.as_mut()) else { continue };
                for slot in &mut brain.slots {
                    let rule = slot.definition(c);
                    let senses = rule.settings.controls == 0;
                    if (phase == b::SENSE) != senses || slot.due > now {
                        continue;
                    }
                    slot.due = ASLEEP;
                    if slot.enabled {
                        let interval = rule.settings.interval;
                        let next = if !slot.sampled && interval > 1 { 1 + (id % u64::from(interval)) as u32 } else { interval };
                        slot.sampled = true;
                        batch.entry(slot.batch(phase)).or_default().push(actor(id, slot.current, BrainOffer::default(), next));
                    } else if valid(slot.wanted) || valid(slot.current) {
                        slot.wanted = BrainOffer::default();
                        decisions.insert(id);
                    }
                }
            }
            self.brain_callbacks(c, batch, now, &mut decisions);
            self.flush_brain_writes(c);
        }
        let mut stop = Batch::default();
        let mut apply = Batch::default();
        let mut candidates = Vec::new();
        let mut chosen = Vec::new();
        for &(_, id) in ready {
            let Some(e) = self.entities.get_mut(id) else { continue };
            let Some(brain) = &mut e.brain else { continue };
            if !decisions.contains(&id) {
                for slot in &mut brain.slots {
                    if valid(slot.current) && slot.step <= now {
                        slot.step = ASLEEP;
                        let period = slot.definition(c).settings.step;
                        apply.entry(slot.batch(b::STEP)).or_default().push(actor(id, slot.current, slot.current, period));
                    }
                }
                brain.refresh();
                continue;
            }
            candidates.clear();
            chosen.clear();
            for (i, slot) in brain.slots.iter().enumerate() {
                if slot.enabled && valid(slot.wanted) {
                    if same_target(slot.current, slot.wanted) {
                        chosen.push(i);
                    } else {
                        candidates.push(i);
                    }
                }
            }
            candidates.sort_unstable_by(|&a, &b| {
                brain.slots[b].wanted.priority.total_cmp(&brain.slots[a].wanted.priority).then(a.cmp(&b))
            });
            for &i in &candidates {
                let mask = brain.slots[i].definition(c).settings.controls;
                let wins = chosen.iter().filter(|&&j| brain.slots[j].definition(c).settings.controls & mask != 0).all(|&j| {
                    let slot = &brain.slots[j];
                    let keep = if same_target(slot.current, slot.wanted) { slot.wanted.keep } else { slot.wanted.priority };
                    brain.slots[i].wanted.priority > keep
                });
                if wins {
                    chosen.retain(|&j| brain.slots[j].definition(c).settings.controls & mask == 0);
                    chosen.push(i);
                }
            }
            brain.controls = chosen.iter().fold(0, |mask, &i| mask | brain.slots[i].definition(c).settings.controls);
            for (i, slot) in brain.slots.iter_mut().enumerate() {
                let rule = slot.definition(c);
                let old = slot.current;
                let next = if chosen.contains(&i) { slot.wanted } else { BrainOffer::default() };
                let same = same_target(old, next);
                let lost = valid(old) && !same;
                if lost {
                    if rule.settings.controls & b::MOVE != 0 {
                        e.nav = None;
                        e.walk = None;
                    }
                    stop.entry(slot.batch(b::STOP)).or_default().push(actor(id, old, next, 0));
                }
                if !valid(next) {
                    slot.step = ASLEEP;
                } else if !same {
                    let step = rule.settings.step;
                    slot.step = deadline(now, step);
                    apply.entry(slot.batch(b::START)).or_default().push(actor(id, old, next, step));
                } else {
                    if old.point != next.point || old.speed != next.speed {
                        apply.entry(slot.batch(b::UPDATE)).or_default().push(actor(id, old, next, 0));
                    }
                    if slot.step <= now {
                        slot.step = ASLEEP;
                        apply.entry(slot.batch(b::STEP)).or_default().push(actor(id, old, next, rule.settings.step));
                    }
                }
                if log::log_enabled!(target: "brain", log::Level::Debug) && (lost || !same && valid(next)) {
                    log::debug!(target: "brain", "{id} {}: {old:?} -> {next:?}", rule.name);
                }
                slot.current = next;
            }
            brain.refresh();
        }
        self.brain_callbacks(c, stop, now, &mut decisions);
        self.brain_callbacks(c, apply, now, &mut decisions);
    }

    pub(super) fn brain_due(&self, e: &super::entity::Entity) -> bool {
        e.brain.as_ref().is_some_and(|brain| brain.next <= self.time)
    }

    pub(super) fn brain_deadline(&self, id: u64) -> u64 {
        self.entities.get(id).and_then(|e| e.brain.as_ref()).map_or(ASLEEP, |brain| brain.next)
    }
}
