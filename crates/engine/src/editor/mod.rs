//! The content editor: a mod's blocks and items made in a form, with their code beside it, both
//! editable and kept in step. The form is built from the SDK's own list of builder methods
//! (`code::METHODS`): each call is a card, each parameter a field chosen by its type, and a few
//! kinds of value get a picker from what the game has registered (pictures, sounds, tabs, tools,
//! tags, items). The work stays while the game runs, so the screen can be closed and opened again.

pub mod code;
mod model;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use weft_abi::entry_kind as ek;

use code::{Call, Param, Thing, What};

use crate::app::{Ctx, Request};
use crate::content::Content;
use crate::mods_screen::{badge, icon_button, separator};
use crate::screens::{BuildCx, Screen, close_button, label};
use crate::ui::render::ModelView;
use crate::ui::text::{self, Arg};
use crate::ui::{Align, Kind, Rect, Scrollbar, Style, Ui};

const LIST_W: f32 = 146.0;
const FORM_W: f32 = 310.0;
/// What the form's rows span: its width less its padding and scroll bar. Everything in it ends
/// at the same right edge.
const CONTENT_W: f32 = FORM_W - 26.0;
/// A parameter's name column in a call card.
const NAME_W: f32 = 78.0;
/// Its value's.
const VALUE_W: f32 = CONTENT_W - NAME_W - 4.0;
/// Fields and value buttons alike.
const FIELD_H: f32 = 16.0;

/// A list to choose an argument (or a new call) from.
#[derive(Clone, Copy, PartialEq)]
enum List {
    Method,
    Logic,
    Example(What),
    Picture(&'static str),
    Sound,
    Tab,
    Tool,
    Tier,
    Tag,
    Item,
}

#[derive(Clone, Copy, PartialEq)]
struct Picker {
    list: List,
    /// The call and argument it sets; `None` for a new call.
    target: Option<(usize, usize)>,
    anchor: Rect,
    face: Option<usize>,
}

#[derive(Clone)]
struct Snapshot {
    things: Vec<Thing>,
    picked: usize,
    code: Option<String>,
    error: Option<String>,
}

struct Session {
    things: Vec<Thing>,
    picked: usize,
    /// The code as the player typed it, while it is theirs; written from the things otherwise.
    code: Option<String>,
    code_error: Option<String>,
    /// Bumped when the form changes the code, so the code field shows the new text; and when the
    /// code changes the form, so its fields do.
    code_gen: u32,
    form_gen: u32,
    picker: Option<Picker>,
    find: String,
    code_scroll: Rc<Cell<[f32; 2]>>,
    /// The call being moved by its handle.
    dragging: Option<usize>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    edit_group: Option<String>,
    models: Vec<model::Preview>,
    view: ModelView,
    orbit: Option<glam::Vec2>,
}

thread_local! {
    static SESSION: Rc<RefCell<Session>> = Rc::new(RefCell::new(Session {
        things: vec![Thing::new(What::Block, "ruby_block")],
        picked: 0,
        code: None,
        code_error: None,
        code_gen: 0,
        form_gen: 0,
        picker: None,
        find: String::new(),
        code_scroll: Rc::default(),
        dragging: None,
        undo: Vec::new(),
        redo: Vec::new(),
        edit_group: None,
        models: Vec::new(),
        view: ModelView::default(),
        orbit: None,
    }));
}

type S = Rc<RefCell<Session>>;

impl Session {
    fn snapshot(&self) -> Snapshot {
        Snapshot { things: self.things.clone(), picked: self.picked, code: self.code.clone(), error: self.code_error.clone() }
    }

