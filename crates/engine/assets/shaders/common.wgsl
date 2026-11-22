// Shared by the engine's shaders: put in front of each one (shader packs' too).

// The uv of each face corner (`CORNERS`, from the prelude), the two triangles of a face, and
// vanilla's face shading per direction (-X +X -Y +Y -Z +Z).
var<private> UVS: array<vec2<f32>, 4> = array<vec2<f32>, 4>(vec2(0.0, 1.0), vec2(1.0, 1.0), vec2(1.0, 0.0), vec2(0.0, 0.0));
var<private> TRI: array<u32, 6> = array<u32, 6>(0u, 1u, 2u, 0u, 2u, 3u);
var<private> SHADE: array<f32, 6> = array<f32, 6>(0.6, 0.6, 0.5, 1.0, 0.8, 0.8);

// `SHADE` blended for any unit normal.
fn normal_shade(n: vec3<f32>) -> f32 {
    return n.x * n.x * 0.6 + n.z * n.z * 0.8 + n.y * n.y * select(0.5, 1.0, n.y > 0.0);
}

// How far into the fog a point at `dist` is, 0..1.
fn fog_amount(dist: f32, start: f32, end: f32) -> f32 {
    if dist <= start {
        return 0.0;
    }
    if dist >= end {
        return 1.0;
    }
    return (dist - start) / (end - start);
}

fn rain_mist_amount(dist: f32, view: Scene) -> f32 {
    return view.environment.w * fog_amount(dist, view.params.x, view.params.y);
}

fn cloud_hash(seed: u32, cell: vec2<i32>) -> f32 {
    let v = seed ^ (bitcast<u32>(cell.x) * 0x9e3779b1u) ^ (bitcast<u32>(cell.y) * 0x85ebca77u);
    let a = (v ^ (v >> 16u)) * 0x7feb352du;
    let b = (a ^ (a >> 15u)) * 0x846ca68bu;
    return f32((b ^ (b >> 16u)) >> 8u) / 16777216.0;
}

fn cloud_noise(seed: u32, origin: vec2<i32>, at: vec2<f32>, frequency: vec2<i32>) -> f32 {
    let scaled = at * vec2<f32>(frequency);
    let cell = floor(scaled);
    let grid = origin * frequency + vec2<i32>(cell);
    let frac = scaled - cell;
    let f = frac * frac * (3.0 - 2.0 * frac);
    let a = cloud_hash(seed, grid);
    let b = cloud_hash(seed, grid + vec2(1, 0));
    let c = cloud_hash(seed, grid + vec2(0, 1));
    let d = cloud_hash(seed, grid + vec2(1, 1));
    return mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
}

// Match weather.rs. Each square column has one footprint and weather type.
const CLOUD_COLUMNS: f32 = 24.0;

fn cloud_warp(seed: u32, origin: vec2<i32>, at: vec2<f32>) -> vec2<f32> {
    let warp = cloud_noise(seed ^ 0xa341316cu, origin, at, vec2(1)) - 0.5;
    return at + vec2(0.78, 0.54) * warp;
}

fn cloud_climate(seed: u32, origin: vec2<i32>, at: vec2<f32>) -> f32 {
    let parity = origin & vec2(1);
    let climate_origin = (origin - parity) / 2;
    let climate_at = (at + vec2<f32>(parity)) * 0.5;
    let bend = cloud_noise(seed ^ 0xab5399d3u, climate_origin, climate_at, vec2(1)) - 0.5;
    return cloud_noise(seed ^ 0xb73102adu, climate_origin, climate_at + vec2(0.18, -0.10) * bend, vec2(1));
}

fn cloud_weather_mask(seed: u32, origin: vec2<i32>, at: vec2<f32>, partial: f32, detail: f32, edge: f32) -> vec3<f32> {
    let cover = (partial + edge * 0.13 - 0.47) * 4.7;
    if cover <= 0.0 {
        return vec3(0.0);
    }
    let weather = cloud_climate(seed, origin, at);
    let margin = 0.051 + max(0.03 * detail + 0.07 * edge - 0.025, 0.0);
    let gap = min(abs(weather - 0.51), abs(weather - 0.77)) - margin;
    let kind = select(0.0, 1.0, weather >= 0.51) + select(0.0, 1.0, weather >= 0.77);
    return vec3(cover * smoothstep(0.0, 0.045, gap), kind, detail * 0.65 + edge * 0.35);
}

fn cloud_field(seed: u32, origin: vec2<i32>, at: vec2<f32>, impostor: bool) -> vec3<f32> {
    let warped = cloud_warp(seed, origin, at);
    let coarse = cloud_noise(seed, origin, warped, vec2(1, 2));
    let detail = cloud_noise(seed ^ 0x3ca1ef53u, origin, warped, vec2(3, 6));
    let partial = coarse * 0.64 + detail * 0.23;
    if partial + 0.13 <= 0.47 {
        return vec3(0.0);
    }
    var edge = 0.5;
    if !impostor {
        edge = cloud_noise(seed ^ 0x5a528247u, origin, warped, vec2(9, 16));
    }
    return cloud_weather_mask(seed, origin, at, partial, detail, edge);
}

