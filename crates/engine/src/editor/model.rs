//! Literal builder arguments lowered to the same model boxes used by the game.

use weft_abi as abi;

use super::code::{self, Call, Thing, What};
use crate::assets::Assets;
use crate::block_model::FileBox;
use crate::content::{Content, ModelBox};

#[derive(Default)]
pub struct Preview {
    pub boxes: Vec<ModelBox>,
    pub sprite: Option<u32>,
    pub errors: Vec<String>,
    pub translucent: bool,
    dynamic: Vec<String>,
}

impl Preview {
    fn texture(&mut self, name: &str, c: &Content, assets: &Assets) -> u32 {
        if let Some(id) = c.textures.iter().position(|t| t == name) {
            return id as u32;
        }
        if let Some(id) = self.dynamic.iter().position(|t| t == name) {
            return (c.textures.len() + id) as u32;
        }
        let layers: Vec<_> = abi::layers(name).collect();
        let valid = !layers.is_empty()
            && layers.iter().all(|l| {
                assets
                    .path(&assets.picture(l.name), "textures", "png")
                    .is_some()
                    && l.kind.is_none_or(|k| c.tint_kinds.iter().any(|t| t.0 == k))
            });
        if valid {
            let id = c.textures.len() + self.dynamic.len();
            self.dynamic.push(name.into());
            id as u32
        } else {
            if !self.errors.iter().any(|e| e == name) {
                self.errors.push(name.into());
            }
            0
        }
    }

    pub fn texture_id(&self, id: u32, c: &Content, textures: &crate::client::textures::GameTextures) -> u32 {
        self.dynamic
            .get((id as usize).wrapping_sub(c.textures.len()))
            .map_or(id, |spec| textures.layered(spec))
    }

    pub fn drawn_boxes(&self, c: &Content, textures: &crate::client::textures::GameTextures) -> std::borrow::Cow<'_, [ModelBox]> {
        if self.dynamic.is_empty() {
            return std::borrow::Cow::Borrowed(&self.boxes);
        }
        let mut boxes = self.boxes.clone();
        for bx in &mut boxes {
            bx.textures = bx.textures.map(|id| self.texture_id(id, c, textures));
        }
        std::borrow::Cow::Owned(boxes)
    }
}

fn asset_arg(name: &str, folder: &str) -> String {
    abi::map_layer_names(name, |name| name.replacen(&format!(":{folder}/"), ":", 1))
}

fn texture(name: &str, folder: &str) -> Option<String> {
    if name.is_empty() {
        return Some(String::new());
    }
    code::picture_name(name, folder)
}

fn direction(s: &str) -> Option<usize> {
    let name = s.trim().rsplit("::").next()?.to_lowercase();
    abi::FACE_NAMES.iter().position(|n| *n == name)
}

fn byte(s: &str) -> Option<u8> {
    s.trim().trim_end_matches("u8").parse().ok()
}

fn uv(s: &str) -> Option<[f32; 4]> {
    match s.trim().rsplit("::").next()? {
        "UV_FROM_BOX" => Some(abi::UV_FROM_BOX),
        "UV_NO_FACE" => Some(abi::UV_NO_FACE),
        _ => code::array(s, code::number),
    }
}

pub fn faces(s: &str) -> Option<[(String, [f32; 4]); 6]> {
    code::array(s, |s| {
        let tuple = code::parts(s.trim().strip_prefix('(')?.strip_suffix(')')?)?;
        if tuple.len() != 2 {
            return None;
        }
        Some((code::unquote(tuple[0])?, uv(tuple[1])?))
    })
}

pub fn faces_code(faces: &[(String, [f32; 4]); 6]) -> String {
    let values: Vec<_> = faces
        .iter()
        .map(|(name, uv)| format!("({}, [{}])", code::quote(name), uv.map(code::num).join(", ")))
        .collect();
    format!("[{}]", values.join(", "))
}

struct Builder {
    faces: [String; 6],
    picked: u8,
    boxes: Vec<FileBox>,
    turns: [u8; 6],
    turn: abi::Turn,
    shape: abi::BlockModel,
    tall: bool,
    file: Option<String>,
    replacements: Vec<(String, String)>,
    folder: &'static str,
    item_layers: bool,
    translucent: bool,
}

impl Builder {
    fn layers(&mut self, mut change: impl FnMut(&mut String)) {
        for b in &mut self.boxes {
            for (face, t) in b.textures.iter_mut().enumerate() {
                if self.picked & (1 << face) != 0 && !t.is_empty() {
                    change(t);
                }
            }
        }
        for (face, t) in self.faces.iter_mut().enumerate() {
            if self.picked & (1 << face) != 0 {
                change(t);
            }
        }
    }

