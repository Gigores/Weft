use std::rc::Rc;

use crate::app::{Ctx, Request};
use crate::screens::{BuildCx, Screen, back_button, button, frame, label};
use crate::ui::{Kind, Style, Ui};

const TICKET_MAX_LEN: usize = 4096;

#[derive(Default)]
pub struct Multiplayer {
    clipboard_status: Option<&'static str>,
}

impl Multiplayer {
    fn show_clipboard_status(c: &mut Ctx, status: Option<&'static str>) {
        if let Some(screen) = c.screen::<Self>()
            && screen.clipboard_status != status
        {
            screen.clipboard_status = status;
            c.requests.push(Request::Rebuild);
        }
    }
}

impl Screen for Multiplayer {
    fn name(&self) -> &str {
        "core:multiplayer"
    }

    fn build(&mut self, ui: &mut Ui, cx: &BuildCx) {
        frame(ui, cx, "multiplayer.title", |ui| {
            if cx.in_game && !cx.remote {
                ui.panel("panel", Style::default().pad(8.0).gap(4.0).stretch().w(340.0), |ui| {
                    let hosting = super::p2p::HOSTING.lock().is_some();
                    if hosting {
                        label(ui, cx, "multiplayer.your_port", &[crate::ui::text::Arg::Int(crate::settings::server_port() as i64)], "dim");
                    } else {
                        button(ui, cx, "multiplayer.host", &[], 200.0, |c| c.requests.push(Request::Host));
                    }
                });
            }
            ui.panel("panel", Style::default().pad(8.0).gap(4.0).stretch().w(340.0), |ui| {
                label(ui, cx, "multiplayer.ticket", &[], "default");
                ui.container(Style::row().gap(4.0).stretch(), |ui| {
                    let input = ui.add(Kind::input("", TICKET_MAX_LEN), Style::default().h(18.0).w(0.0).grow());
                    ui.set_id(input, "ticket");
                    if cfg!(target_os = "android") || crate::settings::touch_controls() {
                        let paste = button(ui, cx, "multiplayer.paste_ticket", &[], 70.0, |c| {
                            let clip = crate::clipboard::get().unwrap_or_default();
                            let text: String = clip
                                .trim()
                                .lines()
                                .next()
                                .unwrap_or_default()
                                .trim()
                                .chars()
                                .filter(|ch| !ch.is_control())
                                .take(TICKET_MAX_LEN)
                                .collect();
                            if text.is_empty() {
                                Self::show_clipboard_status(c, Some("multiplayer.clipboard_empty"));
                            } else {
                                c.set_input("ticket", text);
                                Self::show_clipboard_status(c, None);
                            }
                        });
                        ui.set_id(paste, "paste_ticket");
                    }
                    let copy = crate::mods_screen::icon_button(ui, cx, "button", "icon_copy", "multiplayer.copy_ticket", &[], |c| {
                        let ticket = super::current_ticket().unwrap_or_else(|| super::p2p::player_key(&c.settings.run_dir));
                        let status = if !ticket.is_empty() && crate::clipboard::set(&ticket) {
                            "multiplayer.ticket_copied"
                        } else {
                            "multiplayer.copy_failed"
                        };
                        Self::show_clipboard_status(c, Some(status));
                    });
                    ui.set_id(copy, "copy_ticket");
                });
                if let Some(status) = self.clipboard_status {
                    label(ui, cx, status, &[], "dim");
                }
                ui.button(
                    "button",
                    Style::default().min_w(120.0).min_h(20.0).pad_xy(6.0, 4.0),
                    Rc::new(|c: &mut Ctx| {
                        let ticket = c.input_text("ticket").unwrap_or_default();
                        c.requests.push(Request::Join(ticket));
                    }),
                    |ui| {
                        ui.text(cx.t("multiplayer.join", &[]), "button");
                    },
                );
            });
            back_button(ui, cx, "menu.back", &[], 200.0, |c| c.requests.push(Request::Close));
        });
    }
}
