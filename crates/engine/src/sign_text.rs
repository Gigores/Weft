use std::{cell::RefCell, rc::Rc};

use glam::{DVec3, Mat4, Vec3};
use weft_render::Backend;

use crate::app::{Ctx, Request};
use crate::assets::Assets;
use crate::client::textures::GameTextures;
use crate::client::{Client, ClientMenu};
use crate::content::content;
use crate::gpu_layout::{GpuBox, box_mode, rows, ui_mode};
use crate::screens::{BuildCx, button, label};
use crate::ui::slider::{Slider, Thumb};
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

#[derive(Clone, Copy, Default)]
enum Page {
    #[default]
    Text,
    Color,
    Items,
}

#[derive(Default)]
pub(crate) struct Draft {
    window: Option<u32>,
    value: String,
    caret: usize,
    color: u32,
    search: String,
    page: Page,
}

type Edit = Rc<RefCell<Draft>>;

fn save(ctx: &mut Ctx, draft: &Draft) {
    if let Some(client) = ctx.client.as_deref_mut()
        && let Some(window) = draft.window
    {
        let lines = draft
            .value
            .split('\n')
            .take(LINES)
            .map(str::to_owned)
            .collect();
        client.send(crate::net::ToServer::WriteSign { window, lines });
    }
}

fn insert(ctx: &mut Ctx, edit: &Edit, token: &str) {
    let mut draft = edit.borrow_mut();
    if let Some(caret) = input_caret(ctx) {
        draft.caret = caret;
    }
    let at = draft
        .value
        .char_indices()
        .nth(draft.caret)
        .map_or(draft.value.len(), |(i, _)| i);
    let mut value = draft.value.clone();
    value.insert_str(at, token);
    if value
        .lines()
        .all(|line| line.chars().count() <= MAX_LINE as usize)
    {
        draft.value = value;
        draft.caret += token.chars().count();
        if let Some(top) = &mut ctx.top
            && let Some(id) = top.ui.find("sign_text")
            && let Kind::Input { value, caret, .. } = &mut top.ui.nodes[id].kind
        {
            *value = draft.value.clone();
            *caret = draft.caret;
            top.ui.focused = Some(id);
        }
        save(ctx, &draft);
    }
    draft.page = Page::Text;
    ctx.requests.push(Request::Rebuild);
}

fn input_caret(ctx: &Ctx) -> Option<usize> {
    let ui = &ctx.top.as_ref()?.ui;
    match &ui.nodes[ui.find("sign_text")?].kind {
        Kind::Input { caret, .. } => Some(*caret),
        _ => None,
    }
}

fn open_page(ctx: &mut Ctx, edit: &Edit, page: Page) {
    let mut draft = edit.borrow_mut();
    if let Some(caret) = input_caret(ctx) {
        draft.caret = caret;
    }
    draft.page = page;
    ctx.requests.push(Request::Rebuild);
}

pub(crate) fn editor(ui: &mut Ui, cx: &BuildCx, menu: &ClientMenu, edit: &Edit) {
    {
        let mut draft = edit.borrow_mut();
        if draft.window != Some(menu.window) {
            *draft = Draft { window: Some(menu.window), value: menu.name.clone(), color: 0xFFFFFF, ..Default::default() };
        }
    }
    let width = (cx.size.x - 16.0).clamp(170.0, 248.0);
    ui.panel("overlay", Style::default().grow().center(), |ui| {
        ui.panel("panel", Style::default().pad(10.0).gap(7.0).w(width), |ui| {
            let page = edit.borrow().page;
            let title = match page {
                Page::Text => "sign.edit",
                Page::Color => "sign.color",
                Page::Items => "sign.choose_icon",
            };
            ui.container(
                Style::row()
                    .gap(5.0)
                    .w(width - 20.0)
                    .align(crate::ui::Align::Center),
                |ui| {
                    label(ui, cx, title, &[], "title");
                    ui.spacer();
                    if matches!(page, Page::Color) {
                        let state = edit.clone();
                        crate::screens::close_button_with(ui, 10.0, move |ctx| open_page(ctx, &state, Page::Text));
                    } else {
                        crate::screens::close_button(ui, 10.0);
                    }
                },
            );
            match page {
                Page::Text => text_editor(ui, cx, edit, menu.block, width - 20.0),
                Page::Color => color_picker(ui, cx, edit, width - 20.0),
                Page::Items => item_picker(ui, cx, edit, width - 20.0),
            }
        });
    });
}