    fn apply(&mut self, call: &Call) -> Option<()> {
        let arg = |i| call.args.get(i).map(String::as_str);
        let text = |i| code::unquote(arg(i)?);
        let folder = self.folder;
        let picture = |i| texture(&text(i)?, folder);
        let faces = match call.method.as_str() {
            "texture" => Some(0x3f),
            "west" => Some(1),
            "east" => Some(2),
            "bottom" => Some(4),
            "top" => Some(8),
            "ends" => Some(12),
            "front" | "north" => Some(16),
            "back" | "south" => Some(32),
            "sides" => Some(51),
            _ => None,
        };
        if let Some(faces) = faces {
            let t = picture(0)?;
            self.picked = faces;
            self.item_layers = true;
            for (i, face) in self.faces.iter_mut().enumerate() {
                if faces & (1 << i) != 0 {
                    *face = t.clone();
                }
            }
            return Some(());
        }
        match call.method.as_str() {
            "cutout" => self.translucent = false,
            "translucent" => self.translucent = true,
            "model_box" | "model_box_faces" => {
                let (min, max) = (code::three(arg(0)?)?, code::three(arg(1)?)?);
                let (textures, uv) = if call.method == "model_box_faces" {
                    let faces = self::faces(arg(2)?)?;
                    let textures = faces
                        .each_ref()
                        .map(|f| texture(&f.0, "block"))
                        .into_iter()
                        .collect::<Option<Vec<_>>>()?
                        .try_into()
                        .ok()?;
                    (textures, faces.map(|f| f.1))
                } else {
                    (Default::default(), [abi::UV_FROM_BOX; 6])
                };
                self.boxes
                    .push(FileBox { min, max, textures, uv, turn: [0; 6], tilt: abi::BoxTilt::NONE, flat: false });
                self.shape = abi::BlockModel::Boxes;
            }
            "turned" => {
                let turns: [u8; 6] = code::array(arg(0)?, byte)?;
                if let Some(b) = self.boxes.last_mut() {
                    b.turn = std::array::from_fn(|i| (b.turn[i] + turns[i] % 4) % 4);
                }
            }
            "face_turns" => self.turns = code::array(arg(0)?, byte)?.map(|v| v % 4),
            "tilted" => {
                let origin = code::three(arg(0)?)?;
                let axis = abi::FACE_DIRS[direction(arg(1)?)?];
                let degrees = code::number(arg(2)?)?;
                if let Some(b) = self.boxes.last_mut() {
                    b.tilt = abi::BoxTilt { origin, axis: axis.map(|v| v as f32), degrees };
                }
            }
            "unshaded" => {
                if let Some(b) = self.boxes.last_mut() {
                    b.flat = true;
                }
            }
            "layer" => {
                let t = picture(0)?;
                if self.folder == "item" && !self.item_layers {
                    self.faces.fill(String::new());
                }
                self.item_layers = true;
                self.layers(|face| {
                    if !face.is_empty() {
                        face.push(' ');
                    }
                    face.push_str(&t);
                });
            }
            "tint" => {
                self.item_layers = true;
                let value = arg(0)?.trim().trim_end_matches("u32").replace('_', "");
                let color = match value.strip_prefix("0x") {
                    Some(v) => u32::from_str_radix(v, 16).ok()?,
                    None => value.parse().ok()?,
                };
                self.layers(|face| {
                    let mut layers: Vec<_> = abi::layers(face).collect();
                    if let Some(top) = layers.last_mut() {
                        top.tint = Some(color & 0xffffff);
                    }
                    *face = abi::layers_text(layers);
                });
            }
            "model_file" => self.file = Some(text(0)?),
            "model_texture" => self.replacements.push((text(0)?, picture(1)?)),
            "plant" => self.shape = abi::BlockModel::Cross,
            "air" => self.shape = abi::BlockModel::None,
            "tall_plant" => {
                self.shape = abi::BlockModel::Cross;
                self.tall = true;
                self.faces.fill(picture(0)?);
                self.faces[3] = picture(1)?;
            }
            "orientation" => {
                let s = arg(0)?.trim();
                let inner = s
                    .strip_prefix("Orientation::new(")
                    .or_else(|| s.strip_prefix("weft_sdk::Orientation::new("))?
                    .strip_suffix(')')?;
                let args = code::parts(inner)?;
                if args.len() != 2 {
                    return None;
                }
                let (attach, front) = (direction(args[0])?, direction(args[1])?);
                self.turn = abi::Turn::all().find(|t| t.face(2) == attach && t.face(4) == front)?;
            }
            // These can change appearance through arbitrary Rust expressions.
            "tint_kind" => {
                let name = arg(0)?.trim();
                let kind = if let Some(open) = name.find("::named(") {
                    code::unquote(name[open + 8..].strip_suffix(')')?)?
                } else {
                    let constant = name.rsplit("::").next()?;
                    if !name.contains("tints::") || !matches!(constant, "GRASS" | "FOLIAGE" | "WATER") {
                        return None;
                    }
                    format!("core:{}", constant.to_lowercase())
                };
                self.layers(|face| {
                    let mut layers: Vec<_> = abi::layers(face).collect();
                    if let Some(top) = layers.last_mut() {
                        top.kind = Some(&kind);
                    }
                    *face = abi::layers_text(layers);
                });
            }
            "states" | "set" | "set_text" => return None,
            _ => {}
        }
        Some(())
    }
}

