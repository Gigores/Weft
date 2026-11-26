//! Debug drawing in the world, switched per group with `/gizmo <group>` (F3+G cube borders, F3+B
//! hitboxes like vanilla). Client groups are built here every frame; server groups and mods'
//! groups arrive as shapes, only for groups this player turned on.

use std::time::Instant;

use glam::{DVec3, IVec3};

use super::Client;
use crate::content::content;
use crate::net::{GizmoShape, GizmoVolume};

pub const MOB_BRAIN: &str = "mob_brain";
pub const MOB_PATH: &str = "mob_path";
pub const MOB_VISION: &str = "mob_vision";
pub const BUILTIN: &[&str] = &["cubes", "hitboxes", "collision", "light", "regions", "visibility", MOB_BRAIN, MOB_PATH, MOB_VISION];

/// A line between two points, in the color `0xRRGGBBAA`.
pub type Line = (DVec3, DVec3, u32);
pub enum Fill {
    Box(DVec3, DVec3, u32),
    Triangle([DVec3; 3], u32),
}
/// Where, what, its color and its size (1: menu text).
pub type Label = (DVec3, crate::ui::text::Msg, u32, f32);

#[derive(Default)]
pub struct Gizmos {
    /// Groups turned on, sorted.
    pub enabled: Vec<String>,
    shapes: Vec<(GizmoShape, Instant)>,
}

impl Gizmos {
    pub fn on(&self, group: &str) -> bool {
        group.is_empty() || group == GizmoShape::ASSEMBLY || self.enabled.iter().any(|g| g == group)
    }

    /// Switches a group; returns whether it is on now.
    pub fn toggle(&mut self, group: &str) -> bool {
        if let Some(i) = self.enabled.iter().position(|g| g == group) {
            self.enabled.remove(i);
            self.shapes.retain(|s| s.0.group != group);
            false
        } else {
            self.enabled.push(group.to_string());
            self.enabled.sort();
            true
        }
    }

    pub fn receive(&mut self, shapes: Vec<GizmoShape>) {
        let now = Instant::now();
        // A group sent again replaces what it showed before.
        let mut groups: Vec<&str> = shapes.iter().map(|s| s.group.as_str()).collect();
        groups.sort_unstable();
        groups.dedup();
        self.shapes
            .retain(|(s, _)| groups.binary_search(&s.group.as_str()).is_err());
        for s in shapes {
            if self.on(&s.group) {
                let until = now + std::time::Duration::from_secs_f64(s.ticks as f64 * crate::world::TICK);
                self.shapes.push((s, until));
            }
        }
    }

