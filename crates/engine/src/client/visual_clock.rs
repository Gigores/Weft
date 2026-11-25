use crate::world::TPS;

const PREDICTION_LIMIT: f64 = 2.0 * TPS as f64;
const MAX_SLEW: f64 = 0.25;

#[derive(Clone, Copy)]
struct Sample {
    dimension: u32,
    day: u64,
    ticks: u64,
    nominal_rate: f64,
}

impl Sample {
    fn day_offset(self) -> i128 {
        i128::from(self.day) - i128::from(self.ticks)
    }

    fn same_timeline(self, other: Self) -> bool {
        self.dimension == other.dimension && self.ticks <= other.ticks && self.day_offset() == other.day_offset()
    }
}

/// The clock rendered by the client. Ordinary clock packets adjust its pace,
/// while a new dimension or an explicit time change can move it to a new phase.
#[derive(Default)]
pub(super) struct VisualClock {
    sample: Option<Sample>,
    elapsed: f64,
    speed: f64,
    pub(super) time: f64,
}

impl VisualClock {
    pub(super) fn synchronize(&mut self, dimension: u32, day: u64, ticks: u64, nominal_rate: f64) {
        let next = Sample { dimension, day, ticks, nominal_rate };
        let mut speed = nominal_rate;
        if let Some(previous) = self.sample {
            if previous.same_timeline(next) {
                let advanced = ticks - previous.ticks;
                let same_rate = previous.nominal_rate == nominal_rate;
                if advanced == 0 && same_rate && self.elapsed < f64::from(TPS) / nominal_rate * 0.5 {
                    // Teleports can send a second copy of the current clock.
                    return;
                }
                if advanced == 0 {
                    speed = 0.0;
                    self.time = day as f64;
                } else {
                    if day as f64 - self.time > 2.0 * PREDICTION_LIMIT {
                        // A tick sprint can bring many seconds of world time in one frame.
                        self.time = day as f64;
                    }
                    if same_rate && self.speed > 0.0 {
                        speed = self.speed;
                        if advanced >= u64::from(TPS) && self.elapsed > 0.0 {
                            let observed = (advanced as f64 / self.elapsed).min(nominal_rate);
                            speed += (observed - speed) * 0.5;
                        }
                    }
                }
            } else {
                self.time = day as f64;
                if previous.dimension == dimension && previous.ticks == ticks {
                    speed = self.speed;
                }
            }
        } else {
            self.time = day as f64;
        }
        self.sample = Some(next);
        self.elapsed = 0.0;
        self.speed = speed;
    }

    pub(super) fn advance(&mut self, dt: f64) {
        let Some(sample) = self.sample else { return };
        let previous_lead = (self.elapsed * self.speed).min(PREDICTION_LIMIT);
        self.elapsed += dt;
        let lead = (self.elapsed * self.speed).min(PREDICTION_LIMIT);
        let step = lead - previous_lead;
        let error = sample.day as f64 + lead - (self.time + step);
        let max_adjustment = self.speed * MAX_SLEW;
        let adjustment = (error * 2.0).clamp(-max_adjustment, max_adjustment) * dt;
        self.time += (step + adjustment).max(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uneven_clock_packets_only_change_the_rate() {
        let mut clock = VisualClock::default();
        let mut ticks = 1_000;
        clock.synchronize(0, ticks, ticks, 20.0);
        for delivered_after in [1.4, 0.6, 1.8, 1.7, 1.9, 1.8, 1.7, 1.8] {
            let dt = delivered_after / 120.0;
            for _ in 0..120 {
                let before = clock.time;
                clock.advance(dt);
                assert!(clock.time >= before && clock.time - before <= 25.0 * dt + 1e-10);
            }
            let before = clock.time;
            ticks += 20;
            clock.synchronize(0, ticks, ticks, 20.0);
            assert_eq!(clock.time, before);
        }
        assert!((clock.speed - 20.0 / 1.8).abs() < 1.0);
        assert!((clock.time - ticks as f64).abs() < 10.0);
    }

    #[test]
    fn duplicate_packets_and_a_stopped_clock_are_distinct() {
        let mut clock = VisualClock::default();
        clock.synchronize(0, 100, 100, 20.0);
        clock.advance(0.25);
        clock.synchronize(0, 100, 100, 20.0);
        assert_eq!(clock.time, 105.0);
        clock.advance(0.75);
        clock.synchronize(0, 100, 100, 20.0);
        clock.advance(2.0);
        assert_eq!(clock.time, 100.0);
        clock.synchronize(0, 42, 100, 20.0);
        clock.advance(2.0);
        assert_eq!(clock.time, 42.0);
        clock.synchronize(0, 62, 120, 20.0);
        clock.advance(0.5);
        assert!(clock.time > 42.0);
    }

    #[test]
    fn explicit_jumps_rebase_the_visual_clock() {
        let mut clock = VisualClock::default();
        clock.synchronize(0, 900, 100, 20.0);
        clock.advance(0.5);
        clock.synchronize(0, 50, 120, 20.0);
        assert_eq!(clock.time, 50.0);
        clock.synchronize(0, 1_050, 1_120, 20.0);
        assert_eq!(clock.time, 1_050.0);
        clock.synchronize(1, 200, 300, 20.0);
        assert_eq!(clock.time, 200.0);
    }
}
