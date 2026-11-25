use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use glam::DVec3;
use kira::Frame;
use kira::effect::filter::{FilterBuilder, FilterHandle};
use kira::info::Info;
use kira::sound::{Sound, SoundData};
use kira::track::{SpatialTrackBuilder, SpatialTrackHandle, TrackBuilder, TrackHandle};
use kira::{Decibels, Tween};

use super::{Arrival, SourceFilter, audio_position, db, travel_time};

pub(super) const REACH: f64 = 48.0;
const ACOUSTICS_INTERVAL: Duration = Duration::from_millis(100);
const TWEEN: Tween = Tween { start_time: kira::StartTime::Immediate, duration: Duration::from_millis(30), easing: kira::Easing::Linear };
const QUEUE_MS: usize = 120;

struct Feed {
    send: Sender<f32>,
    pcm: Receiver<f32>,
    capacity: usize,
    delay: AtomicU32,
    closed: AtomicBool,
}

impl Feed {
    fn new(rate: u32, delay: f64) -> Self {
        let capacity = rate as usize * QUEUE_MS / 1000;
        let (send, pcm) = bounded(capacity);
        Self { send, pcm, capacity, delay: AtomicU32::new((delay * rate as f64) as u32), closed: AtomicBool::new(false) }
    }

    fn push(&self, samples: &[f32]) {
        for &sample in &samples[samples.len().saturating_sub(self.capacity)..] {
            let sample = if sample.is_finite() { sample.clamp(-1.0, 1.0) } else { 0.0 };
            if let Err(TrySendError::Full(sample)) = self.send.try_send(sample) {
                let _ = self.pcm.try_recv();
                let _ = self.send.try_send(sample);
            }
        }
    }
}

struct Pcm {
    feed: Arc<Feed>,
    rate: f64,
    history: Vec<f32>,
    next: usize,
    phase: f64,
    delay: f64,
}

impl Pcm {
    fn new(feed: Arc<Feed>, rate: u32) -> Self {
        let delay = f64::from(feed.delay.load(Ordering::Relaxed));
        Self { feed, rate: rate.into(), history: vec![0.0; rate as usize / 4 + 4], next: 0, phase: 0.0, delay }
    }

    fn sample(&mut self, step: f64, target: f64) -> f32 {
        self.phase += step;
        while self.phase >= 1.0 {
            self.phase -= 1.0;
            self.history[self.next] = self.feed.pcm.try_recv().unwrap_or(0.0);
            self.next = (self.next + 1) % self.history.len();
        }
        // Slew changes so a move or a different reflection does not jump in the waveform.
        self.delay += (target - self.delay).clamp(-0.2 * step, 0.2 * step);
        let back = self.delay + 2.0 - self.phase;
        let n = back as usize;
        let k = (back - n as f64) as f32;
        let len = self.history.len();
        let a = self.history[(self.next + len - n) % len];
        let b = self.history[(self.next + len - n - 1) % len];
        a + (b - a) * k
    }
}

impl SoundData for Pcm {
    type Error = Infallible;
    type Handle = ();

    fn into_sound(self) -> Result<(Box<dyn Sound>, ()), Infallible> {
        Ok((Box::new(self), ()))
    }
}

impl Sound for Pcm {
    fn process(&mut self, out: &mut [Frame], dt: f64, _: &Info) {
        let target = f64::from(self.feed.delay.load(Ordering::Relaxed));
        for frame in out {
            *frame = Frame::from_mono(self.sample(dt * self.rate, target));
        }
    }

    fn finished(&self) -> bool {
        self.feed.closed.load(Ordering::Relaxed)
    }
}

enum Track {
    Spatial(SpatialTrackHandle),
    Plain(TrackHandle),
}

struct Path {
    feed: Arc<Feed>,
    track: Track,
    filter: FilterHandle,
    previous: Option<(glam::Vec3, f32, f32)>,
}

