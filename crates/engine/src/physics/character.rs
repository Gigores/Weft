use glam::DVec3;

use super::{Aabb, BoxInterval, Cylinder, RotatedBox, Solid, Transform, WALKABLE_Y, convex_contact};

const EPSILON: f64 = 1e-7;
const SKIN: f64 = 1e-5;
pub(super) const RECOVERY: f64 = 0.25;
const CONTACTS: usize = 6;

pub(super) struct Collider {
    pub bounds: Aabb,
    shape: Obstacle,
}

impl Collider {
    pub(super) fn new(solid: Solid, place: Transform) -> Self {
        let bounds = solid.world_bounds(place);
        let shape = match solid {
            Solid::Box(b) => Obstacle::Box(RotatedBox::new(b, place)),
            Solid::Cylinder(c) => Obstacle::Cylinder(c.placed(place)),
        };
        Self { bounds, shape }
    }
}

#[derive(Clone, Copy)]
enum Obstacle {
    Box(RotatedBox),
    Cylinder(Cylinder),
}

impl Obstacle {
    fn project(self, area: Aabb, delta: DVec3) -> Option<BoxInterval> {
        match self {
            Self::Box(b) => b.project::<true>(area, delta),
            Self::Cylinder(c) => {
                let hit = convex_contact(Solid::Box(area), Transform::default(), Solid::Cylinder(c), delta)?;
                Some(BoxInterval {
                    enter: if hit.depth > EPSILON { -1.0 } else { hit.fraction },
                    leave: f64::INFINITY,
                    entry_normal: -hit.normal,
                    overlap_normal: -hit.normal,
                    depth: hit.depth,
                })
            }
        }
    }
}

pub(super) struct Travel {
    pub area: Aabb,
    pub velocity: DVec3,
    pub hit: [bool; 3],
    pub floor: bool,
}

#[derive(Clone, Copy)]
struct Hit {
    fraction: f64,
    normal: DVec3,
}

pub(super) struct Sweep<'a> {
    pub aligned: &'a [Aabb],
    pub structures: &'a [Collider],
}