fn texture_cloud_at(s: Sky, cell: vec2<f32>, dims: vec2<f32>) -> bool {
    let at = fract((cell + s.uv.zw + vec2(0.5)) / dims);
    return textureSampleLevel(textures[s.ids.x], nearest_clamp, at, 0.0).a >= 0.5;
}

struct CloudHit {
    color: vec4<f32>,
    distance: f32,
    optical_distance: f32,
}

// The lattice shifts between bands so the rectangular shelves overhang each other.
fn cloud_band_offset(band: i32) -> vec2<f32> {
    return fract(vec2(0.375, 0.625) * f32(band - 2));
}

fn cloud_band_fill(field: vec3<f32>, band: i32) -> f32 {
    let middle = 2.0 + 3.2 * (field.z - 0.5);
    let thickness = 0.68 + min(field.x, 1.0) * (1.30 + field.y * 0.19);
    return thickness - abs(f32(band) - middle);
}

fn cloud_cell_density(seed: u32, origin: vec2<i32>, cell: vec2<f32>, band: i32) -> vec2<f32> {
    let at = (cell + cloud_band_offset(band) + vec2(0.5)) / CLOUD_COLUMNS;
    let field = cloud_field(seed, origin, at, false);
    if field.x <= 0.0 {
        return vec2(0.0);
    }
    let fill = cloud_band_fill(field, band);
    let density = smoothstep(0.0, 0.8, fill) * (0.6 + 0.4 * smoothstep(0.0, 0.65, field.x));
    var overhead = 0.0;
    for (var above = band + 1; above < 5; above++) {
        overhead += smoothstep(0.0, 0.8, cloud_band_fill(field, above));
    }
    // Keep the weather type and approximate overhead optical thickness in one half.
    return vec2(density, field.y + overhead * 0.125);
}

const CLOUD_BRICK: i32 = 8;
const CLOUD_UNCACHED: u32 = 0xffffffffu;
const CLOUD_EMPTY_BRICK: u32 = 0xfffffffeu;
const CLOUD_OPAQUE: f32 = 0.95;

struct CloudLighting {
    directed: vec3<f32>,
    from_above: f32,
    flashes: u32,
}

struct CloudAccumulation {
    light: vec3<f32>,
    alpha: f32,
    first: f32,
    optical_distance: f32,
}

fn cloud_lighting(s: Sky, dir: vec3<f32>, flashes: u32) -> CloudLighting {
    var directed_sun = vec3(0.0);
    var sun_from_above = 1.0;
    if s.cloud_sun.w > 0.0 {
        // Two polynomial forward lobes approximate the broad glow and bright rim.
        // The view and light directions are constant along this ray.
        let cosine = dot(dir, s.axes[2].xyz);
        let forward = max(cosine, 0.0);
        let square = forward * forward;
        let fourth = square * square;
        let eighth = fourth * fourth;
        let narrow = eighth * eighth;
        let back = max(-cosine, 0.0);
        directed_sun = s.cloud_sun.rgb * (0.16 + 0.48 * eighth + 0.95 * narrow * narrow + 0.10 * back * back);
        sun_from_above = smoothstep(0.02, 0.36, s.axes[2].y);
    }
    return CloudLighting(directed_sun, sun_from_above, flashes);
}

fn cloud_radiance(s: Sky, weather: f32, kind: f32, optical_depth: f32, sun: CloudLighting) -> vec3<f32> {
    var base = s.color.rgb;
    if s.ids.w != 0u && weather > 0.0 {
        base = select(s.axes[0].xyz, s.axes[1].xyz, weather == 2.0);
    }
    let scattered_tint = base / max(max(max(base.r, base.g), base.b), 0.001);
    let ambient = 0.77 + 0.13 * exp2(-0.75 * optical_depth);
    let sun_path = s.cloud_sun.w * optical_depth * (1.0 + 0.30 * kind);
    let transmittance = exp2(-1.6 * sun_path);
    let scatter = s.cloud_sun.rgb * 0.12 + transmittance * (s.cloud_sun.rgb * 0.19 + sun.directed * 0.30) * sun.from_above;
    let radiance = base * ambient + scattered_tint * scatter;
    // Keep distinctions in sunlit cloud tops when the surface has no HDR headroom.
    let peak = max(max(radiance.r, radiance.g), radiance.b);
    let highlight = max(peak - 0.75, 0.0);
    let shoulder = 0.75 + highlight / (1.0 + 4.0 * highlight);
    return radiance * min(1.0, shoulder / max(peak, 0.001));
}

