// Shared cache readers for the sky and translucent world.
fn cloud_cache_index(cell: vec2<i32>, band: i32, side: i32) -> u32 {
    let p = cell & vec2(side - 1);
    return u32((band * side + p.y) * side + p.x);
}

fn cloud_brick_index(cell: vec2<i32>, band: i32, side: i32) -> u32 {
    let count = side / CLOUD_BRICK;
    let p = (cell & vec2(side - 1)) / CLOUD_BRICK;
    return u32(5 * side * side + (band * count + p.y) * count + p.x);
}

fn cloud_packed_cell(view: Scene, cell: vec2<i32>, band: i32) -> u32 {
    return cloud_cells[cloud_cache_index(cell, band, i32(view.atmosphere_cache.x))];
}

fn cloud_cache_value(view: Scene, cell: vec2<i32>, band: i32) -> u32 {
    let side = i32(view.atmosphere_cache.x);
    let relative = cell - view.cloud_cache.xy;
    if any(relative < vec2(0)) || any(relative >= vec2(side)) {
        return CLOUD_UNCACHED;
    }
    if cloud_cells[cloud_brick_index(cell, band, side)] == 0u {
        return CLOUD_EMPTY_BRICK;
    }
    return cloud_packed_cell(view, cell, band);
}

const RAIN_STRIDE: u32 = 8u;
const RAIN_SLICES: u32 = 16u;
const RAIN_SLICE: f32 = 96.0;

fn rain_exposed(view: Scene, rel: vec3<f32>) -> bool {
    let side = i32(RAIN_COVER_CUBES * 16u / RAIN_COVER_CELL);
    let at = vec2<i32>(floor((view.rain_cover.xz + rel.xz) / f32(RAIN_COVER_CELL)));
    if view.rain_cover.w == 0.0 || any(at < vec2(0)) || any(at >= vec2(side)) {
        return true;
    }
    return rel.y + view.rain_cover.y >= f32(rain_cover[u32(at.y * side + at.x)]);
}

fn rain_at_cell(cell: u32, range: f32) -> vec4<f32> {
    let offset = cell * (RAIN_SLICES + 1u);
    let bounds = bitcast<vec2<f32>>(rain_depth[offset]);
    let distance = min(range, bounds.y);
    if distance <= bounds.x {
        return vec4(0.0);
    }
    if range >= bounds.y {
        let packed = rain_depth[offset + RAIN_SLICES];
        return vec4(unpack2x16float(packed.x), unpack2x16float(packed.y));
    }
    let layer = min(u32(distance / RAIN_SLICE), RAIN_SLICES - 1u);
    let start = max(bounds.x, f32(layer) * RAIN_SLICE);
    let end = min(bounds.y, f32(layer + 1u) * RAIN_SLICE);
    let a = select(vec2(0u), rain_depth[offset + layer], layer != 0u);
    let b = rain_depth[offset + layer + 1u];
    let first = vec4(unpack2x16float(a.x), unpack2x16float(a.y));
    let second = vec4(unpack2x16float(b.x), unpack2x16float(b.y));
    return mix(first, second, clamp((distance - start) / max(end - start, 1e-6), 0.0, 1.0));
}

fn rain_at(view: Scene, pixel: vec2<f32>, range: f32) -> vec4<f32> {
    if range <= 36.0 {
        return vec4(0.0);
    }
    let taps = atmosphere_filter(pixel, vec4(view.atmosphere_extent.xy, RAIN_STRIDE, view.atmosphere_cache.y));
    let i = taps.index;
    return mat4x4(rain_at_cell(i.x, range), rain_at_cell(i.y, range), rain_at_cell(i.z, range), rain_at_cell(i.w, range)) * taps.weight;
}

fn rain_composite_color(s: Sky, dir: vec3<f32>, rain: vec4<f32>, view: Scene) -> vec3<f32> {
    if (view.atmosphere_cache.w & ATMOSPHERE_FLAGS_RAIN_MEDIA) == 0u {
        return rain.rgb;
    }
    var rainbow = vec3(0.0);
    if rain.z > 0.0 {
        rainbow = rainbow_radiance(s, dir);
    }
    return vec3(rain.x) + view.fog_color.rgb * rain.y + rainbow * rain.z;
}

fn resolve_rain(s: Sky, dir: vec3<f32>, limit: f32, cloud: CloudHit, view: Scene, pixel: vec2<f32>) -> vec4<f32> {
    if view.atmosphere_cache.y == 0u {
        let color = composite_rain_veil(s, dir, limit, cloud, view);
        return vec4(color.rgb * color.a, color.a);
    }
    let front = min(cloud.optical_distance, limit);
    let rain_end = min(1536.0, select(limit, front, cloud.color.a >= CLOUD_OPAQUE));
    if rain_end <= 36.0 || min(36.0 * dir.y, rain_end * dir.y) >= rain_ceiling(s) {
        return vec4(cloud.color.rgb * cloud.color.a, cloud.color.a);
    }
    if cloud.color.a >= CLOUD_OPAQUE {
        let rain = rain_at(view, pixel, front);
        return vec4(rain_composite_color(s, dir, rain, view), rain.a)
            + vec4(cloud.color.rgb * cloud.color.a, cloud.color.a) * (1.0 - rain.a);
    }
    let rain = rain_at(view, pixel, limit);
    if all(rain == vec4(0.0)) {
        return vec4(cloud.color.rgb * cloud.color.a, cloud.color.a);
    }
    if cloud.color.a <= 0.0 {
        return vec4(rain_composite_color(s, dir, rain, view), rain.a);
    }
    let before_cloud = rain_at(view, pixel, front);
    let visible_rain = mix(rain, before_cloud, cloud.color.a);
    let color = rain_composite_color(s, dir, visible_rain, view) + cloud.color.rgb * cloud.color.a * (1.0 - before_cloud.a);
    return vec4(color, cloud.color.a + (1.0 - cloud.color.a) * rain.a);
}