fn board_face(state: u32) -> Option<(&'static str, [f32; 4], f32)> {
    let c = content();
    let state_info = c.states.get(state as usize)?;
    let surface = c.components.id(weft_abi::comp::SIGN_SURFACE)?;
    let [x, y, z, _, _, face] = c
        .components
        .get::<[f32; 6]>(weft_abi::entry_kind::BLOCK as usize, state, surface)?;
    let face = state_info.turn.face(face as usize);
    let axis = face / 2;
    let (u, v) = match axis {
        0 => (2, 1),
        1 => (0, 2),
        _ => (0, 1),
    };
    let center = state_info.turn.point([x / 16.0, y / 16.0, z / 16.0], 1.0);
    let nearest_face = |b: &crate::content::ModelBox| {
        let edge = if face % 2 == 0 { b.min[axis] } else { b.max[axis] };
        (center[axis] - edge).abs()
    };
    let board = state_info
        .boxes
        .iter()
        .filter(|b| {
            [u, v]
                .into_iter()
                .all(|i| (b.min[i]..=b.max[i]).contains(&center[i]))
        })
        .min_by(|a, b| nearest_face(a).total_cmp(&nearest_face(b)))?;
    let aspect = (board.max[u] - board.min[u]) / (board.max[v] - board.min[v]);
    let uv = board.uv[face];
    let uv = if uv == weft_abi::UV_FROM_BOX {
        weft_abi::box_face_uv(face, board.min.map(|x| x * 16.0), board.max.map(|x| x * 16.0))
    } else {
        uv
    };
    Some((c.textures.get(board.textures[face] as usize)?, uv.map(|x| x / 16.0), aspect))
}

fn text_editor(ui: &mut Ui, cx: &BuildCx, edit: &Edit, state: u32, width: f32) {
    let (value, caret) = {
        let draft = edit.borrow();
        (draft.value.clone(), draft.caret)
    };
    let face = board_face(state);
    let (board_w, board_h) = face.map_or((width, 63.0), |(_, _, aspect)| {
        let h = (cx.size.y - 135.0).clamp(36.0, 80.0).min(width / aspect);
        (h * aspect, h)
    });
    ui.container(Style::default().w(width).center(), |ui| {
        ui.container(Style::default().size(board_w, board_h), |ui| {
            if let Some((texture, uv, _)) = face {
                ui.add(
                    Kind::ImageRegion { texture: texture.to_owned(), uv },
                    Style::default()
                        .size(board_w, board_h)
                        .at(0.0, 0.0, [0.0, 0.0]),
                );
            }
            ui.container(
                Style::default()
                    .size(board_w, board_h)
                    .pad(5.0)
                    .center()
                    .scroll(),
                |ui| {
                    let id = ui.text(text::rich(&value, cx.lang, None), "label");
                    ui.set_id(id, "sign_preview");
                    if let Some(t) = ui.text_mut(id) {
                        t.center = true;
                        t.color = Some(0xFFFFFFFF);
                    }
                },
            );
        });
    });

    let mut input = Kind::text_area(&value, (MAX_LINE as usize + 1) * LINES - 1, None);
    if let Kind::Input { caret: at, on_change, .. } = &mut input {
        *at = caret.min(value.chars().count());
        let edit = edit.clone();
        let lang = (*cx.lang).clone();
        *on_change = Some(Rc::new(move |ctx, value| {
            let clean = sanitize(value.split('\n').map(str::to_owned).collect());
            if clean != value {
                ctx.set_input("sign_text", clean.clone());
            }
            let mut draft = edit.borrow_mut();
            if draft.value != clean {
                draft.value = clean;
                save(ctx, &draft);
            }
            if let Some(top) = &mut ctx.top
                && let Some(id) = top.ui.find("sign_preview")
                && let Some(t) = top.ui.text_mut(id)
            {
                t.spans = text::rich(&draft.value, &lang, None);
                top.ui.dirty = true;
            }
        }));
    }
    let id = ui.add(input, Style::default().h(49.0).w(width));
    ui.set_id(id, "sign_text");
    ui.focused = Some(id);
    ui.container(
        Style::row()
            .gap(4.0)
            .w(width)
            .align(crate::ui::Align::Center),
        |ui| {
            let state = edit.clone();
            let color =
                ui.button("button", Style::default().size(22.0, 20.0), Rc::new(move |ctx| open_page(ctx, &state, Page::Color)), |ui| {
                    toolbar_icon(ui, "palette");
                });
            ui.set_id(color, "sign_color");
            ui.nodes[color].tooltip = Some(cx.t("sign.color", &[]));
            ui.spacer();
            let state = edit.clone();
            let icons = button(ui, cx, "sign.icon", &[], 58.0, move |ctx| open_page(ctx, &state, Page::Items));
            ui.set_id(icons, "sign_icons");
        },
    );
}