    fn remember(&mut self, group: Option<&str>) {
        if group.is_none() || self.edit_group.as_deref() != group {
            self.undo.push(self.snapshot());
            if self.undo.len() > 64 {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.edit_group = group.map(String::from);
    }

    fn history(&mut self, redo: bool) {
        let saved = if redo { self.redo.pop() } else { self.undo.pop() };
        let Some(saved) = saved else { return };
        let current = self.snapshot();
        if redo {
            self.undo.push(current);
        } else {
            self.redo.push(current);
        }
        self.things = saved.things;
        self.picked = saved.picked;
        self.code = saved.code;
        self.code_error = saved.error;
        self.picker = None;
        self.dragging = None;
        self.edit_group = None;
        self.models.clear();
        self.form_gen += 1;
        self.code_gen += 1;
    }

    fn unique_name(&self, base: &str) -> String {
        if !self.things.iter().any(|t| t.name == base) {
            return base.into();
        }
        (2..)
            .map(|i| format!("{base}_{i}"))
            .find(|n| !self.things.iter().any(|t| t.name == *n))
            .unwrap()
    }

    fn add(&mut self, mut thing: Thing) {
        thing.name = self.unique_name(&thing.name);
        self.things.push(thing);
        self.picked = self.things.len() - 1;
        self.picker = None;
        self.form_gen += 1;
    }

    fn thing(&mut self) -> &mut Thing {
        let i = self.picked.min(self.things.len() - 1);
        &mut self.things[i]
    }

    /// The form changed something: the code is written again from it.
    fn changed(&mut self) {
        self.code = None;
        self.code_error = None;
        self.code_gen += 1;
        self.models.clear();
    }
}

/// An action changing the session through the form.
fn edit<F: Fn(&mut Session) + 'static>(s: &S, f: F) -> impl Fn(&mut Ctx) + 'static + use<F> {
    let s = s.clone();
    move |c: &mut Ctx| {
        let mut st = s.borrow_mut();
        st.remember(None);
        f(&mut st);
        st.changed();
        c.requests.push(Request::Rebuild);
    }
}

fn open_picker(s: &S, ctx: &mut Ctx, list: List, target: Option<(usize, usize)>, id: &str) {
    let anchor = ctx
        .top
        .as_ref()
        .and_then(|top| top.ui.find(id).map(|n| top.ui.nodes[n].rect))
        .unwrap_or_default();
    let mut st = s.borrow_mut();
    st.picker = Some(Picker { list, target, anchor, face: None });
    st.find.clear();
    st.edit_group = None;
    if let Some(top) = &mut ctx.top {
        top.ui.focused = None;
    }
    ctx.requests.push(Request::Rebuild);
}

/// Sets argument `arg` of call `call` of the picked thing.
fn set_arg(st: &mut Session, call: usize, arg: usize, value: String) {
    if let Some(c) = st.thing().calls.get_mut(call) {
        if c.args.len() <= arg {
            c.args.resize(arg + 1, String::new());
        }
        c.args[arg] = value;
    }
}

/// What lowercase names and references may hold; anything else is dropped as it is typed.
fn clean(s: &str) -> String {
    s.chars()
        .map(|c| c.to_ascii_lowercase())
        .filter(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | ':' | '/' | '.'))
        .collect()
}

/// How a parameter is edited: by its type, with a picker where the game knows the choices.
fn list_for(kind: What, method: &str, p: &Param) -> Option<List> {
    if !p.picture.is_empty() {
        return Some(List::Picture(match p.picture.as_str() {
            "item" => "item",
            "entity" => "entity",
            _ => "block",
        }));
    }
    match (method, p.name.as_str(), p.ty.as_str()) {
        (_, _, "&CreativeTab") => Some(List::Tab),
        ("sound_group", _, _) => Some(List::Sound),
        (_, _, "GateLogic") => Some(List::Logic),
        ("mined_by", "class", _) => Some(List::Tool),
        ("mined_by", "level", _) => Some(List::Tier),
        ("tag" | "untag", _, _) => Some(List::Tag),
        ("drops", "item", _) if kind == What::Block => Some(List::Item),
        _ => None,
    }
}

#[derive(Clone)]
enum Icon {
    None,
    Item(u32),
    /// A texture by its id in `Content::textures`.
    Picture(u32),
    Texture(String),
}

/// A picture's icon, if the content has it.
fn picture(c: &Content, name: &str) -> Icon {
    c.textures
        .iter()
        .position(|n| n == name)
        .map_or(Icon::None, |i| Icon::Picture(i as u32))
}

struct Choice {
    label: String,
    icon: Icon,
    /// The argument as code (or the method's name for a new call).
    value: String,
    doc: String,
}

/// The first item having component `comp`.
fn item_with(c: &Content, comp: Option<u32>) -> Icon {
    let Some(comp) = comp else { return Icon::None };
    (0..c.items.len() as u32)
        .find(|&i| c.components.has(crate::components::ITEM, i, comp))
        .map_or(Icon::None, Icon::Item)
}

/// A block's own item, by the block's name.
fn block_item(c: &Content, block: &str) -> Icon {
    c.game
        .catalog
        .ids(ek::ITEM)
        .get(block)
        .map_or(Icon::None, |&i| Icon::Item(i))
}

fn tier_key(code: &str) -> String {
    format!("editor.tier_{}", code.rsplit("::").next().unwrap_or("").to_lowercase())
}

/// Everything `list` offers, from what is registered.
fn choices(c: &Content, kind: What, list: List, things: &[Thing], cx: &BuildCx) -> Vec<Choice> {
    let plain = |s: &str, icon: Icon, value: String| Choice { label: s.into(), icon, value, doc: String::new() };
    match list {
        List::Example(kind) => {
            let mut out = vec![Choice {
                label: text::plain_text(&cx.t(if kind == What::Block { "editor.empty_block" } else { "editor.empty_item" }, &[])),
                icon: Icon::None,
                value: "new".into(),
                doc: String::new(),
            }];
            match kind {
                What::Block => out.extend(
                    c.blocks
                        .iter()
                        .enumerate()
                        .skip(1)
                        .filter(|(_, b)| !c.states[b.first_state as usize].boxes.is_empty())
                        .map(|(i, b)| Choice {
                            label: b.name.clone(),
                            icon: block_item(c, &b.name),
                            value: i.to_string(),
                            doc: String::new(),
                        }),
                ),
                What::Item => out.extend(
                    c.items
                        .iter()
                        .enumerate()
                        .filter(|(_, item)| {
                            c.textures
                                .get(item.icon as usize)
                                .is_some_and(|name| name.contains(":item/"))
                        })
                        .map(|(i, item)| Choice {
                            label: item.name.clone(),
                            icon: Icon::Item(i as u32),
                            value: i.to_string(),
                            doc: String::new(),
                        }),
                ),
            }
            out
        }
        List::Logic => code::LOGIC_NAMES
            .iter()
            .map(|name| plain(&name.to_uppercase(), Icon::None, format!("GateLogic::{name}")))
            .collect(),
        List::Method => code::methods(kind)
            .map(|m| Choice { label: m.name.clone(), icon: Icon::None, value: m.name.clone(), doc: m.doc.clone() })
            .collect(),
        List::Picture(folder) => c
            .textures
            .iter()
            .filter(|n| n.contains(&format!(":{folder}/")) && !n.contains("destroy_"))
            .map(|n| {
                let arg = n.replacen(&format!("{folder}/"), "", 1);
                plain(&arg, picture(c, n), code::quote(&arg))
            })
            .collect(),
        List::Sound => {
            let sound = c.components.id(weft_abi::comp::SOUND);
            let mut groups: Vec<String> = c
                .game
                .catalog
                .ids(ek::SOUND)
                .keys()
                .filter_map(|k| k.strip_suffix(".break"))
                .filter(|k| k.contains(":block."))
                .map(String::from)
                .collect();
            groups.sort();
            groups
                .into_iter()
                .map(|g| {
                    // Shown as a block that sounds like this.
                    let icon = c
                        .blocks
                        .iter()
                        .enumerate()
                        .find(|(b, _)| sound.and_then(|s| c.components.text(crate::components::BLOCK, *b as u32, s)) == Some(g.as_str()))
                        .map_or(Icon::None, |(_, b)| block_item(c, &b.name));
                    plain(&g, icon, code::quote(&g))
                })
                .collect()
        }
        List::Tab => c
            .creative_tabs
            .iter()
            .map(|t| {
                use crate::content::CreativeTabIcon;
                let icon = match &t.icon {
                    CreativeTabIcon::Item(item) => Icon::Item(*item),
                    CreativeTabIcon::Texture(texture) => Icon::Texture(texture.clone()),
                };
                plain(&t.name, icon, format!("&CreativeTab::named({})", code::quote(&t.name)))
            })
            .collect(),
        List::Tool => c
            .tool_classes
            .iter()
            .filter_map(|&(mine, tools)| {
                let class = c.components.names[mine as usize].strip_prefix("mineable/")?;
                Some(plain(class, item_with(c, tools), code::quote(class)))
            })
            .collect(),
        List::Tier => code::TIERS
            .iter()
            .map(|n| {
                let value = format!("abi::tool_tier::{n}");
                plain(&text::plain_text(&cx.t(&tier_key(&value), &[])), Icon::None, value)
            })
            .collect(),
        List::Tag => {
            let mut tags: Vec<&String> = (0..c.components.count())
                .filter(|&i| c.components.is_tag(i))
                .map(|i| &c.components.names[i as usize])
                .collect();
            tags.sort();
            tags.into_iter()
                .map(|t| plain(t, Icon::None, code::quote(t)))
                .collect()
        }
        List::Item => {
            let mut v: Vec<Choice> = things
                .iter()
                .filter(|t| t.kind == What::Item)
                .map(|t| plain(&t.name, t.picture().map_or(Icon::None, |p| picture(c, &p)), code::quote(&t.name)))
                .collect();
            v.extend(
                registered_items(c)
                    .into_iter()
                    .map(|(n, i)| plain(n, Icon::Item(i), code::quote(n))),
            );
            v
        }
    }
}

fn property_name(cx: &BuildCx, name: &str) -> String {
    let key = format!("editor.property.{name}");
    if cx.lang.get(&key) == key { name.into() } else { text::plain_text(&cx.t(&key, &[])) }
}

fn property_tip(cx: &BuildCx, kind: What, name: &str) -> Vec<text::Span> {
    match code::method(kind, name) {
        Some(m) if !m.doc.is_empty() => text::plain(&format!("{name}\n{}", m.doc)),
        _ => cx.t("editor.no_doc", &[]),
    }
}

/// Whether an item of this name exists. A name without a namespace is the mod's own (as in the
/// SDK), so only the session can have it.
fn known_item(c: &Content, things: &[Thing], name: &str) -> bool {
    if name.contains(':') { c.game.catalog.ids(ek::ITEM).contains_key(name) } else { things.iter().any(|t| t.name == name) }
}

fn icon(ui: &mut Ui, icon: &Icon, side: f32) {
    match icon {
        Icon::None => {
            ui.add(Kind::Container { class: None }, Style::default().size(side, side));
        }
        Icon::Item(i) => {
            item_icon(ui, *i, side);
        }
        Icon::Picture(p) => {
            ui.add(Kind::Custom { tag: TAG_PICTURE, data: *p as u64 }, Style::default().size(side, side));
        }
        Icon::Texture(texture) => {
            ui.add(Kind::Image { texture: texture.clone(), color: 0xFFFFFFFF }, Style::default().size(side, side));
        }
    }
}

pub(crate) fn registered_items(c: &Content) -> Vec<(&str, u32)> {
    let mut items: Vec<_> = c
        .game
        .catalog
        .ids(ek::ITEM)
        .iter()
        .map(|(name, &id)| (name.as_str(), id))
        .collect();
    items.sort_unstable_by_key(|&(name, _)| name);
    items
}

pub(crate) fn item_icon(ui: &mut Ui, id: u32, side: f32) {
    ui.add(Kind::Custom { tag: crate::client::modui::TAG_ICON, data: u64::from(id) }, Style::default().size(side, side));
}

fn no_wrap(ui: &mut Ui, n: crate::ui::NodeId) {
    if let Some(t) = ui.text_mut(n) {
        t.wrap = false;
    }
}

/// Text cut to `most` chars, the whole of it in a tooltip.
fn short_text(ui: &mut Ui, s: &str, most: usize, class: &str) {
    let cut = s.chars().count() > most;
    let shown: String = if cut { s.chars().take(most - 1).chain(Some('…')).collect() } else { s.into() };
    let t = ui.text(text::plain(&shown), class);
    no_wrap(ui, t);
    if cut {
        ui.nodes[t].tooltip = Some(text::plain(s));
    }
}

pub struct Editor {
    s: S,
}

impl Default for Editor {
    fn default() -> Self {
        Editor { s: SESSION.with(Rc::clone) }
    }
}

impl Screen for Editor {
    fn name(&self) -> &str {
        "core:editor"
    }

