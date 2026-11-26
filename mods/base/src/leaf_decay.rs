use std::collections::{HashSet, VecDeque};

use weft_sdk::*;

static LEAVES: Tag = Tag::named("leaves");
static SUPPORT: Tag = Tag::named("leaf_support");
const DISTANCE: u8 = 7;

pub fn leaves(block: BlockBuilder<'_>) -> BlockBuilder<'_> {
    block
        .prop("persistent", ["false", "true"])
        .placement(|_, state| state.set("persistent", "true"))
        .on_neighbor(|e| {
            let w = e.world();
            if w.prop(e.pos(), "persistent") == Some("false") {
                schedule(&w, e.pos());
            }
        })
        .on_scheduled(decay)
}

pub fn support(block: BlockBuilder<'_>) -> BlockBuilder<'_> {
    block.tag(SUPPORT.name()).on_break(|w, change| {
        walk(w, change.pos, |pos, _| {
            if w.prop(pos, "persistent") == Some("false") {
                schedule(w, pos);
            }
            false
        });
    })
}

fn schedule(w: &World, p: BlockPos) {
    w.schedule_tick(p, 20 + (w.random() % 41) as u32);
}

fn decay(event: &mut BlockEvent) {
    let (w, pos) = (event.world(), event.pos());
    if w.prop(pos, "persistent") != Some("false") {
        return;
    }
    let mut unloaded = false;
    let held = walk(&w, pos, |_, state| {
        unloaded |= state.is_none();
        state.is_some_and(|s| SUPPORT.on_block(s))
    });
    if !held && unloaded {
        schedule(&w, pos);
    } else if !held {
        w.part_remove(pos, event.state());
    }
}

fn walk(w: &World, start: BlockPos, mut visit: impl FnMut(BlockPos, Option<u32>) -> bool) -> bool {
    let mut pending = VecDeque::from([(start, 0u8)]);
    let mut seen = HashSet::from([start]);
    while let Some((pos, distance)) = pending.pop_front() {
        for dir in Dir::ALL {
            let at = pos + dir.offset();
            if seen.insert(at) {
                let state = w.block_loaded(at);
                if visit(at, state) {
                    return true;
                }
                if distance + 1 < DISTANCE && state.is_some_and(|s| LEAVES.on_block(s)) {
                    pending.push_back((at, distance + 1));
                }
            }
        }
    }
    false
}
