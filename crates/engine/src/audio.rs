//! Sound: events (`registry::SoundDef`) play one of their files, positioned in the world or not;
//! every category has its own volume (`core:volume_*` settings). All categories share one
//! environment track with low-pass filtering, volume gain and a reverb send.
//! Music streams on its own track after quiet stretches. Without an audio device everything
//! here does nothing.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use glam::{DVec3, Quat};
use kira::effect::filter::{FilterBuilder, FilterHandle};
use kira::effect::reverb::{ReverbBuilder, ReverbHandle};
use kira::sound::PlaybackState;
use kira::sound::static_sound::{StaticSoundData, StaticSoundHandle};
use kira::sound::streaming::{StreamingSoundData, StreamingSoundHandle};
use kira::track::{SendTrackBuilder, SendTrackHandle, SpatialTrackBuilder, SpatialTrackHandle, TrackBuilder, TrackHandle};
use kira::{AudioManager, AudioManagerSettings, Decibels, DefaultBackend, Mix, Tween};
use rustc_hash::FxHashMap;
use weft_abi::{MusicSource, MusicStatus, Str, local_music as op};

use crate::assets::Assets;
use crate::content::content;

mod live;

pub(crate) const STREAM_REACH: f64 = live::REACH;
const MAX_STREAMS: usize = 32;

/// A sound to play this frame: event id, position (`None` = not positioned), volume, pitch (0 =
/// the event's random range).
#[derive(Clone, Copy, Debug)]
pub struct Play {
    pub sound: u32,
    pub pos: Option<DVec3>,
    pub volume: f32,
    pub pitch: f32,
}

pub(crate) fn travel_time(distance: f64) -> f64 {
    distance / 343.0
}

