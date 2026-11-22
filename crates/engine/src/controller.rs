use crate::input::Key;
use crate::net::codec::{R, W, Wire};

pub const SETTINGS: &str = "core:controller_settings";
pub const SCREEN: &str = "core:controller";
pub const BINDS_PER_FACE: usize = 7;
pub const BIND_COUNT: usize = 6 * BINDS_PER_FACE;
pub const LEVELS: usize = 16;
pub const MAX_SPEED: f32 = 720.0;
pub const MAX_ANGLE: f32 = 36000.0;
pub const MAX_RAILS: i32 = 32;
pub const FACE_LABELS: [&str; 6] =
    ["controller.left", "controller.right", "controller.bottom", "controller.top", "controller.front", "controller.back"];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bind {
    pub key: Key,
    pub level: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Helm {
    pub output: [Option<Bind>; BIND_COUNT],
    pub debug_drive: bool,
    pub exit: Option<Key>,
}

impl Default for Helm {
    fn default() -> Self {
        Self { output: [None; BIND_COUNT], debug_drive: false, exit: None }
    }
}

impl Helm {
    pub fn levels(&self, held: u64) -> [u8; 6] {
        let mut levels = [0; 6];
        for (i, bind) in self.output.iter().enumerate() {
            if held & (1 << i) != 0
                && let Some(bind) = bind
            {
                let face = i / BINDS_PER_FACE;
                levels[face] = levels[face].max(bind.level);
            }
        }
        levels
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Motion {
    #[default]
    Absolute,
    Relative,
    Clockwise,
    Counterclockwise,
    Freewheel,
}

impl Motion {
    pub const ALL: [Self; 5] = [Self::Absolute, Self::Relative, Self::Clockwise, Self::Counterclockwise, Self::Freewheel];
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rule {
    pub min: u8,
    pub max: u8,
    pub motion: Motion,
    pub degrees: f32,
    pub speed: f32,
}

impl Rule {
    pub fn matches(self, signal: u8) -> bool {
        (self.min..=self.max).contains(&signal)
    }
    pub fn valid(self) -> bool {
        self.min <= self.max
            && self.max < LEVELS as u8
            && (-MAX_ANGLE..=MAX_ANGLE).contains(&self.degrees)
            && (0.0..=MAX_SPEED).contains(&self.speed)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rotor {
    pub rules: Vec<Rule>,
    pub hold: bool,
}

impl Rotor {
    pub fn rule(&self, signal: u8) -> Option<Rule> {
        self.rules.iter().copied().find(|rule| rule.matches(signal))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Settings {
    Helm(Helm),
    Rotor(Rotor),
    Guide(Guide),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Guide {
    pub min: f32,
    pub max: f32,
    pub rest: f32,
    pub stiffness: f32,
    pub damping: f32,
}

impl Default for Guide {
    fn default() -> Self {
        Self { min: 1.0, max: MAX_RAILS as f32, rest: MAX_RAILS as f32, stiffness: 60.0, damping: 12.0 }
    }
}

impl Guide {
    pub fn valid(self) -> bool {
        (1.0..=self.max).contains(&self.min)
            && (self.min..=MAX_RAILS as f32).contains(&self.max)
            && (self.min..=self.max).contains(&self.rest)
            && (0.0..=100000.0).contains(&self.stiffness)
            && (0.0..=100000.0).contains(&self.damping)
    }
}

fn put_key(key: Option<Key>, w: &mut W) {
    key.map(Key::id).unwrap_or_default().put(w);
}

fn get_key(r: &mut R) -> Option<Option<Key>> {
    let name = String::get(r)?;
    if name.is_empty() { Some(None) } else { Some(Some(Key::parse(&name)?)) }
}

impl Wire for Rule {
    fn put(&self, w: &mut W) {
        self.min.put(w);
        self.max.put(w);
        (self.motion as u8).put(w);
        self.degrees.put(w);
        self.speed.put(w);
    }

    fn get(r: &mut R) -> Option<Self> {
        let rule = Self {
            min: u8::get(r)?,
            max: u8::get(r)?,
            motion: *Motion::ALL.get(u8::get(r)? as usize)?,
            degrees: f32::get(r)?,
            speed: f32::get(r)?,
        };
        rule.valid().then_some(rule)
    }
}

impl Wire for Settings {
    fn put(&self, w: &mut W) {
        match self {
            Self::Helm(helm) => {
                0u8.put(w);
                (if helm.debug_drive { 2u8 } else { 0u8 }).put(w);
                put_key(helm.exit, w);
                for bind in &helm.output {
                    put_key(bind.map(|b| b.key), w);
                    if let Some(bind) = bind {
                        bind.level.put(w);
                    }
                }
            }
            Self::Rotor(rotor) => {
                1u8.put(w);
                rotor.hold.put(w);
                (rotor.rules.len() as u8).put(w);
                for rule in &rotor.rules {
                    rule.put(w);
                }
            }
            Self::Guide(g) => {
                2u8.put(w);
                (g.min, g.max, g.rest, g.stiffness, g.damping).put(w);
            }
        }
    }

    fn get(r: &mut R) -> Option<Self> {
        match u8::get(r)? {
            0 => {
                let debug_drive = match u8::get(r)? {
                    // Legacy profiles used a boolean, on by default.
                    0 | 1 => false,
                    2 => true,
                    _ => return None,
                };
                let exit = get_key(r)?;
                let mut helm = Helm { debug_drive, exit, ..Helm::default() };
                for bind in &mut helm.output {
                    if let Some(key) = get_key(r)? {
                        let level = u8::get(r)?;
                        if level >= LEVELS as u8 {
                            return None;
                        }
                        *bind = Some(Bind { key, level });
                    }
                }
                Some(Self::Helm(helm))
            }
            1 => {
                let hold = bool::get(r)?;
                let count = u8::get(r)? as usize;
                if count > LEVELS {
                    return None;
                }
                let mut rules: Vec<Rule> = Vec::with_capacity(count);
                for _ in 0..count {
                    let rule = Rule::get(r)?;
                    if rules
                        .iter()
                        .any(|other| other.min <= rule.max && rule.min <= other.max)
                    {
                        return None;
                    }
                    rules.push(rule);
                }
                Some(Self::Rotor(Rotor { rules, hold }))
            }
            2 => {
                let (min, max, rest, stiffness, damping) = <(f32, f32, f32, f32, f32)>::get(r)?;
                let guide = Guide { min, max, rest, stiffness, damping };
                guide.valid().then_some(Self::Guide(guide))
            }
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Slide {
    pub side: u8,
    pub length: f64,
    pub velocity: f64,
    pub parent_velocity: Option<glam::DVec3>,
}

impl Wire for Slide {
    fn put(&self, w: &mut W) {
        (self.side, self.length, self.velocity).put(w);
    }

    fn get(r: &mut R) -> Option<Self> {
        let (side, length, velocity) = <(u8, f64, f64)>::get(r)?;
        (side < 6 && (1.0..=f64::from(MAX_RAILS)).contains(&length) && velocity.is_finite()).then_some(Self {
            side,
            length,
            velocity,
            parent_velocity: None,
        })
    }
}

pub struct Pilot {
    pub mount: u64,
    pub controller: u64,
    pub profile: Helm,
    pub held: u64,
    pub pulses: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drive {
    pub rule: Rule,
    pub target: f64,
}

impl Drive {
    pub fn new(rule: Rule, current: f64) -> Self {
        let degrees = f64::from(rule.degrees).to_radians();
        let target = match rule.motion {
            Motion::Absolute => {
                let turn = (degrees - current).rem_euclid(std::f64::consts::TAU);
                current + if turn > std::f64::consts::PI { turn - std::f64::consts::TAU } else { turn }
            }
            Motion::Relative => current + degrees,
            Motion::Clockwise | Motion::Counterclockwise | Motion::Freewheel => current,
        };
        Self { rule, target }
    }
}

impl Wire for Drive {
    fn put(&self, w: &mut W) {
        self.rule.put(w);
        self.target.put(w);
    }

    fn get(r: &mut R) -> Option<Self> {
        let drive = Self { rule: Rule::get(r)?, target: f64::get(r)? };
        drive.target.is_finite().then_some(drive)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::KeyCode;

    #[test]
    fn helm_reduces_each_face_to_its_strongest_held_binding() {
        let mut helm = Helm::default();
        helm.output[0] = Some(Bind { key: Key::Code(KeyCode::KeyW), level: 5 });
        helm.output[1] = Some(Bind { key: Key::Code(KeyCode::KeyS), level: 15 });
        helm.output[BINDS_PER_FACE] = Some(Bind { key: Key::Code(KeyCode::KeyW), level: 11 });
        assert_eq!(helm.levels(0), [0; 6]);
        assert_eq!(helm.levels(1 | 1 << BINDS_PER_FACE), [5, 11, 0, 0, 0, 0]);
        assert_eq!(helm.levels(3 | 1 << BINDS_PER_FACE), [15, 11, 0, 0, 0, 0]);
        let saved = Settings::Helm(helm.clone());
        assert_eq!(Settings::from_bytes(&saved.to_bytes()), Some(saved));
        helm.debug_drive = true;
        let debug = Settings::Helm(helm.clone());
        let mut bytes = debug.to_bytes();
        assert_eq!(Settings::from_bytes(&bytes), Some(debug));
        bytes[1] = 1;
        helm.debug_drive = false;
        assert_eq!(Settings::from_bytes(&bytes), Some(Settings::Helm(helm)));
    }

    #[test]
    fn rotor_has_a_zero_rule_and_remembers_the_relative_destination() {
        let zero = Rule { min: 0, max: 0, motion: Motion::Absolute, degrees: 0.0, speed: 45.0 };
        let relative = Rule { min: 5, max: 9, motion: Motion::Relative, degrees: -60.0, speed: 90.0 };
        let rotor = Rotor { rules: vec![zero, relative], hold: true };
        assert_eq!(rotor.rule(0), Some(zero));
        assert_eq!(rotor.rule(5), rotor.rule(9));
        assert!(rotor.rule(10).is_none());
        let drive = Drive::new(relative, 30f64.to_radians());
        assert!((drive.target.to_degrees() + 30.0).abs() < 1e-10);
        assert_eq!(Drive::from_bytes(&drive.to_bytes()), Some(drive));
        let absolute = Drive::new(zero, 350f64.to_radians());
        assert!((absolute.target.to_degrees() - 360.0).abs() < 1e-10);
    }
}
