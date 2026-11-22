use weft_sdk::Plan;

pub fn sphere(radius: f32) -> impl Iterator<Item = (i32, i32, i32)> {
    let extent = radius.floor() as i32;
    let limit = radius as f64 * radius as f64;
    (-extent..=extent).flat_map(move |x| {
        let x2 = x as f64 * x as f64;
        (-extent..=extent).flat_map(move |y| {
            let xy = x2 + y as f64 * y as f64;
            (-extent..=extent)
                .filter(move |&z| xy + z as f64 * z as f64 <= limit)
                .map(move |z| (x, y, z))
        })
    })
}
/// Whether the ground within `r` blocks of a start is flat enough for it.
pub fn gentle(p: &Plan, r: i32, max_range: i32) -> bool {
    let at = p.pos();
    let mut lo = i32::MAX;
    let mut hi = i32::MIN;
    for dz in -r..=r {
        for dx in -r..=r {
            let h = p.ground(at.x + dx, at.z + dz).height;
            if h == i32::MIN {
                return false;
            }
            lo = lo.min(h);
            hi = hi.max(h);
        }
    }
    hi - lo <= max_range
}
