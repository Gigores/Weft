use std::time::{Duration, Instant};

use glam::DVec3;
use rustc_hash::FxHashMap;
use weft_abi::{self as abi, GazeOptions, GazeState, RayHit, gaze as g};

use super::dim::Dim;
use super::entity::Entities;
use crate::content::Content;

// A long server stall provides no evidence the viewer kept looking.
const REAL_SAMPLE_GAP: Duration = Duration::from_millis(250);
type Watcher = (u64, (u32, usize));

#[derive(Default)]
pub(super) struct Watches {
    rules: FxHashMap<Watcher, Vec<Watch>>,
    rays: FxHashMap<u64, (f64, Option<RayHit>)>,
}

struct Focus {
    hit: RayHit,
    ticks: u64,
    began: Instant,
    last: Instant,
    held: bool,
}

struct Watch {
    viewer: u64,
    options: GazeOptions,
    renewed: u64,
    focus: Option<Focus>,
    transitions: u32,
}

impl Watch {
    fn sample(&mut self, hit: Option<RayHit>, now: Instant, gap: Duration) -> bool {
        let options = self.options;
        let hit = hit.filter(|hit| {
            hit.distance <= options.range
                && (options.target == 0 || hit.entity == options.target)
                && (options.part == g::ANY_PART || hit.part == options.part)
        });
        let mut transitions = 0;
        if let Some(focus) = &self.focus
            && (hit.is_none_or(|hit| hit.entity != focus.hit.entity) || options.clock == g::REAL && now.duration_since(focus.last) > gap)
        {
            self.focus = None;
            self.transitions &= !g::REACHED;
            transitions |= g::LEAVE;
        }
        if let Some(hit) = hit {
            let continued = self.focus.is_some();
            let focus = self.focus.get_or_insert_with(|| {
                transitions |= g::ENTER;
                Focus { hit, ticks: 0, began: now, last: now, held: false }
            });
            focus.hit = hit;
            focus.last = now;
            focus.ticks += u64::from(continued);
            let elapsed = if options.clock == g::REAL { milliseconds(now.duration_since(focus.began)) } else { focus.ticks };
            if !focus.held && elapsed >= options.threshold {
                focus.held = true;
                transitions |= g::REACHED;
            }
        }
        self.transitions |= transitions;
        transitions != 0
    }

    fn read(&mut self) -> GazeState {
        let flags = std::mem::take(&mut self.transitions);
        match &self.focus {
            Some(focus) => GazeState {
                hit: focus.hit,
                ticks: focus.ticks,
                milliseconds: milliseconds(focus.last.duration_since(focus.began)),
                flags: flags | if focus.held { g::HELD } else { 0 },
            },
            None => GazeState { flags, ..GazeState::default() },
        }
    }
}

fn sample_gap(tick_rate: u32) -> Duration {
    REAL_SAMPLE_GAP.max(Duration::from_secs_f64(2.0 / f64::from(tick_rate.max(1))))
}

