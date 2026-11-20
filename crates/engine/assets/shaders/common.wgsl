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

fn cloud_field(seed: u32, origin: vec2<i32>, at: vec2<f32>) -> vec3<f32> {
    let warped = cloud_warp(seed, origin, at);
    let coarse = cloud_noise(seed, origin, warped, vec2(1, 2));
    let detail = cloud_noise(seed ^ 0x3ca1ef53u, origin, warped, vec2(3, 6));
    let partial = coarse * 0.64 + detail * 0.23;
    if partial + 0.13 <= 0.47 {
        return vec3(0.0);
    }
    let edge = cloud_noise(seed ^ 0x5a528247u, origin, warped, vec2(9, 16));
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

// Walk each intersected band front to back, stepping exactly at voxel boundaries.
fn cloud_volume(s: Sky, dir: vec3<f32>, near: f32, far: f32, view: Scene) -> CloudHit {
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
    let has_sun = s.cloud_sun.w > 0.0;
    var scattered_sun = vec3(0.0);
    var sun_from_above = 1.0;
    if has_sun {
        // Two polynomial forward lobes approximate the broad glow and bright rim.
        // The view and light directions are constant along this ray.
        let cosine = dot(dir, s.axes[2].xyz);
        let forward = max(cosine, 0.0);
        let square = forward * forward;
        let fourth = square * square;
        let eighth = fourth * fourth;
        let narrow = eighth * eighth;
        let back = max(-cosine, 0.0);
        scattered_sun = s.cloud_sun.rgb * (0.14 + 0.52 * eighth + 1.02 * narrow * narrow + 0.10 * back * back);
        sun_from_above = smoothstep(-0.06, 0.22, s.axes[2].y);
    }
    var light = vec3(0.0);
    var alpha = 0.0;
    var first = far;
    var optical_distance = 0.0;
    for (var band = first_band; band != last_band + direction && alpha < 0.99; band += direction) {
        let boundary = f32(band) + select(0.0, 1.0, dir.y < 0.0);
        let enter = (boundary - height_at_camera) * band_time * f32(direction);
        let band_start = select(max(enter, near), near, band == first_band);
        let band_end = select(min(enter + band_time, far), far, band == last_band);
        let shift = cloud_band_offset(band);
        let start = s.uv.xy * CLOUD_COLUMNS + ray * band_start - shift;
        var cell = floor(start);
        let offset = select(start - cell, cell + vec2(1.0) - start, ray > vec2(0.0));
        var next = select(band_start + offset * delta, vec2(1e12), ray == vec2(0.0));
        let shade = 0.82 + 0.050 * f32(band);
        let sun_path = mix(f32(band) + 0.5, 4.5 - f32(band), sun_from_above) * s.cloud_sun.w;
        var t = band_start;
        while t < band_end && alpha < 0.99 {
            let end = min(band_end, min(next.x, next.y));
            let field = cloud_field(s.ids.z, origin, (cell + shift + vec2(0.5)) / CLOUD_COLUMNS);
            if field.x > 0.0 && end > t {
                let fill = cloud_band_fill(field, band);
                if fill > 0.0 {
                    let density = smoothstep(0.0, 0.8, fill) * (0.6 + 0.4 * smoothstep(0.0, 0.65, field.x));
                    let kind = select(s.extra.w, field.y, s.ids.w != 0u);
                    let extinction = select(0.085, select(0.145, 0.22, kind == 2.0), kind > 0.0);
                    let absorption = (1.0 - alpha) * (1.0 - exp(-density * (end - t) * extinction));
                    var base = s.color.rgb;
                    if s.ids.w != 0u && field.y > 0.0 {
                        base = select(s.axes[0].xyz, s.axes[1].xyz, field.y == 2.0);
                    }
                    var radiance = base * shade;
                    if has_sun {
                        // Approximate how far the light travels through the cloud from
                        // the local density, band height and solar elevation. Reuse the
                        // view sample instead of marching a second ray into the sun.
                        let depth = density * sun_path * (1.0 + 0.30 * kind);
                        radiance += scattered_sun * (1.0 - 0.97 * smoothstep(0.0, 1.0, depth));
                    }
                    first = min(first, t);
                    optical_distance += (t + end) * 0.5 * absorption;
                    light += radiance * absorption;
                    alpha += absorption;
                }
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
    let distance = optical_distance / max(alpha, 0.00001);
    let fog = fog_amount(distance, view.environment.y, view.environment.z);
    let haze = max(smoothstep(0.0, 1.0, fog), rain_mist_amount(distance, view));
    let color = mix(light / max(alpha, 0.00001), view.fog_color.rgb, haze);
    return CloudHit(vec4(color, alpha * s.color.a * (1.0 - smoothstep(0.6, 1.0, fog))), first, distance);
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

// A soft mass beneath the cloud. Only the weather field moves it, with the cloud's drift.
fn composite_rain_veil(s: Sky, dir: vec3<f32>, limit: f32, cloud: CloudHit, view: Scene) -> vec4<f32> {
    if s.ids.w == 0u || max(s.axes[0].w, s.axes[1].w) <= 0.0 {
        return cloud.color;
    }
    let top = s.extra.z + s.extra.y * 0.27;
    let ceiling = top + 18.0;
    let reach = min(1536.0, view.environment.z);
    var near = 36.0;
    var far = min(limit, reach);
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
    let origin = vec2<i32>(s.uv.zw);
    let ray = dir.xz / s.extra.x;
    var light = vec3(0.0);
    var alpha = 0.0;
    var front_alpha = 0.0;
    // Fixed bin boundaries keep samples from jumping when a ray meets the ground
    // or moves from one side of the cloud base to the other.
    for (var bin = floor(near / 36.0) * 36.0; bin < far && alpha < 0.88; bin += 36.0) {
        let start = max(bin, near);
        let end = min(bin + 36.0, far);
        let t = (start + end) * 0.5;
        let at = s.uv.xy + ray * t;
        // Extruding the fine cloud pattern to the ground turns each small
        // variation into a stripe. Keep only the broad cloud footprint here.
        let coarse = cloud_noise(s.ids.z, origin, cloud_warp(s.ids.z, origin, at), vec2(1, 2));
        if coarse <= 0.48 {
            continue;
        }
        let climate = cloud_climate(s.ids.z, origin, at);
        let storm = climate >= 0.77;
        let rain_band = smoothstep(0.57, 0.63, climate) * (1.0 - smoothstep(0.66, 0.72, climate));
        let storm_band = smoothstep(0.83, 0.90, climate);
        let wetness = select(rain_band, storm_band, storm);
        let preset = select(s.axes[0], s.axes[1], storm);
        if wetness <= 0.0 || preset.w <= 0.0 {
            continue;
        }
        let attach = smoothstep(-18.0, 28.0, top - dir.y * t);
        let range = smoothstep(36.0, 105.0, t) * (1.0 - smoothstep(reach * 0.55, reach, t));
        let density = smoothstep(0.48, 0.74, coarse) * wetness * attach * range * preset.w * s.color.a;
        let extinction = density * select(0.0075, 0.012, storm);
        let absorption = (1.0 - alpha) * (1.0 - exp(-extinction * (end - start)));
        // Place the cloud at its opacity-weighted distance along the ray.
        // Split the one rain bin crossing that distance without resampling the field.
        var front_absorption = 0.0;
        if cloud.optical_distance >= end {
            front_absorption = absorption;
        } else if cloud.optical_distance > start {
            front_absorption = (1.0 - alpha) * (1.0 - exp(-extinction * (cloud.optical_distance - start)));
        }
        let luma = vec3(0.30, 0.59, 0.11);
        let color = mix(vec3(dot(preset.rgb, luma) * 0.55), mix(vec3(dot(view.fog_color.rgb, luma)), view.fog_color.rgb, 0.28), 0.58);
        light += mix(color, view.fog_color.rgb, rain_mist_amount(t, view)) * mix(absorption, front_absorption, cloud.color.a);
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