const CLOUD_FLASH_RADIUS: f32 = 210.0;

fn cloud_flash_end(start: vec3<f32>, end: vec4<f32>) -> vec3<f32> {
    return mix(start, end.xyz, select(1.0, 0.25, end.w > 0.0));
}

fn cloud_flashes_on_ray(view: Scene, dir: vec3<f32>, near: f32, far: f32) -> u32 {
    var mask = 0u;
    for (var i = 0u; i < 3u && view.lightning_start[i].w > 0.0; i++) {
        let start = view.lightning_start[i].xyz;
        let end = cloud_flash_end(start, view.lightning_end[i]);
        let center = (start + end) * 0.5;
        let radius = CLOUD_FLASH_RADIUS + length(end - start) * 0.5;
        let to_ray = center - dir * clamp(dot(center, dir), near, far);
        if dot(to_ray, to_ray) < radius * radius {
            mask |= 1u << i;
        }
    }
    return mask;
}

fn lightning_cloud(view: Scene, pos: vec3<f32>, radiance: vec3<f32>, flashes: u32) -> vec3<f32> {
    var brightness = 0.0;
    for (var mask = flashes; mask != 0u; mask &= mask - 1u) {
        let i = firstTrailingBit(mask);
        let start = view.lightning_start[i];
        let span = cloud_flash_end(start.xyz, view.lightning_end[i]) - start.xyz;
        let rel = pos - start.xyz;
        let along = clamp(dot(rel, span) / max(dot(span, span), 1.0), 0.0, 1.0);
        let distance = length(rel - span * along);
        let glow = max(1.0 - distance / CLOUD_FLASH_RADIUS, 0.0);
        brightness += start.w * glow * glow * 2.4;
    }
    return mix(radiance, vec3(0.87, 0.92, 1.0), 1.0 - exp(-brightness));
}

fn lightning_ground(view: Scene, pos: vec3<f32>, sky_light: f32) -> vec3<f32> {
    if view.lightning_start[0].w <= 0.0 || sky_light < 0.38 {
        return vec3(0.0);
    }
    var brightness = 0.0;
    for (var i = 0u; i < 3u && view.lightning_start[i].w > 0.0; i++) {
        let end = view.lightning_end[i];
        if end.w > 0.0 {
            let glow = max(1.0 - distance(end.xyz, pos) / 116.0, 0.0);
            brightness += view.lightning_start[i].w * glow * glow;
        }
    }
    return vec3(0.61, 0.70, 0.86) * brightness * smoothstep(0.38, 0.94, sky_light);
}

fn accumulate_cloud(
    a: ptr<function, CloudAccumulation>, s: Sky, view: Scene, dir: vec3<f32>, weather: f32, overhead: f32, density: f32,
    near: f32, far: f32, weight: f32, sun: CloudLighting,
) {
    let kind = select(s.extra.w, weather, s.ids.w != 0u);
    let extinction = select(0.085, select(0.145, 0.22, kind == 2.0), kind > 0.0);
    let absorption = (1.0 - (*a).alpha) * (1.0 - exp(-density * weight * (far - near) * extinction));
    (*a).first = min((*a).first, near);
    (*a).optical_distance += (near + far) * 0.5 * absorption;
    var radiance = cloud_radiance(s, weather, kind, overhead + density * 0.35, sun);
    if sun.flashes != 0u {
        radiance = lightning_cloud(view, dir * ((near + far) * 0.5), radiance, sun.flashes);
    }
    (*a).light += radiance * absorption;
    (*a).alpha += absorption;
}