impl Drop for Path {
    fn drop(&mut self) {
        self.feed.closed.store(true, Ordering::Relaxed);
    }
}

pub(super) struct Stream {
    pub pos: Option<DVec3>,
    pub custom_filter: SourceFilter,
    rate: u32,
    paths: Vec<Path>,
    arrivals: [Option<Arrival>; 3],
    traced: Option<Instant>,
}

impl Stream {
    pub fn new(
        group: &mut TrackHandle,
        listener: &kira::listener::ListenerHandle,
        ear: DVec3,
        rate: u32,
        pos: Option<DVec3>,
    ) -> Option<Self> {
        let mut paths = Vec::new();
        if let Some(pos) = pos {
            for _ in 0..3 {
                let mut builder = SpatialTrackBuilder::new()
                    .distances((1.5, REACH as f32))
                    .attenuation_function(kira::Easing::Linear)
                    .volume(Decibels::SILENCE)
                    .sub_track_capacity(0)
                    .sound_capacity(1);
                let filter = builder.add_effect(FilterBuilder::new().cutoff(20_000.0));
                let mut track = group
                    .add_spatial_sub_track(listener, audio_position(pos, ear), builder)
                    .ok()?;
                let feed = Arc::new(Feed::new(rate, travel_time(pos.distance(ear).min(REACH))));
                track.play(Pcm::new(feed.clone(), rate)).ok()?;
                paths.push(Path { feed, track: Track::Spatial(track), filter, previous: None });
            }
        } else {
            let mut builder = TrackBuilder::new().sub_track_capacity(0).sound_capacity(1);
            let filter = builder.add_effect(FilterBuilder::new().cutoff(20_000.0));
            let mut track = group.add_sub_track(builder).ok()?;
            let feed = Arc::new(Feed::new(rate, 0.0));
            track.play(Pcm::new(feed.clone(), rate)).ok()?;
            paths.push(Path { feed, track: Track::Plain(track), filter, previous: None });
        }
        Some(Self { pos, custom_filter: SourceFilter::default(), rate, paths, arrivals: [None; 3], traced: None })
    }

    pub fn push(&self, samples: &[f32]) {
        for path in &self.paths {
            path.feed.push(samples);
        }
    }

