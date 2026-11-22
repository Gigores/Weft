use std::rc::Rc;

use crate::app::{Ctx, Request};
use crate::controller::{self, Bind, Motion, Rule, Settings};
use crate::input::{Key, key_name};
use crate::net::{ToServer, codec::Wire};
use crate::screens::{BuildCx, Screen, button, close_button, label};
use crate::ui::{
    Align, Justify, Kind, NodeId, Style, Ui,
    slider::{Slider, Thumb},
    text::{self, Arg},
};
use crate::world::GameMode;

const MOTIONS: [&str; 5] =
    ["controller.absolute", "controller.relative", "controller.forward", "controller.reverse", "controller.freewheel"];
const PANEL_WIDTH: f32 = 334.0;
const PANEL_PADDING: f32 = 10.0;
const PANEL_CONTENT: f32 = PANEL_WIDTH - 2.0 * PANEL_PADDING;

#[derive(Clone, Copy)]
enum Capture {
    Bind(usize),
    Exit,
}

pub struct ControllerScreen {
    draft: Option<(u64, Settings)>,
    face: usize,
    row: usize,
    /// Input text survives normal rebuilds, within the rule it was typed for.
    rule_edit: usize,
    capture: Option<Capture>,
    wait_for_left_release: bool,
    consume_left: bool,
    invalid: bool,
    empty_levels: [u8; controller::BINDS_PER_FACE],
}

impl Default for ControllerScreen {
    fn default() -> Self {
        Self {
            draft: None,
            face: 0,
            row: 0,
            rule_edit: 0,
            capture: None,
            wait_for_left_release: false,
            consume_left: false,
            invalid: false,
            empty_levels: [15; controller::BINDS_PER_FACE],
        }
    }
}

fn mark(ui: &mut Ui, node: usize, active: bool) {
    if let Kind::Button { selected, .. } = &mut ui.nodes[node].kind {
        *selected = active;
    }
}

fn parse<T: std::str::FromStr>(ctx: &Ctx, id: &str) -> Option<T> {
    ctx.input_text(id)?.trim().parse().ok()
}

fn guide_travel(ctx: &Ctx) -> Option<controller::Guide> {
    let guide = controller::Guide {
        min: parse(ctx, "minimum")?,
        max: parse(ctx, "maximum")?,
        rest: parse(ctx, "rest")?,
        ..controller::Guide::default()
    };
    guide.valid().then_some(guide)
}

fn refresh_guide(ctx: &mut Ctx) {
    let Some(travel) = guide_travel(ctx) else { return };
    if let Some((_, Settings::Guide(guide))) = ctx
        .screen::<ControllerScreen>()
        .and_then(|s| s.draft.as_mut())
        && (guide.min, guide.max, guide.rest) != (travel.min, travel.max, travel.rest)
    {
        (guide.min, guide.max, guide.rest) = (travel.min, travel.max, travel.rest);
        ctx.requests.push(Request::Rebuild);
    }
}