    fn keys_first(&self) -> bool {
        true
    }

    fn key(&mut self, key: crate::input::Key, ctx: &mut Ctx) -> bool {
        use crate::input::Key;
        use winit::keyboard::KeyCode;
        if key == Key::Code(KeyCode::Escape) && self.s.borrow().picker.is_some() {
            self.s.borrow_mut().picker = None;
            ctx.requests.push(Request::Rebuild);
            return true;
        }
        let ctrl = ctx.input.held_key(Key::Code(KeyCode::ControlLeft)) || ctx.input.held_key(Key::Code(KeyCode::ControlRight));
        if ctrl && matches!(key, Key::Code(KeyCode::KeyZ | KeyCode::KeyY)) {
            let shift = ctx.input.held_key(Key::Code(KeyCode::ShiftLeft)) || ctx.input.held_key(Key::Code(KeyCode::ShiftRight));
            self.s
                .borrow_mut()
                .history(key == Key::Code(KeyCode::KeyY) || shift);
            ctx.requests.push(Request::Rebuild);
            return true;
        }
        false
    }

    fn pointer(&mut self, cursor: glam::Vec2, ui: &mut Ui, ctx: &mut Ctx, _hud: &mut crate::hud::Hud) -> bool {
        let mut st = self.s.borrow_mut();
        if st.picker.is_some() {
            if !ui
                .find("editor_picker")
                .is_some_and(|n| ui.nodes[n].rect.contains(cursor))
            {
                if ctx.input.pressed_button(0) {
                    st.picker = None;
                    ctx.requests.push(Request::Rebuild);
                    return true;
                }
                return ctx.input.scroll() != 0.0;
            }
            return false;
        }
        let over_preview = ui
            .find("editor_preview")
            .is_some_and(|n| ui.nodes[n].rect.contains(cursor));
        if (over_preview || st.orbit.is_some()) && ctx.input.scroll() != 0.0 {
            st.view.zoom = (st.view.zoom * (ctx.input.scroll() * 0.12).exp()).clamp(0.3, 3.0);
        }
        if let Some(last) = st.orbit {
            if ctx.input.held_button(0) {
                let delta = cursor - last;
                st.view.yaw -= delta.x * 0.015;
                st.view.pitch = (st.view.pitch + delta.y * 0.015).clamp(-1.45, 1.45);
                st.orbit = Some(cursor);
            } else {
                st.orbit = None;
            }
            return true;
        }
        if over_preview {
            if ctx.input.pressed_button(0) {
                st.orbit = Some(cursor);
                return true;
            }
            if ctx.input.scroll() != 0.0 {
                return true;
            }
        }
        let Some(from) = st.dragging else {
            let handle = ui
                .hit(cursor)
                .and_then(|h| ui.nodes[h].id.as_deref())
                .and_then(|i| i.strip_prefix("call_handle:"))
                .and_then(|i| i.parse::<usize>().ok());
            let Some(i) = handle.filter(|_| ctx.input.pressed_button(0)) else { return false };
            st.remember(None);
            st.dragging = Some(i);
            ctx.requests.push(Request::Rebuild);
            return true;
        };
        ui.hovered = None;
        if !ctx.input.held_button(0) {
            st.dragging = None;
            ctx.requests.push(Request::Rebuild);
            return true;
        }
        let count = st.thing().calls.len();
        let to = (0..count)
            .filter(|&i| i != from)
            .filter_map(|i| ui.find(&format!("call:{i}")).map(|n| (i, ui.nodes[n].rect)))
            .filter(|(_, r)| r.y + r.h / 2.0 < cursor.y)
            .count();
        if to != from && to < count {
            let call = st.thing().calls.remove(from);
            st.thing().calls.insert(to, call);
            st.dragging = Some(to);
            st.form_gen += 1;
            st.changed();
            ctx.requests.push(Request::Rebuild);
        }
        true
    }