// Walk each intersected band front to back, stepping exactly at voxel boundaries.
fn trace_cloud_bands(
    a: ptr<function, CloudAccumulation>, s: Sky, dir: vec3<f32>, near: f32, far: f32, view: Scene, sun: CloudLighting,
) {
    let lod = view.cloud_lod.xy;
    let origin = vec2<i32>(s.uv.zw);
    let ray = dir.xz * CLOUD_COLUMNS / s.extra.x;
    let step = sign(ray);
    let delta = 1.0 / max(abs(ray), vec2(1e-12));
    let band_height = s.extra.y / 5.0;
    let height_at_camera = -s.extra.z / band_height;
    let height_rate = dir.y / band_height;
    let band_time = 1.0 / max(abs(height_rate), 1e-12);
    let direction = select(1, -1, dir.y < 0.0);
    let first_band = i32(clamp(floor(height_at_camera + height_rate * near), 0.0, 4.0));
    let last_band = i32(clamp(floor(height_at_camera + height_rate * far), 0.0, 4.0));
    let transition = far > lod.x;
    for (var band = first_band; band != last_band + direction && (*a).alpha < CLOUD_OPAQUE; band += direction) {
        let boundary = f32(band) + select(0.0, 1.0, dir.y < 0.0);
        let enter = (boundary - height_at_camera) * band_time * f32(direction);
        let band_start = select(max(enter, near), near, band == first_band);
        let band_end = select(min(enter + band_time, far), far, band == last_band);
        let shift = cloud_band_offset(band);
        let ray_origin = s.uv.xy * CLOUD_COLUMNS - shift;
        let start = ray_origin + ray * band_start;
        var cell = floor(start);
        let offset = select(start - cell, cell + vec2(1.0) - start, ray > vec2(0.0));
        var next = select(band_start + offset * delta, vec2(1e12), ray == vec2(0.0));
        var t = band_start;
        while t < band_end && (*a).alpha < CLOUD_OPAQUE {
            let world_cell = origin * i32(CLOUD_COLUMNS) + vec2<i32>(cell);
            let cached = cloud_cache_value(view, world_cell, band);
            if cached == CLOUD_EMPTY_BRICK {
                let inner = world_cell & vec2(CLOUD_BRICK - 1);
                let remaining = select(inner, vec2(CLOUD_BRICK - 1) - inner, ray > vec2(0.0));
                let boundary = next + vec2<f32>(remaining) * delta;
                t = min(band_end, max(t + 0.001, min(boundary.x, boundary.y)));
                // Enter the next brick on the correct side of its grid plane.
                let p = ray_origin + ray * t;
                cell = floor(p + step * 0.00001);
                let offset = max(select(p - cell, cell + vec2(1.0) - p, ray > vec2(0.0)), vec2(0.0));
                next = select(t + offset * delta, vec2(1e12), ray == vec2(0.0));
                continue;
            }
            let end = min(band_end, min(next.x, next.y));
            var sample_density: vec2<f32>;
            if cached == CLOUD_UNCACHED {
                sample_density = cloud_cell_density(s.ids.z, origin, cell, band);
            } else {
                sample_density = unpack2x16float(cached);
            }
            if sample_density.x > 0.0 && end > t {
                var weight = 1.0;
                if transition {
                    weight = 1.0 - smoothstep(lod.x, lod.y, (t + end) * 0.5);
                }
                accumulate_cloud(a, s, view, dir, floor(sample_density.y), fract(sample_density.y) * 8.0, sample_density.x, t, end, weight, sun);
            }
            t = end;
            if next.x < next.y {
                next.x += delta.x;
                cell.x += step.x;
            } else {
                next.y += delta.y;
                cell.y += step.y;
            }
        }
    }
}

// A distant layer uses the same moving weather footprint as the voxels. Project its
// mean density through the slab with one lookup when possible, and bound the cost of
// grazing rays that see long stretches of the footprint.
fn trace_cloud_impostor(
    a: ptr<function, CloudAccumulation>, s: Sky, dir: vec3<f32>, near: f32, far: f32, view: Scene, sun: CloudLighting,
) {
    let lod = view.cloud_lod.xy;
    let origin = vec2<i32>(s.uv.zw);
    let ray = dir.xz / s.extra.x;
    let stride = max(96.0, (far - near) / 16.0);
    let overhead = mix(0.15, 2.4, smoothstep(-0.035, 0.035, dir.y));
    for (var t = near; t < far && (*a).alpha < CLOUD_OPAQUE; t += stride) {
        let end = min(t + stride, far);
        let middle = (t + end) * 0.5;
        let field = cloud_field(s.ids.z, origin, s.uv.xy + ray * middle, true);
        if field.x > 0.0 {
            let density = smoothstep(0.0, 0.72, field.x) * 0.58;
            let weight = smoothstep(lod.x, lod.y, middle);
            accumulate_cloud(a, s, view, dir, field.y, overhead * density, density, t, end, weight, sun);
        }
    }
}

fn cloud_volume(s: Sky, dir: vec3<f32>, near: f32, far: f32, view: Scene) -> CloudHit {
    let lod = view.cloud_lod.xy;
    let sun = cloud_lighting(s, dir, cloud_flashes_on_ray(view, dir, near, far));
    var result = CloudAccumulation(vec3(0.0), 0.0, far, 0.0);
    let detailed_end = min(far, lod.y);
    if near < detailed_end {
        trace_cloud_bands(&result, s, dir, near, detailed_end, view, sun);
    }
    if far > lod.x && result.alpha < CLOUD_OPAQUE {
        trace_cloud_impostor(&result, s, dir, max(near, lod.x), far, view, sun);
    }
    let distance = result.optical_distance / max(result.alpha, 0.00001);
    let fog = fog_amount(distance, view.environment.y, view.environment.z);
    let haze = max(smoothstep(0.0, 1.0, fog), rain_mist_amount(distance, view));
    let color = mix(result.light / max(result.alpha, 0.00001), view.fog_color.rgb, haze);
    return CloudHit(vec4(color, result.alpha * s.color.a * (1.0 - smoothstep(0.6, 1.0, fog))), result.first, distance);
}