fn current_values(ctx: &mut Ctx) -> bool {
    let Some(screen) = ctx.screen::<ControllerScreen>() else { return false };
    let (face, row) = (screen.face, screen.row);
    let rule_edit = screen.rule_edit;
    let rotor = matches!(screen.draft, Some((_, Settings::Rotor(_))));
    let guide = matches!(screen.draft, Some((_, Settings::Guide(_))));
    let has_rule = matches!(screen.draft, Some((_, Settings::Rotor(ref r))) if r.rules.get(row).is_some());
    let mut invalid = false;
    if guide {
        let read =
            || Some(controller::Guide { stiffness: parse(ctx, "stiffness")?, damping: parse(ctx, "damping")?, ..guide_travel(ctx)? });
        let values = read().filter(|g| g.valid());
        if let Some((_, Settings::Guide(guide))) = ctx
            .screen::<ControllerScreen>()
            .and_then(|s| s.draft.as_mut())
        {
            if let Some(values) = values { *guide = values } else { invalid = true }
        }
    } else if !rotor {
        let values: [Option<u8>; controller::BINDS_PER_FACE] =
            std::array::from_fn(|i| parse(ctx, &format!("power{i}")).filter(|&p| p < controller::LEVELS as u8));
        if let Some(screen) = ctx.screen::<ControllerScreen>()
            && let Some((_, Settings::Helm(helm))) = &mut screen.draft
        {
            let start = face * controller::BINDS_PER_FACE;
            for (i, value) in values.into_iter().enumerate() {
                if let Some(value) = value {
                    screen.empty_levels[i] = value;
                    if let Some(bind) = &mut helm.output[start + i] {
                        bind.level = value;
                    }
                } else {
                    invalid = true;
                }
            }
        }
    } else if has_rule {
        let [min, max, degrees, speed] = rule_fields(rule_edit);
        let values = parse(ctx, &min)
            .zip(parse(ctx, &max))
            .zip(parse(ctx, &speed));
        let degrees = parse(ctx, &degrees);
        if let Some((_, Settings::Rotor(rotor))) = ctx
            .screen::<ControllerScreen>()
            .and_then(|s| s.draft.as_mut())
        {
            let valid = values.and_then(|((min, max), speed)| {
                let degrees = match rotor.rules[row].motion {
                    Motion::Absolute | Motion::Relative => degrees?,
                    _ => rotor.rules[row].degrees,
                };
                let rule = Rule { min, max, degrees, speed, ..rotor.rules[row] };
                (rule.valid()
                    && !rotor
                        .rules
                        .iter()
                        .enumerate()
                        .any(|(i, other)| i != row && other.min <= rule.max && rule.min <= other.max))
                .then_some(rule)
            });
            if let Some(rule) = valid { rotor.rules[row] = rule } else { invalid = true }
        }
    }
    if let Some(s) = ctx.screen::<ControllerScreen>() {
        s.invalid = invalid;
    }
    if invalid {
        ctx.requests.push(Request::Rebuild);
    }
    !invalid
}

fn change(ctx: &mut Ctx, f: impl FnOnce(&mut ControllerScreen)) {
    if current_values(ctx)
        && let Some(s) = ctx.screen::<ControllerScreen>()
    {
        let previous = s.row;
        f(s);
        if s.row != previous {
            s.rule_edit += 1;
        }
        ctx.requests.push(Request::Rebuild);
    }
}

fn save(ctx: &mut Ctx) -> bool {
    if !current_values(ctx) {
        return false;
    }
    let draft = ctx
        .screen::<ControllerScreen>()
        .and_then(|s| s.draft.clone());
    if let Some((controller, settings)) = draft
        && let Some(c) = &mut ctx.client
        && !c
            .controller_edit
            .as_ref()
            .is_some_and(|(id, _, original)| *id == controller && *original == settings)
    {
        c.send(ToServer::ControllerSave { controller, data: settings.to_bytes() });
    }
    true
}

impl Screen for ControllerScreen {
    fn name(&self) -> &str {
        controller::SCREEN
    }
    fn keys_first(&self) -> bool {
        self.capture.is_some()
    }
    fn close_action(&self) -> Option<fn(&mut Ctx) -> bool> {
        self.draft.is_some().then_some(save)
    }
    fn closes_on_respawn(&self) -> bool {
        true
    }

    fn key(&mut self, key: Key, ctx: &mut Ctx) -> bool {
        if key == Key::Mouse(0) && self.wait_for_left_release {
            return true;
        }
        let Some(capture) = self.capture.take() else { return false };
        self.consume_left = key == Key::Mouse(0);
        if !matches!(key, Key::Code(winit::keyboard::KeyCode::Escape))
            && Key::parse(&key.id()).is_some()
            && let Some((_, Settings::Helm(helm))) = &mut self.draft
        {
            match capture {
                Capture::Bind(i) => {
                    helm.output[i] = Some(Bind { key, level: self.empty_levels[i % controller::BINDS_PER_FACE] });
                }
                Capture::Exit => helm.exit = Some(key),
            }
        }
        ctx.requests.push(Request::Rebuild);
        true
    }

    fn pointer(&mut self, _: glam::Vec2, ui: &mut Ui, ctx: &mut Ctx, _: &mut crate::hud::Hud) -> bool {
        if !ctx.input.held_button(0) && !ctx.input.pressed_button(0) {
            self.wait_for_left_release = false;
            self.consume_left = false;
        }
        if self.capture.is_some() || self.consume_left {
            return true;
        }
        if let Some((_, Settings::Helm(helm))) = &self.draft {
            let start = self.face * controller::BINDS_PER_FACE;
            for i in 0..controller::BINDS_PER_FACE {
                if let Some(node) = ui.find(&format!("key{i}")) {
                    mark(ui, node, helm.output[start + i].is_some_and(|bind| ctx.input.active_key(bind.key)));
                }
            }
        }
        false
    }