    fn build(&mut self, ui: &mut Ui, cx: &BuildCx) {
        let Some(c) = crate::content::try_content() else { return };
        let s = self.s.clone();
        {
            let mut st = s.borrow_mut();
            st.picked = st.picked.min(st.things.len().saturating_sub(1));
            st.models = st
                .things
                .iter()
                .map(|t| model::build(t, c, cx.assets))
                .collect();
        }
        let tall = (cx.size.y - 70.0).clamp(160.0, 430.0);
        let wide = (cx.size.x - 24.0).clamp(660.0, 960.0);
        let bg = if cx.in_game { "overlay" } else { "background" };
        ui.root_style(Style::default().stretch());
        ui.panel(bg, Style::default().grow().center().pad(8.0), |ui| {
            ui.panel("panel", Style::default().w(wide).pad(10.0).gap(6.0), |ui| {
                ui.container(Style::row().gap(6.0).align(Align::Center).w(wide - 20.0), |ui| {
                    let t = label(ui, cx, "editor.title", &[], "title");
                    if let Some(t) = ui.text_mut(t) {
                        t.scale = 2.0;
                        t.wrap = false;
                    }
                    for (redo, image, tip) in [(false, "icon_undo", "editor.undo"), (true, "icon_redo", "editor.redo")] {
                        let s2 = s.clone();
                        let b = icon_button(ui, cx, "button", image, tip, &[], move |ctx| {
                            s2.borrow_mut().history(redo);
                            ctx.requests.push(Request::Rebuild);
                        });
                        ui.set_id(b, tip);
                        let enabled = if redo { !s.borrow().redo.is_empty() } else { !s.borrow().undo.is_empty() };
                        if let Kind::Button { enabled: e, .. } = &mut ui.nodes[b].kind {
                            *e = enabled;
                        }
                    }
                    ui.spacer();
                    for (kind, image, tip) in
                        [(What::Block, "editor_block", "editor.new_block"), (What::Item, "editor_item", "editor.new_item")]
                    {
                        let s2 = s.clone();
                        let b = icon_button(ui, cx, "button_install", image, tip, &[], move |ctx| {
                            open_picker(&s2, ctx, List::Example(kind), None, tip);
                        });
                        ui.set_id(b, tip);
                    }
                    close_button(ui, 15.0);
                });
                let gap = Scrollbar::HANDLE[0] + Scrollbar::OUTSIDE_GAP * 2.0;
                let right_w = wide - 20.0 - LIST_W - FORM_W - 2.0 * gap;
                ui.container(Style::row().gap(gap).w(wide - 20.0), |ui| {
                    self.list(ui, cx, tall);
                    ui.panel(
                        "well",
                        Style::default()
                            .pad(6.0)
                            .gap(5.0)
                            .w(FORM_W)
                            .h(tall)
                            .scroll(),
                        |ui| self.form(ui, cx, c),
                    );
                    ui.container(Style::default().gap(6.0).w(right_w).h(tall), |ui| {
                        self.code(ui, cx, right_w, (tall - 6.0) * 0.5);
                        self.preview_panel(ui, cx, right_w, (tall - 6.0) * 0.5);
                    });
                });
            });
        });
        if let Some(p) = s.borrow().picker {
            let width = FORM_W;
            let height = (cx.size.y - 24.0).min(260.0);
            let x =
                if p.anchor.x + p.anchor.w + width + 8.0 < cx.size.x { p.anchor.x + p.anchor.w + 4.0 } else { p.anchor.x - width - 4.0 }
                    .clamp(8.0, (cx.size.x - width - 8.0).max(8.0));
            let y = p.anchor.y.clamp(8.0, (cx.size.y - height - 8.0).max(8.0));
            let popup = ui.panel(
                "panel",
                Style::default()
                    .w(width)
                    .h(height)
                    .pad(6.0)
                    .gap(5.0)
                    .at(x, y, [0.0, 0.0]),
                |ui| {
                    self.picker(ui, cx, c, p, height - 12.0);
                },
            );
            ui.set_id(popup, "editor_picker");
            ui.modal = Some(popup);
        }
    }
}

impl Editor {
    fn list(&self, ui: &mut Ui, cx: &BuildCx, tall: f32) {
        let s = &self.s;
        let (things, picked) = {
            let st = s.borrow();
            (st.things.clone(), st.picked)
        };
        ui.panel(
            "well",
            Style::default()
                .pad(4.0)
                .gap(3.0)
                .w(LIST_W)
                .h(tall)
                .scroll_outside(),
            |ui| {
                for (i, th) in things.iter().enumerate() {
                    let s2 = s.clone();
                    let b = ui.button(
                        "tile",
                        Style::row()
                            .gap(3.0)
                            .pad(3.0)
                            .w(LIST_W - 8.0)
                            .align(Align::Center),
                        Rc::new(move |ctx| {
                            let mut st = s2.borrow_mut();
                            st.picked = i;
                            st.picker = None;
                            st.edit_group = None;
                            st.form_gen += 1;
                            ctx.requests.push(Request::Rebuild);
                        }),
                        |ui| {
                            preview(ui, i, 16.0);
                            ui.container(
                                Style::default()
                                    .gap(1.0)
                                    .w(LIST_W - 8.0 - 6.0 - 16.0 - 9.0 - 34.0),
                                |ui| {
                                    short_text(ui, &th.name, 13, "default");
                                    label(ui, cx, if th.kind == What::Block { "editor.block" } else { "editor.item" }, &[], "dim");
                                },
                            );
                            let b = icon_button(
                                ui,
                                cx,
                                "button",
                                "icon_copy",
                                "editor.duplicate",
                                &[],
                                edit(s, move |st| {
                                    let mut copy = st.things[i].clone();
                                    copy.name = format!("{}_copy", copy.name);
                                    st.add(copy);
                                }),
                            );
                            ui.set_id(b, &format!("editor_duplicate:{i}"));
                            let b = icon_button(
                                ui,
                                cx,
                                "button",
                                "icon_trash",
                                "editor.remove",
                                &[],
                                edit(s, move |st| {
                                    st.things.remove(i);
                                    if st.picked > i {
                                        st.picked -= 1;
                                    }
                                    st.picked = st.picked.min(st.things.len().saturating_sub(1));
                                    st.picker = None;
                                    st.form_gen += 1;
                                }),
                            );
                            ui.set_id(b, &format!("editor_remove:{i}"));
                        },
                    );
                    if let Kind::Button { selected, .. } = &mut ui.nodes[b].kind {
                        *selected = i == picked;
                    }
                    ui.set_id(b, &format!("thing:{}", th.name));
                }
            },
        );
    }

    fn call_actions(&self, ui: &mut Ui, cx: &BuildCx, kind: What, call: &Call, ci: usize) {
        let s = &self.s;
        let name = call.method.clone();
        let b = icon_button(
            ui,
            cx,
            "button",
            "icon_refresh",
            "editor.reset_property",
            &[],
            edit(s, move |st| {
                let defaults = Thing::new(kind, "")
                    .calls
                    .into_iter()
                    .find(|c| c.method == name);
                let args = defaults.map(|c| c.args).unwrap_or_else(|| {
                    code::method(kind, &name).map_or_else(Vec::new, |m| m.params.iter().map(code::default_arg).collect())
                });
                st.thing().calls[ci].args = args;
                st.form_gen += 1;
            }),
        );
        ui.set_id(b, &format!("editor_reset:{ci}"));
        let b = icon_button(
            ui,
            cx,
            "button",
            "icon_trash",
            "editor.remove_call",
            &[],
            edit(s, move |st| {
                st.thing().calls.remove(ci);
                st.form_gen += 1;
            }),
        );
        ui.set_id(b, &format!("editor_remove_call:{ci}"));
    }