    /// Lines, filled boxes and labels to draw this frame.
    pub fn collect(&mut self, client: &Client) -> (Vec<Line>, Vec<Fill>, Vec<Label>, Option<u32>) {
        let mut lines = Vec::new();
        let mut fills = Vec::new();
        let mut labels = Vec::new();
        let now = Instant::now();
        self.shapes.retain(|s| s.1 > now);
        let camera = self.on(MOB_VISION).then(|| client.render_camera().pos);
        let mut tint = [0u32; 3];
        let mut inside = 0;
        for (s, _) in &self.shapes {
            if let Some((eye, v)) = camera.zip(s.volume) {
                if draw_volume(s.a, v, s.color, eye, &mut lines, &mut fills) {
                    for (ch, shift) in tint.iter_mut().zip([24, 16, 8]) {
                        *ch += (s.color >> shift) & 255;
                    }
                    inside += 1;
                }
            } else {
                shape(s, &mut lines, &mut fills, &mut labels);
            }
        }
        let tint = (inside > 0).then(|| ((tint[0] / inside) << 24) | ((tint[1] / inside) << 16) | ((tint[2] / inside) << 8) | 51);
        if self.enabled.is_empty() {
            return (lines, fills, labels, tint);
        }
        let c = content();
        let me = client.player.pos;
        let feet = me.floor().as_ivec3();
        if self.on("cubes") {
            let cube: IVec3 = feet >> 4;
            for dz in -1..=1 {
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        let o = ((cube + IVec3::new(dx, dy, dz)) * 16).as_dvec3();
                        let color = if (dx, dy, dz) == (0, 0, 0) { 0xFFE040FF } else { 0x4070FFFF };
                        cuboid(&mut lines, o, o + DVec3::splat(16.0), color);
                    }
                }
            }
        }
        if self.on("hitboxes") {
            // Its own only where its body is seen. White the box blocks and pushing go by, pink
            // the parts of its hitbox, where hits land.
            let own = client
                .camera_has(crate::tag_bits::camera::BODY)
                .then_some((&client.me, client.player.dims()));
            let near = client
                .entities
                .values()
                .filter(|e| (e.pos - me).length_squared() < 64.0 * 64.0)
                .map(|e| (e, e.dims()));
            let mut parts = crate::hitbox::Scratch::default();
            for (e, dims) in near.chain(own) {
                let crate::physics::Dims { half_width: hw, height: h, eye } = dims;
                cuboid(&mut lines, e.pos - DVec3::new(hw, 0.0, hw), e.pos + DVec3::new(hw, h, hw), 0xFFFFFFFF);
                if let Some(hb) = c.game.entities[e.kind as usize]
                    .hitboxes
                    .as_ref()
                    .filter(|_| e.is_body())
                {
                    crate::hitbox::place(hb, &e.hit_body(), &mut parts);
                    for p in &parts.parts {
                        placed_cuboid(&mut lines, p, 0xFF60D0FF);
                    }
                }
                let eye = e.pos + DVec3::Y * eye;
                let (sy, cy) = (e.yaw as f64).sin_cos();
                lines.push((eye, eye + DVec3::new(-sy, 0.0, -cy) * 1.5, 0x4060FFFF));
            }
        }
        if self.on("collision") {
            let mut boxes = Vec::new();
            for dy in -2..=3 {
                for dz in -3..=3 {
                    for dx in -3..=3 {
                        let p = feet + IVec3::new(dx, dy, dz);
                        let Some(state) = client.world.block_or_air(p) else { continue };
                        boxes.clear();
                        crate::physics::boxes(c, state, &mut boxes);
                        let o = p.as_dvec3();
                        for b in &boxes {
                            cuboid(&mut lines, o + DVec3::new(b[0], b[1], b[2]), o + DVec3::new(b[3], b[4], b[5]), 0x40FF60FF);
                        }
                    }
                }
            }
        }
        if self.on("light") {
            // Colored by the hostile spawn rules of this dimension, as the server checks them
            // (light = sky light times daylight, or block light, whichever is more): red where
            // monsters may spawn now, yellow where they may at night, green where never.
            let dim = client.world.dim;
            let hostile = |s: &&crate::registry::SpawnRule| {
                c.game.categories[s.category as usize].name == "core:hostile" && (s.dims.is_empty() || s.dims.contains(&dim))
            };
            let rules: Vec<Option<u32>> = c
                .game
                .spawns
                .iter()
                .filter(hostile)
                .map(|s| s.when)
                .collect();
            let day = client.daylight();
            let spawns = |p: IVec3, below: u32, daylight: f32| {
                let ask = crate::conditions::Ask {
                    pos: p.as_dvec3() + DVec3::splat(0.5),
                    state: below,
                    daylight: Some(daylight),
                    ..Default::default()
                };
                rules
                    .iter()
                    .any(|w| w.is_none_or(|w| crate::conditions::on_client(Some(client), w, &ask)))
            };
            for dy in -4..=4 {
                for dz in -6..=6 {
                    for dx in -6..=6 {
                        let p = feet + IVec3::new(dx, dy, dz);
                        let (Some(below), Some(here)) = (client.world.block_or_air(p - IVec3::Y), client.world.block_or_air(p)) else {
                            continue;
                        };
                        if !c.solid(below) || c.solid(here) {
                            continue;
                        }
                        let block = weft_abi::light::block(client.world.light(p)).min(15);
                        let color = if spawns(p, below, day) {
                            0xFF5040FF
                        } else if spawns(p, below, 0.0) {
                            0xFFD040FF
                        } else {
                            0x60FF60FF
                        };
                        labels.push((
                            p.as_dvec3() + DVec3::new(0.5, 0.05, 0.5),
                            crate::ui::text::Msg::new("gizmo.value", vec![crate::ui::text::Arg::Int(block as i64)]),
                            color,
                            1.0,
                        ));
                    }
                }
            }
        }
        (lines, fills, labels, tint)
    }
}

/// What a server's or a mod's shape draws.
pub fn shape(s: &GizmoShape, lines: &mut Vec<Line>, fills: &mut Vec<Fill>, labels: &mut Vec<Label>) {
    match s.kind as u32 {
        weft_abi::gizmo_kind::LINE => lines.push((s.a, s.b, s.color)),
        weft_abi::gizmo_kind::BOX => cuboid(lines, s.a.min(s.b), s.a.max(s.b), s.color),
        weft_abi::gizmo_kind::FILL => fills.push(Fill::Box(s.a.min(s.b), s.a.max(s.b), s.color)),
        _ if s.kind == GizmoShape::TEXT => labels.push((s.a, s.text.clone(), s.color, if s.size > 0.0 { s.size } else { 1.0 })),
        _ => {}
    }
}