    fn build(&mut self, ui: &mut Ui, cx: &BuildCx) {
        if self.draft.is_none() {
            if let Some((id, face, settings)) = cx.client.and_then(|c| c.controller_edit.clone()) {
                self.face = usize::from(face).min(5);
                self.draft = Some((id, settings));
            }
        }
        let Some((_, settings)) = &self.draft else { return };
        let title = match settings {
            Settings::Helm(_) => "controller.block",
            Settings::Rotor(_) => "controller.rotor",
            Settings::Guide(_) => "controller.guide",
        };
        ui.root_style(Style::default().stretch());
        ui.panel("overlay", Style::default().grow().center().pad(8.0), |ui| {
            ui.panel(
                "panel",
                Style::default()
                    .w(PANEL_WIDTH)
                    .stretch()
                    .gap(9.0)
                    .pad(PANEL_PADDING),
                |ui| {
                    ui.container(Style::row().justify(Justify::Between).gap(6.0), |ui| {
                        let heading = if matches!(settings, Settings::Helm(_)) { "controller.block_face" } else { title };
                        let args = [Arg::Key(controller::FACE_LABELS[self.face].into())];
                        let n = label(ui, cx, heading, &args, "title");
                        if let Some(t) = ui.text_mut(n) {
                            t.scale = 1.45;
                            t.wrap = false;
                        }
                        close_button(ui, 14.0);
                        if let Some(n) = ui.find("screen_close") {
                            ui.nodes[n].tooltip = Some(cx.t("controller.close", &[]));
                        }
                    });
                    match settings {
                        Settings::Helm(helm) => self.helm(ui, cx, helm),
                        Settings::Rotor(rotor) => self.rotor(ui, cx, rotor),
                        Settings::Guide(guide) => self.guide(ui, cx, *guide),
                    }
                    if self.invalid {
                        let key = if matches!(settings, Settings::Guide(_)) { "controller.guide_invalid" } else { "controller.invalid" };
                        label(ui, cx, key, &[Arg::Int(i64::from(controller::MAX_RAILS))], "default");
                    }
                },
            );
        });
    }
}

impl ControllerScreen {
    fn begin_capture(&mut self, capture: Capture) {
        self.capture = Some(capture);
        self.wait_for_left_release = true;
    }

    fn numeric(&self, ui: &mut Ui, id: &str, value: impl std::fmt::Display, width: f32) -> NodeId {
        let node = ui.add(Kind::input(&value.to_string(), 12), Style::default().size(width, 20.0).pad(4.0));
        ui.set_id(node, id);
        node
    }

    fn guide(&self, ui: &mut Ui, cx: &BuildCx, guide: controller::Guide) {
        guide_diagram(ui, cx, guide);
        let gap = 8.0;
        ui.container(Style::row().gap(gap), |ui| {
            for (id, name, value) in [
                ("minimum", "controller.minimum", guide.min),
                ("maximum", "controller.maximum", guide.max),
                ("rest", "controller.rest", guide.rest),
            ] {
                ui.container(Style::default().gap(4.0), |ui| {
                    label(ui, cx, name, &[], "default");
                    let n = self.numeric(ui, id, value, (PANEL_CONTENT - 2.0 * gap) / 3.0);
                    if let Kind::Input { on_change, .. } = &mut ui.nodes[n].kind {
                        *on_change = Some(Rc::new(|ctx, _| refresh_guide(ctx)));
                    }
                });
            }
        });
        divider(ui, cx);
        ui.container(Style::row().gap(gap), |ui| {
            for (id, name, icon, tip, value) in [
                ("stiffness", "controller.stiffness", "core:gui/icon_spring", "controller.stiffness_help", guide.stiffness),
                ("damping", "controller.damping", "core:gui/icon_damper", "controller.damping_help", guide.damping),
            ] {
                ui.container(Style::default().gap(4.0), |ui| {
                    ui.container(Style::row().gap(4.0), |ui| {
                        let n = ui.add(
                            Kind::Image { texture: icon.into(), color: cx.look.text("panel_dim", "") },
                            Style::default().size(9.0, 9.0),
                        );
                        ui.nodes[n].tooltip = Some(cx.t(tip, &[]));
                        let n = label(ui, cx, name, &[], "default");
                        ui.nodes[n].tooltip = Some(cx.t(tip, &[]));
                    });
                    let n = self.numeric(ui, id, value, (PANEL_CONTENT - gap) / 2.0);
                    ui.nodes[n].tooltip = Some(cx.t(tip, &[]));
                });
            }
        });
    }