    fn form(&self, ui: &mut Ui, cx: &BuildCx, c: &Content) {
        let s = &self.s;
        let (things, picked, generation) = {
            let st = s.borrow();
            (st.things.clone(), st.picked, st.form_gen)
        };
        let Some(t) = things.get(picked) else {
            label(ui, cx, "editor.empty", &[], "dim");
            return;
        };
        let id = |what: &str| format!("editor_{what}:{picked}:{generation}");
        let s2 = s.clone();
        field(ui, &t.name, CONTENT_W, &id("name"), move |ctx, v| {
            let mut st = s2.borrow_mut();
            if st.thing().name == v {
                return;
            }
            st.remember(Some("name"));
            st.thing().name = v.into();
            st.changed();
            ctx.requests.push(Request::Rebuild);
        });
        label(ui, cx, if t.kind == What::Block { "editor.block" } else { "editor.item" }, &[], "dim");
        if t.name.is_empty()
            || !t
                .name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            || !t
                .name
                .starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
            || things
                .iter()
                .enumerate()
                .any(|(i, x)| i != picked && x.name == t.name)
        {
            label(ui, cx, "editor.invalid_name", &[], "warning");
        }
        separator(ui);
        for (ci, call) in t.calls.iter().enumerate() {
            let m = code::method(t.kind, &call.method);
            let params: Vec<&Param> = m.map_or_else(Vec::new, |m| m.params.iter().collect());
            let n = params.len().max(call.args.len());
            let compact = n <= 1;
            let card = ui.container(Style::default().gap(4.0).w(CONTENT_W), |ui| {
                ui.container(Style::row().gap(4.0).align(Align::Center).w(CONTENT_W), |ui| {
                    let label = ui.text(text::plain(&property_name(cx, &call.method)), "default");
                    ui.nodes[label].style = Style::default().w(if compact { NAME_W } else { CONTENT_W - 40.0 });
                    no_wrap(ui, label);
                    ui.nodes[label].tooltip = Some(property_tip(cx, t.kind, &call.method));
                    ui.set_id(label, &format!("call_handle:{ci}"));
                    if compact && n == 1 {
                        self.value(
                            ui,
                            cx,
                            c,
                            &things,
                            t.kind,
                            call,
                            ci,
                            0,
                            params.first().copied(),
                            call.args.first().map_or("", String::as_str),
                            VALUE_W - 40.0,
                            &id(&format!("{ci}_0")),
                        );
                    } else {
                        ui.spacer();
                    }
                    self.call_actions(ui, cx, t.kind, call, ci);
                });
                if !compact {
                    for ai in 0..n {
                        let p = params.get(ai).copied();
                        let value = call.args.get(ai).map_or("", String::as_str);
                        ui.container(Style::row().gap(4.0).align(Align::Center), |ui| {
                            ui.container(Style::default().w(NAME_W), |ui| {
                                let key = if call.method == "mined_by" {
                                    if ai == 0 { "tool" } else { "tier" }
                                } else {
                                    p.map_or("", |p| p.name.as_str())
                                };
                                short_text(ui, &property_name(cx, key), 18, "dim");
                            });
                            self.value(ui, cx, c, &things, t.kind, call, ci, ai, p, value, VALUE_W, &id(&format!("{ci}_{ai}")));
                        });
                    }
                }
                if m.is_none() {
                    badge(ui, cx, "badge_unverified", "editor.unknown", &[], Some("editor.unknown_tip"));
                }
            });
            ui.set_id(card, &format!("call:{ci}"));
        }
        separator(ui);
        let s2 = s.clone();
        let b = crate::screens::button(ui, cx, "editor.add_call", &[], CONTENT_W, move |ctx| {
            open_picker(&s2, ctx, List::Method, None, "editor_add_call");
        });
        ui.set_id(b, "editor_add_call");
    }

    fn preview_panel(&self, ui: &mut Ui, cx: &BuildCx, w: f32, h: f32) {
        if self.s.borrow().things.is_empty() {
            ui.panel("well", Style::default().w(w).h(h), |_| {});
            return;
        }
        let (picked, errors) = {
            let st = self.s.borrow();
            (
                st.picked,
                st.models
                    .get(st.picked)
                    .map(|m| m.errors.clone())
                    .unwrap_or_default(),
            )
        };
        ui.panel("well", Style::default().w(w).h(h).pad(5.0).gap(3.0), |ui| {
            ui.container(Style::row().w(w - 10.0).align(Align::Center), |ui| {
                label(ui, cx, "editor.preview", &[], "dim");
                ui.spacer();
                let s = self.s.clone();
                let b = icon_button(ui, cx, "button", "icon_refresh", "editor.reset_view", &[], move |ctx| {
                    s.borrow_mut().view = ModelView::default();
                    ctx.requests.push(Request::Rebuild);
                });
                ui.set_id(b, "editor_reset_view");
            });
            let warning_h = if errors.is_empty() { 0.0 } else { 22.0 };
            let n = ui.add(
                Kind::Custom { tag: TAG_PREVIEW, data: picked as u64 },
                Style::default()
                    .w(w - 10.0)
                    .h((h - 43.0 - warning_h).max(24.0)),
            );
            ui.set_id(n, "editor_preview");
            label(ui, cx, "editor.orbit", &[], "dim");
            if !errors.is_empty() {
                label(ui, cx, "editor.preview_partial", &[Arg::Text(errors.join(", "))], "warning");
            }
        });
    }