fn draw_volume(origin: DVec3, v: GizmoVolume, color: u32, eye: DVec3, lines: &mut Vec<Line>, fills: &mut Vec<Fill>) -> bool {
    use crate::physics::look_dir;
    use std::f64::consts::{FRAC_PI_2, PI, TAU};
    let forward = look_dir(v.yaw, v.pitch);
    let side = look_dir(v.yaw + std::f32::consts::FRAC_PI_2, 0.0);
    let up = look_dir(v.yaw, v.pitch + std::f32::consts::FRAC_PI_2);
    let delta = eye - origin;
    let distance = delta.length();
    let sphere = v.horizontal == PI && v.vertical == FRAC_PI_2 && v.near == 0.0;
    let (a, b) = (delta.dot(forward), delta.dot(side));
    let inside = distance >= v.near
        && distance <= v.far
        && (sphere || (b.atan2(a).abs() <= v.horizontal && delta.dot(up).atan2(a.hypot(b)).abs() <= v.vertical));
    let direction = |a: f64, b: f64| (forward * a.cos() + side * a.sin()) * b.cos() + up * b.sin();
    let point = |(a, b), radius| origin + direction(a, b) * radius;
    let mut surface = |[a, b, c, d]: [DVec3; 4], rgb: u32| {
        if !inside {
            let alpha = (rgb & !255u32) | 51;
            fills.push(Fill::Triangle([a, b, c], alpha));
            fills.push(Fill::Triangle([a, c, d], alpha));
        }
    };
    let (h, pitch) = (v.horizontal, v.vertical);
    if !inside {
        let (cols, rows) = if sphere { (24, 12) } else { (8, 6) };
        for i in 0..cols {
            for j in 0..rows {
                let az = |i| -h + (2.0 * h / f64::from(cols)) * f64::from(i);
                let el = |j| -pitch + (2.0 * pitch / f64::from(rows)) * f64::from(j);
                let corners = [(az(i), el(j)), (az(i + 1), el(j)), (az(i + 1), el(j + 1)), (az(i), el(j + 1))];
                surface(corners.map(|p| point(p, v.far)), color);
                if v.near > 0.0 {
                    let [a, b, c, d] = corners.map(|p| point(p, v.near));
                    surface([d, c, b, a], 0xFFBD54FF);
                }
            }
        }
    }
    if sphere {
        for (u, w) in [(DVec3::X, DVec3::Z), (DVec3::X, DVec3::Y), (DVec3::Y, DVec3::Z)] {
            let ring = |i| {
                let (s, c) = (TAU * f64::from(i) / 48.0).sin_cos();
                origin + (u * c + w * s) * v.far
            };
            for i in 0..48 {
                lines.push((ring(i), ring(i + 1), color));
            }
        }
    } else {
        let edges = [(-h, -pitch), (h, -pitch), (h, pitch), (-h, pitch)];
        for (i, &(a, b)) in edges.iter().enumerate() {
            let (next_a, next_b) = edges[(i + 1) % edges.len()];
            let mut last = (a, b);
            lines.push((point(last, v.near), point(last, v.far), color));
            let steps = if i % 2 == 0 { 8 } else { 6 };
            for step in 1..=steps {
                let t = f64::from(step) / f64::from(steps);
                let next = (a + (next_a - a) * t, b + (next_b - b) * t);
                let (a, b) = (point(last, v.far), point(next, v.far));
                let (c, d) = (point(next, v.near), point(last, v.near));
                lines.push((a, b, color));
                if v.near > 0.0 {
                    lines.push((c, d, 0xFFBD54FF));
                }
                surface([d, c, b, a], color);
                last = next;
            }
        }
    }
    inside
}

/// The twelve edges of an axis-aligned box.
/// A hitbox part where it is (`hitbox::place`).
fn placed_cuboid(out: &mut Vec<Line>, part: &crate::hitbox::Placed, color: u32) {
    let (a, b) = (part.min, part.max);
    let p = |x: bool, y: bool, z: bool| {
        part.to_world
            .transform_point3(DVec3::new(if x { b.x } else { a.x }, if y { b.y } else { a.y }, if z { b.z } else { a.z }))
    };
    for i in 0..4 {
        let (u, v) = (i & 1 != 0, i & 2 != 0);
        out.push((p(false, u, v), p(true, u, v), color));
        out.push((p(u, false, v), p(u, true, v), color));
        out.push((p(u, v, false), p(u, v, true), color));
    }
}

pub fn cuboid(out: &mut Vec<Line>, a: DVec3, b: DVec3, color: u32) {
    let p = |x: bool, y: bool, z: bool| DVec3::new(if x { b.x } else { a.x }, if y { b.y } else { a.y }, if z { b.z } else { a.z });
    for i in 0..4 {
        let (u, v) = (i & 1 != 0, i & 2 != 0);
        out.push((p(false, u, v), p(true, u, v), color));
        out.push((p(u, false, v), p(u, true, v), color));
        out.push((p(u, v, false), p(u, v, true), color));
    }
}