fn milliseconds(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

impl Watches {
    pub(super) fn clear(&mut self, watcher: Watcher) {
        self.rules.remove(&watcher);
    }

    pub(super) fn finish(&mut self, watcher: Watcher, now: u64) {
        if let Some(watches) = self.rules.get_mut(&watcher) {
            watches.retain(|watch| watch.renewed == now);
            if watches.is_empty() {
                self.rules.remove(&watcher);
            }
        }
    }

    pub(super) fn forget_missing(&mut self, entities: &Entities) {
        self.rules
            .retain(|(actor, _), _| entities.get(*actor).is_some_and(|e| e.alive()));
    }
}

impl Dim {
    pub(crate) fn watch_gaze(&mut self, actor: u64, viewer: u64, mut options: GazeOptions) -> GazeState {
        let Some(rule) = self.observations.rule else { return GazeState::default() };
        if viewer == 0 || !options.range.is_finite() || options.range <= 0.0 {
            return GazeState::default();
        }
        options.range = options.range.min(1024.0);
        let watches = self.gaze.rules.entry((actor, rule)).or_default();
        let watch = match watches
            .iter()
            .position(|w| w.viewer == viewer && w.options == options)
        {
            Some(index) => &mut watches[index],
            None => {
                watches.push(Watch { viewer, options, renewed: self.time, focus: None, transitions: 0 });
                watches.last_mut().unwrap()
            }
        };
        watch.renewed = self.time;
        watch.read()
    }

    pub(super) fn sample_gaze(&mut self, c: &Content, tick_rate: u32) {
        if self.gaze.rules.is_empty() {
            return;
        }
        self.gaze.forget_missing(&self.entities);
        let mut rays = std::mem::take(&mut self.gaze.rays);
        for watch in self.gaze.rules.values().flatten() {
            if watch.options.target != 0
                && self
                    .entities
                    .get(watch.options.target)
                    .is_none_or(|target| !target.alive())
            {
                continue;
            }
            let (range, _) = rays.entry(watch.viewer).or_default();
            *range = range.max(watch.options.range);
        }
        for (&viewer, (range, hit)) in &mut rays {
            *hit = self
                .entities
                .get(viewer)
                .filter(|e| e.alive() && e.is_body())
                .and_then(|e| {
                    let origin = e.pos + DVec3::Y * e.eye(c);
                    let dir = crate::physics::look_dir(e.yaw, e.pitch);
                    self.raycast(0, c, origin, dir, *range, abi::ray::BLOCKS | abi::ray::ENTITIES, &[viewer])
                        .filter(|hit| hit.entity != 0)
                });
        }
        let now = Instant::now();
        let gap = sample_gap(tick_rate);
        for (&(actor, rule), watches) in &mut self.gaze.rules {
            let mut wake = false;
            for watch in watches {
                wake |= watch.sample(rays.get(&watch.viewer).and_then(|(_, hit)| *hit), now, gap);
            }
            if wake {
                Self::wake_observer(&mut self.entities, actor, rule, self.time);
            }
        }
        rays.clear();
        self.gaze.rays = rays;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_target_never_inherits_a_pending_threshold() {
        let start = Instant::now();
        let options = GazeOptions { target: 0, range: 8.0, threshold: 1, part: g::ANY_PART, clock: g::TICKS };
        let mut watch = Watch { viewer: 1, options, renewed: 0, focus: None, transitions: 0 };
        let hit = RayHit { entity: 2, ..RayHit::default() };
        watch.sample(Some(hit), start, REAL_SAMPLE_GAP);
        watch.sample(Some(hit), start + Duration::from_millis(50), REAL_SAMPLE_GAP);
        watch.sample(Some(RayHit { entity: 3, ..hit }), start + Duration::from_millis(100), REAL_SAMPLE_GAP);
        let state = watch.read();
        assert_eq!(state.hit.entity, 3);
        assert_eq!(state.flags, g::ENTER | g::LEAVE);
    }

    #[test]
    fn gaze_uses_the_selected_clock_and_interrupts_on_a_different_part() {
        let start = Instant::now();
        let head = RayHit { entity: 2, part: 7, distance: 4.0, ..RayHit::default() };
        for clock in [g::TICKS, g::REAL] {
            let options = GazeOptions { target: 2, range: 8.0, threshold: if clock == g::REAL { 600 } else { 2 }, part: 7, clock };
            let mut watch = Watch { viewer: 1, options, renewed: 0, focus: None, transitions: 0 };
            assert!(watch.sample(Some(head), start, REAL_SAMPLE_GAP));
            assert_eq!(watch.read().flags, g::ENTER);
            assert!(!watch.sample(Some(head), start + Duration::from_millis(100), REAL_SAMPLE_GAP));
            assert_eq!(watch.read().flags, 0);
            assert!(watch.sample(Some(head), start + Duration::from_millis(800), REAL_SAMPLE_GAP));
            let state = watch.read();
            if clock == g::REAL {
                assert_eq!(state.flags, g::LEAVE | g::ENTER);
                assert_eq!(state.milliseconds, 0);
                for ms in [1000, 1200] {
                    assert!(!watch.sample(Some(head), start + Duration::from_millis(ms), REAL_SAMPLE_GAP));
                }
                assert!(watch.sample(Some(head), start + Duration::from_millis(1400), REAL_SAMPLE_GAP));
                assert_eq!(watch.read().flags, g::REACHED | g::HELD);
            } else {
                assert_eq!(state.flags, g::REACHED | g::HELD);
            }
            assert!(watch.sample(Some(RayHit { part: 8, ..head }), start + Duration::from_millis(1450), REAL_SAMPLE_GAP));
            assert_eq!(watch.read().flags, g::LEAVE);
            assert_eq!(watch.read().flags, 0);
        }
    }
}
