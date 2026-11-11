//! Fingers on a touch screen, turned into what a mouse and keys give. Over a screen a finger is
//! the mouse where it touches, its left button down while it stays. In the world the HUD's touch
//! elements (`BUTTONS`) press their action's key; one on the joystick (`STICK`, or near it)
//! steers from its middle, as Bedrock's (pushed past its edge forward it sprints), and one
//! anywhere else turns the view: a short tap uses, holding still attacks.

use std::time::{Duration, Instant};

use glam::Vec2;
use weft_abi::hud_anchor as a;
use winit::event::TouchPhase;
use winit::keyboard::KeyCode;

use crate::input::{Input, Key};

/// The touch elements of the HUD: element, anchor, order, the action it presses (empty: Escape,
/// what pauses) and whether a tap switches it on and the next off instead of holding it. Each
/// shows `gui/touch_<last part of its name>`.
pub const BUTTONS: &[(&str, &str, i32, &str, bool)] = &[
    ("core:touch.pause", a::TOP_RIGHT, -2, "", false),
    ("core:touch.chat", a::TOP_RIGHT, -1, "core:chat", false),
    ("core:touch.inventory", a::BOTTOM_RIGHT, 2, "core:inventory", false),
    ("core:touch.sneak", a::BOTTOM_RIGHT, 1, "core:sneak", true),
    ("core:touch.jump", a::BOTTOM_RIGHT, 0, "core:jump", false),
];

/// The joystick: its frame, and the knob that follows the finger within it.
pub const STICK: &str = "core:touch.move";
/// GUI pixels: the frame's side, and how far from its middle the knob goes.
pub const STICK_SIZE: f32 = 56.0;
pub const KNOB_REACH: f32 = 16.0;

/// A finger still longer than this attacks; let go sooner it was a tap.
const HOLD: Duration = Duration::from_millis(300);
/// GUI pixels a finger may wander and still be still.
const STILL: f32 = 6.0;
/// GUI pixels from the middle where steering starts, and where it sprints: past the frame.
const DEAD: f32 = 6.0;
const SPRINT: f32 = STICK_SIZE / 2.0;
/// The view turns this much per physical pixel a finger moves, against the mouse's one.
const LOOK: f32 = 0.6;

enum Role {
    /// The mouse, over a screen.
    Mouse,
    Button {
        key: Key,
        toggle: bool,
    },
    /// From the joystick's middle, with the keys it holds.
    Stick {
        middle: Vec2,
        held: Vec<Key>,
    },
    Look {
        moved: bool,
        attacking: Option<Key>,
    },
}

struct Finger {
    id: u64,
    /// GUI pixels.
    start: Vec2,
    last: Vec2,
    since: Instant,
    role: Role,
}

#[derive(Default)]
pub struct Touches {
    fingers: Vec<Finger>,
}

/// The key that sets `action` off, to press it on the player's behalf.
fn key_of(input: &Input, action: &str) -> Option<Key> {
    if action.is_empty() {
        return Some(Key::Code(KeyCode::Escape));
    }
    input.binding_of(action).map(|b| b.chord.key)
}

/// The movement keys a finger `off` the joystick's middle holds, pressing and letting go of
/// what changed since `held`.
fn steer(input: &mut Input, off: Vec2, held: &[Key]) -> Vec<Key> {
    let mut want: Vec<Key> = [
        (off.y < -DEAD, "core:forward"),
        (off.y > DEAD, "core:back"),
        (off.x < -DEAD, "core:left"),
        (off.x > DEAD, "core:right"),
        (-off.y > SPRINT, "core:sprint"),
    ]
    .into_iter()
    .filter(|w| w.0)
    .filter_map(|w| key_of(input, w.1))
    .collect();
    want.dedup();
    for &k in held.iter().filter(|k| !want.contains(k)) {
        input.key_up(k);
    }
    for &k in want.iter().filter(|k| !held.contains(k)) {
        input.key_down(k);
    }
    want
}