pub fn build(thing: &Thing, c: &Content, assets: &Assets) -> Preview {
    let mut out = Preview::default();
    let folder = thing.kind.folder();
    let own = texture(&thing.name, folder).unwrap_or_default();
    let mut b = Builder {
        faces: std::array::from_fn(|_| own.clone()),
        picked: 0x3f,
        boxes: Vec::new(),
        turns: [0; 6],
        turn: abi::Turn::NONE,
        shape: abi::BlockModel::Cube,
        tall: false,
        file: None,
        replacements: Vec::new(),
        folder,
        item_layers: false,
        translucent: false,
    };
    for call in &thing.calls {
        if b.apply(call).is_none() {
            out.errors.push(call.method.clone());
        }
    }
    out.translucent = b.translucent;
    if let Some(file) = &b.file {
        let read = |name: &str| String::from_utf8(assets.read(name, "models", "json")?).ok();
        match crate::block_model::load_with(&texture(file, folder).unwrap_or_else(|| file.clone()), &read, &b.replacements) {
            Ok(Some(model)) => {
                if thing.kind == What::Item
                    && let Some(sprite) = model.sprite
                {
                    b.faces[0] = sprite;
                }
                if let Some(cube) = model.cube {
                    for (i, t) in cube.into_iter().enumerate().filter(|(_, t)| !t.is_empty()) {
                        b.faces[i] = t;
                    }
                }
                if !model.boxes.is_empty() {
                    b.boxes = model.boxes;
                    b.shape = abi::BlockModel::Boxes;
                }
            }
            _ => out.errors.push("model_file".into()),
        }
    }
    if thing.kind == What::Item && b.shape != abi::BlockModel::Boxes {
        out.sprite = Some(out.texture(&b.faces[0], c, assets));
        return out;
    }
    let mut resolve = |name: &str| out.texture(name, c, assets);
    let faces = std::array::from_fn(|face| {
        let needed = b.shape != abi::BlockModel::None
            && (b.shape != abi::BlockModel::Boxes
                || b.boxes
                    .iter()
                    .any(|bx| bx.textures[face].is_empty() && bx.uv[face] != abi::UV_NO_FACE));
        if needed { resolve(&b.faces[face]) } else { 0 }
    });
    out.boxes = if b.shape == abi::BlockModel::None {
        Vec::new()
    } else if b.shape == abi::BlockModel::Boxes {
        b.boxes
            .iter()
            .map(|bx| bx.model_box(|name, face| if name.is_empty() { faces[face] } else { resolve(name) }))
            .collect()
    } else if b.shape == abi::BlockModel::Cross {
        crate::blocks::cross(faces, b.tall)
    } else {
        let mut cube = ModelBox::whole(faces);
        cube.turn = b.turns;
        vec![cube]
    };
    if b.turn != abi::Turn::NONE {
        out.boxes = out.boxes.iter().map(|bx| bx.turned(b.turn)).collect();
    }
    out
}