impl Sweep<'_> {
    fn near(&self, area: Aabb) -> impl Iterator<Item = Obstacle> + '_ {
        self.aligned
            .iter()
            .filter(move |b| b.intersects(&area))
            .copied()
            .map(|b| Obstacle::Box(RotatedBox::axis_aligned(b)))
            .chain(
                self.structures
                    .iter()
                    .filter(move |c| c.bounds.intersects(&area))
                    .map(|c| c.shape),
            )
    }

    fn first(&self, area: Aabb, delta: DVec3, overlaps: bool) -> Option<Hit> {
        let sweep = area.swept(delta);
        self.near(sweep)
            .filter_map(|shape| {
                let hit = shape.project(area, delta)?;
                if hit.leave <= EPSILON || hit.enter > 1.0 {
                    return None;
                }
                if hit.enter >= -EPSILON {
                    let normal = -hit.entry_normal;
                    return (delta.dot(normal) < -EPSILON).then_some(Hit { fraction: hit.enter.max(0.0), normal });
                }
                // Initial overlaps should block movement further into the solid, while
                // permitting lateral motion or a retreat.
                let normal = -hit.overlap_normal;
                (overlaps && hit.depth > EPSILON && delta.dot(normal) < -EPSILON).then_some(Hit { fraction: 0.0, normal })
            })
            .min_by(|a, b| {
                a.fraction
                    .total_cmp(&b.fraction)
                    .then_with(|| b.normal.y.total_cmp(&a.normal.y))
                    .then_with(|| a.normal.x.total_cmp(&b.normal.x))
                    .then_with(|| a.normal.z.total_cmp(&b.normal.z))
            })
    }

    pub fn recover(&self, mut area: Aabb) -> Aabb {
        let mut budget = RECOVERY;
        for _ in 0..CONTACTS {
            let hit = self
                .near(area)
                .filter_map(|shape| shape.project(area, DVec3::ZERO))
                .max_by(|a, b| {
                    a.depth
                        .total_cmp(&b.depth)
                        .then_with(|| b.overlap_normal.y.total_cmp(&a.overlap_normal.y))
                        .then_with(|| a.overlap_normal.x.total_cmp(&b.overlap_normal.x))
                        .then_with(|| a.overlap_normal.z.total_cmp(&b.overlap_normal.z))
                });
            let Some(hit) = hit else { break };
            let delta = -hit.overlap_normal * (hit.depth + SKIN).min(budget);
            // The shortest separation from a tilted wall can point into the floor.
            // Constrain the correction by other solids and keep its lateral component.
            let next = self.advance(area, delta, false, 0.0).area;
            let distance = (next.min - area.min).length();
            if distance < EPSILON {
                break;
            }
            area = next;
            budget -= distance;
            if budget < SKIN {
                break;
            }
        }
        area
    }

    pub fn travel(&self, area: Aabb, delta: DVec3, grip: f64) -> Travel {
        self.advance(area, delta, true, grip)
    }

    fn advance(&self, mut area: Aabb, delta: DVec3, overlaps: bool, grip: f64) -> Travel {
        let (mut planes, mut count) = ([DVec3::ZERO; CONTACTS], 0);
        let mut remaining = 1.0;
        let mut velocity = delta;
        let mut wish = delta;
        let mut hit = [false; 3];
        let mut floor = false;
        for _ in 0..CONTACTS {
            let motion = velocity * remaining;
            if motion.length_squared() <= EPSILON * EPSILON {
                break;
            }
            let Some(contact) = self.first(area, motion, overlaps) else {
                area = area.shifted(motion);
                break;
            };
            let safe = (contact.fraction - SKIN / -motion.dot(contact.normal)).max(0.0);
            area = area.shifted(motion * safe);
            remaining *= 1.0 - contact.fraction;
            let normal = contact.normal;
            if normal.y >= WALKABLE_Y && delta.y <= 0.0 {
                floor = true;
                if grip > 0.0 {
                    let slope = normal.with_y(0.0).length() / normal.y;
                    let slide = if slope <= grip { 0.0 } else { 1.0 - grip / slope };
                    wish.y = wish.y.max(delta.y * slide);
                }
            }
            if normal.y.abs() < WALKABLE_Y {
                hit[0] |= normal.x.abs() > EPSILON;
                hit[2] |= normal.z.abs() > EPSILON;
            } else {
                hit[1] = true;
            }
            if planes[..count]
                .iter()
                .any(|n| n.dot(normal) > 1.0 - EPSILON)
            {
                break;
            }
            planes[count] = normal;
            count += 1;
            velocity = clip(wish, &planes[..count]);
        }
        Travel { area, velocity, hit, floor }
    }

    pub fn vertical(&self, area: Aabb, dy: f64) -> Travel {
        let delta = DVec3::Y * dy;
        let hit = self.first(area, delta, true);
        let floor = dy < 0.0 && hit.is_some_and(|h| h.normal.y >= WALKABLE_Y);
        let fraction = hit.map_or(1.0, |h| h.fraction);
        Travel {
            area: area.shifted(delta * fraction),
            velocity: if hit.is_some() { DVec3::ZERO } else { delta },
            hit: [false, hit.is_some(), false],
            floor,
        }
    }

    pub fn floor(&self, area: Aabb, reach: f64) -> Option<Aabb> {
        // The sweep stops a skin width before the surface. Cross that gap even when
        // there is no horizontal motion, since touching AABBs can miss the broad phase.
        let delta = -DVec3::Y * (reach + 2.0 * SKIN);
        let hit = self.first(area, delta, true)?;
        (hit.normal.y >= WALKABLE_Y).then_some(area.shifted(delta * hit.fraction))
    }
}

