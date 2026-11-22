//! Screens that arrange the overlay: the HUD editor, where the elements (`hud::Hud`) are dragged
//! around the live picture, and the debug options (F3+F6), which say when each F3 line shows.

use glam::Vec2;
use std::rc::Rc;

use crate::app::{Ctx, Request};
use crate::hud::{self, Edit, Hud};
use crate::screens::{BuildCx, Screen, back_button, button, frame, label};
use crate::ui::text::Arg;
use crate::ui::{Justify, Kind, NodeId, Style, Ui};

/// Grid step in GUI pixels, and how near the middle of the window an element snaps to it.
pub(crate) const GRID: f32 = 8.0;
const SNAP: f32 = 3.0;

enum Pending {
    ToggleHidden,
    Reset,
}

#[derive(Default)]
pub struct HudEditor {
    selected: Option<String>,
    grid: bool,
    confirm_reset: bool,
    /// The element being dragged and where the cursor holds it, from its corner.
    drag: Option<(String, Vec2)>,
    /// The touch control being sized by its corner: where the cursor took it and its side then.
    sizing: Option<(String, Vec2, f32)>,
    /// Where the tool box stands (its top middle) once moved, and the cursor's hold on it.
    tools: Option<Vec2>,
    tools_drag: Option<Vec2>,
    pending: Option<Pending>,
}

impl HudEditor {
    /// Where element `name` goes for its corner at `corner`: tied to the nearest of the window's
    /// nine points, so it keeps its place when the window changes size.
    fn place(&self, hud: &mut Hud, name: &str, corner: Vec2, size: Vec2) {
        let (w, h) = (size.x, size.y);
        let window = hud.ui.size;
        let third = |c: f32, all: f32| {
            if c < all / 3.0 {
                0.0
            } else if c < all * 2.0 / 3.0 {
                0.5
            } else {
                1.0
            }
        };
        let (ax, ay) = (third(corner.x + w / 2.0, window.x), third(corner.y + h / 2.0, window.y));
        let offset = |edge: f32, span: f32, all: f32, anchor: f32| {
            let base = anchor * (all - span);
            if anchor == 0.5 && (edge - base).abs() <= SNAP {
                return 0.0;
            }
            let edge = if self.grid { (edge / GRID).round() * GRID } else { edge };
            edge - base
        };
        let x = offset(corner.x, w, window.x, ax);
        let y = offset(corner.y, h, window.y, ay);
        hud.place(name, [ax, ay, x, y]);
    }

    fn over_tools(ui: &Ui, cursor: Vec2) -> bool {
        ui.find("hud_tools")
            .is_some_and(|t| ui.nodes[t].rect.contains(cursor))
    }

    pub(crate) fn element_tooltip(&self, cursor: Vec2, ui: &Ui, hud: &Hud) -> Option<NodeId> {
        if self.confirm_reset || self.drag.is_some() || self.sizing.is_some() || self.tools_drag.is_some() || Self::over_tools(ui, cursor) {
            return None;
        }
        hud.element_node(&hud.element_at(cursor)?)
    }

    fn confirm_reset(c: &mut Ctx, reset: bool) {
        if let Some(s) = c.screen::<HudEditor>() {
            s.confirm_reset = false;
            if reset {
                s.selected = None;
            }
        }
        if reset {
            c.requests.push(Request::ResetHudLayout);
        }
        c.requests.push(Request::Rebuild);
    }
}

impl Screen for HudEditor {
    fn name(&self) -> &str {
        "core:hud_editor"
    }

    fn pauses(&self) -> bool {
        true
    }

    fn close_action(&self) -> Option<fn(&mut Ctx) -> bool> {
        self.confirm_reset.then_some(|c| {
            Self::confirm_reset(c, false);
            false
        })
    }

