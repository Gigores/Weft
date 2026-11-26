use glam::DVec3;
use weft_abi::{BrainVision, gizmo_kind, permission};

use super::{DimSet, Player, entity::Entity, path};
use crate::client::gizmo::{MOB_BRAIN, MOB_PATH, MOB_VISION};
use crate::content::Content;
use crate::net::{GizmoShape, GizmoVolume};
use crate::ui::text::{Arg, Msg};

const REFRESH: u64 = 2;
const LIFETIME: u32 = 6;
const RANGE: f64 = 24.0;
const MAX_MOBS: usize = 12;

pub(super) fn collect(c: &Content, p: &Player, dim: &DimSet, tick: u64, eye: DVec3, out: &mut Vec<GizmoShape>) {
    if tick % REFRESH != 0 {
        return;
    }
    let on = |group: &str| p.gizmos.iter().any(|g| g == group);
    let (brain, route, vision) = (on(MOB_BRAIN), on(MOB_PATH), on(MOB_VISION));
    if !(brain || route || vision)
        || dim
            .entity(p.entity)
            .is_none_or(|e| e.permission < permission::GAME_MASTER)
    {
        return;
    }

    let mut mobs: Vec<_> = p
        .known
        .iter()
        .filter_map(|&id| {
            let region = dim.entity_region_ref(id)?;
            let e = region.entities.get(id)?;
            let distance = e.pos.distance_squared(eye);
            let notice = if vision { e.brain.as_ref().map_or(0.0, |b| b.notice_range(c)) } else { 0.0 };
            let sight = if vision && distance >= RANGE * RANGE {
                e.brain
                    .as_ref()
                    .into_iter()
                    .flat_map(|b| b.vision(c))
                    .map(|v| v.range.abs())
                    .filter(|r| r.is_finite())
                    .fold(notice, f64::max)
            } else {
                notice
            };
            (distance < (RANGE + sight).powi(2) && !e.player && (e.brain.is_some() || e.nav.is_some()))
                .then_some((distance, e, region, notice))
        })
        .collect();
    mobs.sort_unstable_by(|a, b| a.0.total_cmp(&b.0).then(a.1.id.cmp(&b.1.id)));

    // Each group gets its own limit so nearby fish do not hide the sentry's eyesight.
    let (mut brains, mut routes, mut eyes) = (0, 0, 0);
    for (distance, e, region, notice) in mobs {
        if brain && distance < RANGE * RANGE && brains < MAX_MOBS && e.brain.is_some() {
            draw_brain(c, e, out);
            brains += 1;
        }
        if route
            && distance < RANGE * RANGE
            && routes < MAX_MOBS
            && let Some(nav) = &e.nav
        {
            draw_route(c, e, nav, nav.frame.map(|id| region.space_pose(id)), out);
            routes += 1;
        }
        if vision
            && eyes < MAX_MOBS
            && let Some(state) = &e.brain
        {
            if notice > 0.0 && distance < (RANGE + notice).powi(2) {
                volume(
                    out,
                    e.pos,
                    GizmoVolume {
                        yaw: 0.0,
                        pitch: 0.0,
                        horizontal: std::f64::consts::PI,
                        vertical: std::f64::consts::FRAC_PI_2,
                        near: 0.0,
                        far: notice,
                    },
                    0x7BADFFFF,
                );
                eyes += 1;
            }
            for profile in state.vision(c).take(MAX_MOBS - eyes) {
                draw_vision(c, e, profile, out);
                eyes += 1;
            }
        }
    }
}

fn draw_brain(c: &Content, e: &Entity, out: &mut Vec<GizmoShape>) {
    let brain = e.brain.as_ref().unwrap();
    let mut active = brain
        .actions(c)
        .map(|(name, offer)| format!("{name} [{:.1}]", offer.keep))
        .peekable();
    let text = if active.peek().is_none() {
        Msg::new("gizmo.mob_idle", Vec::new())
    } else {
        let actions = active.collect::<Vec<_>>().join("\n");
        Msg::new("gizmo.value", vec![Arg::Text(actions)])
    };
    let at = e.pos + DVec3::Y * (e.height(c) + 0.3);
    out.push(GizmoShape::text(MOB_BRAIN, at, 0.9, 0xFFFFFFFF, LIFETIME, text));
}

fn draw_route(c: &Content, e: &Entity, nav: &path::Nav, place: Option<crate::physics::Transform>, out: &mut Vec<GizmoShape>) {
    let dims = e.dims(c);
    let to_world = |p| place.map_or(p, |t| t.to_world(p));
    let lift = DVec3::Y * 0.09;
    let mut last = e.pos + lift;
    let nodes = nav.path.get(nav.next..).unwrap_or_default();
    let color = if nav.path.is_empty() && nav.end.is_some() { 0x7AFF8EFF } else { 0x55DCFFFF };
    for &node in nodes.iter().take(32) {
        let point = if nav.swimming() { path::swim_point(node, dims) } else { path::walk_point(node, dims) };
        let point = to_world(point) + lift;
        line(out, MOB_PATH, last, point, color);
        last = point;
    }
    if nodes.len() <= 32
        && let Some(end) = nav.end
    {
        line(out, MOB_PATH, last, to_world(end) + lift, color);
    }
    let target = to_world(nav.target) + lift;
    out.push(GizmoShape::shape(MOB_PATH, gizmo_kind::BOX as u8, target - 0.12, target + 0.12, 0xFFBD54FF, LIFETIME));
}

fn draw_vision(c: &Content, e: &Entity, v: &BrainVision, out: &mut Vec<GizmoShape>) {
    if !v.range.is_finite()
        || v.range == 0.0
        || !v.blind_near.is_finite()
        || !v.horizontal.is_finite()
        || !v.vertical.is_finite()
        || !v.yaw_offset.is_finite()
        || !v.pitch_offset.is_finite()
        || v.horizontal < 0.0
        || v.vertical < 0.0
        || v.blind_near.abs() >= v.range.abs()
    {
        return;
    }
    let origin = e.pos + DVec3::Y * e.eye(c);
    let yaw = e.yaw + v.yaw_offset.to_radians();
    let pitch = e.pitch + v.pitch_offset.to_radians();
    volume(
        out,
        origin,
        GizmoVolume {
            yaw,
            pitch,
            horizontal: v.horizontal.min(360.0).to_radians() * 0.5,
            vertical: v.vertical.min(180.0).to_radians() * 0.5,
            near: v.blind_near.abs(),
            far: v.range.abs(),
        },
        0x6FFF9EFF,
    );
}

fn volume(out: &mut Vec<GizmoShape>, origin: DVec3, bounds: GizmoVolume, color: u32) {
    let mut shape = GizmoShape::shape(MOB_VISION, GizmoShape::VOLUME, origin, origin, color, LIFETIME);
    shape.volume = Some(bounds);
    out.push(shape);
}

fn line(out: &mut Vec<GizmoShape>, group: &str, a: DVec3, b: DVec3, color: u32) {
    if a.is_finite() && b.is_finite() && a != b {
        out.push(GizmoShape::shape(group, gizmo_kind::LINE as u8, a, b, color, LIFETIME));
    }
}
