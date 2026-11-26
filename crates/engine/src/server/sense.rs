use glam::{DVec3, I64Vec3, IVec3};
use rustc_hash::{FxHashMap, FxHashSet};
use weft_abi::Neighbor;

use super::dim::Dim;
use super::entity::Entity;

const TILE: f64 = 8.0;
const CACHED_RADIUS: f64 = 32.0;
const MIN_CACHED_POPULATION: usize = 64;
const MAX_GROUPS: usize = 128;

type Source = (u64, u32);
type Watcher = (u64, (u32, usize));

#[derive(Default)]
pub(super) struct Observations {
    pub(super) rule: Option<(u32, usize)>,
    by_source: FxHashMap<Source, FxHashSet<Watcher>>,
    by_watcher: FxHashMap<Watcher, Vec<(Source, u64)>>,
}

impl Observations {
    pub(super) fn is_empty(&self) -> bool {
        self.by_source.is_empty()
    }

    pub(super) fn observing(&self, source: Source) -> bool {
        self.by_source.contains_key(&source)
    }

    pub(super) fn watch(&mut self, watcher: Watcher, source: Source, now: u64) {
        let sources = self.by_watcher.entry(watcher).or_default();
        if let Some((_, sampled)) = sources.iter_mut().find(|(key, _)| *key == source) {
            *sampled = now;
        } else {
            sources.push((source, now));
            self.by_source.entry(source).or_default().insert(watcher);
        }
    }

    pub(super) fn watchers(&self, source: Source) -> impl Iterator<Item = Watcher> + '_ {
        self.by_source.get(&source).into_iter().flatten().copied()
    }

    pub(super) fn finish(&mut self, watcher: Watcher, now: u64) {
        let Some(sources) = self.by_watcher.get_mut(&watcher) else { return };
        for (source, _) in sources.extract_if(.., |(_, sampled)| *sampled != now) {
            Self::forget(&mut self.by_source, source, watcher);
        }
        if sources.is_empty() {
            self.by_watcher.remove(&watcher);
        }
    }

    pub(super) fn clear(&mut self, watcher: Watcher) {
        if let Some(sources) = self.by_watcher.remove(&watcher) {
            for (source, _) in sources {
                Self::forget(&mut self.by_source, source, watcher);
            }
        }
    }

    fn forget(by_source: &mut FxHashMap<Source, FxHashSet<Watcher>>, source: Source, watcher: Watcher) {
        if let Some(watchers) = by_source.get_mut(&source) {
            watchers.remove(&watcher);
            if watchers.is_empty() {
                by_source.remove(&source);
            }
        }
    }

    pub(super) fn remove_entities(&mut self, mut removed: impl FnMut(u64) -> bool) -> Vec<Watcher> {
        let retired: Vec<_> = self
            .by_watcher
            .keys()
            .copied()
            .filter(|w| removed(w.0))
            .collect();
        for watcher in retired {
            self.clear(watcher);
        }
        let gone: Vec<_> = self
            .by_source
            .keys()
            .copied()
            .filter(|s| removed(s.0))
            .collect();
        let mut wake = Vec::new();
        for source in gone {
            for watcher in self.by_source.remove(&source).unwrap() {
                if let Some(sources) = self.by_watcher.get_mut(&watcher) {
                    sources.retain(|(key, _)| *key != source);
                    if sources.is_empty() {
                        self.by_watcher.remove(&watcher);
                    }
                }
                wake.push(watcher);
            }
        }
        wake
    }
}

#[derive(Default)]
pub(super) struct Environment {
    active: bool,
    groups: Groups,
    blocks: FxHashMap<IVec3, Option<u32>>,
}

impl Environment {
    pub(super) fn begin(&mut self) {
        self.end();
        self.active = true;
    }

    pub(super) fn end(&mut self) {
        self.active = false;
        self.groups.candidates.clear();
        self.blocks.clear();
    }
}

fn sample(e: &Entity) -> Option<Neighbor> {
    (e.is_body() && e.alive()).then(|| Neighbor { id: e.id, pos: e.pos.to_array().into(), kind: e.kind })
}

fn tile(pos: DVec3) -> I64Vec3 {
    (pos / TILE).floor().as_i64vec3()
}

#[derive(Default)]
struct Groups {
    candidates: FxHashMap<(I64Vec3, i64), Vec<Neighbor>>,
}

impl Groups {
    fn around(&mut self, pos: DVec3, radius: f64, entities: impl Iterator<Item = Neighbor>) -> Option<&[Neighbor]> {
        let center = tile(pos);
        let reach = (radius / TILE).ceil() as i64;
        let full = self.candidates.len() >= MAX_GROUPS;
        let entries = match self.candidates.entry((center, reach)) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(_) if full => return None,
            std::collections::hash_map::Entry::Vacant(entry) => {
                let low = (center.as_dvec3() - reach as f64) * TILE;
                let high = (center.as_dvec3() + reach as f64 + 1.0) * TILE;
                entry.insert(
                    entities
                        .filter(|e| {
                            let p = DVec3::from_array(e.pos.to_array());
                            p.cmpge(low).all() && p.cmplt(high).all()
                        })
                        .collect(),
                )
            }
        };
        Some(entries)
    }
}

fn query(candidates: impl Iterator<Item = Neighbor>, center: DVec3, radius: f64, kind: u32, skip: u64, out: &mut [Neighbor]) -> usize {
    let squared = radius * radius;
    let mut count = 0;
    for candidate in candidates {
        if candidate.id != skip
            && (kind == u32::MAX || candidate.kind == kind)
            && (DVec3::from_array(candidate.pos.to_array()) - center).length_squared() <= squared
        {
            if let Some(slot) = out.get_mut(count) {
                *slot = candidate;
            }
            count += 1;
        }
    }
    count
}

impl Dim {
    pub(crate) fn nearby(&mut self, frame: u64, pos: DVec3, radius: f64, kind: u32, skip: u64, out: &mut [Neighbor]) -> usize {
        if !pos.is_finite() || !radius.is_finite() || radius < 0.0 {
            return 0;
        }
        let pose = self.space_pose(frame);
        let pos = pose.to_world(pos);
        let entities = || self.entities.list.iter().filter_map(sample);
        let cached = if frame == 0 && self.senses.active && radius <= CACHED_RADIUS && self.entities.list.len() >= MIN_CACHED_POPULATION {
            self.senses.groups.around(pos, radius, entities())
        } else {
            None
        };
        let count = match cached {
            Some(entries) => query(entries.iter().copied(), pos, radius, kind, skip, out),
            None => query(entities(), pos, radius, kind, skip, out),
        };
        if frame != 0 {
            for entry in out.iter_mut().take(count) {
                entry.pos = pose
                    .to_local(DVec3::from_array(entry.pos.to_array()))
                    .to_array()
                    .into();
            }
        }
        count
    }

    pub(crate) fn sense_block(&mut self, frame: u64, pos: IVec3) -> Option<u32> {
        if frame != 0 || !self.senses.active {
            return self.seen(frame, pos);
        }
        if let Some(&state) = self.senses.blocks.get(&pos) {
            return state;
        }
        let state = self.seen(0, pos);
        if self.senses.blocks.len() < 8192 {
            self.senses.blocks.insert(pos, state);
        }
        state
    }
}