    fn pointer(&mut self, cursor: Vec2, ui: &mut Ui, ctx: &mut Ctx, hud: &mut Hud) -> bool {
        hud.set_edit(Some(Edit { selected: self.selected.clone(), grid: self.grid }));
        match (self.pending.take(), self.selected.as_deref()) {
            (Some(Pending::ToggleHidden), Some(name)) => hud.toggle_hidden(name),
            (Some(Pending::Reset), Some(name)) => hud.reset(name),
            _ => {}
        }
        if self.confirm_reset {
            return false;
        }
        let (left, right) = (ctx.input.pressed_button(0), ctx.input.pressed_button(1));
        let held = ctx.input.held_button(0);
        if let Some((name, from, side)) = self.sizing.clone() {
            if held {
                let moved = cursor - from;
                crate::touch::set_size(&name, side + moved.x.max(-moved.y));
            } else {
                self.sizing = None;
                crate::touch::save();
            }
            return true;
        }
        if let Some((name, grab)) = self.drag.clone() {
            if held {
                if let Some(r) = hud.element_rect(&name) {
                    self.place(hud, &name, cursor - grab, Vec2::new(r.w, r.h));
                }
            } else {
                self.drag = None;
                hud.save();
            }
            return true;
        }
        if let Some(grab) = self.tools_drag {
            if held {
                let at = (cursor - grab).clamp(Vec2::new(24.0, 0.0), (ui.size - Vec2::new(24.0, 20.0)).max(Vec2::new(24.0, 0.0)));
                self.tools = Some(at);
                if let Some(tools) = ui.find("hud_tools") {
                    ui.nodes[tools].style.at = Some([at.x, at.y, 0.5, 0.0]);
                    ui.dirty = true;
                }
            } else {
                self.tools_drag = None;
            }
            return true;
        }
        if Self::over_tools(ui, cursor) {
            // Its empty parts move it; its buttons work as usual.
            if left && ui.hit(cursor).is_none() {
                let at = self.tools.unwrap_or(Vec2::new(ui.size.x / 2.0, 8.0));
                self.tools_drag = Some(cursor - at);
                return true;
            }
            return false;
        }
        // A touch control's top right corner sizes it (`hud::HANDLE` shows where).
        let corner = |n: &str| hud.element_rect(n).map(|r| Vec2::new(r.x + r.w, r.y));
        if left
            && crate::settings::touch_controls()
            && let Some((name, side)) = crate::touch::sizes()
                .into_iter()
                .find(|(n, _)| corner(n).is_some_and(|c| c.distance(cursor) <= hud::HANDLE))
        {
            self.selected = Some(name.clone());
            self.sizing = Some((name, cursor, side));
            ctx.requests.push(Request::Rebuild);
            return true;
        }
        if left || right {
            let found = hud.element_at(cursor);
            if found != self.selected {
                self.selected = found.clone();
                ctx.requests.push(Request::Rebuild);
            }
            if let Some(name) = found {
                if right {
                    hud.toggle_hidden(&name);
                } else if let Some(r) = hud.element_rect(&name) {
                    self.drag = Some((name, cursor - Vec2::new(r.x, r.y)));
                }
            }
            return true;
        }
        false
    }

