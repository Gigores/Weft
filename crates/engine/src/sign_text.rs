use glam::{DVec3, Mat4, Vec3};
use weft_render::Backend;

use crate::app::Request;
use crate::assets::Assets;
use crate::client::textures::GameTextures;
use crate::client::{Client, ClientMenu};
use crate::content::content;
use crate::gpu_layout::{GpuBox, box_mode, rows, ui_mode};
use crate::screens::{BuildCx, back_button, button, label};
use crate::ui::{Kind, Style, Ui, lang::Lang, render::UiRenderer, text};

const LINES: usize = 4;
const MAX_LINE: u32 = 100;

pub fn sanitize(lines: Vec<String>) -> String {
    lines
        .into_iter()
        .take(LINES)
        .map(|line| {
            line.chars()
                .filter(|ch| !ch.is_control())
                .take(MAX_LINE as usize)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn editor(ui: &mut Ui, cx: &BuildCx, menu: &ClientMenu) {
    ui.panel("overlay", Style::default().grow().center(), |ui| {
        ui.panel("panel", Style::default().pad(8.0).gap(4.0).w(230.0), |ui| {
            label(ui, cx, "sign.edit", &[], "title");
            let max_chars = (MAX_LINE as usize + 1) * LINES - 1;
            let field = ui.add(Kind::text_area(menu.name.trim_end(), max_chars, None), Style::default().h(64.0).stretch());
            ui.set_id(field, "sign_text");
            ui.container(Style::row().gap(2.0), |ui| {
                for (caption, code) in [
                    ("sign.red", "{#AA3333}"),
                    ("sign.green", "{#337737}"),
                    ("sign.blue", "{#3344AA}"),
                    ("sign.reset", "{/}"),
                    ("sign.icon", "{item:base:diamond}"),
                ] {
                    button(ui, cx, caption, &[], 40.0, move |c| {
                        let Some(top) = &mut c.top else { return };
                        let Some(field) = top.ui.find("sign_text") else { return };
                        let Kind::Input { value, caret, max, .. } = &mut top.ui.nodes[field].kind else { return };
                        if value.chars().count() + code.len() <= *max {
                            let at = value
                                .char_indices()
                                .nth(*caret)
                                .map_or(value.len(), |(at, _)| at);
                            value.insert_str(at, code);
                            *caret += code.len();
                            top.ui.focused = Some(field);
                            top.ui.dirty = true;
                        }
                    });
                }
            });
            let window = menu.window;
            ui.container(Style::row().gap(4.0).center(), |ui| {
                button(ui, cx, "menu.done", &[], 100.0, move |c| {
                    let lines = c
                        .input_text("sign_text")
                        .unwrap_or_default()
                        .lines()
                        .take(LINES)
                        .map(str::to_owned)
                        .collect();
                    if let Some(client) = c.client.as_deref_mut() {
                        client.send(crate::net::ToServer::WriteSign { window, lines });
                    }
                    c.requests.push(Request::Close);
                });
                back_button(ui, cx, "menu.cancel", &[], 100.0, |c| c.requests.push(Request::Close));
            });
        });
    });
}

pub fn boxes(client: &Client, ui: &mut UiRenderer, gpu: &mut dyn Backend, assets: &Assets, lang: &Lang, tex: &GameTextures) -> Vec<GpuBox> {
    let c = content();
    let Some(comp) = c.components.id(weft_abi::comp::SIGN_TEXT) else { return Vec::new() };
    let Some(surface) = c.components.id(weft_abi::comp::SIGN_SURFACE) else { return Vec::new() };
    let cam = client.render_camera();
    let mut boxes = Vec::new();
    for (&id, block) in &client.showing_blocks {
        let Some(state) = block.view.animation_state else { continue };
        let Some(value) = client.entity_components.get(&(id, comp)) else { continue };
        let Ok(value) = std::str::from_utf8(value) else { continue };
        if value.trim().is_empty() {
            continue;
        }
        let Some(info) = c.states.get(state as usize) else { continue };
        let Some(plane) = c
            .components
            .get::<[f32; 6]>(weft_abi::entry_kind::BLOCK as usize, state, surface)
        else {
            continue;
        };
        let [x, y, z, width, height, face] = plane;
        if !plane.iter().all(|v| v.is_finite()) || width <= 0.0 || height <= 0.0 {
            continue;
        }
        let Some(normal) = crate::world::DIRS.get(face as usize).map(|p| p.as_dvec3()) else { continue };
        let up = if normal.y == 0.0 { DVec3::Y } else { -DVec3::Z };
        let turned = |v: DVec3| Vec3::from_array(info.turn.vector_f(v.as_vec3().to_array())).as_dvec3();
        let place = client.frame_place(block.view.frame);
        let center = place.to_world(block.view.local + turned(DVec3::new(x as f64, y as f64, z as f64) / 16.0 - 0.5 + normal * 0.002));
        let front = place.vector_to_world(turned(normal));
        let delta = cam.pos - center;
        if delta.length_squared() > 18.0 * 18.0 || delta.dot(front) <= 0.0 || delta.dot(cam.forward().as_dvec3()) > 0.75 {
            continue;
        }
        let right = place.vector_to_world(turned(up.cross(normal))).as_vec3();
        let down = place.vector_to_world(turned(-up)).as_vec3();
        let layout = text::layout(&text::rich(value, lang, None), &ui.font, None);
        let pixel = (width / layout.width.max(1.0)).min(height / layout.height.max(1.0)) / 16.0;
        let origin = (center - cam.pos).as_vec3() - (right * layout.width + down * layout.height) * (pixel * 0.5);
        ui.load_images(gpu, assets, &layout);
        let start = ui.quads.len();
        let mut icons = Vec::new();
        ui.glyph_run(&layout.items, 0.0, 0.0, 1.0, 0x241709FF, None, &mut icons);
        for (x, y, size, item) in icons {
            ui.text_icon(c, tex, x, y, size, item);
        }
        let light = client.world.light(center.floor().as_ivec3());
        for q in ui.quads.drain(start..) {
            if !matches!(q.mode, ui_mode::FONT | ui_mode::ICON | ui_mode::TEXTURE) {
                continue;
            }
            let v = (right * q.v[0] + down * q.v[1]) * pixel;
            let u = (right * q.u[0] + down * q.u[1]) * pixel;
            let pos = origin + (right * q.p0[0] + down * q.p0[1]) * pixel + v;
            let m = Mat4::from_cols(u.extend(0.0), (-v).extend(0.0), Vec3::ZERO.extend(0.0), pos.extend(1.0));
            let color = |shift: u32| ((q.color >> shift) & 255) as f32 / 255.0;
            let mode = if q.mode == ui_mode::FONT { box_mode::SIGN_GLYPH } else { box_mode::SIGN_IMAGE };
            boxes.push(GpuBox {
                rows: rows(m),
                dims: [1.0, 0.0, 0.0, 0.0],
                uvo: q.uv,
                color: [color(24), color(16), color(8), 0.0],
                ids: [0, mode, q.tex, 0],
                ids2: [0, light, 0, 0],
            });
        }
    }
    boxes
}