fn swatch(ui: &mut Ui, rgb: u32, side: f32) -> crate::ui::NodeId {
    let mut style = Style::default().size(side, side);
    style.fill = Some(rgb << 8 | 255);
    ui.container(style, |_| {})
}

fn toolbar_icon(ui: &mut Ui, name: &str) {
    ui.add(Kind::Image { texture: format!("core:gui/{name}"), color: 0xFFFFFFFF }, Style::default().size(12.0, 12.0));
}

fn set_color(ctx: &mut Ctx, edit: &Edit, color: u32, sync_hex: bool) {
    edit.borrow_mut().color = color;
    if sync_hex {
        ctx.set_input("sign_hex", format!("#{color:06X}"));
    }
    if let Some(top) = &mut ctx.top
        && let Some(id) = top.ui.find("sign_swatch")
    {
        top.ui.nodes[id].style.fill = Some(color << 8 | 255);
        top.ui.dirty = true;
    }
}

fn color_picker(ui: &mut Ui, cx: &BuildCx, edit: &Edit, width: f32) {
    let rgb = edit.borrow().color;
    ui.container(Style::row().gap(4.0), |ui| {
        for color in [0xFFD049, 0x70C7FF, 0xFFAE65, 0x81F9AA, 0xB5A7F2, 0xFFFFFF] {
            let state = edit.clone();
            ui.button("button", Style::default().size(19.0, 19.0), Rc::new(move |ctx| set_color(ctx, &state, color, true)), |ui| {
                swatch(ui, color, 14.0);
            });
        }
    });
    ui.container(Style::row().gap(5.0).align(crate::ui::Align::Center), |ui| {
        let id = swatch(ui, rgb, 15.0);
        ui.set_id(id, "sign_swatch");
        let mut input = Kind::input(&format!("#{rgb:06X}"), 7);
        let state = edit.clone();
        if let Kind::Input { on_change, .. } = &mut input {
            *on_change = Some(Rc::new(move |ctx, value| {
                let hex = value.trim_start_matches('#');
                if hex.len() == 6
                    && let Ok(color) = u32::from_str_radix(hex, 16)
                {
                    set_color(ctx, &state, color, false);
                }
            }));
        }
        let id = ui.add(input, Style::default().w(width - 20.0).h(18.0));
        ui.set_id(id, "sign_hex");
    });
    for (channel, shift) in [("R", 16), ("G", 8), ("B", 0)] {
        let ratio = move |color: u32| ((color >> shift) & 255) as f32 / 255.0;
        ui.container(Style::row().gap(5.0).align(crate::ui::Align::Center), |ui| {
            ui.text(text::plain(channel), "label");
            let state = edit.clone();
            let thumb = Thumb::new(ratio(rgb), move |ctx, value| {
                let color = (state.borrow().color & !(255 << shift)) | (((value * 255.0).round() as u32) << shift);
                set_color(ctx, &state, color, true);
            });
            let mut slider = Slider::new(vec![thumb]);
            slider.steps = 255;
            let state = edit.clone();
            slider.binding = Some((vec![0.0], Box::new(move |_, values| values[0] = ratio(state.borrow().color))));
            ui.add(Kind::Slider(slider), Style::default().w(width - 16.0).h(11.0));
        });
    }
    ui.container(Style::row().w(width), |ui| {
        ui.spacer();
        let state = edit.clone();
        let id = ui.button(
            "button",
            Style::default().size(22.0, 20.0),
            Rc::new(move |ctx| {
                let rgb = state.borrow().color;
                insert(ctx, &state, &format!("{{#{rgb:06X}}}"));
            }),
            |ui| toolbar_icon(ui, "icon_verified"),
        );
        ui.set_id(id, "sign_apply_color");
        ui.nodes[id].tooltip = Some(cx.t("sign.apply_color", &[]));
    });
}

