use glam::{DQuat, DVec3};
use rustc_hash::FxHashMap;

use super::dim::{Dim, DimEvent};
use super::entity::Motion;
use super::space::CubeCells;
use super::structure::{TURN_PROBE_TRAVEL, structure_pose_clear};
use crate::content::Content;
use crate::physics::Transform;

pub(super) struct Grab {
    player: u64,
    point: DVec3,
    depth: f64,
    rotation: DQuat,
}

impl Dim {
    fn grab_eye(&self, c: &Content, player: u64) -> Option<(DVec3, DVec3)> {
        let e = self
            .entities
            .get(player)
            .filter(|e| e.player && e.alive() && !e.removed && e.vehicle.is_none())?;
        let eye = e.pos + DVec3::Y * e.eye(c);
        let look = crate::physics::look_dir(e.yaw, e.pitch);
        (c.body_grab_tool(self.entities.held(player), e.mode) && eye.is_finite() && look.is_finite()).then_some((eye, look))
    }

    fn grab_aboard(&self, player: u64, root: u64) -> bool {
        self.entities
            .get(player)
            .and_then(|e| e.frame)
            .and_then(|id| self.entities.get(id))
            .is_some_and(|e| self.entities.follows(e).id == root)
    }

    fn grabbed_by(&self, player: u64) -> Option<u64> {
        self.bodies.keys().copied().find(|&id| {
            self.entities
                .get(id)
                .and_then(|e| e.grab.as_deref())
                .is_some_and(|grab| grab.player == player)
        })
    }

    pub(super) fn release_grab(&mut self, player: u64) -> bool {
        let Some(id) = self.grabbed_by(player) else { return false };
        self.entities.get_mut(id).unwrap().grab = None;
        true
    }

    pub(super) fn toggle_grab(&mut self, c: &'static Content, player: u64) -> Option<u64> {
        if self.release_grab(player) {
            return None;
        }
        let (eye, look) = self.grab_eye(c, player)?;
        let reach = self.block_reach(0, c, player);
        let hit = self.ray(0, c, eye, look, reach, 0)?;
        let root = self.entities.follows(self.entities.get(hit.frame)?);
        let space = self.bodies.get(&root.id)?;
        if root.removed || root.motion == Motion::Parked || root.grab.is_some() || space.signals.on_the_way {
            return None;
        }
        if self.grab_aboard(player, root.id) {
            return None;
        }
        let id = root.id;
        let place = root.body_place();
        let e = self.entities.get_mut(id)?;
        let detached = e.mount.take().is_some() || e.floater_driven;
        e.frame = None;
        e.motion = Motion::Free;
        e.vel = DVec3::ZERO;
        if let Some(ship) = e.ship.as_mut() {
            ship.spin = DVec3::ZERO;
        }
        e.grab = Some(Box::new(Grab { player, point: place.to_local(eye + look * hit.dist), depth: hit.dist, rotation: place.rotation() }));
        if detached {
            self.enable_dynamics(id, None);
        }
        log::debug!(target: "physics", "grab {id} by {player}, depth {:.3}", hit.dist);
        Some(id)
    }

    pub(super) fn turn_grab(&mut self, c: &Content, player: u64, [x, y]: [f32; 2]) {
        let Some((_, look)) = self.grab_eye(c, player) else { return };
        let Some(id) = self.grabbed_by(player) else { return };
        if !x.is_finite() || !y.is_finite() {
            return;
        }
        let right = look.cross(DVec3::Y).normalize_or_zero();
        let up = right.cross(look);
        let drag = right * f64::from(y.clamp(-1.0, 1.0)) + up * f64::from(x.clamp(-1.0, 1.0));
        let grab = self.entities.get_mut(id).unwrap().grab.as_mut().unwrap();
        grab.rotation = (DQuat::from_scaled_axis(drag) * grab.rotation).normalize();
    }

    pub(super) fn advance_grabs(&mut self, c: &Content, children: &FxHashMap<u64, Vec<u64>>) {
        let active: Vec<_> = self
            .bodies
            .keys()
            .copied()
            .filter(|&id| self.entities.get(id).is_some_and(|e| e.grab.is_some()))
            .collect();
        for id in active {
            let e = self.entities.get(id).unwrap();
            let grab = e.grab.as_ref().unwrap();
            let player = grab.player;
            let old = e.body_place();
            let start = old.to_world(grab.point);
            let aim = self
                .grab_eye(c, player)
                .filter(|(eye, _)| eye.distance_squared(start) <= 32.0 * 32.0 && !self.grab_aboard(player, id));
            let Some((eye, look)) = aim.filter(|_| !e.removed && e.motion != Motion::Parked) else {
                self.entities.get_mut(id).unwrap().grab = None;
                self.events.push(DimEvent::GrabReleased(player));
                continue;
            };
            let parts = self.mounted_parts(id, old, children);
            let goal = eye + look * grab.depth;
            let angle = (grab.rotation * old.rotation().conjugate())
                .to_scaled_axis()
                .length();
            let radius = parts
                .iter()
                .filter_map(|p| {
                    let b = p.relative.box_to_world(p.space.colliders(c).bounds?);
                    Some(
                        (b.min - grab.point)
                            .abs()
                            .max((b.max - grab.point).abs())
                            .length(),
                    )
                })
                .fold(0.0, f64::max);
            let travel = start.distance(goal) + angle * radius;
            if travel <= 1e-6 {
                continue;
            }
            let pose = |t| {
                let rotation = old.rotation().slerp(grab.rotation, t);
                let pos = start.lerp(goal, t) - rotation * (grab.point - DVec3::splat(0.5)) - DVec3::Y * 0.5;
                Transform::rotated(pos, rotation)
            };
            let steps = (travel / TURN_PROBE_TRAVEL).ceil().max(1.0);
            let extent = (12.0 / steps).min(1.0);
            let world = CubeCells(&self.space.cubes);
            let peers = self.mount_obstacles(c, old, &parts, (goal - start) * extent, angle * radius * extent);
            let fits = |t| structure_pose_clear(c, &world, &parts, &peers, pose(t));
            let mut clear = 0.0;
            for i in 1..=12 {
                let next = (f64::from(i) / steps).min(1.0);
                if !fits(next) {
                    let mut blocked = next;
                    for _ in 0..5 {
                        let mid = (clear + blocked) * 0.5;
                        if fits(mid) {
                            clear = mid;
                        } else {
                            blocked = mid;
                        }
                    }
                    break;
                }
                clear = next;
                if clear == 1.0 {
                    break;
                }
            }
            log::trace!(target: "physics", "grab {id} from {start:?} toward {goal:?}, angle {angle:.3}, clear {clear:.4}");
            if clear == 0.0 {
                continue;
            }
            let target = pose(clear);
            let mounts: Vec<_> = parts.iter().skip(1).map(|p| p.id).collect();
            let e = self.entities.get_mut(id).unwrap();
            e.set_body_pose(target);
            for child in mounts {
                self.entities.update_mount_pose(child);
            }
        }
    }
}