/// A registered state's geometry becomes ordinary editable SDK calls, without a second set of templates.
fn appearance(c: &Content, kind: What, index: usize) -> Thing {
    if kind == What::Item {
        let item = &c.items[index];
        let mut t = Thing::new(kind, item.name.rsplit(':').next().unwrap_or("item"));
        if let Some(tex) = item
            .layers
            .as_deref()
            .or_else(|| c.textures.get(item.icon as usize).map(String::as_str))
        {
            t.calls = vec![Call::new("texture", &[&code::quote(&asset_arg(tex, "item"))])];
        }
        return t;
    }
    let block = &c.blocks[index];
    let state = &c.states[block.first_state as usize];
    let mut t = Thing { kind, name: block.name.rsplit(':').next().unwrap_or("block").into(), calls: Vec::new() };
    t.name = t
        .name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c.to_ascii_lowercase() } else { '_' })
        .collect();
    if t.name.starts_with(|c: char| c.is_ascii_digit()) {
        t.name.insert(0, '_');
    }
    t.calls
        .push(Call::new("hardness", &[&code::num(state.hardness)]));
    if state.light != [0; 3] {
        t.calls.push(Call::new(
            "light_rgb",
            &state
                .light
                .map(|v| v.to_string())
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        ));
    }
    if state.flags & abi::block_flags::SOLID == 0 {
        t.calls.push(Call::new("not_solid", &[]));
    }
    let model_bounds: Vec<_> = state
        .boxes
        .iter()
        .map(|b| [b.min[0], b.min[1], b.min[2], b.max[0], b.max[1], b.max[2]])
        .collect();
    for bounds in state
        .selection
        .iter()
        .filter(|_| state.selection != model_bounds)
    {
        let min = bounds[..3]
            .iter()
            .map(|v| code::num(v * 16.0))
            .collect::<Vec<_>>()
            .join(", ");
        let max = bounds[3..]
            .iter()
            .map(|v| code::num(v * 16.0))
            .collect::<Vec<_>>()
            .join(", ");
        t.calls
            .push(Call::new("hitbox", &[&format!("[{min}]"), &format!("[{max}]")]));
    }

    if state.layer != abi::RenderLayer::Opaque {
        t.calls
            .push(Call::new(if state.layer == abi::RenderLayer::Translucent { "translucent" } else { "cutout" }, &[]));
    }
    if let Some(comp) = c.components.id(abi::comp::SOUND)
        && let Some(sound) = c
            .components
            .text(crate::components::BLOCK, block.first_state, comp)
    {
        t.calls
            .push(Call::new("sound_group", &[&code::quote(sound)]));
    }
    let level = c
        .components
        .id(abi::comp::HARVEST_LEVEL)
        .and_then(|id| {
            c.components
                .get::<u32>(crate::components::BLOCK, block.first_state, id)
        })
        .unwrap_or(abi::tool_tier::HAND);
    for &(mine, _) in &c.tool_classes {
        if c.components
            .has(crate::components::BLOCK, block.first_state, mine)
        {
            if let Some(class) = c.components.names[mine as usize].strip_prefix("mineable/") {
                let tier = [
                    (abi::tool_tier::HAND, "HAND"),
                    (abi::tool_tier::WOOD, "WOOD"),
                    (abi::tool_tier::STONE, "STONE"),
                    (abi::tool_tier::IRON, "IRON"),
                    (abi::tool_tier::DIAMOND, "DIAMOND"),
                ]
                .into_iter()
                .find(|(value, _)| *value == level)
                .map_or_else(|| level.to_string(), |(_, name)| format!("abi::tool_tier::{name}"));
                t.calls
                    .push(Call::new("mined_by", &[&code::quote(class), &tier]));
            }
        }
    }
    let vector = |v: [f32; 3]| format!("[{}]", v.map(code::num).join(", "));
    if let Some(cube) = ModelBox::full_cube(&state.boxes) {
        t.calls
            .push(Call::new("texture", &[&code::quote(&asset_arg(&c.textures[cube.textures[0] as usize], "block"))]));
        for (face, method) in ["west", "east", "bottom", "top", "north", "south"]
            .into_iter()
            .enumerate()
        {
            if cube.textures[face] != cube.textures[0] {
                t.calls
                    .push(Call::new(method, &[&code::quote(&asset_arg(&c.textures[cube.textures[face] as usize], "block"))]));
            }
        }
        if cube.turn != [0; 6] {
            t.calls
                .push(Call::new("face_turns", &[&format!("{:?}", cube.turn)]));
        }
        return t;
    }
    for bx in &state.boxes {
        let faces: Vec<_> = (0..6)
            .map(|f| {
                let uv = bx.uv[f].map(code::num).join(", ");
                format!("({}, [{uv}])", code::quote(&asset_arg(&c.textures[bx.textures[f] as usize], "block")))
            })
            .collect();
        t.calls.push(Call::new(
            "model_box_faces",
            &[&vector(bx.min.map(|v| v * 16.0)), &vector(bx.max.map(|v| v * 16.0)), &format!("[{}]", faces.join(", "))],
        ));
        if bx.turn != [0; 6] {
            t.calls
                .push(Call::new("turned", &[&format!("{:?}", bx.turn)]));
        }
        if bx.flat {
            t.calls.push(Call::new("unshaded", &[]));
        }
        if bx.tilt.degrees != 0.0 {
            if let Some(axis) = abi::FACE_DIRS
                .iter()
                .position(|d| d.map(|v| v as f32) == bx.tilt.axis)
            {
                t.calls.push(Call::new(
                    "tilted",
                    &[
                        &vector(bx.tilt.origin.map(|v| v * 16.0)),
                        &format!("Dir::{}{}", abi::FACE_NAMES[axis][..1].to_uppercase(), &abi::FACE_NAMES[axis][1..]),
                        &code::num(bx.tilt.degrees),
                    ],
                ));
            }
        }
    }
    t
}