    fn build(&mut self, ui: &mut Ui, cx: &BuildCx) {
        ui.root_style(Style::default().stretch());
        let at = self.tools.unwrap_or(Vec2::new(cx.size.x / 2.0, 8.0));
        let width = (cx.size.x - 16.0).clamp(1.0, 444.0);
        let compact = width < 360.0;
        let col = if compact { width - 12.0 } else { (width - 20.0) / 3.0 }.max(64.0);
        let style = Style::default()
            .pad(6.0)
            .gap(4.0)
            .w(width)
            .stretch()
            .at(at.x, at.y, [0.5, 0.0]);
        let tools = ui.panel("panel", style, |ui| {
            let help = label(ui, cx, "hud.edit.help", &[], "dim");
            ui.nodes[help].style.self_align = Some(crate::ui::Align::Center);
            let toolbar = if compact { Style::default() } else { Style::row() };
            let column = || {
                Style::default()
                    .w(col)
                    .gap(4.0)
                    .stretch()
                    .justify(Justify::Center)
            };
            ui.container(toolbar.gap(4.0).align(crate::ui::Align::Stretch), |ui| {
                ui.container(column(), |ui| {
                    split_control(ui, cx, col, "hud.edit.debug", cx.debug, "core:debug_options", |c| {
                        c.requests.push(Request::SetDebug(!c.debug));
                    });
                });
                ui.container(column(), |ui| {
                    let grid = button(ui, cx, "hud.edit.grid", &[], col, |c| {
                        if let Some(s) = c.screen::<HudEditor>() {
                            s.grid = !s.grid;
                        }
                        c.requests.push(Request::Rebuild);
                    });
                    if let Kind::Button { selected, .. } = &mut ui.nodes[grid].kind {
                        *selected = self.grid;
                    }
                    ui.set_id(grid, "hud_grid");
                    split_control(ui, cx, col, "hud.edit.touch", crate::settings::touch_controls(), "core:touch_buttons", |c| {
                        crate::settings::set("touch_controls", (!crate::settings::touch_controls()) as u8 as f64);
                        c.requests.push(Request::Rebuild);
                    });
                });
                ui.container(column(), |ui| {
                    let reset = button(ui, cx, "hud.edit.reset_all", &[], col, |c| {
                        if let Some(s) = c.screen::<HudEditor>() {
                            s.confirm_reset = true;
                        }
                        c.requests.push(Request::Rebuild);
                    });
                    ui.set_id(reset, "hud_reset_all");
                    back_button(ui, cx, "menu.done", &[], col, |c| c.requests.push(Request::Close));
                });
            });
            if self.selected.is_some() {
                ui.container(Style::row().gap(4.0).justify(Justify::Center), |ui| {
                    let act = |ui: &mut Ui, key: &str, what: fn() -> Pending| {
                        button(ui, cx, key, &[], (width - 16.0) / 2.0, move |c| {
                            if let Some(s) = c.screen::<HudEditor>() {
                                s.pending = Some(what());
                            }
                        });
                    };
                    act(ui, "hud.edit.hide", || Pending::ToggleHidden);
                    act(ui, "hud.edit.reset", || Pending::Reset);
                });
            }
        });
        ui.set_id(tools, "hud_tools");
        if self.confirm_reset {
            let modal = ui.panel(
                "overlay",
                Style::default()
                    .size(cx.size.x, cx.size.y)
                    .center()
                    .at(0.0, 0.0, [0.0, 0.0]),
                |ui| {
                    ui.panel(
                        "panel",
                        Style::default()
                            .w(width.min(300.0))
                            .pad(10.0)
                            .gap(8.0)
                            .center(),
                        |ui| {
                            label(ui, cx, "hud.edit.reset_question", &[], "label");
                            label(ui, cx, "hud.edit.reset_note", &[], "dim");
                            ui.container(Style::row().gap(6.0), |ui| {
                                button(ui, cx, "hud.edit.reset_all", &[], 84.0, |c| Self::confirm_reset(c, true));
                                back_button(ui, cx, "menu.cancel", &[], 84.0, |c| Self::confirm_reset(c, false));
                            });
                        },
                    );
                },
            );
            ui.modal = Some(modal);
        }
    }
}

/// Two hit targets, with the title centered across their combined width.
fn split_control(ui: &mut Ui, cx: &BuildCx, width: f32, key: &str, on: bool, screen: &'static str, toggle: impl Fn(&mut Ctx) + 'static) {
    const LED: f32 = 20.0;
    const GAP: f32 = 2.0;
    ui.container(Style::row().w(width).gap(GAP), |ui| {
        let led = ui.button("button", Style::default().size(LED, LED).center(), Rc::new(toggle), |ui| {
            let texture = if on { "core:gui/led_on" } else { "core:gui/led_off" };
            ui.add(Kind::Image { texture: texture.into(), color: 0xFFFFFFFF }, Style::default().size(8.0, 8.0));
        });
        let action = if on { "hud.edit.disable" } else { "hud.edit.enable" };
        ui.nodes[led].tooltip = Some(cx.t(action, &[Arg::Key(key.into())]));
        ui.set_id(led, &format!("{screen}:toggle"));
        let configure = ui.button(
            "button",
            Style::default().size(width - LED - GAP, LED),
            Rc::new(move |c: &mut Ctx| {
                c.requests.push(Request::Screen(screen.into()));
            }),
            |ui| {
                let t = ui.text(cx.t(key, &[]), "button");
                ui.nodes[t].style = Style::default().at_from([0.5, 0.5], -(LED + GAP) * 0.5, 0.0, [0.5, 0.5]);
                ui.text_mut(t).unwrap().wrap = false;
                if let Some(texture) = cx.look.texture("menu_settings", "") {
                    let style = Style::default()
                        .size(9.0, 9.0)
                        .at_from([1.0, 0.5], -LED * 0.5, 0.0, [0.5, 0.5]);
                    ui.add(Kind::Image { texture, color: 0xFFFFFFFF }, style);
                }
            },
        );
        ui.nodes[configure].tooltip = Some(cx.t("hud.edit.configure", &[Arg::Key(key.into())]));
        ui.set_id(configure, screen);
    });
}

