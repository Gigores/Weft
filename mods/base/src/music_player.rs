use std::sync::Mutex;

use weft_sdk::client::{MusicSource, MusicState};
use weft_sdk::*;

static PLAYER: Block = Block::named("music_player");
static MENU: Menu = Menu::named("music_player");
static OPEN: Channel<MusicSource> = Channel::named("open_music_player");
static SOURCE: Mutex<Option<MusicSource>> = Mutex::new(None);

pub fn register(r: &mut Registry) {
    r.block(&PLAYER)
        .facing(Facings::HORIZONTAL, Toward::Away)
        .texture("music_player_side")
        .states(|p, s| {
            let full = [0.0, 0.0, 16.0, 16.0];
            let mut faces = [("music_player_side", full); 6];
            faces[Dir::North as usize].0 = "music_player_front";
            faces[Dir::South as usize].0 = "music_player_back";
            faces[Dir::Up as usize].0 = "music_player_top";
            let mut record = [("music_player_side", UV_FROM_BOX); 6];
            record[Dir::Up as usize] = ("music_player_record", full);
            s.orientation(Orientation::front(p.facing()))
                .model_box_faces([0.0; 3], [16.0, 14.0, 16.0], faces)
                .model_box_faces([1.0, 14.0, 1.0], [15.0, 15.0, 15.0], record)
        })
        .sound_group("wood")
        .hardness(2.0)
        .mined_by("axe", tool_tier::HAND)
        .tab(&crate::tabs::FUNCTIONAL)
        .on_use(|u| {
            let w = u.world();
            let source = MusicSource { frame: w.frame().unwrap_or(0), pos: u.pos(), state: u.state(), dimension: w.dimension(), pad: 0 };
            w.send(Who::Player(u.player()), &OPEN, &source);
            true
        });
    r.client_packet(&OPEN, |c, source| {
        *SOURCE.lock().unwrap() = Some(source);
        c.refresh_music();
        if c.screen() != MENU.name() {
            c.menu_open(&MENU);
        }
    });
    r.menu(&MENU).screen(screen).closes_on_respawn();
}

fn file_label(file: &str, limit: usize) -> String {
    let mut chars = file.chars();
    let mut name: String = chars
        .by_ref()
        .take(limit)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if chars.next().is_some() {
        name.push('…');
    }
    name
}

fn time(seconds: f64) -> String {
    let seconds = seconds.max(0.0) as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn source() -> MusicSource {
    SOURCE.lock().unwrap().unwrap_or_default()
}

fn screen(ui: &mut Ui) {
    let view = ui.view();
    let playing = view.music_playback(source());
    let mut tracks = (0..).map_while(|i| view.music_track_info(i)).peekable();
    let active = matches!(playing.state, MusicState::Playing | MusicState::Paused);
    ui.column(S::column().center(), |ui| {
        ui.panel("panel", S::column().w(290.0).stretch().gap(8.0).pad(10.0), |ui| {
            ui.row(S::row().gap(4.0), |ui| {
                ui.text("block.base.music_player");
                ui.spacer();
                let action = S::row().size(20.0, 20.0).pad(2.0);
                ui.button("music.base.folder_icon", action.clone().tooltip("music.base.folder.tooltip"), |c| {
                    c.client().open_music_folder();
                });
                ui.button("music.base.refresh_icon", action.tooltip("music.base.refresh"), |c| {
                    c.client().refresh_music();
                });
            });
            ui.panel("well", S::column().h(94.0).pad(2.0).stretch().scroll_thin(), |ui| {
                if tracks.peek().is_none() {
                    ui.row(S::row().pad(6.0), |ui| {
                        ui.text_styled(S::column().class("dim"), "music.base.empty", &[]);
                    });
                }
                for (index, track) in tracks.enumerate() {
                    let name = file_label(&track.file, 37);
                    let duration = if track.duration > 0.0 { time(track.duration) } else { "-".into() };
                    let row = S::row()
                        .h(21.0)
                        .class("base:music_track")
                        .selected(playing.track == track.file);
                    let style = if name == track.file { row } else { row.tooltip_args("music.base.file", &[Arg::Text(&track.file)]) };
                    ui.button_content(
                        style,
                        index as u32,
                        |c, index| {
                            if let Some(file) = c.music_track(index) {
                                c.client().play_music(source(), &file);
                            }
                        },
                        |ui| {
                            ui.row(S::row().grow().gap(4.0), |ui| {
                                ui.text_styled(S::column().grow().no_wrap(), "music.base.file", &[Arg::Text(&name)]);
                                ui.text_styled(S::column().class("dim").no_wrap(), "music.base.file", &[Arg::Text(&duration)]);
                            });
                        },
                    );
                }
            });
            ui.column(S::column().stretch().gap(3.0), |ui| {
                ui.row(S::row().h(11.0).gap(5.0), |ui| {
                    if active {
                        let name = file_label(&playing.track, 33);
                        ui.text_styled(S::column().grow().no_wrap(), "music.base.file", &[Arg::Text(&name)]);
                    } else if playing.state == MusicState::Error {
                        ui.text_styled(S::column().grow().no_wrap(), "music.base.error", &[]);
                    } else {
                        ui.spacer();
                    }
                    ui.text_bind_styled(S::column().class("dim").no_wrap().mono(), |c, out| {
                        let p = c.music_playback(source());
                        if p.duration > 0.0 {
                            out.text(&format!("{} / {}", time(p.position), time(p.duration)));
                        } else {
                            out.text("-");
                        }
                    });
                });
                ui.slider_on_release(
                    S::row().h(12.0).disabled(!active),
                    |c| {
                        let p = c.music_playback(source());
                        if p.duration > 0.0 { (p.position / p.duration) as f32 } else { 0.0 }
                    },
                    |c, value| {
                        c.client().seek_music(source(), value);
                    },
                );
            });
            ui.row(S::row().gap(4.0), |ui| {
                let control = S::row().size(24.0, 20.0).pad(2.0).disabled(!active);
                let (icon, tip) = if playing.state == MusicState::Playing {
                    ("music.base.pause_icon", "music.base.pause")
                } else {
                    ("music.base.play_icon", "music.base.resume")
                };
                ui.button(icon, control.clone().tooltip(tip), |c| {
                    c.client().toggle_music_pause(source());
                });
                ui.button("music.base.stop_icon", control.tooltip("music.base.stop"), |c| {
                    c.client().stop_music(source());
                });
                ui.spacer();
                ui.row(S::row().tooltip("music.base.volume"), |ui| {
                    ui.image("core:gui/icon_sound", 8.0, 8.0);
                });
                ui.slider(
                    S::row().w(79.0),
                    |c| c.music_playback(source()).volume,
                    |c, value| {
                        c.client().set_music_volume(value);
                    },
                );
                ui.text_bind_styled(S::column().w(28.0).mono().no_wrap(), |c, out| {
                    let volume = (c.music_playback(source()).volume * 100.0).round() as i64;
                    out.set("music.base.percent", &[Arg::Int(volume)]);
                });
            });
        });
    });
}