impl Touches {
    /// Where the knob is from the joystick's middle, GUI pixels.
    pub fn knob(&self) -> Vec2 {
        self.fingers
            .iter()
            .find_map(|f| match &f.role {
                Role::Stick { middle, .. } => Some((f.last - *middle).clamp_length_max(KNOB_REACH)),
                _ => None,
            })
            .unwrap_or(Vec2::ZERO)
    }

    /// A finger at physical `at`. `world`: the game has the input, no screen over it. `element`
    /// finds the HUD element at a point in GUI pixels, `stick` is the joystick's place.
    #[allow(clippy::too_many_arguments)]
    pub fn finger(
        &mut self,
        input: &mut Input,
        id: u64,
        phase: TouchPhase,
        at: Vec2,
        scale: f32,
        world: bool,
        element: &dyn Fn(Vec2) -> Option<String>,
        stick: Option<crate::ui::Rect>,
    ) {
        let gui = at / scale;
        match phase {
            TouchPhase::Started => {
                let role = if !world {
                    input.cursor = at;
                    input.key_down(Key::Mouse(0));
                    Role::Mouse
                } else if let Some((_, _, _, action, toggle)) = element(gui).and_then(|e| BUTTONS.iter().find(|b| b.0 == e)) {
                    let Some(key) = key_of(input, action) else { return };
                    if *toggle && input.held_key(key) {
                        input.key_up(key);
                    } else {
                        input.key_down(key);
                    }
                    Role::Button { key, toggle: *toggle }
                } else if let Some(r) = stick.filter(|r| {
                    // Half its size around it too: a thumb finds it without looking.
                    let (mx, my) = (r.w / 2.0, r.h / 2.0);
                    gui.x >= r.x - mx && gui.x <= r.x + r.w + mx && gui.y >= r.y - my && gui.y <= r.y + r.h + my
                }) {
                    let middle = Vec2::new(r.x + r.w / 2.0, r.y + r.h / 2.0);
                    Role::Stick { middle, held: steer(input, gui - middle, &[]) }
                } else {
                    Role::Look { moved: false, attacking: None }
                };
                self.fingers
                    .push(Finger { id, start: gui, last: gui, since: Instant::now(), role });
            }
            TouchPhase::Moved => {
                let Some(f) = self.fingers.iter_mut().find(|f| f.id == id) else { return };
                match &mut f.role {
                    Role::Mouse => input.cursor = at,
                    Role::Button { .. } => {}
                    Role::Stick { middle, held } => *held = steer(input, gui - *middle, held),
                    Role::Look { moved, .. } => {
                        input.mouse_delta += (gui - f.last) * scale * LOOK;
                        *moved |= gui.distance(f.start) > STILL;
                    }
                }
                f.last = gui;
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                let Some(i) = self.fingers.iter().position(|f| f.id == id) else { return };
                let f = self.fingers.remove(i);
                match f.role {
                    Role::Mouse => input.key_up(Key::Mouse(0)),
                    Role::Button { key, toggle: false } => input.key_up(key),
                    Role::Button { .. } => {}
                    Role::Stick { held, .. } => held.into_iter().for_each(|k| input.key_up(k)),
                    Role::Look { attacking: Some(k), .. } => input.key_up(k),
                    Role::Look { moved: false, .. } if f.since.elapsed() < HOLD => {
                        // Down and up in one frame: pressed for it, held for none.
                        if let Some(k) = key_of(input, "core:use") {
                            input.key_down(k);
                            input.key_up(k);
                        }
                    }
                    Role::Look { .. } => {}
                }
            }
        }
    }

    /// Each frame: a finger held still on the view long enough starts attacking.
    pub fn tick(&mut self, input: &mut Input) {
        for f in &mut self.fingers {
            if let Role::Look { moved: false, attacking: attacking @ None } = &mut f.role
                && f.since.elapsed() >= HOLD
            {
                *attacking = key_of(input, "core:attack");
                if let Some(k) = *attacking {
                    input.key_down(k);
                }
            }
        }
    }
}