    fn helm(&self, ui: &mut Ui, cx: &BuildCx, helm: &controller::Helm) {
        ui.container(Style::row().gap(6.0).pad_xy(4.0, 0.0), |ui| {
            ui.container(Style::default().w(74.0), |ui| {
                label(ui, cx, "controller.key", &[], "dim");
            });
            label(ui, cx, "controller.power", &[], "dim");
        });
        let start = self.face * controller::BINDS_PER_FACE;
        ui.panel("well", Style::default().stretch().gap(4.0).pad(5.0), |ui| {
            for i in 0..controller::BINDS_PER_FACE {
                let index = start + i;
                let bind = helm.output[index];
                let level = bind.map_or(self.empty_levels[i], |b| b.level);
                let field = format!("power{i}");
                let track = format!("power_track{i}");
                ui.container(Style::row().gap(6.0), |ui| {
                    let name = if matches!(self.capture, Some(Capture::Bind(j)) if j == index) {
                        "controller.press"
                    } else if bind.is_some() {
                        "controller.name"
                    } else {
                        "controller.bind"
                    };
                    let key = bind.map(|b| key_name(b.key)).unwrap_or_default();
                    let n =
                        button(ui, cx, name, &[Arg::Text(key)], 74.0, move |ctx| change(ctx, |s| s.begin_capture(Capture::Bind(index))));
                    ui.set_id(n, &format!("key{i}"));
                    level_track(ui, &track, &[(&field, level)], Style::default().grow().h(12.0));
                    let n = self.numeric(ui, &field, level, 28.0);
                    connect_levels(ui, n, &track, &[&field]);
                    let n = glyph(ui, cx, "×", "controller.remove_bind", 16.0, "", move |ctx| {
                        change(ctx, |s| {
                            if let Some((_, Settings::Helm(helm))) = &mut s.draft {
                                helm.output[index] = None;
                            }
                        })
                    });
                    enable(ui, n, bind.is_some());
                });
            }
        });
        divider(ui, cx);
        ui.container(Style::row().gap(6.0).justify(Justify::End), |ui| {
            ui.container(Style::row().gap(5.0), |ui| {
                label(ui, cx, "controller.exit_label", &[], "default");
                let name = if matches!(self.capture, Some(Capture::Exit)) {
                    "controller.press"
                } else if helm.exit.is_some() {
                    "controller.name"
                } else {
                    "controller.default"
                };
                let args = [Arg::Text(helm.exit.map(key_name).unwrap_or_default())];
                button(ui, cx, name, &args, 46.0, |ctx| change(ctx, |s| s.begin_capture(Capture::Exit)));
                if helm.exit.is_some() {
                    glyph(ui, cx, "×", "controller.reset_exit", 16.0, "", |ctx| {
                        change(ctx, |s| {
                            if let Some((_, Settings::Helm(helm))) = &mut s.draft {
                                helm.exit = None;
                            }
                        })
                    });
                }
            });
        });
        if cx.client.is_some_and(|c| c.mode == GameMode::CREATIVE) {
            ui.container(Style::row(), |ui| {
                led(ui, cx, "controller.debug_drive", helm.debug_drive, |ctx| {
                    if ctx
                        .client
                        .as_ref()
                        .is_some_and(|c| c.mode == GameMode::CREATIVE)
                    {
                        change(ctx, |s| {
                            if let Some((_, Settings::Helm(helm))) = &mut s.draft {
                                helm.debug_drive = !helm.debug_drive;
                            }
                        });
                    }
                });
            });
        }
    }

