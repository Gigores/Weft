use std::time::Duration;

use crate::{abi, host};

use super::Context;
use abi::gaze as g;

/// A server-side watch for the first entity under someone's crosshair.
#[derive(Clone, Copy)]
pub struct Gaze(pub(super) abi::GazeOptions);

impl Gaze {
    /// Watch an entity within range. Target zero accepts any entity.
    pub fn at(target: u64, range: f64) -> Self {
        Self(abi::GazeOptions { target, range, threshold: 0, part: g::ANY_PART, clock: g::TICKS })
    }

    /// Require the named hitbox part to be the first hit.
    pub fn part(mut self, name: &str) -> Self {
        self.0.part = abi::hitbox_part_id(name);
        self
    }

    /// Threshold that advances with simulation time.
    pub fn for_ticks(mut self, ticks: u32) -> Self {
        self.0.threshold = u64::from(ticks);
        self.0.clock = g::TICKS;
        self
    }

    /// Threshold that advances with monotonic server time. Long breaks in sampling reset it.
    pub fn for_real_time(mut self, duration: Duration) -> Self {
        self.0.threshold = duration.as_millis().min(u128::from(u64::MAX)) as u64;
        self.0.clock = g::REAL;
        self
    }
}

#[derive(Clone, Copy)]
pub struct GazeState(abi::GazeState);

impl GazeState {
    pub fn hit(self) -> Option<abi::RayHit> {
        (self.0.hit.entity != 0).then_some(self.0.hit)
    }

    pub fn entered(self) -> bool {
        self.0.flags & g::ENTER != 0
    }

    pub fn left(self) -> bool {
        self.0.flags & g::LEAVE != 0
    }

    pub fn reached_threshold(self) -> bool {
        self.0.flags & g::REACHED != 0
    }

    pub fn held(self) -> bool {
        self.0.flags & g::HELD != 0
    }

    pub fn ticks(self) -> u64 {
        self.0.ticks
    }

    pub fn real_time(self) -> Duration {
        Duration::from_millis(self.0.milliseconds)
    }
}

impl Context<'_> {
    /// Subscribe in a sensor or offer. The rule wakes on entry, exit and threshold crossing.
    /// Each evaluation renews the watches it reads. Transition flags are coalesced until read.
    /// A threshold crossing belongs to the current focus and expires when that focus ends.
    pub fn watch_gaze(&self, viewer: u64, gaze: Gaze) -> GazeState {
        let mut state = abi::GazeState::default();
        unsafe { (host().brain_gaze)(self.world().raw(), self.id(), viewer, &gaze.0, &mut state) };
        GazeState(state)
    }
}