/// When each line of the F3 overlay shows: with F3, always, or never. A line set to always stays
/// on the HUD without F3 (a frame counter, say), where the editor can put it.
pub struct DebugOptions;

impl Screen for DebugOptions {
    fn name(&self) -> &str {
        "core:debug_options"
    }

    fn pauses(&self) -> bool {
        true
    }

    fn build(&mut self, ui: &mut Ui, cx: &BuildCx) {
        let lines = hud::debug_lines();
        let groups = crate::settings::debug_groups();
        frame(ui, cx, "hud.options.title", |ui| {
            let list = ui.panel(
                "panel",
                Style::default()
                    .pad(8.0)
                    .gap(3.0)
                    .stretch()
                    .w(420.0)
                    .h(0.0)
                    .grow()
                    .min_h(60.0)
                    .scroll(),
                |ui| {
                    let row = |ui: &mut Ui, id: String, name: Vec<crate::ui::text::Span>| {
                        let state = lines.get(&id).copied().unwrap_or_default();
                        ui.container(Style::row().gap(6.0).self_align(crate::ui::Align::Stretch), |ui| {
                            let n = ui.text(name, "default");
                            ui.nodes[n].style.grow = 1.0;
                            let shown = id.clone();
                            let b = button(ui, cx, state.key(), &[], 90.0, move |c| {
                                hud::set_debug_line(&id, state.next());
                                c.requests.push(Request::Rebuild);
                            });
                            ui.set_id(b, &format!("line:{shown}"));
                        });
                    };
                    let mods = hud::mod_debug_groups();
                    let group = |ui: &mut Ui, i: usize, title: &str| {
                        let on = groups & (1 << i) != 0;
                        let key = if on { "settings.on" } else { "settings.off" };
                        ui.add(Kind::Container { class: None }, Style::default().h(3.0));
                        ui.container(Style::row().gap(6.0).self_align(crate::ui::Align::Stretch), |ui| {
                            let n = ui.text(cx.t(title, &[]), "default");
                            ui.nodes[n].style.grow = 1.0;
                            let b = button(ui, cx, key, &[], 90.0, move |c| {
                                let groups = crate::settings::debug_groups();
                                crate::settings::set("debug_groups", (groups ^ (1 << i)) as f64);
                                c.requests.push(Request::Rebuild);
                            });
                            ui.set_id(b, &format!("group:{i}"));
                        });
                    };
                    for (i, (title, _, panels)) in hud::DEBUG_GROUPS.iter().enumerate() {
                        group(ui, i, title);
                        for (_, panel) in panels.iter() {
                            for (key, _) in panel.iter() {
                                let id = hud::line_id(key).to_string();
                                let name = cx.t(&format!("hud.options.line.{id}"), &[]);
                                row(ui, id, name);
                            }
                        }
                    }
                    for (k, m) in mods.iter().enumerate() {
                        group(ui, hud::DEBUG_GROUPS.len() + k, m.0);
                        row(ui, m.3.to_string(), cx.t(m.0, &[]));
                    }
                    label(ui, cx, "hud.debug.group.help", &[], "default");
                    row(ui, "help".to_string(), cx.t("hud.options.line.help", &[]));
                },
            );
            ui.set_id(list, "debug_options");
            ui.container(Style::row().gap(6.0), |ui| {
                button(ui, cx, "hud.options.reset", &[], 120.0, |c| {
                    hud::reset_debug_lines();
                    c.requests.push(Request::Rebuild);
                });
                back_button(ui, cx, "menu.done", &[], 120.0, |c| c.requests.push(Request::Close));
            });
        });
    }
}
