use std::rc::Rc;

use crate::app::Ctx;

pub type Change = Rc<dyn Fn(&mut Ctx, f32)>;
pub type Bind = Box<dyn Fn(&Ctx, &mut [f32])>;

pub struct Thumb {
    pub value: f32,
    pub change: Option<Change>,
    pub release: Option<Change>,
}

impl Thumb {
    pub fn new(value: f32, change: impl Fn(&mut Ctx, f32) + 'static) -> Self {
        Self { value, change: Some(Rc::new(change)), release: None }
    }
}

pub struct Slider {
    pub thumbs: Vec<Thumb>,
    pub active: usize,
    pub enabled: bool,
    pub ordered: bool,
    pub snap: bool,
    pub steps: u32,
    pub step_value: Option<Box<dyn Fn(f32, f32) -> f32>>,
    pub track: String,
    pub fill: Option<[usize; 2]>,
    pub binding: Option<(Vec<f32>, Bind)>,
    overlapping: bool,
}

impl Slider {
    pub const KNOB_WIDTH: f32 = 8.0;

    pub fn new(thumbs: Vec<Thumb>) -> Self {
        Self {
            thumbs,
            active: 0,
            enabled: true,
            ordered: false,
            snap: false,
            steps: 100,
            step_value: None,
            track: "slider".into(),
            fill: None,
            binding: None,
            overlapping: false,
        }
    }

    pub fn value_at(x: f32, width: f32) -> f32 {
        ((x - Self::KNOB_WIDTH * 0.5) / (width - Self::KNOB_WIDTH).max(1.0)).clamp(0.0, 1.0)
    }

    pub fn begin(&mut self, value: f32) {
        if let Some((index, thumb)) = self
            .thumbs
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| (a.value - value).abs().total_cmp(&(b.value - value).abs()))
        {
            self.active = index;
            self.overlapping = self.ordered
                && self
                    .thumbs
                    .iter()
                    .enumerate()
                    .any(|(i, t)| i != index && t.value == thumb.value);
        }
    }

    pub fn drag(&mut self, value: f32) -> Option<(Option<Change>, f32)> {
        let value = self.position(value);
        if self.overlapping {
            let same = self.thumbs.get(self.active)?.value;
            if (value - same).abs() < 1e-4 {
                return None;
            }
            if value > same {
                self.active = self.thumbs.iter().rposition(|t| t.value == same)?;
            }
            self.overlapping = false;
        }
        self.set(value)
    }

    pub fn nudge(&mut self, by: f32) -> Option<(Option<Change>, f32)> {
        let value = self.thumbs.get(self.active)?.value;
        let steps = self.steps.max(1) as f32;
        let next = self
            .step_value
            .as_ref()
            .map_or_else(|| ((value * steps).round() + by) / steps, |step| step(value, by));
        self.set(self.position(next))
    }

    pub fn poll(&mut self, ctx: &Ctx, held: bool) -> bool {
        if held && self.thumbs.len() == 1 && self.thumbs[0].release.is_some() {
            return false;
        }
        let Some((values, bind)) = &mut self.binding else { return false };
        for (value, thumb) in values.iter_mut().zip(&self.thumbs) {
            *value = thumb.value;
        }
        bind(ctx, values);
        let mut changed = false;
        let mut min = 0.0;
        let fixed = self
            .thumbs
            .get(self.active)
            .filter(|t| held && t.release.is_some())
            .map(|t| t.value);
        for (index, (thumb, &value)) in self.thumbs.iter_mut().zip(values.iter()).enumerate() {
            if !(held && index == self.active && thumb.release.is_some()) {
                if value.is_finite() {
                    let max = if self.ordered && index < self.active { fixed.unwrap_or(1.0) } else { 1.0 };
                    let value = value.clamp(min, max);
                    if value != thumb.value {
                        changed = true;
                        thumb.value = value;
                    }
                }
            }
            if self.ordered {
                min = thumb.value;
            }
        }
        changed
    }

    fn position(&self, value: f32) -> f32 {
        if self.snap {
            let steps = self.steps.max(1) as f32;
            (value * steps).round() / steps
        } else {
            value
        }
    }

    fn set(&mut self, value: f32) -> Option<(Option<Change>, f32)> {
        let index = self.active;
        let min = if self.ordered && index > 0 { self.thumbs[index - 1].value } else { 0.0 };
        let max = if self.ordered { self.thumbs.get(index + 1).map_or(1.0, |t| t.value) } else { 1.0 };
        let thumb = self.thumbs.get_mut(index)?;
        let value = value.max(min).min(max);
        if value == thumb.value {
            return None;
        }
        thumb.value = value;
        Some((thumb.change.clone(), value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slider(values: impl IntoIterator<Item = f32>) -> Slider {
        Slider::new(
            values
                .into_iter()
                .map(|value| Thumb::new(value, |_, _| {}))
                .collect(),
        )
    }

    #[test]
    fn coincident_range_endpoints_separate_in_either_direction() {
        for (direction, moved) in [(0.2, 0), (0.8, 1)] {
            let mut range = slider([0.5, 0.5]);
            range.ordered = true;
            range.steps = 10;
            range.snap = true;
            range.begin(0.5);
            assert!(range.drag(0.5).is_none());
            assert_eq!(range.drag(direction).map(|(_, value)| value), Some(direction));
            assert_eq!(range.active, moved);
            assert_eq!(range.thumbs[1 - moved].value, 0.5);
            for _ in 0..10 {
                range.nudge(if moved == 0 { 1.0 } else { -1.0 });
            }
            assert_eq!(range.thumbs[moved].value, 0.5);
        }
    }

    #[test]
    fn many_free_handles_keep_the_picked_index_across_other_handles() {
        let mut track = slider((0..100).map(|i| i as f32 / 100.0));
        track.begin(0.37);
        track.drag(0.9);
        assert_eq!(track.active, 37);
        assert_eq!(track.thumbs[37].value, 0.9);
        assert_eq!(track.thumbs[38].value, 0.38);
        assert_eq!(Slider::value_at(4.0, 108.0), 0.0);
        assert_eq!(Slider::value_at(104.0, 108.0), 1.0);
    }
}
