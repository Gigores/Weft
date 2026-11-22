//! What hits land on: an entity's hitbox parts (`abi::Hitboxes`) where they are at a moment.
//! One function of what the server and every client know alike (its place, body and look, its
//! stride by the tick, its pose), so a hit is found the same everywhere without sending bones.
//! Bound boxes use the registered model's base pose, sampled by the same player as rendering.

use glam::{DAffine3, DQuat, DVec3, Mat4};
use weft_abi as abi;

use crate::model::{self, Posture};
use crate::physics::{self, Aabb};
use crate::world::Pose;

/// Where an entity is and how it holds itself, as hits see it.
#[derive(Clone, Copy)]
pub struct Body {
    pub pos: DVec3,
    pub body_yaw: f32,
    /// The look: yaw and pitch.
    pub yaw: f32,
    pub pitch: f32,
    pub posture: Posture,
    pub size: abi::EntitySize,
    /// Its own height for its kind's: one made bigger has bigger parts.
    pub scale: f64,
}

pub struct Rig {
    parts: Vec<abi::HitboxPart>,
    bones: Vec<crate::registry::Part>,
    bindings: Vec<Option<(usize, Mat4)>>,
    anims: Vec<model::Bound>,
    actions: model::Actions,
}

impl Rig {
    pub fn new(boxes: abi::Hitboxes) -> Self {
        Self { parts: boxes.parts, bones: Vec::new(), bindings: Vec::new(), anims: Vec::new(), actions: model::Actions::default() }
    }

    fn bind(
        &mut self,
        name: &str,
        mut bones: Vec<crate::registry::Part>,
        own: &[std::sync::Arc<model::Animation>],
        stock: &[std::sync::Arc<model::Animation>],
    ) {
        (self.anims, self.actions) = model::bind_actions(&bones, own, stock);
        let mut rest = Vec::new();
        model::pose(&bones, Mat4::IDENTITY, &model::Motion { pitch: 0.0, head_yaw: 0.0, aim_root: None, clips: &[] }, &mut rest);
        self.bindings = self
            .parts
            .iter()
            .map(|p| {
                if p.bone.is_empty() {
                    return None;
                }
                let Some(i) = model::bone_named(&bones, &p.bone) else {
                    log::warn!("{name}: hitbox {} names missing bone {}", p.name, p.bone);
                    return None;
                };
                Some((i, rest[i].inverse()))
            })
            .collect();
        for bone in &mut bones {
            bone.cuboids = Vec::new();
            bone.mesh = Vec::new();
        }
        self.bones = bones;
    }

    fn pose(&self, b: &Body, out: &mut Vec<Mat4>) {
        let mut clips = self
            .anims
            .first()
            .map(|bound| [model::Playing { bound, t: 0.0, w: 0.0, mirror: false, aim: false }; model::MAX_CLIPS]);
        let mut count = 0;
        if let Some(clips) = &mut clips {
            b.posture.play(|action, how, _| {
                let Some(ids) = self.actions.get(action) else { return false };
                for &i in ids {
                    if count == clips.len() {
                        break;
                    }
                    let bound = &self.anims[i as usize];
                    let (t, w) = bound.sample(how);
                    if w > 0.0 {
                        clips[count] = model::Playing { bound, t, w, mirror: false, aim: false };
                        count += 1;
                    }
                }
                true
            });
        }
        let sleeping = b.posture.pose.def().tilt == abi::pose_tilt::ON_BACK;
        let lying = model::body_tilt(b.posture.pose, b.pitch).is_some();
        let (pitch, head_yaw) = model::head_look(sleeping, lying, b.pitch, b.yaw - b.body_yaw);
        let clips = clips.as_ref().map_or(&[][..], |a| &a[..count]);
        model::pose(&self.bones, Mat4::IDENTITY, &model::Motion { pitch, head_yaw, aim_root: None, clips }, out);
    }
}