    fn rotor(&self, ui: &mut Ui, cx: &BuildCx, rotor: &controller::Rotor) {
        let rule = rotor.rules.get(self.row);
        let [min, max, degrees, speed] = rule_fields(self.rule_edit);
        let endpoints = [min.as_str(), max.as_str()];
        ui.container(Style::row().align(Align::Stretch).gap(10.0), |ui| {
            ui.panel("well", Style::default().w(77.0).stretch().pad(4.0).gap(4.0), |ui| {
                ui.container(Style { bar_thin: true, ..Style::default().h(128.0).scroll().gap(2.0) }, |ui| {
                    for (i, rule) in rotor.rules.iter().enumerate() {
                        let node =
                            ui.button("button", Style::default().size(62.0, 20.0), Rc::new(move |ctx| change(ctx, |s| s.row = i)), |ui| {
                                let n = ui.text(range_text(rule.min, rule.max), "button");
                                if i == self.row {
                                    ui.set_id(n, "active_range");
                                }
                                let remove = glyph(ui, cx, "×", "controller.remove_range", 13.0, "", move |ctx| remove_rule(ctx, i));
                                ui.nodes[remove].style = ui.nodes[remove]
                                    .style
                                    .at_from([1.0, 0.5], -1.0, 0.0, [1.0, 0.5]);
                            });
                        mark(ui, node, i == self.row);
                    }
                });
                let add = glyph(ui, cx, "+", "controller.add", 19.0, "button", |ctx| {
                    change(ctx, |s| {
                        if let Some((_, Settings::Rotor(rotor))) = &mut s.draft
                            && let Some(level) = (0..controller::LEVELS as u8).find(|&level| rotor.rule(level).is_none())
                        {
                            s.row = rotor.rules.len();
                            rotor
                                .rules
                                .push(Rule { min: level, max: level, motion: Motion::Absolute, degrees: 0.0, speed: 45.0 });
                        }
                    })
                });
                ui.nodes[add].style.w = crate::ui::Len::Auto;
                enable(ui, add, (0..controller::LEVELS as u8).any(|level| rotor.rule(level).is_none()));
            });
            ui.container(Style::default().grow().stretch().gap(6.0), |ui| {
                let Some(rule) = rule else {
                    label(ui, cx, "controller.legacy", &[], "dim");
                    return;
                };
                label(ui, cx, "controller.range_label", &[], "default");
                ui.container(Style::row().justify(Justify::Between), |ui| {
                    for (id, name, value) in [(endpoints[0], "controller.from", rule.min), (endpoints[1], "controller.to", rule.max)] {
                        ui.container(Style::row().gap(5.0), |ui| {
                            label(ui, cx, name, &[], "default");
                            let n = self.numeric(ui, id, value, 31.0);
                            connect_levels(ui, n, "signal_range", &endpoints);
                        });
                    }
                });
                level_track(ui, "signal_range", &[(endpoints[0], rule.min), (endpoints[1], rule.max)], Style::default().h(12.0));
                ui.container(Style::row().justify(Justify::Between), |ui| {
                    label(ui, cx, "controller.name", &[Arg::Int(0)], "dim");
                    label(ui, cx, "controller.name", &[Arg::Int(i64::from(controller::LEVELS as u8 - 1))], "dim");
                });
                button(ui, cx, MOTIONS[rule.motion as usize], &[], 130.0, |ctx| {
                    change(ctx, |s| {
                        if let Some((_, Settings::Rotor(rotor))) = &mut s.draft {
                            let rule = &mut rotor.rules[s.row];
                            rule.motion = Motion::ALL[(rule.motion as usize + 1) % Motion::ALL.len()];
                        }
                    })
                });
                ui.container(Style::row().gap(7.0), |ui| {
                    if matches!(rule.motion, Motion::Absolute | Motion::Relative) {
                        self.rotor_number(ui, cx, &degrees, "controller.angle", "controller.degree_unit", rule.degrees);
                    }
                    self.rotor_number(ui, cx, &speed, "controller.speed_label", "controller.speed_unit", rule.speed);
                });
            });
        });
        divider(ui, cx);
        ui.container(Style::row(), |ui| {
            led(ui, cx, "controller.stop_hold", rotor.hold, |ctx| {
                change(ctx, |s| {
                    if let Some((_, Settings::Rotor(rotor))) = &mut s.draft {
                        rotor.hold = !rotor.hold;
                    }
                })
            });
        });
    }