// Trace up to the first cloud surface, stopping at the fragment when testing world geometry.
fn trace_cloud(s: Sky, dir: vec3<f32>, limit: f32, view: Scene) -> CloudHit {
    let size = s.extra.x;
    let low = s.extra.z;
    let reach = view.environment.z;
    let high = low + s.extra.y;
    var near = 0.0;
    var far = min(reach, limit);
    if dir.y != 0.0 {
        near = max(near, min(low / dir.y, high / dir.y));
        far = min(far, max(low / dir.y, high / dir.y));
    } else if low > 0.0 || high < 0.0 {
        return CloudHit(vec4(0.0), 0.0, 0.0);
    }
    if near >= far {
        return CloudHit(vec4(0.0), 0.0, 0.0);
    }
    if s.ids.x == 0xffffffffu {
        return cloud_volume(s, dir, near, far, view);
    }
    let dims = vec2<f32>(f32(s.ids.z), f32(s.ids.w));
    let start = s.uv.xy + dir.xz * near / size;
    var cell = floor(start);
    let turn = sign(dir.xz);
    let delta = size / max(abs(dir.xz), vec2(0.000001));
    var next = near + select(start - cell, cell + vec2(1.0) - start, dir.xz > vec2(0.0)) * delta;
    var t = near;
    // 0: came in through a plane, 1 and 2: through a side along x or z.
    var side = 0u;
    // From inside a cloud, it isn't what is seen.
    var within = near == 0.0 && texture_cloud_at(s, cell, dims);
    var hit = false;
    let steps = u32(ceil((far - near) / size * (abs(dir.x) + abs(dir.z)))) + 2u;
    for (var i = 0u; i < steps; i++) {
        let filled = texture_cloud_at(s, cell, dims);
        if filled && !within {
            hit = true;
            break;
        }
        within = within && filled;
        if next.x < next.y {
            t = next.x;
            next.x += delta.x;
            cell.x += turn.x;
            side = 1u;
        } else {
            t = next.y;
            next.y += delta.y;
            cell.y += turn.y;
            side = 2u;
        }
        if t >= far {
            break;
        }
    }
    if !hit {
        return CloudHit(vec4(0.0), 0.0, 0.0);
    }
    // Lit from above, like blocks.
    var shade = select(0.9, 0.8, side == 1u);
    if side == 0u {
        shade = select(1.0, 0.7, dir.y > 0.0);
    }
    let fog = fog_amount(t, view.environment.y, reach);
    let haze = max(smoothstep(0.0, 1.0, fog), rain_mist_amount(t, view));
    let color = mix(s.color.rgb * shade, view.fog_color.rgb, haze);
    let alpha = s.color.a * (1.0 - smoothstep(0.6, 1.0, fog));
    return CloudHit(vec4(color, alpha), t, t);
}

// A square ring on a grid perpendicular to the direction opposite the sun.
// The grid follows the sun in world space, so camera rotation doesn't move the pixels.
fn rainbow_radiance(s: Sky, dir: vec3<f32>) -> vec3<f32> {
    let sun = s.axes[2].xyz;
    if s.cloud_sun.w <= 0.0 || sun.y <= 0.0 {
        return vec3(0.0);
    }
    let cosine = -dot(dir, sun);
    // Reject directions beyond the far corners or inside the nearest edges.
    if cosine <= 0.58 || cosine >= 0.82 {
        return vec3(0.0);
    }
    let vertical = select(vec3(0.0, 1.0, 0.0), vec3(1.0, 0.0, 0.0), sun.y > 0.999);
    let right = normalize(cross(vertical, sun));
    let up = cross(sun, right);
    let projected = vec2(dot(dir, right), dot(dir, up)) * (28.0 / cosine);
    let pixel = abs(floor(projected) + vec2(0.5));
    // Trim each corner on the same grid to get short, blocky steps.
    let corner = floor((pixel.x + pixel.y + 5.0) * 0.5) + 0.5;
    let radius = max(max(pixel.x, pixel.y), corner);
    if radius < 21.0 || radius > 27.0 {
        return vec3(0.0);
    }
    let band = (radius - 21.0) / 6.0;
    let edge = smoothstep(0.0, 0.14, band) * (1.0 - smoothstep(0.84, 1.0, band));
    let red = smoothstep(0.54, 0.83, band) + 0.36 * (1.0 - smoothstep(0.14, 0.37, band));
    let green = smoothstep(0.17, 0.45, band) * (1.0 - smoothstep(0.65, 0.94, band));
    let blue = 1.0 - smoothstep(0.34, 0.69, band);
    return vec3(red, green, blue) * s.cloud_sun.rgb * (0.55 * edge * smoothstep(0.0, 0.06, sun.y));
}