pub fn bind_rigs(
    core: &std::path::Path,
    mods: &[crate::content::ModMeta],
    components: &crate::components::Components,
    kinds: &mut [crate::registry::EntityKind],
    errors: &mut Vec<String>,
) {
    use std::sync::Arc;
    use weft_abi::comp;
    let read = |name: &str, folder: &str, ext: &str| {
        let (ns, file) = name.split_once(':').unwrap_or(("core", name));
        let root = if ns == "core" { core } else { &mods.iter().find(|m| m.id == ns)?.assets };
        std::fs::read_to_string(root.join(folder).join(format!("{file}.{ext}"))).ok()
    };
    let clips = |name: &str| match read(name, "animations", "animation.json") {
        Some(src) => model::load_animations(&src).map(|a| a.into_iter().map(Arc::new).collect::<Vec<_>>()),
        None => Ok(Vec::new()),
    };
    let stocked: rustc_hash::FxHashMap<_, _> = model::STOCK_FILES
        .into_iter()
        .map(|name| {
            (
                name,
                clips(name).unwrap_or_else(|error| {
                    errors.push(format!("animations {name}: {error}"));
                    Vec::new()
                }),
            )
        })
        .collect();
    for (id, kind) in kinds.iter_mut().enumerate() {
        let Some(rig) = kind
            .hitboxes
            .as_mut()
            .filter(|r| r.parts.iter().any(|p| !p.bone.is_empty()))
        else {
            continue;
        };
        let name =
            |key| model::asset_name(components, crate::components::ENTITY, id as u32, key, &kind.name).unwrap_or_else(|| kind.name.clone());
        let (bones, _) = model::entity_geometry(read, &name(comp::MODEL), &kind.parts);
        let own_name = name(comp::ANIMATIONS);
        let own = clips(&own_name).unwrap_or_else(|error| {
            errors.push(format!("animations {own_name}: {error}"));
            Vec::new()
        });
        let stock = &stocked[model::stock_file(&bones)];
        rig.bind(&kind.name, bones, &own, stock);
    }
}

/// One part placed: its id (`abi::hitbox_part`), its box in its own pixels, and from those to
/// the world.
#[derive(Clone, Copy)]
pub struct Placed {
    pub id: u32,
    pub min: DVec3,
    pub max: DVec3,
    pub to_world: DAffine3,
}

#[derive(Default)]
pub struct Scratch {
    pub parts: Vec<Placed>,
    bones: Vec<Mat4>,
}

/// The stride's beat at `walk`: the stock `walk` clip's, a full step each 1.417 / 0.6 of it.
fn beat(walk: f32) -> f64 {
    walk as f64 * 0.6 / 1.41726 * std::f64::consts::TAU
}