    fn rotor_number(&self, ui: &mut Ui, cx: &BuildCx, id: &str, label_key: &str, unit: &str, value: f32) {
        ui.container(Style::default().grow().stretch().gap(3.0), |ui| {
            label(ui, cx, label_key, &[], "default");
            let n = self.numeric(ui, id, value, 80.0);
            ui.nodes[n].style.w = crate::ui::Len::Auto;
            ui.nodes[n].style.max_w = 90.0;
            let n = label(ui, cx, unit, &[], "dim");
            ui.nodes[n].style = Style::default().at_from([1.0, 1.0], -6.0, -10.0, [1.0, 0.5]);
        });
    }
}

fn guide_diagram(ui: &mut Ui, cx: &BuildCx, guide: controller::Guide) {
    const INSET: f32 = 22.0;
    const TRACK: f32 = PANEL_CONTENT - 2.0 * INSET;
    let travel = guide.max - guide.min;
    let (left, right, rest) = if travel > 0.0 {
        (0.0, TRACK, TRACK * (guide.rest - guide.min) / travel)
    } else {
        (TRACK / 2.0 - 5.0, TRACK / 2.0 + 5.0, TRACK / 2.0)
    };
    let point = |x: f32, y: f32| Style::default().at_from([0.0, 0.5], INSET + x, y, [0.5, 0.5]);
    let steel = cx.look.text("panel_dim", "");
    let spring = cx.look.text("slider_fill", "");
    ui.panel("well", Style::default().h(71.0), |ui| {
        let bar = |ui: &mut Ui, x, y, w, h, color| {
            ui.container(Style { fill: Some(color), ..point(x, y).size(w, h) }, |_| {});
        };
        bar(ui, (left + right) / 2.0, 0.0, right - left, 4.0, cx.look.text("controller_divider", ""));
        let spring_start = left + 2.0;
        let spring_length = (rest - 5.5 - spring_start).max(0.0);
        let turns = (spring_length / 11.0).floor() as usize;
        for i in 0..turns {
            let t = (i as f32 + 0.5) / turns as f32;
            ui.add(
                Kind::Image { texture: "core:gui/guide_coil".into(), color: spring },
                point(spring_start + spring_length * t, 0.0).size(7.0, 16.0),
            );
        }
        bar(ui, left, 0.0, 4.0, 26.0, steel);
        bar(ui, right, 0.0, 4.0, 26.0, steel);
        bar(ui, rest, 0.0, 11.0, 26.0, spring);
        bar(ui, rest, 0.0, 7.0, 22.0, steel);
        let left_label = if travel > 0.0 { left } else { left - 15.0 };
        let right_label = if travel > 0.0 { right } else { right + 15.0 };
        for (key, x, y) in
            [("controller.minimum", left_label, 23.0), ("controller.maximum", right_label, 23.0), ("controller.rest", rest, -23.0)]
        {
            let n = label(ui, cx, key, &[], "dim");
            ui.nodes[n].style = point(x, y);
            if let Some(t) = ui.text_mut(n) {
                t.wrap = false;
            }
        }
    });
}

fn remove_rule(ctx: &mut Ctx, row: usize) {
    let active = ctx
        .screen::<ControllerScreen>()
        .is_some_and(|s| s.row == row);
    if !active && !current_values(ctx) {
        return;
    }
    if let Some(s) = ctx.screen::<ControllerScreen>()
        && let Some((_, Settings::Rotor(rotor))) = &mut s.draft
    {
        rotor.rules.remove(row);
        s.row = (s.row - usize::from(row < s.row)).min(rotor.rules.len().saturating_sub(1));
        s.rule_edit += 1;
        s.invalid = false;
        ctx.requests.push(Request::Rebuild);
    }
}

const MAX_SIGNAL: f32 = (controller::LEVELS - 1) as f32;

