//! Light emitted by entities through `LIGHT`: from their kind, held and worn items, a dropped
//! item stack, or being on fire.

use glam::{DVec3, IVec3};
use rustc_hash::FxHashSet;

use super::dim::Dim;
use super::entity::Entity;
use crate::content::{Content, in_main_hand, in_off_hand};
use crate::world::cube_of;

/// Light of a burning entity.
const BURNING: [u8; 3] = [15, 12, 6];

/// The per-channel maximum of two lights.
pub(super) fn brightest(a: [u8; 3], b: [u8; 3]) -> [u8; 3] {
    [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])]
}

impl Dim {
    /// Computes every entity's light and relights the world where an entity's light or cell
    /// changed, or where it stopped emitting. A cell is only relit if its resulting light differs,
    /// so stationary entities cost only the lookups.
    pub(super) fn tick_entity_lights(&mut self, c: &'static Content) {
        if self.frame != 0 {
            return;
        }
        // Entity id, cell, light.
        let mut now: Vec<(u64, IVec3, [u8; 3])> = Vec::new();
        for e in &self.entities.list {
            if e.removed || (e.player && !e.alive()) || e.block_frame() != 0 || e.mode.has(crate::tag_bits::game_mode::HIDDEN) {
                continue;
            }
            if !e.player && e.anchor.is_none() && self.idle.contains(&cube_of(e.pos.floor().as_ivec3())) {
                continue;
            }
            let light = self.light_of(c, e);
            if light != [0; 3] {
                let middle = e.pos + DVec3::Y * (e.height(c) * 0.5);
                now.push((e.id, middle.floor().as_ivec3(), light));
            }
        }
        if now.is_empty() && self.space.entity_lights.is_empty() {
            return;
        }
        now.sort_unstable_by_key(|n| n.0);

        // Cells whose light sources changed and need relighting.
        let mut touched: Vec<IVec3> = Vec::new();
        let keep: FxHashSet<u64> = now.iter().map(|n| n.0).collect();
        let mut gone: Vec<u64> = self
            .space
            .entity_lights
            .keys()
            .copied()
            .filter(|id| !keep.contains(id))
            .collect();
        gone.sort_unstable();
        for id in gone {
            if let Some((cell, _)) = self.space.entity_lights.remove(&id) {
                touched.push(cell);
            }
        }
        for (id, cell, l) in now {
            match self.space.entity_lights.insert(id, (cell, l)) {
                Some((was, was_light)) if was == cell && was_light == l => {}
                Some((was, _)) => touched.extend([was, cell]),
                None => touched.push(cell),
            }
        }
        if !touched.is_empty() {
            self.relight_sources(c, touched);
        }
    }

    /// The brightest light among the entity's kind, fire, dropped stack, both hands and worn items.
    fn light_of(&self, c: &Content, e: &Entity) -> [u8; 3] {
        let mut light = c.entity_light(e.kind);
        if e.fire > 0 {
            light = brightest(light, BURNING);
        }
        if !e.item.is_empty() {
            light = brightest(light, c.stack_light(&e.item));
        }
        if let Some(inv) = self.entities.inventories.get(&e.id) {
            let worn = &c.game.entities[e.kind as usize].worn;
            let worn = inv
                .get(worn.start as usize..worn.end as usize)
                .unwrap_or_default();
            let hands = [in_main_hand(c, inv, e.selected), in_off_hand(c, inv)];
            for s in hands.iter().chain(worn).filter(|s| !s.is_empty()) {
                light = brightest(light, c.stack_light(s));
            }
        }
        light
    }
}
