use glam::{DQuat, DVec3};
use parry3d_f64::{
    math::{Pose, Rotation, Vector},
    query,
    shape::{Cuboid, Cylinder, Shape},
};

use super::{BoxContact, Solid, Transform};

fn geometry(solid: Solid, place: Transform) -> (Pose, Geometry) {
    let pose =
        |center: DVec3, rotation: DQuat| Pose::from_parts(Vector::from_array(center.to_array()), Rotation::from_array(rotation.to_array()));
    match solid {
        Solid::Box(b) => {
            let center = place.to_world((b.min + b.max) * 0.5);
            let half = Vector::from_array(((b.max - b.min) * 0.5).to_array());
            (pose(center, place.rotation()), Geometry::Box(Cuboid::new(half)))
        }
        Solid::Cylinder(c) => {
            let c = c.placed(place);
            let pose = pose(c.center, DQuat::from_rotation_arc(DVec3::Y, c.axis));
            (pose, Geometry::Cylinder(Cylinder::new(c.half, c.radius)))
        }
    }
}

enum Geometry {
    Box(Cuboid),
    Cylinder(Cylinder),
}

pub(super) fn cylinder_ray(c: super::Cylinder, from: DVec3, direction: DVec3, max: f64) -> Option<(f64, u8)> {
    let pose = Pose::from_parts(Vector::ZERO, Rotation::from_array(DQuat::from_rotation_arc(DVec3::Y, c.axis).to_array()));
    let ray = query::Ray::new(Vector::from_array((from - c.center).to_array()), Vector::from_array(direction.to_array()));
    let shape = Cylinder::new(c.half, c.radius);
    let hit = query::RayCast::cast_ray_and_get_normal(&shape, &pose, &ray, max, true)?;
    let normal = if hit.time_of_impact == 0.0 { -direction } else { DVec3::from_array(hit.normal.to_array()) };
    let axis = (0..3).max_by(|&a, &b| normal[a].abs().total_cmp(&normal[b].abs()))?;
    Some((hit.time_of_impact, (axis * 2) as u8 + (normal[axis] > 0.0) as u8))
}

impl Geometry {
    fn shape(&self) -> &dyn Shape {
        match self {
            Self::Box(shape) => shape,
            Self::Cylinder(shape) => shape,
        }
    }
}

/// Swept contact in the target's coordinate system. The normal points toward the moving shape.
pub(super) fn convex_contact(moving: Solid, place: Transform, target: Solid, delta: DVec3) -> Option<BoxContact> {
    let (a, shape_a) = geometry(moving, place);
    let (mut b, shape_b) = geometry(target, Transform::of(Transform::FEET));
    // Keep the convex query near zero even far from world spawn.
    b.translation -= a.translation;
    let origin = a.translation;
    let a = Pose::from_parts(Vector::ZERO, a.rotation);
    let (shape_a, shape_b) = (shape_a.shape(), shape_b.shape());
    let engine = |v: Vector| DVec3::from_array(v.to_array());
    let initial = || query::contact(&a, shape_a, &b, shape_b, 0.0).ok().flatten();
    let (fraction, mut normal, mut point, depth) = if delta == DVec3::ZERO {
        let hit = initial().filter(|hit| hit.dist < -1e-7)?;
        (0.0, engine(hit.normal2), engine(origin + (hit.point1 + hit.point2) * 0.5), -hit.dist)
    } else {
        let options =
            query::ShapeCastOptions { max_time_of_impact: 1.0, compute_impact_geometry_on_penetration: true, ..Default::default() };
        let velocity = Vector::from_array(delta.to_array());
        let hit = query::cast_shapes(&a, velocity, shape_a, &b, Vector::ZERO, shape_b, options)
            .ok()
            .flatten()?;
        let fraction = hit.time_of_impact;
        let normal = engine(b.rotation * hit.normal2);
        let depth = if fraction < 1e-7 { initial().map_or(0.0, |hit| (-hit.dist).max(0.0)) } else { 0.0 };
        if depth <= 1e-7 && delta.dot(normal) >= -1e-7 {
            return None;
        }
        let point = (a * hit.witness1 + velocity * fraction + b * hit.witness2) * 0.5;
        (fraction, normal, engine(origin + point), depth)
    };
    if let (Solid::Cylinder(cylinder), Solid::Box(bounds)) = (moving, target) {
        // On a flat face the contact is a tread line, not an arbitrary end of the axle.
        let cylinder = cylinder.placed(place);
        let face = DVec3::select(normal.abs().cmpgt(DVec3::splat(0.9999)), normal.signum(), DVec3::ZERO);
        if face != DVec3::ZERO && cylinder.axis.dot(face).abs() < 1e-7 {
            let patch = cylinder.center + delta * fraction - face * cylinder.radius;
            let tangent = DVec3::ONE - face.abs();
            if ((patch - bounds.min) * tangent).min_element() >= -1e-6 && ((bounds.max - patch) * tangent).min_element() >= -1e-6 {
                point = patch;
                normal = face;
            }
        }
    }
    Some(BoxContact { fraction, normal, point, depth })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physics::{Aabb, Cylinder};

    #[test]
    fn rolling_cylinder_hits_the_same_floor_at_every_spin_angle() {
        let ground = Solid::Box(Aabb { min: DVec3::new(-4.0, -1.0, -4.0), max: DVec3::new(4.0, 0.0, 4.0) });
        let wheel = Solid::Cylinder(Cylinder { center: DVec3::splat(0.5), axis: DVec3::X, radius: 0.75, half: 0.35, grip: 1.0 });
        for angle in [0.0, 0.33, 0.79, 1.57, 3.5] {
            let pose = Transform::rotated(DVec3::new(0.0, 1.5, 0.0), DQuat::from_rotation_x(angle));
            let hit = convex_contact(wheel, pose, ground, DVec3::NEG_Y * 3.0).unwrap();
            assert!((hit.fraction - 1.25 / 3.0).abs() < 1e-5);
            assert!(hit.normal.dot(DVec3::Y) > 0.999);
            assert!(hit.point.distance(DVec3::ZERO) < 1e-4);
        }
    }

    #[test]
    fn the_corners_of_a_cylinders_bounds_are_empty() {
        let wheel = Solid::Cylinder(Cylinder { center: DVec3::ZERO, axis: DVec3::Z, radius: 1.0, half: 0.25, grip: 0.0 });
        let corner = Solid::Box(Aabb { min: DVec3::new(0.8, 0.8, -0.1), max: DVec3::new(0.9, 0.9, 0.1) });
        assert!(wheel.bounds().intersects(&corner.bounds()));
        assert!(convex_contact(wheel, Transform::default(), corner, DVec3::ZERO).is_none());
        let hit = convex_contact(wheel, Transform::default(), corner, DVec3::splat(0.3).with_z(0.0)).unwrap();
        assert!(hit.normal.dot(DVec3::new(-1.0, -1.0, 0.0).normalize()) > 0.999);
    }
}