fn level_track(ui: &mut Ui, id: &str, levels: &[(&str, u8)], style: Style) {
    let range = levels.len() == 2;
    let thumbs = levels
        .iter()
        .map(|&(field, value)| {
            let field = field.to_owned();
            Thumb::new(f32::from(value) / MAX_SIGNAL, move |ctx, value| {
                ctx.set_input(&field, ((value * MAX_SIGNAL).round() as u8).to_string());
                if range {
                    refresh_range(ctx);
                }
            })
        })
        .collect();
    let mut slider = Slider::new(thumbs);
    slider.steps = MAX_SIGNAL as u32;
    slider.snap = true;
    slider.ordered = range;
    slider.fill = slider.ordered.then_some([0, 1]);
    let n = ui.add(Kind::Slider(slider), style);
    ui.set_id(n, id);
}

fn signal(ctx: &Ctx, field: &str) -> Option<u8> {
    parse::<u8>(ctx, field).filter(|&value| f32::from(value) <= MAX_SIGNAL)
}

fn range_text(min: u8, max: u8) -> Vec<text::Span> {
    text::plain(&format!("{min}–{max}"))
}

fn rule_fields(edit: usize) -> [String; 4] {
    ["min", "max", "degrees", "speed"].map(|field| format!("rule{edit}:{field}"))
}

fn refresh_range(ctx: &mut Ctx) {
    let Some(edit) = ctx.screen::<ControllerScreen>().map(|s| s.rule_edit) else { return };
    let [min, max, _, _] = rule_fields(edit);
    let Some((a, b)) = signal(ctx, &min)
        .zip(signal(ctx, &max))
        .filter(|(a, b)| a <= b)
    else {
        return;
    };
    if let Some(s) = ctx.screen::<ControllerScreen>()
        && let Some((_, Settings::Rotor(rotor))) = &mut s.draft
        && let Some(rule) = rotor.rules.get_mut(s.row)
    {
        (rule.min, rule.max) = (a, b);
    }
    if let Some(top) = &mut ctx.top
        && let Some(n) = top.ui.find("active_range")
        && let Some(t) = top.ui.text_mut(n)
    {
        t.spans = range_text(a, b);
        top.ui.dirty = true;
    }
}

fn connect_levels(ui: &mut Ui, field: NodeId, track: &str, fields: &[&str]) {
    let track = track.to_owned();
    let fields: Vec<String> = fields.iter().map(|f| (*f).to_owned()).collect();
    if let Kind::Input { on_change, .. } = &mut ui.nodes[field].kind {
        *on_change = Some(Rc::new(move |ctx, _| {
            let Some(values) = fields
                .iter()
                .map(|f| signal(ctx, f))
                .collect::<Option<Vec<_>>>()
            else {
                return;
            };
            if values.len() == 2 && values[0] > values[1] {
                return;
            }
            if values.len() == 2 {
                refresh_range(ctx);
            }
            if let Some(top) = &mut ctx.top
                && let Some(n) = top.ui.find(&track)
                && let Kind::Slider(slider) = &mut top.ui.nodes[n].kind
            {
                for (thumb, value) in slider.thumbs.iter_mut().zip(values) {
                    thumb.value = f32::from(value) / MAX_SIGNAL;
                }
            }
        }));
    }
}

fn enable(ui: &mut Ui, id: NodeId, enable: bool) {
    if let Kind::Button { enabled, .. } = &mut ui.nodes[id].kind {
        *enabled = enable;
    }
}

fn glyph(ui: &mut Ui, cx: &BuildCx, icon: &str, tooltip: &str, side: f32, class: &str, action: impl Fn(&mut Ctx) + 'static) -> NodeId {
    let n = ui.button(class, Style::default().size(side, side).center(), Rc::new(action), |ui| {
        ui.text(text::plain(icon), "button");
    });
    ui.nodes[n].tooltip = Some(cx.t(tooltip, &[]));
    n
}

fn led(ui: &mut Ui, cx: &BuildCx, key: &str, on: bool, action: impl Fn(&mut Ctx) + 'static) {
    ui.button("", Style::row().gap(5.0).pad_xy(0.0, 3.0), Rc::new(action), |ui| {
        let icon = if on { "core:gui/led_on" } else { "core:gui/led_off" };
        ui.add(Kind::Image { texture: icon.into(), color: 0xFFFFFFFF }, Style::default().size(8.0, 8.0));
        label(ui, cx, key, &[], "button");
    });
}

fn divider(ui: &mut Ui, cx: &BuildCx) {
    ui.container(Style { fill: Some(cx.look.text("controller_divider", "")), ..Style::default().h(0.5) }, |_| {});
}