fn item_picker(ui: &mut Ui, cx: &BuildCx, edit: &Edit, width: f32) {
    let search = edit.borrow().search.clone();
    let mut input = Kind::input(&search, 80);
    let state = edit.clone();
    if let Kind::Input { on_change, .. } = &mut input {
        *on_change = Some(Rc::new(move |ctx, value| {
            state.borrow_mut().search = value.to_owned();
            ctx.requests.push(Request::Rebuild);
        }));
    }
    let field = ui.add(input, Style::default().w(width).h(18.0));
    ui.set_id(field, "sign_item_search");
    ui.nodes[field].tooltip = Some(cx.t("editor.find", &[]));
    ui.focused = Some(field);

    let query = search.to_lowercase();
    let items = crate::editor::registered_items(content());
    let columns = ((width - 16.0) / 22.0).floor() as usize;
    let visible: Vec<_> = items
        .into_iter()
        .filter_map(|(name, id)| {
            let spans = text::Name::Item(id).spans(cx.lang, None);
            let label = text::plain_text(&spans).to_lowercase();
            query
                .split_whitespace()
                .all(|word| name.contains(word) || label.contains(word))
                .then_some((name, id, spans))
        })
        .collect();
    let height = (cx.size.y - 100.0).clamp(45.0, 118.0);
    ui.panel(
        "well",
        Style::default()
            .w(width)
            .h(height)
            .scroll()
            .pad(3.0)
            .gap(2.0),
        |ui| {
            if visible.is_empty() {
                label(ui, cx, "editor.nothing", &[], "dim");
            }
            for row in visible.chunks(columns.max(1)) {
                ui.container(Style::row().gap(2.0), |ui| {
                    for (name, id, spans) in row {
                        let state = edit.clone();
                        let token = format!("{{item:{name}}}");
                        let b =
                            ui.button("button", Style::default().size(20.0, 20.0), Rc::new(move |ctx| insert(ctx, &state, &token)), |ui| {
                                crate::editor::item_icon(ui, *id, 14.0);
                            });
                        ui.set_id(b, &format!("sign_item:{name}"));
                        ui.nodes[b].tooltip = Some(spans.clone());
                    }
                });
            }
        },
    );
    picker_back(ui, cx, edit);
}

fn picker_back(ui: &mut Ui, cx: &BuildCx, edit: &Edit) {
    let state = edit.clone();
    button(ui, cx, "menu.back", &[], 62.0, move |ctx| open_page(ctx, &state, Page::Text));
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
        ui.glyph_run(&layout.items, 0.0, 0.0, 1.0, 0xFFFFFFFF, None, &mut icons);
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