// One cloud-footprint sample along the light ray approximates the shadow on a rain bin.
fn rain_sunlight(s: Sky, origin: vec2<i32>, at: vec2<f32>, height: f32) -> f32 {
    let rise = s.extra.z + s.extra.y * 0.5 - height;
    if rise <= 0.0 {
        return 1.0;
    }
    let sun = s.axes[2].xyz;
    let above = at + sun.xz * (rise / (max(sun.y, 0.002) * s.extra.x));
    let shadow = cloud_field(s.ids.z, origin, above, false).x;
    return 1.0 - smoothstep(0.06, 0.67, shadow);
}

fn rain_medium(s: Sky, dir: vec3<f32>, t: f32, reach: f32, rainbow: vec3<f32>, view: Scene) -> vec4<f32> {
    let rel = dir * t;
    if !rain_exposed(view, rel) {
        return vec4(0.0);
    }
    let top = s.extra.z + s.extra.y * 0.27;
    let origin = vec2<i32>(s.uv.zw);
    let at = s.uv.xy + rel.xz / s.extra.x;
    var coverage = 1.0;
    var storm = s.extra.w >= 2.0;
    var wetness = 1.0;
    if s.ids.w != 0u {
        // Only the coarse footprint feeds the rain shafts.
        let coarse = cloud_noise(s.ids.z, origin, cloud_warp(s.ids.z, origin, at), vec2(1, 2));
        if coarse <= 0.48 {
            return vec4(0.0);
        }
        coverage = smoothstep(0.48, 0.74, coarse);
        let climate = cloud_climate(s.ids.z, origin, at);
        storm = climate >= 0.77;
        let rain_band = smoothstep(0.57, 0.63, climate) * (1.0 - smoothstep(0.66, 0.72, climate));
        wetness = select(rain_band, smoothstep(0.83, 0.90, climate), storm);
    }
    let preset = select(s.axes[0], s.axes[1], storm);
    if wetness <= 0.0 || preset.w <= 0.0 {
        return vec4(0.0);
    }
    let attach = smoothstep(-18.0, 28.0, top - dir.y * t);
    let range = smoothstep(36.0, 105.0, t) * (1.0 - smoothstep(reach * 0.55, reach, t));
    let density = coverage * wetness * attach * range * preset.w * s.color.a;
    let extinction = density * select(0.0075, 0.012, storm);
    let luma = vec3(0.30, 0.59, 0.11);
    let fog = view.fog_color.rgb;
    var color = mix(vec3(dot(preset.rgb, luma) * 0.55), mix(vec3(dot(fog, luma)), fog, 0.28), 0.58);
    if extinction > 0.0 && max(max(rainbow.r, rainbow.g), rainbow.b) > 0.002 {
        color += rainbow * rain_sunlight(s, origin, at, dir.y * t);
    }
    return vec4(mix(color, fog, rain_mist_amount(t, view)), extinction);
}

fn rain_ceiling(s: Sky) -> f32 {
    return s.extra.z + s.extra.y * 0.27 + 18.0;
}

// A soft mass beneath the cloud. Only the weather field moves it, with the cloud's drift.
fn composite_rain_veil(s: Sky, dir: vec3<f32>, limit: f32, cloud: CloudHit, view: Scene) -> vec4<f32> {
    if max(s.axes[0].w, s.axes[1].w) <= 0.0 {
        return cloud.color;
    }
    let ceiling = rain_ceiling(s);
    let reach = min(1536.0, view.environment.z);
    var near = 36.0;
    // Once the cloud is nearly opaque, rain behind it contributes at most five percent.
    var far = min(min(limit, reach), select(limit, cloud.optical_distance, cloud.color.a >= CLOUD_OPAQUE));
    if dir.y > 0.0 {
        far = min(far, ceiling / dir.y);
    } else if dir.y < 0.0 {
        near = max(near, ceiling / dir.y);
    } else if ceiling < 0.0 {
        return cloud.color;
    }
    if near >= far {
        return cloud.color;
    }
    let rainbow = rainbow_radiance(s, dir);
    var light = vec3(0.0);
    var alpha = 0.0;
    var front_alpha = 0.0;
    // Fixed bin boundaries keep samples from jumping when a ray meets the ground
    // or moves from one side of the cloud base to the other.
    for (var bin = floor(near / 36.0) * 36.0; bin < far && alpha < 0.88; bin += 36.0) {
        let start = max(bin, near);
        let end = min(bin + 36.0, far);
        let t = (start + end) * 0.5;
        let medium = rain_medium(s, dir, t, reach, rainbow, view);
        let extinction = medium.a;
        if extinction <= 0.0 {
            continue;
        }
        let absorption = (1.0 - alpha) * (1.0 - exp(-extinction * (end - start)));
        // Place the cloud at its opacity-weighted distance along the ray.
        // Split the one rain bin crossing that distance without resampling the field.
        var front_absorption = 0.0;
        if cloud.optical_distance >= end {
            front_absorption = absorption;
        } else if cloud.optical_distance > start {
            front_absorption = (1.0 - alpha) * (1.0 - exp(-extinction * (cloud.optical_distance - start)));
        }
        light += medium.rgb * mix(absorption, front_absorption, cloud.color.a);
        alpha += absorption;
        front_alpha += front_absorption;
    }
    light += cloud.color.rgb * cloud.color.a * (1.0 - front_alpha);
    let opacity = cloud.color.a + (1.0 - cloud.color.a) * alpha;
    return vec4(light / max(opacity, 0.00001), opacity);
}