    /// One argument's field.
    #[allow(clippy::too_many_arguments)]
    fn value(
        &self,
        ui: &mut Ui,
        cx: &BuildCx,
        c: &Content,
        things: &[Thing],
        kind: What,
        call: &Call,
        ci: usize,
        ai: usize,
        p: Option<&Param>,
        value: &str,
        width: f32,
        id: &str,
    ) {
        let s = &self.s;
        if let Some(p) = p {
            if let Some(values) = code::struct_values(value, p) {
                ui.container(Style::default().gap(3.0).w(width), |ui| {
                    for (index, (name, ty, value)) in values.iter().enumerate() {
                        ui.container(Style::row().gap(3.0).align(Align::Center), |ui| {
                            short_text(ui, &property_name(cx, name), 18, "dim");
                            let s2 = s.clone();
                            let (fields, root_type, quoted) = (values.clone(), p.ty.clone(), ty == "str");
                            let shown = if quoted { code::unquote(value).unwrap_or_default() } else { value.clone() };
                            field(ui, &shown, (width * 0.55).max(35.0), &format!("{id}_{index}"), move |ctx, text| {
                                let mut fields = fields.clone();
                                fields[index].2 = if quoted { code::quote(text) } else { text.into() };
                                let mut st = s2.borrow_mut();
                                st.remember(Some(&format!("arg:{ci}:{ai}:{index}")));
                                set_arg(&mut st, ci, ai, code::struct_code(&root_type, &fields));
                                st.changed();
                                ctx.requests.push(Request::Rebuild);
                            });
                        });
                    }
                });
                return;
            }
            if p.ty == "Gate"
                && let Some(gate) = code::GateArgs::read(value)
            {
                ui.container(Style::default().gap(3.0).w(width), |ui| {
                    for outputs in [false, true] {
                        label(ui, cx, if outputs { "editor.gate_outputs" } else { "editor.gate_inputs" }, &[], "dim");
                        for row in (0..6).collect::<Vec<_>>().chunks(3) {
                            ui.container(Style::row().gap(2.0), |ui| {
                                for &side in row {
                                    let bit = 1 << side;
                                    let on = (if outputs { gate.outputs } else { gate.inputs }) & bit != 0;
                                    let next = gate.clone();
                                    let b = crate::screens::button(
                                        ui,
                                        cx,
                                        &format!("editor.side_{}", code::SIDES[side].to_lowercase()),
                                        &[],
                                        (width - 4.0) / 3.0,
                                        edit(s, move |st| {
                                            let mut next = next.clone();
                                            if outputs {
                                                next.outputs ^= bit;
                                                next.inputs &= !next.outputs;
                                            } else {
                                                next.inputs ^= bit;
                                                next.outputs &= !next.inputs;
                                            }
                                            set_arg(st, ci, ai, next.code());
                                        }),
                                    );
                                    if let Kind::Button { selected, .. } = &mut ui.nodes[b].kind {
                                        *selected = on;
                                    }
                                }
                            });
                        }
                    }
                    let s2 = s.clone();
                    let next = gate.clone();
                    label(ui, cx, "editor.gate_delay", &[], "dim");
                    field(ui, &gate.delay.to_string(), width, &format!("{id}_delay"), move |ctx, text| {
                        let Ok(delay) = text.parse() else { return };
                        let mut next = next.clone();
                        next.delay = delay;
                        let mut st = s2.borrow_mut();
                        st.remember(Some(&format!("gate:{ci}:{ai}:delay")));
                        set_arg(&mut st, ci, ai, next.code());
                        st.changed();
                        ctx.requests.push(Request::Rebuild);
                    });
                });
                return;
            }
        }
        if call.method == "model_box_faces" && ai == 2 {
            if let Some(faces) = model::faces(value) {
                ui.container(Style::default().gap(4.0).w(width), |ui| {
                    for (face, (name, uv)) in faces.iter().enumerate() {
                        let field_id = format!("{id}_face:{face}");
                        ui.container(Style::row().gap(3.0).align(Align::Center), |ui| {
                            ui.container(Style::default().w(18.0), |ui| {
                                short_text(ui, ["-X", "+X", "-Y", "+Y", "-Z", "+Z"][face], 2, "dim");
                            });
                            let s2 = s.clone();
                            let input_id = field_id.clone();
                            let b = ui.button(
                                "button",
                                Style::row()
                                    .gap(3.0)
                                    .w(width - 21.0)
                                    .h(FIELD_H)
                                    .align(Align::Center),
                                Rc::new(move |ctx| {
                                    open_picker(&s2, ctx, List::Picture("block"), Some((ci, ai)), &input_id);
                                    if let Some(p) = &mut s2.borrow_mut().picker {
                                        p.face = Some(face);
                                    }
                                }),
                                |ui| {
                                    if let Some(picture_name) = code::picture_name(name, "block") {
                                        icon(ui, &picture(c, &picture_name), 14.0);
                                    }
                                    short_text(ui, if name.is_empty() { "-" } else { name }, 22, "button");
                                },
                            );
                            ui.set_id(b, &field_id);
                            ui.nodes[b].tooltip = Some(text::plain(&format!("{}\n{}", weft_abi::FACE_NAMES[face], name)));
                        });
                        ui.container(Style::row().gap(3.0), |ui| {
                            for (axis, value) in uv.iter().enumerate() {
                                let s2 = s.clone();
                                let f = field(
                                    ui,
                                    &code::num(*value),
                                    (width - 9.0) / 4.0,
                                    &format!("{field_id}_uv:{axis}"),
                                    move |ctx, text| {
                                        let Some(value) = code::number(text) else { return };
                                        let mut st = s2.borrow_mut();
                                        let Some(mut faces) = st.thing().calls[ci]
                                            .args
                                            .get(ai)
                                            .and_then(|v| model::faces(v))
                                        else {
                                            return;
                                        };
                                        if faces[face].1[axis] == value {
                                            return;
                                        }
                                        st.remember(Some(&format!("uv:{ci}:{face}:{axis}")));
                                        faces[face].1[axis] = value;
                                        set_arg(&mut st, ci, ai, model::faces_code(&faces));
                                        st.changed();
                                        ctx.requests.push(Request::Rebuild);
                                    },
                                );
                                ui.nodes[f].tooltip = Some(text::plain(["u0", "v0", "u1", "v1"][axis]));
                            }
                        });
                    }
                });
                return;
            }
        }
        let list = p.and_then(|p| list_for(kind, &call.method, p));
        if let Some(list) = list {
            // The value as a button: a click opens the list.
            let shown = choices(c, kind, list, things, cx)
                .into_iter()
                .find(|ch| ch.value == value);
            let (ic, words) = match (&shown, list) {
                (Some(ch), _) => (ch.icon.clone(), ch.label.clone()),
                (None, List::Picture(folder)) => {
                    let arg = code::unquote(value).unwrap_or_default();
                    (code::picture_name(&arg, folder).map_or(Icon::None, |p| picture(c, &p)), arg)
                }
                (None, _) => (Icon::None, code::unquote(value).unwrap_or_else(|| value.to_string())),
            };
            // An item not made yet: room beside it for the button that makes it.
            let missing = list == List::Item && {
                let name = code::unquote(value).unwrap_or_default();
                !name.is_empty() && !known_item(c, things, &name)
            };
            let sound = list == List::Sound;
            let w = width - if missing || sound { 19.0 } else { 0.0 };
            let s2 = s.clone();
            let field_id = id.to_string();
            let b = ui.button(
                "button",
                Style::row()
                    .gap(4.0)
                    .pad_xy(3.0, 1.0)
                    .w(w)
                    .h(FIELD_H)
                    .align(Align::Center),
                Rc::new(move |c: &mut Ctx| {
                    open_picker(&s2, c, list, Some((ci, ai)), &field_id);
                }),
                |ui| {
                    icon(ui, &ic, 14.0);
                    short_text(ui, if words.is_empty() { "-" } else { &words }, 26, "button");
                },
            );
            ui.nodes[b].tooltip = Some(cx.t("editor.choose", &[]));
            ui.set_id(b, id);
            if sound {
                let name = code::unquote(value).map(|v| format!("{v}.break"));
                let sound = name.and_then(|n| c.game.catalog.find(ek::SOUND, &n));
                let b = icon_button(ui, cx, "button", "icon_sound", "editor.listen", &[], move |ctx| {
                    if let Some(sound) = sound {
                        ctx.requests
                            .push(Request::Sound(crate::audio::Play { sound, pos: None, volume: 1.0, pitch: 0.0 }));
                    }
                });
                ui.set_id(b, &format!("editor_listen:{ci}"));
                if let Kind::Button { enabled, .. } = &mut ui.nodes[b].kind {
                    *enabled = sound.is_some();
                }
            }
            if missing {
                let name = code::unquote(value).unwrap_or_default();
                {
                    let b = icon_button(
                        ui,
                        cx,
                        "button_update",
                        "editor_item",
                        "editor.make_it",
                        &[],
                        edit(s, move |st| {
                            st.add(Thing::new(What::Item, &name));
                        }),
                    );
                    ui.set_id(b, "editor_make");
                }
            }
            return;
        }
        let ty = p.map_or("", |p| p.ty.as_str());
        match ty {
            "bool" => {
                let on = value.trim() == "true";
                let b = crate::screens::button(
                    ui,
                    cx,
                    if on { "settings.on" } else { "settings.off" },
                    &[],
                    50.0,
                    edit(s, move |st| {
                        set_arg(st, ci, ai, (!on).to_string());
                    }),
                );
                ui.set_id(b, id);
            }
            "[f32;3]" => {
                let v = code::three(value).unwrap_or([0.0; 3]);
                for k in 0..3 {
                    let s2 = s.clone();
                    let f = field(ui, &code::num(v[k]), (width - 8.0) / 3.0, &format!("{id}_{k}"), move |c, text| {
                        let Ok(n) = text.trim().parse::<f32>() else { return };
                        let mut st = s2.borrow_mut();
                        st.remember(Some(&format!("arg:{ci}:{ai}:{k}")));
                        let mut v = st
                            .thing()
                            .calls
                            .get(ci)
                            .and_then(|c| c.args.get(ai))
                            .and_then(|a| code::three(a))
                            .unwrap_or([0.0; 3]);
                        v[k] = n;
                        set_arg(&mut st, ci, ai, format!("[{}, {}, {}]", code::num(v[0]), code::num(v[1]), code::num(v[2])));
                        st.changed();
                        c.requests.push(Request::Rebuild);
                    });
                    ui.nodes[f].tooltip = Some(text::plain(["x", "y", "z"][k]));
                }
            }
            _ => {
                // Text is edited without its quotes; numbers and anything else as written.
                let quoted = ty == "&str";
                let shown = if quoted { code::unquote(value).unwrap_or_else(|| value.to_string()) } else { value.to_string() };
                let s2 = s.clone();
                let numeric = matches!(ty, "f32" | "f64");
                let f = field(ui, &shown, width, id, move |c, text| {
                    let v = if quoted {
                        code::quote(text)
                    } else if numeric {
                        match text.trim().parse::<f32>() {
                            Ok(n) if !text.trim().ends_with('.') => code::num(n),
                            _ => text.to_string(),
                        }
                    } else {
                        text.to_string()
                    };
                    let mut st = s2.borrow_mut();
                    if st.thing().calls.get(ci).and_then(|c| c.args.get(ai)) == Some(&v) {
                        return;
                    }
                    st.remember(Some(&format!("arg:{ci}:{ai}")));
                    set_arg(&mut st, ci, ai, v);
                    st.changed();
                    c.requests.push(Request::Rebuild);
                });
                if !ty.is_empty() {
                    ui.nodes[f].tooltip = Some(text::plain(ty));
                }
            }
        }
    }