#[derive(Clone, Copy)]
pub(crate) struct Arrival {
    pub pos: DVec3,
    pub distance: f64,
    pub gain: f32,
    pub cutoff: f32,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct SourceFilter {
    pub cutoff: f32,
    pub gain: f32,
}

impl Default for SourceFilter {
    fn default() -> Self {
        Self { cutoff: 20_000.0, gain: 1.0 }
    }
}

impl SourceFilter {
    pub fn from_liquid(source: u16, listener: Option<u16>) -> Self {
        if Some(source) == listener {
            return Self::default();
        }
        content().liquids[source as usize].source_filter
    }
}

#[derive(Clone, Copy, PartialEq)]
pub struct Acoustics {
    pub cutoff: f32,
    pub reverb: f32,
    pub volume: f32,
    pub feedback: f32,
    pub damping: f32,
}

impl Default for Acoustics {
    fn default() -> Self {
        Self { cutoff: 20_000.0, reverb: 0.0, volume: 1.0, feedback: 0.85, damping: 0.60 }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AudioParams {
    pub biome: Option<u16>,
    pub weather: u16,
    pub rain: f32,
    pub enclosed: bool,
    pub daylight: f32,
}

/// Where the listener is, for positions and the environment.
pub struct Listener {
    pub pos: DVec3,
    pub yaw: f32,
    pub pitch: f32,
    pub acoustics: Acoustics,
    pub dim: u32,
    /// The conditions music asks for that hold now, by id.
    pub holds: Vec<u32>,
    pub params: AudioParams,
    pub biome_dist: Vec<(u16, f32)>,
}

pub struct Audio {
    mgr: AudioManager<DefaultBackend>,
    listener: kira::listener::ListenerHandle,
    listener_position: Option<(u32, DVec3)>,
    voices: Vec<SpatialVoice>,
    streams: FxHashMap<u64, live::Stream>,
    next_stream: u64,
    reverb: SendTrackHandle,
    reverb_shape: ReverbHandle,
    environment: TrackHandle,
    lowpass: FilterHandle,
    /// Per category (`sound_category`): its track.
    groups: Vec<TrackHandle>,
    files: FxHashMap<String, Option<StaticSoundData>>,
    /// Files of a `name*` pattern.
    globs: FxHashMap<String, Vec<String>>,
    music: Option<StreamingSoundHandle<kira::sound::FromFileError>>,
    next_music: Instant,
    beds: FxHashMap<usize, (StaticSoundHandle, f32)>,
    spots: FxHashMap<usize, Instant>,
    state: Acoustics,
    /// Per category, then the master volume.
    volumes: Vec<f64>,
    rng: weft_noise::Rng,
    /// Sounds started so far.
    pub played: u32,
    /// Automated runs (tests, scripts): everything plays and counts, silently.
    pub muted: bool,
    active: bool,
    /// Each category's volume setting id, then the master volume's, looked up once.
    volume_ids: Option<Vec<Option<u32>>>,
}

struct SpatialVoice {
    pos: DVec3,
    track: SpatialTrackHandle,
    sound: StaticSoundHandle,
}

fn audio_position(source: DVec3, listener: DVec3) -> glam::Vec3 {
    (source - listener).as_vec3()
}

pub(crate) struct LocalMusic {
    tracks: Vec<LocalTrack>,
    volume: f32,
    playing: Option<LocalPlayback>,
    failed: Option<MusicSource>,
}

#[derive(PartialEq)]
struct LocalTrack {
    name: String,
    stamp: (u64, Option<std::time::SystemTime>),
    duration: f64,
}

struct LocalPlayback {
    source: MusicSource,
    name: String,
    duration: f64,
    paused: bool,
    sound: StreamingSoundHandle<kira::sound::FromFileError>,
    track: SpatialTrackHandle,
    filter: FilterHandle,
    source_filter: SourceFilter,
}

impl Default for LocalMusic {
    fn default() -> Self {
        Self { tracks: Vec::new(), volume: 1.0, playing: None, failed: None }
    }
}

pub(crate) fn music_folder(run: &Path) -> PathBuf {
    run.join("music")
}

impl LocalMusic {
    pub fn refresh(&mut self, folder: &Path) -> std::io::Result<bool> {
        std::fs::create_dir_all(folder)?;
        let mut tracks: Vec<_> = std::fs::read_dir(folder)?
            .flatten()
            .filter_map(|entry| {
                if !entry.file_type().ok()?.is_file() {
                    return None;
                }
                let path = entry.path();
                let format = path.extension()?.to_str()?;
                if !["mp3", "flac", "ogg", "wav"]
                    .iter()
                    .any(|ext| format.eq_ignore_ascii_case(ext))
                {
                    return None;
                }
                let name = entry.file_name().into_string().ok()?;
                let metadata = entry.metadata().ok()?;
                let stamp = (metadata.len(), metadata.modified().ok());
                let duration = self
                    .tracks
                    .binary_search_by(|track| track.name.cmp(&name))
                    .ok()
                    .map(|i| &self.tracks[i])
                    .filter(|track| track.stamp == stamp)
                    .map_or_else(
                        || StreamingSoundData::from_file(&path).map_or(0.0, |data| data.duration().as_secs_f64()),
                        |track| track.duration,
                    );
                Some(LocalTrack { name, stamp, duration })
            })
            .collect();
        tracks.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        let changed = self.tracks != tracks;
        self.tracks = tracks;
        Ok(changed)
    }

    pub fn track(&self, index: u32) -> weft_abi::MusicTrack {
        self.tracks
            .get(index as usize)
            .map_or(weft_abi::MusicTrack { file: Str::EMPTY, duration: 0.0 }, |track| weft_abi::MusicTrack {
                file: Str::new(&track.name),
                duration: track.duration,
            })
    }

    pub fn status(&self, source: MusicSource) -> MusicStatus {
        let empty = MusicStatus { track: Str::EMPTY, position: 0.0, duration: 0.0, volume: self.volume, state: op::STOPPED };
        if self.failed == Some(source) {
            return MusicStatus { state: op::ERROR, ..empty };
        }
        self.playing
            .as_ref()
            .filter(|p| p.source == source)
            .map_or(empty, |p| MusicStatus {
                track: Str::new(&p.name),
                position: p.sound.position(),
                duration: p.duration,
                volume: self.volume,
                state: if p.paused { op::PAUSED } else { op::PLAYING },
            })
    }

    pub fn source(&self) -> Option<MusicSource> {
        self.playing.as_ref().map(|p| p.source)
    }

    pub fn fail(&mut self, source: MusicSource) {
        if self.source() == Some(source) {
            self.playing = None;
        }
        self.failed = Some(source);
    }

    pub fn update(&mut self, source: Option<(DVec3, SourceFilter)>, listener: DVec3) -> bool {
        let Some(playback) = &mut self.playing else { return false };
        if let Some(error) = playback.sound.pop_error() {
            log::warn!("local music {}: {error}", playback.name);
            self.failed = Some(playback.source);
        } else if let Some((position, filter)) = source.filter(|_| playback.sound.state() != PlaybackState::Stopped) {
            playback
                .track
                .set_position(audio_position(position, listener), Tween::default());
            if filter != playback.source_filter {
                playback.filter.set_cutoff(f64::from(filter.cutoff), QUICK);
                playback.track.set_volume(db(f64::from(filter.gain)), QUICK);
                playback.source_filter = filter;
                log::trace!(target: "acoustics::paths", "local music at {position}: source cutoff {:.0}, gain {:.3}", filter.cutoff, filter.gain);
            }
            return false;
        }
        self.playing = None;
        true
    }

    pub fn control(&mut self, source: MusicSource, op: u32, value: f64) -> bool {
        if op == op::VOLUME && value.is_finite() {
            self.volume = value.clamp(0.0, 1.0) as f32;
            if let Some(playback) = &mut self.playing {
                playback
                    .sound
                    .set_volume(db(f64::from(self.volume)), Tween::default());
            }
            return true;
        }
        if op == op::STOP {
            let failed = self.failed == Some(source);
            if failed {
                self.failed = None;
            }
            let active = self.playing.as_ref().is_some_and(|p| p.source == source);
            if active {
                self.playing = None;
            }
            return active || failed;
        }
        let Some(playback) = self.playing.as_mut().filter(|p| p.source == source) else { return false };
        match op {
            op::TOGGLE_PAUSE => {
                if playback.paused {
                    playback.sound.resume(Tween::default());
                } else {
                    playback.sound.pause(Tween::default());
                }
                playback.paused = !playback.paused;
            }
            op::SEEK if value.is_finite() => playback
                .sound
                .seek_to(playback.duration * value.clamp(0.0, 1.0)),
            _ => return false,
        }
        true
    }
}

fn sound_path(assets: &Assets, name: &str) -> Option<std::path::PathBuf> {
    assets
        .path(name, "sounds", "ogg")
        .or_else(|| assets.path(name, "sounds", "wav"))
}

const QUICK: Tween = Tween { start_time: kira::StartTime::Immediate, duration: Duration::from_millis(400), easing: kira::Easing::Linear };

fn db(volume: f64) -> Decibels {
    if volume <= 0.001 { Decibels::SILENCE } else { Decibels((20.0 * volume.log10()) as f32) }
}

impl Audio {
    pub fn new() -> Option<Audio> {
        let mut mgr = match AudioManager::<DefaultBackend>::new(AudioManagerSettings::default()) {
            Ok(m) => m,
            Err(e) => {
                log::warn!("no sound: {e}");
                return None;
            }
        };
        let made = (|| -> Result<Audio, kira::ResourceLimitReached> {
            let listener = mgr.add_listener(glam::Vec3::ZERO, Quat::IDENTITY)?;
            let acoustics = Acoustics::default();
            let mut wet = SendTrackBuilder::new();
            let reverb_shape = wet.add_effect(
                ReverbBuilder::new()
                    .mix(Mix::WET)
                    .damping(f64::from(acoustics.damping))
                    .feedback(f64::from(acoustics.feedback)),
            );
            let reverb = mgr.add_send_track(wet)?;
            let mut track = TrackBuilder::new()
                .sub_track_capacity(content().game.sound_categories.len())
                .with_send(&reverb, Decibels::SILENCE);
            let lowpass = track.add_effect(FilterBuilder::new().cutoff(acoustics.cutoff as f64));
            let mut environment = mgr.add_sub_track(track)?;
            let mut groups = Vec::new();
            for _ in &crate::content::content().game.sound_categories {
                let b = TrackBuilder::new()
                    .sub_track_capacity(256)
                    .sound_capacity(256);
                groups.push(environment.add_sub_track(b)?);
            }
            Ok(Audio {
                mgr,
                listener,
                listener_position: None,
                voices: Vec::new(),
                streams: FxHashMap::default(),
                next_stream: 1,
                reverb,
                reverb_shape,
                environment,
                lowpass,
                groups,
                files: FxHashMap::default(),
                globs: FxHashMap::default(),
                music: None,
                next_music: Instant::now() + Duration::from_secs(20),
                beds: FxHashMap::default(),
                spots: FxHashMap::default(),
                state: acoustics,
                volumes: Vec::new(),
                rng: weft_noise::Rng(0x2545_F491_4F6C_DD1D),
                played: 0,
                muted: false,
                active: true,
                volume_ids: None,
            })
        })();
        match made {
            Ok(a) => Some(a),
            Err(e) => {
                log::warn!("no sound: {e:?}");
                None
            }
        }
    }

    fn random(&mut self) -> f32 {
        self.rng.float()
    }

    pub fn set_active(&mut self, active: bool) {
        if self.active == active {
            return;
        }
        self.active = active;
        self.apply_master_volume();
        if let Some(music) = &mut self.music {
            if active {
                music.resume(Tween::default());
            } else {
                music.pause(Tween::default());
            }
        }
    }

    fn apply_master_volume(&mut self) {
        let volume = if self.muted || !self.active { 0.0 } else { self.volumes.last().copied().unwrap_or(1.0) };
        self.mgr
            .main_track()
            .set_volume(db(volume), Tween::default());
    }

    /// Forgets decoded files, for packs that changed them (F3+T).
    pub fn reload(&mut self) {
        self.files.clear();
        self.globs.clear();
    }

    /// Decoded once, on first use: a file is a few kilobytes and decodes in well under a
    /// millisecond, so nothing waits for a sound on a screen or in the world.
    fn file(&mut self, assets: &Assets, name: &str) -> Option<StaticSoundData> {
        if let Some(d) = self.files.get(name) {
            return d.clone();
        }
        let path = sound_path(assets, name);
        let data = path.and_then(|p| match StaticSoundData::from_file(&p) {
            Ok(d) => Some(d),
            Err(e) => {
                log::warn!("sound {name}: {e}");
                None
            }
        });
        if data.is_none() {
            log::warn!("missing sound {name}");
        }
        self.files.insert(name.to_string(), data.clone());
        data
    }

    /// Settings, listener, environment and music, once a frame.
    pub fn update(&mut self, assets: &Assets, l: Option<&Listener>, local_playing: bool) {
        let ids = self.volume_ids.get_or_insert_with(|| {
            let mut ids: Vec<Option<u32>> = crate::content::content()
                .game
                .sound_categories
                .iter()
                .map(|k| k.setting.as_deref().and_then(crate::config::by_name))
                .collect();
            ids.push(crate::config::by_name("core:volume_master"));
            ids
        });
        // UI has no setting of its own: full volume.
        let volumes: Vec<f64> = ids
            .iter()
            .map(|id| id.map_or(1.0, |id| crate::config::get(id) / 100.0))
            .collect();
        if volumes != self.volumes {
            for (g, v) in self.groups.iter_mut().zip(&volumes) {
                g.set_volume(db(*v), Tween::default());
            }
            self.volumes = volumes;
            self.apply_master_volume();
        }
        let acoustics = l.map_or_else(Acoustics::default, |l| l.acoustics);
        if acoustics != self.state {
            if acoustics.feedback != self.state.feedback {
                self.reverb_shape
                    .set_feedback(acoustics.feedback as f64, QUICK);
            }
            if acoustics.damping != self.state.damping {
                self.reverb_shape
                    .set_damping(acoustics.damping as f64, QUICK);
            }
            self.state = acoustics;
            self.lowpass.set_cutoff(acoustics.cutoff as f64, QUICK);
            self.environment
                .set_volume(db(acoustics.volume as f64), QUICK);
            if let Err(e) = self
                .environment
                .set_send(&self.reverb, db(acoustics.reverb as f64), QUICK)
            {
                log::debug!("environment reverb: {e:?}");
            }
        }
        let next_position = l.map(|l| (l.dim, l.pos));
        if self.listener_position.map(|(dim, _)| dim) != next_position.map(|(dim, _)| dim) {
            self.voices.clear();
            self.clear_streams();
        }
        let moved = self.listener_position != next_position;
        self.listener_position = next_position;
        self.voices.retain_mut(|voice| {
            if voice.sound.state() == PlaybackState::Stopped {
                return false;
            }
            if moved && let Some((_, position)) = next_position {
                voice
                    .track
                    .set_position(audio_position(voice.pos, position), Tween::default());
            }
            true
        });
        let Some(l) = l else {
            if let Some(m) = &mut self.music {
                m.stop(Tween { duration: Duration::from_secs(2), ..Default::default() });
                self.music = None;
            }
            self.beds(assets, None);
            return;
        };
        let orientation = Quat::from_rotation_y(l.yaw) * Quat::from_rotation_x(l.pitch);
        // Kira's listener stays at zero. Subtract in f64 before mixing nearby positions in f32.
        self.listener.set_orientation(orientation, Tween::default());
        if self.active && !local_playing {
            self.music(assets, l);
        }
        self.beds(assets, Some(l));
    }

    fn beds(&mut self, assets: &Assets, l: Option<&Listener>) {
        let fade = Tween { duration: Duration::from_secs(3), ..Default::default() };
        let follow = Tween { duration: Duration::from_secs(1), ..Default::default() };
        for (i, m) in content()
            .game
            .music
            .iter()
            .enumerate()
            .filter(|(_, m)| m.ambient > 0.0)
        {
            let weight = l.map_or(0.0, |l| {
                let holds = m.dimension.is_none_or(|d| d == l.dim)
                    && m.when.iter().all(|w| l.holds.contains(w))
                    && (m.rain[0]..=m.rain[1]).contains(&l.params.rain);
                if !holds {
                    return 0.0;
                }
                if m.biomes.is_empty() {
                    return 1.0;
                }
                let near = |&(_, d): &(u16, f32)| match d {
                    d if d <= 0.0 => 1.0,
                    d if m.biome_fade > 0.0 => (1.0 - d / m.biome_fade).max(0.0),
                    _ => 0.0,
                };
                (m.biomes.iter())
                    .filter_map(|&b| l.biome_dist.iter().find(|&&(id, _)| u32::from(id) == b))
                    .map(near)
                    .fold(0.0, f32::max)
            });
            if let Some(l) = l.filter(|_| weight > 0.0) {
                self.spot(assets, i, m, l, weight);
            } else {
                self.spots.remove(&i);
            }
            let gain = m.ambient * weight;
            let playing = self
                .beds
                .get(&i)
                .is_some_and(|(h, _)| h.state() == PlaybackState::Playing);
            if weight <= 0.0 {
                if let Some((h, _)) = self.beds.get_mut(&i).filter(|_| playing) {
                    h.stop(fade);
                }
                continue;
            }
            if playing {
                if let Some((h, last)) = self.beds.get_mut(&i)
                    && (gain - *last).abs() > 0.005
                {
                    h.set_volume(db(f64::from(gain)), follow);
                    *last = gain;
                }
                continue;
            }
            if m.tracks.is_empty() {
                continue;
            }
            let track = &m.tracks[(self.random() * m.tracks.len() as f32) as usize % m.tracks.len()];
            let Some(data) = self.file(assets, track) else { continue };
            let data = data
                .loop_region(..)
                .volume(db(f64::from(gain)))
                .fade_in_tween(fade);
            match self.groups[crate::registry::MUSIC_CATEGORY as usize].play(data) {
                Ok(h) => {
                    self.beds.insert(i, (h, gain));
                }
                Err(e) => log::warn!("ambient {track}: {e}"),
            }
        }
    }

    fn spot(&mut self, assets: &Assets, i: usize, m: &crate::registry::MusicDef, l: &Listener, weight: f32) {
        if m.spots.is_empty() {
            return;
        }
        let now = Instant::now();
        let gap = |audio: &mut Self| {
            let [min, max] = m.spot_every;
            now + Duration::from_secs_f32(min + (max - min) * audio.random())
        };
        let Some(&due) = self.spots.get(&i) else {
            let first = gap(self);
            self.spots.insert(i, first);
            return;
        };
        if now < due {
            return;
        }
        let next = gap(self);
        self.spots.insert(i, next);
        let sound = m.spots[(self.random() * m.spots.len() as f32) as usize % m.spots.len()];
        let [near, far] = m.spot_distance;
        let pos = (far > 0.0).then(|| {
            let (angle, away) = (self.random() * std::f32::consts::TAU, near + (far - near) * self.random());
            l.pos + DVec3::new(f64::from(angle.cos() * away), 0.0, f64::from(angle.sin() * away))
        });
        self.play(assets, Play { sound, pos, volume: m.ambient.max(0.01) * weight, pitch: 0.0 });
    }

    pub(crate) fn start_local(
        &mut self,
        local: &mut LocalMusic,
        folder: &Path,
        source: MusicSource,
        pos: DVec3,
        listener: DVec3,
        source_filter: SourceFilter,
        name: &str,
    ) -> bool {
        if local
            .tracks
            .binary_search_by(|track| track.name.as_str().cmp(name))
            .is_err()
        {
            local.fail(source);
            return false;
        }
        let result = (|| -> Result<LocalPlayback, String> {
            let data = StreamingSoundData::from_file(folder.join(name)).map_err(|err| err.to_string())?;
            let duration = data.duration().as_secs_f64();
            let mut builder = SpatialTrackBuilder::new()
                .distances((4.0, 48.0))
                .volume(db(f64::from(source_filter.gain)))
                .sound_capacity(1);
            let filter = builder.add_effect(FilterBuilder::new().cutoff(f64::from(source_filter.cutoff)));
            let mut track = self.groups[crate::registry::MUSIC_CATEGORY as usize]
                .add_spatial_sub_track(&self.listener, audio_position(pos, listener), builder)
                .map_err(|err| format!("{err:?}"))?;
            let sound = track
                .play(data.volume(db(f64::from(local.volume))))
                .map_err(|err| err.to_string())?;
            Ok(LocalPlayback { source, name: name.to_owned(), duration, paused: false, sound, track, filter, source_filter })
        })();
        match result {
            Ok(playback) => {
                if let Some(mut music) = self.music.take() {
                    music.stop(Tween::default());
                }
                self.next_music = Instant::now() + Duration::from_secs(20);
                self.played += 1;
                local.failed = None;
                local.playing = Some(playback);
                log::debug!("local music: {name}");
                log::trace!(target: "acoustics::paths", "local music at {pos}: source cutoff {:.0}, gain {:.3}", source_filter.cutoff, source_filter.gain);
                true
            }
            Err(err) => {
                log::warn!("local music {name}: {err}");
                local.fail(source);
                false
            }
        }
    }

    fn music(&mut self, assets: &Assets, l: &Listener) {
        if self
            .music
            .as_ref()
            .is_some_and(|m| m.state() != PlaybackState::Stopped)
            || Instant::now() < self.next_music
        {
            return;
        }
        self.music = None;
        let c = content();
        let fits = |m: &crate::registry::MusicDef| m.dimension.is_none_or(|d| d == l.dim) && m.when.iter().all(|w| l.holds.contains(w));
        // Quiet for a few minutes between tracks, like vanilla's (shorter: there is less music).
        self.next_music = Instant::now() + Duration::from_secs(120 + (self.random() * 180.0) as u64);
        let Some(m) = c
            .game
            .music
            .iter()
            .find(|m| m.ambient == 0.0 && fits(m) && !m.tracks.is_empty())
        else {
            return;
        };
        let track = m.tracks[(self.random() * m.tracks.len() as f32) as usize % m.tracks.len()].clone();
        let Some(path) = sound_path(assets, &track) else {
            log::warn!("missing music {track}");
            return;
        };
        match StreamingSoundData::from_file(&path) {
            Ok(data) => match self.groups[crate::registry::MUSIC_CATEGORY as usize]
                .play(data.fade_in_tween(Tween { duration: Duration::from_secs(3), ..Default::default() }))
            {
                Ok(h) => {
                    log::debug!("music: {track}");
                    self.music = Some(h);
                }
                Err(e) => log::warn!("music {track}: {e}"),
            },
            Err(e) => log::warn!("music {track}: {e}"),
        }
    }

    /// Ends the current track now and lets the next start soon (tests, `/music`).
    pub fn skip_music(&mut self) {
        if let Some(m) = &mut self.music {
            m.stop(QUICK);
        }
        self.music = None;
        self.next_music = Instant::now();
    }

    pub fn music_playing(&self) -> bool {
        self.music
            .as_ref()
            .is_some_and(|m| m.state() != kira::sound::PlaybackState::Stopped)
    }

    pub(crate) fn open_stream(&mut self, category: u32, rate: u32, position: Option<DVec3>) -> Option<u64> {
        if self.streams.len() >= MAX_STREAMS || !(8_000..=96_000).contains(&rate) || position.is_some_and(|p| !p.is_finite()) {
            return None;
        }
        let ear = self.listener_position?.1;
        let stream = live::Stream::new(self.groups.get_mut(category as usize)?, &self.listener, ear, rate, position)?;
        let id = self.next_stream;
        self.next_stream += 1;
        self.streams.insert(id, stream);
        Some(id)
    }

    pub(crate) fn push_stream(&mut self, id: u64, samples: &[f32]) -> bool {
        let Some(stream) = self.streams.get(&id) else { return false };
        if self.active {
            stream.push(samples);
        }
        true
    }

    pub(crate) fn position_stream(&mut self, id: u64, pos: DVec3) -> bool {
        let Some(stream) = self
            .streams
            .get_mut(&id)
            .filter(|s| s.pos.is_some() && pos.is_finite())
        else {
            return false;
        };
        stream.pos = Some(pos);
        true
    }

    pub(crate) fn filter_stream(&mut self, id: u64, cutoff: f32, gain: f32) -> bool {
        let Some(stream) = self
            .streams
            .get_mut(&id)
            .filter(|_| cutoff.is_finite() && gain.is_finite())
        else {
            return false;
        };
        stream.custom_filter = SourceFilter { cutoff: cutoff.clamp(20.0, 20_000.0), gain: gain.clamp(0.0, 2.0) };
        true
    }

    pub(crate) fn close_stream(&mut self, id: u64) -> bool {
        self.streams.remove(&id).is_some()
    }

    pub(crate) fn clear_streams(&mut self) {
        self.streams.clear();
    }

    pub(crate) fn update_streams(&mut self, listener: DVec3, mut trace: impl FnMut(DVec3) -> [Option<Arrival>; 3]) {
        for stream in self.streams.values_mut() {
            stream.update(listener, &mut trace);
        }
    }

    pub fn play(&mut self, assets: &Assets, p: Play) {
        self.play_paths(assets, p, 0.0, &[]);
    }

    pub(crate) fn play_paths(&mut self, assets: &Assets, p: Play, age: f64, paths: &[Arrival]) {
        if !self.active {
            return;
        }
        let started = Instant::now();
        let c = content();
        let Some(def) = c.game.sounds.get(p.sound as usize) else { return };
        if def.files.is_empty() {
            return;
        }
        let mut file = def.files[(self.random() * def.files.len() as f32) as usize % def.files.len()].clone();
        // `hurt*`: one of the files whose names start so, found once.
        if let Some(prefix) = file.strip_suffix('*') {
            let found = self.globs.entry(file.clone()).or_insert_with(|| {
                let mut v: Vec<String> = ["ogg", "wav"]
                    .iter()
                    .flat_map(|e| assets.list("sounds", e))
                    .map(|f| f.0)
                    .filter(|n| n.starts_with(prefix))
                    .collect();
                v.sort();
                v.dedup();
                if v.is_empty() {
                    log::warn!("no sound files {file}");
                }
                v
            });
            if found.is_empty() {
                return;
            }
            let n = found.len();
            let i = (self.random() * n as f32) as usize % n;
            file = self.globs[&file][i].clone();
        }
        let Some(data) = self.file(assets, &file) else { return };
        let pitch = if p.pitch > 0.0 { p.pitch } else { def.pitch[0] + (def.pitch[1] - def.pitch[0]) * self.random() };
        let volume = (p.volume * def.volume).max(0.0);
        let data = data.playback_rate(pitch as f64);
        let g = (def.category as usize).min(self.groups.len().saturating_sub(1));
        let voices = &mut self.voices;
        let group = &mut self.groups[g];
        let played = match p.pos {
            Some(pos) => {
                let Some((_, listener)) = self.listener_position else { return };
                // Heard up to `range` blocks (further for loud ones).
                let reach = def.reach(volume);
                let near = f64::from(def.full_within).min(reach - 0.5);
                let curve = match def.falloff {
                    0 => kira::Easing::Linear,
                    n if n > 0 => kira::Easing::InPowi(n),
                    n => kira::Easing::OutPowi(-n),
                };
                let mut voice = |arrival: Arrival, delay: f64| {
                    let mut b = SpatialTrackBuilder::new()
                        .distances((near.max(0.0) as f32, reach.max(1.5) as f32))
                        .attenuation_function(curve)
                        .sub_track_capacity(0)
                        .sound_capacity(1);
                    if arrival.cutoff < 20_000.0 {
                        b = b.with_effect(FilterBuilder::new().cutoff(f64::from(arrival.cutoff)));
                    }
                    group
                        .add_spatial_sub_track(&self.listener, audio_position(arrival.pos, listener), b)
                        .map_err(|e| format!("{e:?}"))
                        .and_then(|mut t| {
                            let delay = Duration::from_secs_f64(delay).saturating_sub(started.elapsed());
                            let data = data
                                .volume(db(f64::from(volume * arrival.gain)))
                                .start_time(delay);
                            let sound = t.play(data).map_err(|e| e.to_string())?;
                            voices.push(SpatialVoice { pos: arrival.pos, track: t, sound });
                            Ok(())
                        })
                };
                if let Some((&direct, echoes)) = paths.split_first() {
                    let played = voice(direct, (travel_time(direct.distance) - age).max(0.0));
                    for &echo in echoes {
                        if let Err(e) = voice(echo, (travel_time(echo.distance) - age).max(0.0)) {
                            log::trace!(target: "acoustics", "reflection of {}: {e}", def.name);
                        }
                    }
                    played
                } else {
                    voice(Arrival { pos, distance: 0.0, gain: 1.0, cutoff: 20_000.0 }, 0.0)
                }
            }
            None => group
                .play(data.volume(db(volume as f64)))
                .map(|_| ())
                .map_err(|e| e.to_string()),
        };
        match played {
            Ok(()) => self.played += 1,
            Err(e) => log::debug!("sound {}: {e}", def.name),
        }
    }
}