fn trace_atmosphere(s: Sky, dir: vec3<f32>, limit: f32, view: Scene) -> vec4<f32> {
    // Once local mist is fully present, everything beyond its end already
    // resolves to the same fog color as the background.
    let visible_limit = select(limit, min(limit, view.params.y), view.environment.w >= 1.0);
    let cloud = trace_cloud(s, dir, visible_limit, view);
    if s.ids.x != 0xffffffffu {
        return cloud.color;
    }
    return composite_rain_veil(s, dir, visible_limit, cloud, view);
}

struct AtmosphereFilter {
    index: vec4<u32>,
    weight: vec4<f32>,
}

fn atmosphere_filter(pixel: vec2<f32>, grid: vec4<u32>) -> AtmosphereFilter {
    let stride = f32(grid.z);
    let center = f32(grid.z / 2u) + 0.5;
    let last = (grid.xy - 1u) >> vec2(firstTrailingBit(grid.z));
    let low = min(vec2<u32>(max(floor((pixel - center) / stride), vec2(0.0))), last);
    let high = min(low + 1u, last);
    let edge = vec2<f32>(grid.xy) - 0.5;
    let a = min(vec2<f32>(low) * stride + center, edge);
    let b = min(vec2<f32>(high) * stride + center, edge);
    let f = clamp((pixel - a) / max(b - a, vec2(1.0)), vec2(0.0), vec2(1.0));
    let g = 1.0 - f;
    let rows = vec4(low.y, low.y, high.y, high.y) * grid.w;
    return AtmosphereFilter(rows + vec4(low.x, high.x, low.x, high.x), vec4(g.x * g.y, f.x * g.y, g.x * f.y, f.x * f.y));
}

// Tint translucent fragments by the part of the atmosphere between them and the camera.
fn through_cloud(c: vec4<f32>, rel: vec3<f32>, view: Scene) -> vec4<f32> {
    let s = view.cloud;
    if s.color.a <= 0.0 {
        return c;
    }
    let dist = length(rel);
    let cloud = trace_atmosphere(s, rel / max(dist, 0.000001), dist, view);
    return vec4(mix(c.rgb, cloud.rgb, cloud.a), c.a);
}

fn brightness_curve(level: f32) -> f32 {
    return level / (4.0 - 3.0 * level);
}

// Light as the world stores it, a byte each for sky, red, green, blue (0..15), to 0..1.
fn unpack_light(l: u32) -> vec4<f32> {
    let v = vec4<f32>(f32(l & 255u), f32((l >> 8u) & 255u), f32((l >> 16u) & 255u), f32(l >> 24u));
    return min(v, vec4(15.0)) / 15.0;
}

// Vanilla 26.1's lightmap: dim ambient, sky light that turns blue and weaker at night, warm block
// light that whitens towards both ends, then the Brightness setting bends it up keeping the hue.
// `day` daylight 0..1, `ambient` the dimension's floor 0..1, `bright` the Brightness setting,
// `night` night vision 0..1: lit everywhere as under the open sky at noon, lamps adding their
// warmth on top (not vanilla's raising of each color to full, which makes a torch at night look
// like a dark patch).
fn lightmap(light: vec4<f32>, day: f32, ambient: f32, bright: f32, night: f32, sunlight: f32) -> vec3<f32> {
    let d = clamp((mix(day, 1.0, night) - 0.12) / 0.88, 0.0, 1.0);
    let sky = brightness_curve(mix(light.x, 1.0, night)) * mix(0.24, 1.0, d) * mix(sunlight, 1.0, night);
    var c = vec3(ambient) + mix(vec3(0.478, 0.478, 1.0), vec3(1.0), d) * sky;
    let block = light.yzw;
    if any(block > vec3(0.0)) {
        // Vanilla's warmth of block light: yellower the dimmer it is, white at full, by the
        // level of its brightest channel so a colored lamp only grows brighter toward it.
        let level = max(max(block.x, block.y), block.z);
        let tint = vec3(1.0, (level * 0.6 + 0.4) * 0.6 + 0.4, level * level * 0.6 + 0.4);
        // Added to the sky's, each channel on its own: a lamp only ever brightens, and a
        // colored one shows by day too.
        c += min(tint * vec3(brightness_curve(block.x), brightness_curve(block.y), brightness_curve(block.z)) * 1.4, vec3(0.85));
    }
    // Light adds up past what a screen shows: what is bright bends toward full instead of
    // being cut there, so sun and lamp together are brighter than the sun alone.
    let knee = 0.8;
    c = select(c, knee + (1.0 - knee) * (1.0 - exp(-(c - knee) / (1.0 - knee))), c > vec3(knee));
    c = clamp(c, vec3(0.0), vec3(1.0));
    let m = max(max(c.x, c.y), c.z);
    if m > 1e-4 {
        let one_minus_m = 1.0 - m;
        let m2 = one_minus_m * one_minus_m;
        let m4 = m2 * m2;
        let lifted = c * ((1.0 - m4) / m);
        return mix(c, lifted, bright);
    }
    return c;
}