/// Where each part of `h` is on `b`. Standing it fits the box's height; crouching squeezes it
/// down to the lower box; lying, it tips over as the model does.
pub fn place(h: &Rig, b: &Body, scratch: &mut Scratch) {
    scratch.parts.clear();
    let pose = b.posture.pose;
    let stand = physics::posed(b.size, Pose::STAND).height;
    let now = physics::posed(b.size, pose).height;
    let def = pose.def();
    let tilt = model::body_tilt(pose, b.pitch);
    let per_px = b.scale / 16.0;
    let squeeze = if tilt.is_some() { 1.0 } else { now / stand.max(1e-6) };
    // On its back it lies the way it looks, as the model does.
    let yaw = if def.tilt == abi::pose_tilt::ON_BACK { b.yaw } else { b.body_yaw };
    let lying = tilt.map_or(DAffine3::IDENTITY, |a| {
        DAffine3::from_translation(DVec3::Y * now / 2.0)
            * DAffine3::from_rotation_x(f64::from(a))
            * DAffine3::from_translation(-DVec3::Y * stand / 2.0)
    });
    let root = DAffine3::from_translation(b.pos) * DAffine3::from_quat(DQuat::from_rotation_y(yaw as f64));
    let body = root * lying * DAffine3::from_scale(DVec3::new(per_px, per_px * squeeze, per_px));
    let animated = root
        * tilt.map_or(DAffine3::IDENTITY, |a| DAffine3::from_mat4(model::tilted_body(a).as_dmat4()))
        * DAffine3::from_scale(DVec3::splat(per_px));
    if !h.bones.is_empty() {
        h.pose(b, &mut scratch.bones);
    }
    for (i, p) in h.parts.iter().enumerate() {
        let to_world = if let Some((bone, rest)) = h.bindings.get(i).copied().flatten() {
            animated * DAffine3::from_mat4((scratch.bones[bone] * rest).as_dmat4())
        } else {
            let turn = match p.follows {
                abi::hitbox_follows::LOOK => {
                    DQuat::from_rotation_y(physics::angle_diff(b.yaw, yaw) as f64) * DQuat::from_rotation_x(b.pitch as f64)
                }
                abi::hitbox_follows::STRIDE => {
                    DQuat::from_rotation_x((beat(b.posture.walk) + p.phase as f64).cos() * p.swing as f64 * b.posture.stride as f64)
                }
                _ => DQuat::IDENTITY,
            };
            let pivot = DVec3::from_array(p.pivot.map(f64::from));
            body * DAffine3::from_translation(pivot) * DAffine3::from_quat(turn) * DAffine3::from_translation(-pivot)
        };
        scratch.parts.push(Placed {
            id: abi::hitbox_part_id(&p.name),
            min: DVec3::from_array(p.min.map(f64::from)),
            max: DVec3::from_array(p.max.map(f64::from)),
            to_world,
        });
    }
}

/// The part a ray from `o` along `d` (a unit vector) enters first within `max`: its id, how far,
/// the face it went in by in the part's own frame (-Z its front), and where on the part from its
/// low corner, in blocks.
pub fn ray(parts: &[Placed], o: DVec3, d: DVec3, max: f64) -> Option<(u32, f64, u8, DVec3)> {
    let mut best: Option<(u32, f64, u8, DVec3)> = None;
    for p in parts {
        // In the part's pixels the ray goes by the same steps: its length stays the world's.
        let back = p.to_world.inverse();
        let (lo, ld) = (back.transform_point3(o), back.transform_vector3(d));
        let Some((t, face)) = physics::ray_aabb(lo, ld, &Aabb { min: p.min, max: p.max }, max) else { continue };
        if best.is_none_or(|b| t < b.1) {
            // From its low corner in blocks: the part's pixels times their size.
            let px = p.to_world.matrix3.x_axis.length();
            best = Some((p.id, t, face, (lo + ld * t - p.min) * px));
        }
    }
    best
}

/// What a ray from `o` along `d` (a unit vector) hits of an entity within `max`: one of its
/// parts (`h`), else its box (part 0). The part's id, how far, the face it went in by and where on
/// it from its low corner. Its box, grown by how far parts may reach (arms held out, a stride, a
/// head turned), is looked at first.
pub fn ray_entity(h: Option<&Rig>, b: &Body, o: DVec3, d: DVec3, max: f64, scratch: &mut Scratch) -> Option<(u32, f64, u8, DVec3)> {
    let dims = physics::posed(b.size, b.posture.pose);
    let bx = Aabb::around(b.pos, dims.half_width, dims.height);
    let Some(h) = h else {
        let (t, face) = physics::ray_aabb(o, d, &bx, max)?;
        return Some((0, t, face, o + d * t - bx.min));
    };
    let reach = DVec3::splat(b.size.width.max(b.size.height) as f64 * 0.5 + 0.25);
    physics::ray_aabb(o, d, &Aabb { min: bx.min - reach, max: bx.max + reach }, max)?;
    place(h, b, scratch);
    ray(&scratch.parts, o, d, max)
}