// In 3D the nearest allowed motion lies on a plane, the line shared by two, or at zero.
fn clip(wish: DVec3, planes: &[DVec3]) -> DVec3 {
    let legal = |v: DVec3| planes.iter().all(|n| v.dot(*n) >= -EPSILON);
    if legal(wish) {
        return wish;
    }
    let mut best = DVec3::ZERO;
    let mut distance = wish.length_squared();
    let mut consider = |v: DVec3| {
        let d = (v - wish).length_squared();
        if d < distance && legal(v) {
            distance = d;
            best = v;
        }
    };
    for (i, &n) in planes.iter().enumerate() {
        consider(wish - n * wish.dot(n));
        for &m in &planes[..i] {
            let line = n.cross(m).normalize_or_zero();
            consider(line * wish.dot(line));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{DMat3, DQuat};

    fn wall() -> Collider {
        let rotation = DQuat::from_rotation_y(0.4);
        let shape = RotatedBox { center: rotation * (DVec3::X * 0.5), half: DVec3::new(0.5, 3.0, 10.0), axes: DMat3::from_quat(rotation) };
        Collider { shape: Obstacle::Box(shape), bounds: Aabb { min: DVec3::splat(-12.0), max: DVec3::splat(12.0) } }
    }

    fn box_shape(collider: &mut Collider) -> &mut RotatedBox {
        let Obstacle::Box(shape) = &mut collider.shape else { unreachable!() };
        shape
    }

    #[test]
    fn oblique_wall_keeps_both_directions_of_tangent_motion() {
        let mut wall = wall();
        let floor = Aabb { min: DVec3::new(-12.0, -1.0, -12.0), max: DVec3::new(12.0, 0.0, 12.0) };
        let normal = -box_shape(&mut wall).axes.x_axis;
        let tangent = box_shape(&mut wall).axes.z_axis;
        let body = Aabb::around(normal * 0.6, 0.3, 1.8);
        let sweep = Sweep { aligned: &[floor], structures: &[wall] };
        for sign in [-1.0, 1.0] {
            let wish = tangent * sign - normal * 0.6 - DVec3::Y * 0.08;
            let travel = sweep.travel(body, wish, 0.0);
            assert!(((travel.area.min - body.min).dot(tangent) - sign).abs() < 1e-4);
            assert!((travel.velocity.dot(tangent) - sign).abs() < 1e-6);
            assert!(travel.floor && travel.hit[0] && travel.hit[2]);
        }
    }

    #[test]
    fn initial_overlap_recovers_and_blocks_motion_deeper() {
        let mut wall = wall();
        let normal = -box_shape(&mut wall).axes.x_axis;
        let body = Aabb::around(normal * 0.35, 0.3, 1.8);
        let sweep = Sweep { aligned: &[], structures: &[wall] };
        let recovered = sweep.recover(body);
        assert!(
            sweep
                .near(recovered)
                .all(|shape| shape.project(recovered, DVec3::ZERO).is_none())
        );
        let moved = sweep.travel(body, -normal * 0.1, 0.0);
        assert!((moved.area.min - body.min).dot(normal) >= -1e-7);
    }

    #[test]
    fn tilted_wall_recovery_slides_over_the_floor() {
        let mut wall = wall();
        let rotation = DQuat::from_rotation_z(0.15);
        box_shape(&mut wall).axes = DMat3::from_quat(rotation);
        box_shape(&mut wall).center = rotation * (DVec3::X * 0.5);
        let floor = Aabb { min: DVec3::new(-12.0, -1.0, -12.0), max: DVec3::new(12.0, 0.0, 12.0) };
        let body = Aabb::around(-DVec3::X * 0.45, 0.3, 1.8);
        let sweep = Sweep { aligned: &[floor], structures: &[wall] };
        let recovered = sweep.recover(body);
        assert!(recovered.min.x < body.min.x);
        assert!((recovered.min.y - body.min.y).abs() < EPSILON);
        assert!(
            sweep
                .near(recovered)
                .all(|shape| shape.project(recovered, DVec3::ZERO).is_none())
        );
    }

    #[test]
    fn landing_without_horizontal_motion_can_snap_to_the_floor() {
        let deck = Aabb { min: DVec3::new(-1.0, 7.0, -1.0), max: DVec3::new(1.0, 8.0, 1.0) };
        let sweep = Sweep { aligned: &[], structures: &[Collider::new(Solid::Box(deck), Transform::default())] };
        let body = Aabb::around(DVec3::Y * 8.05, 0.3, 1.8);
        let travel = sweep.travel(body, -DVec3::Y * 0.08, 0.0);
        assert!(travel.floor);
        let landed = sweep.floor(travel.area, 0.0).unwrap();
        assert!((landed.min.y - deck.max.y).abs() < EPSILON);
    }

    #[test]
    fn sweep_stops_at_a_wall_between_the_start_and_destination() {
        let mut wall = wall();
        let normal = -box_shape(&mut wall).axes.x_axis;
        let start = Aabb::around(normal * 3.0, 0.3, 1.8);
        let sweep = Sweep { aligned: &[], structures: &[wall] };
        let travel = sweep.travel(start, -normal * 8.0, 0.0);
        assert!(travel.hit[0] && travel.hit[2]);
        assert!(travel.velocity.length_squared() < EPSILON * EPSILON);
        let feet = travel.area.min + DVec3::new(0.3, 0.0, 0.3);
        let support_radius = 0.3 * (normal.x.abs() + normal.z.abs());
        assert!((feet.dot(normal) - support_radius - SKIN).abs() < EPSILON);
    }
}