// What snippets get (`shaders.rs`), here so a shader pack replacing an engine shader keeps them.

// What a block shader (`shaders/block/<name>.wgsl`, `fn shade(p: BlockPixel) -> vec4<f32>`) gets;
// it returns the lit color before fog.
struct BlockPixel {
    // 0..1 across the face.
    uv: vec2<f32>,
    // The texel (with its overlay) and the light on it (shading, AO, tint, glow), both linear.
    albedo: vec4<f32>,
    light: vec3<f32>,
    // Block coordinates of the pixel (in a moving structure, the structure's own).
    pos: vec3<f32>,
    time: f32,
}

// What an entity shader (`fn shade(p: EntityPixel) -> vec4<f32>`) gets; it returns the lit
// color, alpha below 0.5 cuts the pixel out.
struct EntityPixel {
    uv: vec2<f32>,
    // Skin texel and the light on it (shading, hurt flash and glow included), linear.
    albedo: vec4<f32>,
    light: vec3<f32>,
    time: f32,
}

// What a text effect (`shaders/fx/<name>.wgsl`, `fn effect(g: Glyph) -> Glyph`) may change.
struct Glyph {
    // Added to this corner of the glyph, physical pixels.
    offset: vec2<f32>,
    // Linear color and alpha.
    color: vec4<f32>,
    time: f32,
    // Number of the glyph in its text, for waves running along it.
    index: f32,
    // Which corner, 0..1 in each axis.
    corner: vec2<f32>,
    // Physical pixels per GUI pixel.
    scale: f32,
    // The dark copy drawn under the text.
    shadow: bool,
}

// A themed panel filled by a shader (`shaders/gui/<name>.wgsl`, `fn fill(p: GuiPixel) -> vec4<f32>`).
struct GuiPixel {
    // 0..1 across the panel.
    uv: vec2<f32>,
    // Panel size in physical pixels.
    size: vec2<f32>,
    // The theme color, linear.
    color: vec4<f32>,
    time: f32,
}

// What a screen effect (`shaders/post/<name>.wgsl`, `fn apply(p: PostPixel) -> vec4<f32>`) gets.
struct PostPixel {
    // 0..1 across the screen, top left first.
    uv: vec2<f32>,
    // The image so far at `uv`, linear.
    color: vec4<f32>,
    // 0..1 as the mod set it, for fading in and out.
    strength: f32,
    time: f32,
    // Pixels.
    size: vec2<f32>,
    scene: u32,
}

// What a sky shader (`shaders/sky/<name>.wgsl`, `fn shade(p: SkyPixel) -> vec4<f32>`) gets; it
// returns the color and how much it covers, and the opacity is applied after it.
struct SkyPixel {
    // Unit direction of the pixel in the world, and in the object's own frame, which turns with
    // its orbit (x points at a body).
    dir: vec3<f32>,
    local: vec3<f32>,
    // 0..1 across the quad, or across the picture of a dome.
    uv: vec2<f32>,
    // The picture's sample (white without one) and the object's color, both linear.
    albedo: vec4<f32>,
    color: vec4<f32>,
    daylight: f32,
    time: f32,
}

// What a particle shader (`shaders/particle/<name>.wgsl`, `fn shade(p: ParticleFrag) -> vec4<f32>`)
// gets; it returns the color before lighting and fog.
struct ParticleFrag {
    // 0..1 across the quad.
    uv: vec2<f32>,
    // The texture sample and the kind's color at this age, both linear.
    albedo: vec4<f32>,
    color: vec4<f32>,
    // 0 at birth, 1 at the end of its life.
    life: f32,
    time: f32,
}
fn quat_vector(q: vec4<f32>, v: vec3<f32>) -> vec3<f32> {
    let t = 2.0 * cross(q.xyz, v);
    return v + q.w * t + cross(q.xyz, t);
}