    pub fn update(&mut self, ear: DVec3, mut trace: impl FnMut(DVec3) -> [Option<Arrival>; 3]) {
        let pos = self.pos;
        let now = Instant::now();
        if let Some(pos) = pos
            && self.traced.is_none_or(|t| now - t >= ACOUSTICS_INTERVAL)
        {
            self.arrivals = trace(pos);
            self.traced = Some(now);
        }
        for (i, path) in self.paths.iter_mut().enumerate() {
            let arrival = self.arrivals[i].filter(|_| pos.is_some_and(|p| p.distance_squared(ear) < REACH * REACH));
            let (at, gain, cutoff) = if let Some(a) = arrival {
                let at = if i == 0 { pos.unwrap_or(a.pos) } else { a.pos };
                let distance = if i == 0 { at.distance(ear) } else { a.distance };
                let delay = self.rate as f64 * travel_time(distance.min(REACH));
                path.feed.delay.store(delay as u32, Ordering::Relaxed);
                (audio_position(at, ear), a.gain * self.custom_filter.gain, a.cutoff.min(self.custom_filter.cutoff))
            } else {
                (glam::Vec3::ZERO, if pos.is_none() { self.custom_filter.gain } else { 0.0 }, self.custom_filter.cutoff)
            };
            let old = path.previous.replace((at, gain, cutoff));
            if old.is_none_or(|v| v.2 != cutoff) {
                path.filter.set_cutoff(f64::from(cutoff), TWEEN);
            }
            match &mut path.track {
                Track::Spatial(track) => {
                    if gain != 0.0 && old.is_none_or(|v| v.0 != at || v.1 == 0.0) {
                        track.set_position(at, TWEEN);
                    }
                    if old.is_none_or(|v| v.1 != gain) {
                        track.set_volume(db(gain.into()), TWEEN);
                    }
                }
                Track::Plain(track) if old.is_none_or(|v| v.1 != gain) => track.set_volume(db(gain.into()), TWEEN),
                Track::Plain(_) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_queue_keeps_the_newest_pcm_and_silences_underruns() {
        let feed = Arc::new(Feed::new(8_000, 0.0));
        feed.push(&vec![-1.0; feed.capacity]);
        feed.push(&[0.5, 0.25]);
        let mut pcm = Pcm::new(feed.clone(), 8_000);
        for _ in 0..feed.capacity - 2 {
            pcm.sample(1.0, 0.0);
        }
        assert_eq!(pcm.sample(1.0, 0.0), -1.0);
        assert_eq!(pcm.sample(1.0, 0.0), 0.5);
        assert_eq!(pcm.sample(1.0, 0.0), 0.25);
        assert_eq!(pcm.sample(1.0, 0.0), 0.0);
    }

    #[test]
    fn propagation_delay_precedes_the_samples() {
        let feed = Arc::new(Feed::new(8_000, 3.0 / 8_000.0));
        feed.push(&[1.0, 0.0]);
        let mut pcm = Pcm::new(feed, 8_000);
        for _ in 0..4 {
            assert_eq!(pcm.sample(1.0, 3.0), 0.0);
        }
        assert_eq!(pcm.sample(1.0, 3.0), 1.0);
        assert_eq!(pcm.sample(1.0, 3.0), 0.0);
    }

    #[test]
    fn mono_stream_is_panned_in_the_stereo_mixer() {
        use kira::backend::mock::{MockBackend, MockBackendSettings};
        use kira::effect::{Effect, EffectBuilder};
        use kira::{AudioManager, AudioManagerSettings};

        #[derive(Clone, Default)]
        struct Meter(Arc<[AtomicU32; 2]>);

        impl EffectBuilder for Meter {
            type Handle = ();
            fn build(self) -> (Box<dyn Effect>, ()) {
                (Box::new(self), ())
            }
        }

        impl Effect for Meter {
            fn process(&mut self, input: &mut [Frame], _: f64, _: &Info) {
                for sample in input {
                    self.0[0].fetch_add((sample.left.abs() * 1000.0) as u32, Ordering::Relaxed);
                    self.0[1].fetch_add((sample.right.abs() * 1000.0) as u32, Ordering::Relaxed);
                }
            }
        }

        let mut audio = AudioManager::<MockBackend>::new(AudioManagerSettings {
            backend_settings: MockBackendSettings { sample_rate: 48_000 },
            ..Default::default()
        })
        .unwrap();
        let listener = audio
            .add_listener(glam::Vec3::ZERO, glam::Quat::IDENTITY)
            .unwrap();
        let meter = Meter::default();
        let mut group = audio
            .add_sub_track(TrackBuilder::new().with_effect(meter.clone()))
            .unwrap();
        let mut stream = Stream::new(&mut group, &listener, DVec3::ZERO, 48_000, Some(DVec3::X * 8.0)).unwrap();
        let mut render = |source: DVec3| {
            stream.pos = Some(source);
            stream.update(DVec3::ZERO, |pos| [Some(Arrival { pos, distance: 8.0, gain: 1.0, cutoff: 20_000.0 }), None, None]);
            for i in 0..45 {
                if i == 30 {
                    meter.0.iter().for_each(|v| v.store(0, Ordering::Relaxed));
                }
                stream.push(&[0.3; 128]);
                audio.backend_mut().on_start_processing();
                audio.backend_mut().process();
            }
            meter.0.each_ref().map(|v| v.load(Ordering::Relaxed))
        };
        let right = render(DVec3::X * 8.0);
        let left = render(DVec3::NEG_X * 8.0);
        assert!(right[1] > right[0] + 100, "{right:?}");
        assert!(left[0] > left[1] + 100, "{left:?}");
    }
}