    fn picker(&self, ui: &mut Ui, cx: &BuildCx, c: &Content, p: Picker, height: f32) {
        let s = &self.s;
        let (things, picked, find) = {
            let st = s.borrow();
            (st.things.clone(), st.picked, st.find.clone())
        };
        let kind = match p.list {
            List::Example(kind) => kind,
            _ => things.get(picked).map_or(What::Block, |t| t.kind),
        };
        ui.container(Style::row().gap(4.0).w(CONTENT_W).align(Align::Center), |ui| {
            let title = match p.list {
                List::Example(_) => text::plain_text(&cx.t("editor.examples", &[])),
                List::Method => text::plain_text(&cx.t("editor.add_call", &[])),
                _ => p
                    .target
                    .map_or_else(String::new, |(ci, _)| property_name(cx, &things[picked].calls[ci].method)),
            };
            short_text(ui, &title, 40, "default");
            ui.spacer();
            let s2 = s.clone();
            let b = icon_button(ui, cx, "button", "close", "editor.back", &[], move |ctx| {
                s2.borrow_mut().picker = None;
                ctx.requests.push(Request::Rebuild);
            });
            ui.set_id(b, "editor_back");
        });
        let s2 = s.clone();
        let input = field(ui, &find, CONTENT_W, "editor_find", move |ctx, v| {
            s2.borrow_mut().find = v.to_lowercase();
            ctx.requests.push(Request::Rebuild);
        });
        ui.nodes[input].tooltip = Some(cx.t("editor.find", &[]));
        if ui.focused.is_none() {
            ui.focused = Some(input);
        }
        let words: Vec<_> = find.split_whitespace().collect();
        let mut found: Vec<_> = choices(c, kind, p.list, &things, cx)
            .into_iter()
            .filter(|ch| {
                words
                    .iter()
                    .all(|word| ch.label.to_lowercase().contains(word) || ch.doc.to_lowercase().contains(word))
            })
            .collect();
        found.sort_by_key(|ch| match words.first() {
            Some(w) if ch.label.to_lowercase().starts_with(w) => 0,
            Some(w) if ch.label.to_lowercase().contains(w) => 1,
            Some(_) => 2,
            None => 0,
        });
        if !find.is_empty() && matches!(p.list, List::Tag | List::Sound | List::Item) && !found.iter().any(|ch| ch.label == find) {
            let f = clean(&find);
            found.insert(
                0,
                Choice {
                    label: f.clone(),
                    icon: Icon::None,
                    value: code::quote(&f),
                    doc: text::plain_text(&cx.t("editor.write_this", &[])),
                },
            );
        }
        let pick = move |s: &S, value: String| {
            edit(s, move |st| {
                match p.list {
                    List::Example(kind) => {
                        let thing = match value.parse::<usize>() {
                            Ok(index) => model::example(crate::content::content(), kind, index),
                            Err(_) => Thing::new(kind, &format!("new_{}", kind.folder())),
                        };
                        st.add(thing);
                    }
                    _ => match p.target {
                        Some((ci, ai)) => {
                            let value = match p.face {
                                Some(face) => {
                                    let Some(mut faces) = model::faces(&st.thing().calls[ci].args[ai]) else { return };
                                    faces[face].0 = code::unquote(&value).unwrap_or_default();
                                    model::faces_code(&faces)
                                }
                                None => value.clone(),
                            };
                            set_arg(st, ci, ai, value);
                        }
                        None => {
                            let args = code::method(st.thing().kind, &value)
                                .map_or_else(Vec::new, |m| m.params.iter().map(code::default_arg).collect());
                            st.thing().calls.push(Call { method: value.clone(), args });
                        }
                    },
                }
                st.picker = None;
                st.form_gen += 1;
            })
        };
        ui.panel(
            "well",
            Style::default()
                .w(CONTENT_W)
                .h((height - 56.0).max(60.0))
                .pad(3.0)
                .gap(3.0)
                .scroll(),
            |ui| {
                if matches!(p.list, List::Picture(_)) {
                    for row in found.chunks(6) {
                        ui.container(Style::row().gap(3.0), |ui| {
                            for ch in row {
                                let b = ui.button("button", Style::default().size(38.0, 38.0), Rc::new(pick(s, ch.value.clone())), |ui| {
                                    icon(ui, &ch.icon, 32.0)
                                });
                                ui.nodes[b].tooltip = Some(text::plain(&ch.label));
                                ui.set_id(b, &format!("choice:{}", ch.label));
                            }
                        });
                    }
                } else {
                    for ch in &found {
                        let b = ui.button(
                            "tile",
                            Style::row()
                                .gap(4.0)
                                .pad(3.0)
                                .w(CONTENT_W - 18.0)
                                .align(Align::Center),
                            Rc::new(pick(s, ch.value.clone())),
                            |ui| {
                                icon(ui, &ch.icon, 16.0);
                                ui.container(Style::default().gap(2.0).w(CONTENT_W - 46.0), |ui| {
                                    short_text(ui, &ch.label, 40, "default");
                                    if !ch.doc.is_empty() {
                                        short_text(ui, &ch.doc, 86, "dim");
                                    }
                                });
                            },
                        );
                        ui.nodes[b].tooltip = Some(text::plain(&format!("{}\n{}", ch.label, ch.doc)));
                        ui.set_id(b, &format!("choice:{}", ch.label));
                    }
                }
                if found.is_empty() {
                    label(ui, cx, "editor.nothing", &[], "dim");
                }
            },
        );
    }

