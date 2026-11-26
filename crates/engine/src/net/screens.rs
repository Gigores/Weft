use std::rc::Rc;

use crate::app::{Ctx, Request};
use crate::screens::{BuildCx, Screen, back_button, button, frame, label};
use crate::settings::Settings;
use crate::ui::text;
use crate::ui::{Kind, Style, Ui};

const TICKET_MAX_LEN: usize = 4096;

#[derive(Clone, serde::Deserialize, serde::Serialize)]
struct SavedServer {
    address: String,
    name: String,
}

fn server_key(address: &str) -> String {
    address
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

fn saved_path(settings: &Settings) -> std::path::PathBuf {
    settings.run_dir.join("config").join("recent_servers.json")
}

fn read_servers(settings: &Settings) -> Vec<SavedServer> {
    let path = saved_path(settings);
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            log::warn!("could not read recent servers: {e}");
            Vec::new()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            log::warn!("could not read recent servers: {e}");
            Vec::new()
        }
    }
}

fn write_servers(settings: &Settings, servers: &[SavedServer]) {
    if settings.read_only {
        return;
    }
    let path = saved_path(settings);
    let tmp = path.with_extension("tmp");
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&tmp, serde_json::to_vec(servers)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    })();
    if let Err(e) = result {
        log::warn!("could not save recent servers: {e}");
    }
}

pub(crate) fn remember_server(settings: &Settings, address: String) {
    let mut list = read_servers(settings);
    let key = server_key(&address);
    let name = list
        .iter()
        .find(|e| server_key(&e.address) == key)
        .map(|e| e.name.clone())
        .unwrap_or_default();
    list.retain(|e| server_key(&e.address) != key);
    list.insert(0, SavedServer { address, name });
    write_servers(settings, &list);
}

struct ServerEdit {
    old: Option<String>,
    address: String,
    name: String,
}

impl Screen for ServerEdit {
    fn name(&self) -> &str {
        "core:server_edit"
    }

    fn build(&mut self, ui: &mut Ui, cx: &BuildCx) {
        frame(ui, cx, if self.old.is_some() { "multiplayer.edit" } else { "multiplayer.add" }, |ui| {
            for (id, value, limit, key) in [
                ("server_name", self.name.as_str(), 64, "multiplayer.server_name"),
                ("server_address", self.address.as_str(), TICKET_MAX_LEN, "multiplayer.ticket"),
            ] {
                label(ui, cx, key, &[], "default");
                let input = ui.add(Kind::input(value, limit), Style::default().h(18.0).w(240.0));
                ui.set_id(input, id);
            }
            ui.container(Style::row().gap(6.0), |ui| {
                let old = self.old.clone();
                button(ui, cx, "menu.done", &[], 117.0, move |c| {
                    let Some(address) = c
                        .input_text("server_address")
                        .map(|s| s.trim().to_owned())
                        .filter(|s| !s.is_empty())
                    else {
                        return;
                    };
                    let name = c
                        .input_text("server_name")
                        .unwrap_or_default()
                        .trim()
                        .to_owned();
                    let key = server_key(&address);
                    let mut list = read_servers(c.settings);
                    list.retain(|r| {
                        let k = server_key(&r.address);
                        k != key && Some(&k) != old.as_ref()
                    });
                    list.insert(0, SavedServer { address, name });
                    write_servers(c.settings, &list);
                    c.requests.push(Request::Close);
                });
                back_button(ui, cx, "menu.cancel", &[], 117.0, |c| c.requests.push(Request::Close));
            });
        });
    }
}

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
        ui.root_style(Style::default().stretch());
        ui.panel("background", Style::row().grow().stretch(), |ui| {
            ui.panel("sidebar", Style::default().w(180.0).pad(12.0).gap(4.0).stretch(), |ui| {
                let title = label(ui, cx, "multiplayer.title", &[], "title");
                if let Some(t) = ui.text_mut(title) {
                    t.wrap = false;
                    let chars = cx.lang.get("multiplayer.title").chars().count().max(1) as f32;
                    t.scale = (156.0 / (chars * 6.0)).clamp(1.0, 3.0);
                }
                ui.spacer();
                if cx.in_game && !cx.remote {
                    ui.panel("panel", Style::default().pad(8.0).gap(4.0).stretch(), |ui| {
                        let hosting = super::p2p::HOSTING.lock().is_some();
                        if hosting {
                            label(
                                ui,
                                cx,
                                "multiplayer.your_port",
                                &[crate::ui::text::Arg::Int(crate::settings::server_port() as i64)],
                                "dim",
                            );
                        } else {
                            button(ui, cx, "multiplayer.host", &[], 140.0, |c| c.requests.push(Request::Host));
                        }
                    });
                }
                ui.panel("panel", Style::default().pad(8.0).gap(4.0).stretch(), |ui| {
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
                button(ui, cx, "multiplayer.add", &[], 156.0, |c| {
                    c.requests.push(Request::Open(Box::new(ServerEdit {
                        old: None,
                        address: c.input_text("ticket").unwrap_or_default(),
                        name: String::new(),
                    })));
                });
                back_button(ui, cx, "menu.back", &[], 156.0, |c| c.requests.push(Request::Close));
            });
            ui.container(
                Style::default()
                    .grow()
                    .w(0.0)
                    .pad(12.0)
                    .gap(6.0)
                    .stretch()
                    .scroll(),
                |ui| {
                    label(ui, cx, "multiplayer.recent", &[], "title");
                    let saved = read_servers(cx.settings);
                    if saved.is_empty() {
                        label(ui, cx, "multiplayer.no_servers", &[], "dim");
                    }
                    for entry in saved {
                        ui.panel("panel", Style::row().pad(6.0).gap(4.0).stretch(), |ui| {
                            let caption = if entry.name.is_empty() {
                                entry
                                    .address
                                    .split(';')
                                    .next()
                                    .unwrap_or("")
                                    .chars()
                                    .take(32)
                                    .collect::<String>()
                            } else {
                                entry.name.clone()
                            };
                            ui.container(Style::default().w(0.0).grow(), |ui| {
                                ui.text(text::plain(&caption), "default");
                            });
                            let address = entry.address.clone();
                            button(ui, cx, "multiplayer.join", &[], 65.0, move |c| c.requests.push(Request::Join(address.clone())));
                            let edit = entry.clone();
                            crate::mods_screen::icon_button(ui, cx, "button", "menu_editor", "multiplayer.edit", &[], move |c| {
                                c.requests.push(Request::Open(Box::new(ServerEdit {
                                    old: Some(server_key(&edit.address)),
                                    address: edit.address.clone(),
                                    name: edit.name.clone(),
                                })));
                            });
                            let key = server_key(&entry.address);
                            crate::mods_screen::icon_button(ui, cx, "button", "close", "multiplayer.remove", &[], move |c| {
                                let mut list = read_servers(c.settings);
                                list.retain(|r| server_key(&r.address) != key);
                                write_servers(c.settings, &list);
                                c.requests.push(Request::Rebuild);
                            });
                        });
                    }
                },
            );
        });
    }
}