/// Examples are ordinary editable calls, including the behavior's own parameters.
pub fn example(c: &Content, kind: What, index: usize) -> Thing {
    let mut thing = appearance(c, kind, index);
    let (table, entry) =
        if kind == What::Block { (crate::components::BLOCK, c.blocks[index].first_state) } else { (crate::components::ITEM, index as u32) };
    for method in code::methods(kind).filter(|m| m.params.iter().any(|p| !p.component.is_empty())) {
        let mut args = Vec::new();
        for param in &method.params {
            let value = if param.component.is_empty() {
                Some(code::default_arg(param))
            } else {
                c.components
                    .id(&param.component)
                    .and_then(|id| c.components.bytes(table, entry, id))
                    .and_then(|bytes| code::literal(&param.ty, &param.schema, bytes))
            };
            if let Some(value) = value {
                args.push(value);
            } else {
                break;
            }
        }
        if args.len() == method.params.len() {
            thing.calls.push(Call { method: method.name.clone(), args });
        }
    }
    // Scalar SDK setters already carry their component's vocabulary name.
    for method in code::methods(kind).filter(|m| m.params.len() == 1 && m.params[0].component.is_empty()) {
        if thing.calls.iter().any(|call| call.method == method.name) {
            continue;
        }
        let Some(id) = c.components.id(&method.name) else { continue };
        let Some(bytes) = c.components.bytes(table, entry, id) else { continue };
        let schema = &c.components.schemas[id as usize];
        if let Some(value) = code::literal(&method.params[0].ty, schema, bytes) {
            thing.calls.push(Call::new(&method.name, &[&value]));
        }
    }
    if kind == What::Item
        && let Some(file) = &c.items[index].model_file
    {
        thing
            .calls
            .push(Call::new("model_file", &[&code::quote(file)]));
    }
    if kind == What::Block {
        use abi::signal_kind as sk;
        let state = &c.states[entry as usize];
        let turn = state.turn;
        let inputs = (0..6)
            .filter(|&side| state.signal.inputs & (1 << turn.face(side)) != 0)
            .fold(0, |bits, side| bits | 1 << side);
        let outputs = (0..6)
            .filter(|&side| state.signal.gives & (1 << turn.face(side)) != 0)
            .fold(0, |bits, side| bits | 1 << side);
        if state.signal.kind == sk::GATE {
            let logic = c
                .components
                .id("signal_logic")
                .and_then(|id| c.components.text(table, entry, id));
            if let Some(logic) = logic.and_then(|logic| {
                code::LOGIC_NAMES
                    .iter()
                    .find(|name| name.eq_ignore_ascii_case(logic))
            }) {
                let gate = code::GateArgs { inputs, outputs, delay: state.signal.delay as u32 / 2 };
                let name = format!("GateLogic::{logic}");
                thing
                    .calls
                    .push(Call::new("logic_gate", &[&gate.code(), &name]));
            }
        } else if let Some(method) = match state.signal.kind {
            sk::SWITCH => Some(if c.blocks[index].props.iter().any(|p| p.name == "power") { "signal_switch" } else { "signal_constant" }),
            sk::TORCH => Some("signal_torch"),
            sk::REPEATER => Some("signal_repeater"),
            sk::COMPARATOR => Some("signal_comparator"),
            sk::OBSERVER => Some("signal_observer"),
            sk::SINK => Some("signal_sink"),
            _ => None,
        } {
            thing.calls.push(Call::new(method, &[]));
        }
    }
    thing
}