    /// The code, edited in place; the copy button in its corner.
    fn code(&self, ui: &mut Ui, cx: &BuildCx, w: f32, tall: f32) {
        let s = &self.s;
        let (value, generation, error, scroll) = {
            let st = s.borrow();
            (st.code.clone().unwrap_or_else(|| code::write(&st.things)), st.code_gen, st.code_error.clone(), st.code_scroll.clone())
        };
        let color = |k: &str| cx.lang.color(&format!("color.code_{k}")).map(|c| c >> 8);
        let colors = code::Colors {
            keyword: color("keyword"),
            call: color("call"),
            string: color("string"),
            number: color("number"),
            comment: color("comment"),
        };
        let format: crate::ui::Format = Rc::new(move |src: &str| code::highlight(src, &colors));
        let error_h = if error.is_some() { 24.0 } else { 0.0 };
        ui.container(Style::default().gap(3.0).w(w).h(tall), |ui| {
            ui.container(Style::default().w(w).h(tall - error_h), |ui| {
                let area = ui.add(Kind::text_area(&value, 20_000, Some(format)), Style::default().w(w).h(tall - error_h));
                let s2 = s.clone();
                if let Kind::Input { on_change, area: Some(a), .. } = &mut ui.nodes[area].kind {
                    a.scroll = scroll;
                    // Lines stay clear of the copy button in the corner.
                    a.inset = 22.0;
                    *on_change = Some(Rc::new(move |c: &mut Ctx, v: &str| {
                        let mut st = s2.borrow_mut();
                        if st.code.as_deref() == Some(v) {
                            return;
                        }
                        st.remember(Some("code"));
                        st.code = Some(v.to_string());
                        match code::read(v) {
                            Ok(things) => {
                                st.things = things;
                                st.picked = st.picked.min(st.things.len().saturating_sub(1));
                                st.code_error = None;
                                st.models.clear();
                                st.form_gen += 1;
                            }
                            Err(e) => st.code_error = Some(e),
                        }
                        c.requests.push(Request::Rebuild);
                    }));
                }
                ui.set_id(area, &format!("editor_code:{generation}"));
                let all = value.clone();
                let b = icon_button(ui, cx, "button_install", "icon_copy", "editor.copy", &[], move |_| {
                    crate::clipboard::set(&all);
                });
                ui.nodes[b].style = ui.nodes[b]
                    .style
                    .at_from([1.0, 0.0], -4.0 - crate::ui::Scrollbar::ROOM, 4.0, [1.0, 0.0]);
                ui.set_id(b, "editor_copy");
            });
            if let Some(e) = &error {
                label(ui, cx, "editor.code_error", &[Arg::Text(e.clone())], "warning");
            }
        });
    }
}

/// A one-line field calling `on` with what it holds after each change.
fn field(ui: &mut Ui, value: &str, w: f32, id: &str, on: impl Fn(&mut Ctx, &str) + 'static) -> crate::ui::NodeId {
    let input = ui.add(Kind::input(value, 20_000), Style::default().h(FIELD_H).w(w));
    if let Kind::Input { on_change, .. } = &mut ui.nodes[input].kind {
        *on_change = Some(Rc::new(on));
    }
    ui.set_id(input, id);
    input
}

/// The thing as it would look: a block as its model, an item as its picture.
fn preview(ui: &mut Ui, index: usize, side: f32) {
    ui.add(Kind::Custom { tag: TAG_BLOCK, data: index as u64 }, Style::default().size(side, side));
}

/// Texture `data` of `Content::textures`, flat.
pub const TAG_PICTURE: u32 = 21;

/// Thing `data` of the session drawn as a slot shows a block: its boxes, its picture on them.
pub const TAG_BLOCK: u32 = 20;

pub const TAG_PREVIEW: u32 = 22;

pub fn draw_block(
    ren: &mut crate::ui::render::UiRenderer,
    c: &Content,
    textures: &crate::client::textures::GameTextures,
    r: Rect,
    data: u64,
) {
    SESSION.with(|s| {
        let st = s.borrow();
        if let Some(model) = st.models.get(data as usize) {
            if let Some(sprite) = model.sprite {
                ren.picture_layers(c, textures, r, r, model.texture_id(sprite, c, textures));
            } else {
                ren.block_icon(c, textures, r, &model.drawn_boxes(c, textures));
            }
        }
    });
}

pub fn draw_preview(
    ren: &mut crate::ui::render::UiRenderer,
    c: &Content,
    textures: &crate::client::textures::GameTextures,
    look: &crate::ui::look::Look,
    r: Rect,
    data: u64,
) {
    SESSION.with(|s| {
        let st = s.borrow();
        if let Some(model) = st.models.get(data as usize) {
            if let Some(sprite) = model.sprite {
                let size = r.w.min(r.h) * 0.75 * st.view.zoom;
                ren.picture_layers(
                    c,
                    textures,
                    Rect { x: r.x + (r.w - size) / 2.0, y: r.y + (r.h - size) / 2.0, w: size, h: size },
                    r,
                    model.texture_id(sprite, c, textures),
                );
            } else {
                ren.preview_grid(r, &model.drawn_boxes(c, textures), st.view, look.text("preview_grid", ""), look.text("preview_cell", ""));
            }
        }
    });
}

pub fn is_block_preview(index: u64) -> bool {
    SESSION.with(|s| {
        s.borrow()
            .models
            .get(index as usize)
            .is_some_and(|m| m.sprite.is_none())
    })
}

pub fn render_block_preview(
    renderer: &mut crate::client::entity_render::EntityRenderer,
    gpu: &mut dyn weft_render::Backend,
    list: &mut weft_render::CommandList,
    frame: &weft_render::FrameInfo,
    size: [u32; 2],
    textures: &crate::client::textures::GameTextures,
    index: u64,
) -> Option<u32> {
    SESSION.with(|s| {
        let st = s.borrow();
        let c = crate::content::content();
        let model = st.models.get(index as usize)?;
        renderer.draw_box_preview(gpu, list, frame, size, textures, &model.drawn_boxes(c, textures), st.view, model.translucent)
    })
}
