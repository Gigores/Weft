// The sky, drawn after the opaque world wherever nothing covers it. Every object is one entry of
// `sky`; things that fill the screen are a single triangle, bodies a quad, stars one quad per star
// made here from their number.

var<private> CORNER: array<vec2<f32>, 6> = array<vec2<f32>, 6>(
    vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(1.0, 1.0), vec2(0.0, 0.0), vec2(1.0, 1.0), vec2(0.0, 1.0),
);

struct VOut {
    @builtin(position) clip: vec4<f32>,
    // Screen position (-1..1) for what fills the screen, else the direction.
    @location(0) at: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) @interpolate(flat) index: u32,
}

fn mat(s: Sky) -> mat3x3<f32> {
    return mat3x3(s.axes[0].xyz, s.axes[1].xyz, s.axes[2].xyz);
}

fn hash(v: u32) -> u32 {
    let s = v * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}

fn unit(v: u32) -> f32 {
    return f32(hash(v)) / 4294967295.0;
}

fn far_point(dir: vec3<f32>) -> vec4<f32> {
    // At depth 0, the far plane: the sky is behind everything.
    let c = scene.view_proj * vec4(dir * 100.0, 1.0);
    return vec4(c.xy, 0.0, c.w);
}

@vertex
fn vs(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VOut {
    let s = sky[ii];
    var out: VOut;
    out.index = ii;
    if s.ids.y == SKY_BODY {
        let c = CORNER[vi % 6u];
        let local = vec3(1.0, (0.5 - c.y) * 2.0 * s.extra.x, (c.x - 0.5) * 2.0 * s.extra.x);
        let dir = mat(s) * local;
        out.clip = far_point(dir);
        out.at = dir;
        out.uv = mix(s.uv.xy, s.uv.zw, c);
    } else if s.ids.y == SKY_STARS {
        // A random point on the sphere, a random size and turn, from the star's number alone.
        let star = vi / 6u + u32(s.uv.y) * 7919u;
        let z = unit(star * 4u) * 2.0 - 1.0;
        let a = unit(star * 4u + 1u) * 6.2831853;
        let r = sqrt(1.0 - z * z);
        let toward = vec3(r * cos(a), z, r * sin(a));
        let size = s.uv.x * (0.6 + 0.4 * unit(star * 4u + 2u)) * 2.0;
        let turn = unit(star * 4u + 3u) * 6.2831853;
        var side = cross(toward, vec3(0.0, 1.0, 0.0));
        if dot(side, side) < 0.001 {
            side = vec3(1.0, 0.0, 0.0);
        }
        side = normalize(side);
        let up = cross(side, toward);
        let c = CORNER[vi % 6u] - vec2(0.5);
        let q = vec2(c.x * cos(turn) - c.y * sin(turn), c.x * sin(turn) + c.y * cos(turn));
        let dir = mat(s) * (toward + (side * q.x + up * q.y) * size);
        out.clip = far_point(dir);
        out.at = dir;
        out.uv = CORNER[vi % 6u];
    } else if s.ids.y == SKY_LIGHTNING {
        let c = CORNER[vi % 6u];
        let start = s.axes[0].xyz;
        let end = s.axes[1].xyz;
        let middle = (start + end) * 0.5;
        let side = cross(end - start, middle);
        let normal = select(scene.right.xyz, side / max(length(side), 0.0001), dot(side, side) > 0.0001);
        let width = max(s.extra.x * 2.5, s.extra.y * length(middle));
        let pos = mix(start, end, c.y) + normal * ((c.x * 2.0 - 1.0) * width);
        out.clip = scene.view_proj * vec4(pos, 1.0);
        out.at = pos;
        out.uv = c;
    } else {
        let p = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u)) * 2.0 - 1.0;
        // The overlay of a liquid goes over everything, the hand too; depth 1 is the near plane.
        out.clip = vec4(p, select(0.0, 1.0, s.ids.y == SKY_OVERLAY), 1.0);
        out.at = vec3(p, 1.0);
        out.uv = vec2(0.0);
    }
    return out;
}

// Clouds: each pixel's ray walks the picture's grid between the two planes of the layer, so one
// triangle draws them however far they reach. From outside the layer a ray crosses few cells even
// at the horizon, since it soon leaves through the other plane.
// uv: the camera's place in its cell (x, z), the cell in the picture (x, z). extra: cell size,
// thickness and height over the camera. Scene environment.yz supplies cloud fog distances.
struct CloudOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) at: vec3<f32>,
    @location(1) @interpolate(flat) index: u32,
}

struct CloudPixel {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
}

fn cloud_triangle(vi: u32, ii: u32) -> CloudOut {
    let p = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u)) * 2.0 - 1.0;
    var out: CloudOut;
    out.clip = vec4(p, 0.0, 1.0);
    out.at = vec3(p, 1.0);
    out.index = ii;
    return out;
}

@vertex
fn vs_clouds(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> CloudOut {
    return cloud_triangle(vi, ii);
}

@fragment
fn fs_clouds(in: CloudOut) -> CloudPixel {
    let dir = normalize(mat(sky[0]) * in.at);
    let hit = trace_cloud(sky[in.index], dir, scene.environment.z, scene);
    if hit.color.a <= 0.0 {
        discard;
    }
    let clip = scene.view_proj * vec4(dir * max(hit.distance, 0.001), 1.0);
    return CloudPixel(vec4(hit.color.rgb * hit.color.a, hit.color.a), clamp(clip.z / clip.w, 0.0, 1.0));
}

fn atmosphere_limit(dir: vec3<f32>, depth: f32) -> f32 {
    let slope = (scene.view_proj * vec4(dir, 0.0)).w;
    // Infinite reverse Z, with the optional translation from camera bobbing.
    let at_origin = scene.view_proj[3];
    let limit = (at_origin.z - depth * at_origin.w) / max(depth * slope, 1e-8);
    return select(limit, min(limit, scene.params.y), scene.environment.w >= 1.0);
}

fn atmosphere_at(s: Sky, dir: vec3<f32>, depth: f32, pixel: vec2<f32>) -> vec4<f32> {
    let limit = atmosphere_limit(dir, depth);
    return resolve_rain(s, dir, limit, trace_cloud(s, dir, limit, scene), scene, pixel);
}

@vertex
fn vs_atmosphere(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> CloudOut {
    var out = cloud_triangle(vi, ii);
    let s = sky[ii];
    let slab = max(0.0, max(s.extra.z, -s.extra.z - s.extra.y));
    let wet = max(s.axes[0].w, s.axes[1].w) > 0.0;
    let closest = select(slab, min(36.0, slab), wet);
    // Use the smallest forward component over the viewport, so the depth test can
    // reject opaque geometry only when it is in front of every possible effect.
    let m = mat(sky[0]);
    let center = m * vec3(0.0, 0.0, 1.0);
    let longest = sqrt(dot(center, center) + dot(m[0], m[0]) + dot(m[1], m[1]));
    let forward = (scene.view_proj * vec4(center, 0.0)).w * closest / max(longest, 0.000001);
    let origin = scene.view_proj[3];
    out.clip.z = clamp(origin.z / max(forward + origin.w, 0.000001), 0.0, 1.0);
    return out;
}

@fragment
fn fs_atmosphere(in: CloudOut) -> @location(0) vec4<f32> {
    let pixel = vec2<u32>(in.clip.xy);
    let depth = world_depth[pixel.y * scene.atmosphere_extent.x + pixel.x];
    return atmosphere_at(sky[in.index], normalize(mat(sky[0]) * in.at), depth, in.clip.xy);
}

fn atmosphere_sample_pixel(cell: vec2<u32>, size: vec2<u32>, stride: u32) -> vec2<u32> {
    return min(cell * stride + vec2(stride / 2u), size - vec2(1u));
}

var<workgroup> occupied_cloud_brick: atomic<u32>;
var<workgroup> keep_cloud_brick: u32;

@compute @workgroup_size(8, 8)
fn cache_cloud_cells(
    @builtin(workgroup_id) brick: vec3<u32>,
    @builtin(local_invocation_id) lane: vec3<u32>,
    @builtin(local_invocation_index) index: u32,
) {
    let side = i32(scene.atmosphere_cache.x);
    let start = scene.cloud_cache.xy + vec2<i32>(brick.xy) * CLOUD_BRICK;
    let previous = scene.cloud_cache.zw;
    // The toroidal buffer leaves overlapping world bricks at the same addresses.
    if index == 0u {
        keep_cloud_brick = u32(previous.x != -2147483647 && all(start >= previous) && all(start < previous + vec2(side)));
        atomicStore(&occupied_cloud_brick, 0u);
    }
    if workgroupUniformLoad(&keep_cloud_brick) != 0u {
        return;
    }
    let cell = start + vec2<i32>(lane.xy);
    // Floor division is required on the negative side of the weather origin.
    let origin = (cell - (cell % 24 + 24) % 24) / 24;
    let local_cell = vec2<f32>(cell - origin * 24);
    let band = i32(brick.z);
    let packed = pack2x16float(cloud_cell_density(scene.cloud.ids.z, origin, local_cell, band));
    cloud_cells_out[cloud_cache_index(cell, band, side)] = packed;
    if (packed & 0xffffu) != 0u {
        atomicOr(&occupied_cloud_brick, 1u);
    }
    workgroupBarrier();
    if index == 0u {
        cloud_cells_out[cloud_brick_index(start, band, side)] = atomicLoad(&occupied_cloud_brick);
    }
}

fn unnormalized_atmosphere_ray(pixel: vec2<f32>, size: vec2<u32>) -> vec3<f32> {
    let ndc = (pixel / vec2<f32>(size)) * vec2(2.0, -2.0) + vec2(-1.0, 1.0);
    return mat(sky[0]) * vec3(ndc, 1.0);
}

fn atmosphere_ray(pixel: vec2<f32>, size: vec2<u32>) -> vec3<f32> {
    return normalize(unnormalized_atmosphere_ray(pixel, size));
}

fn rain_visible_range(p: vec2<u32>, size: vec2<u32>, reach: f32) -> f32 {
    if scene.atmosphere_cache.z == 0u || min(size.x, size.y) < 16u {
        return reach;
    }
    // Cover the whole support of bilinear rain interpolation with 16-pixel depth tiles.
    let lo = vec2<u32>(max(vec2<i32>(p) - i32(RAIN_STRIDE), vec2(0)));
    let hi = min(p + RAIN_STRIDE, size - 1u);
    var dims = size;
    var offset = 0u;
    for (var level = 0u; level < 4u; level++) {
        offset += dims.x * dims.y;
        dims = (dims + 1u) / 2u;
    }
    let first = lo / 16u;
    let last = hi / 16u;
    var depth = 1.0;
    for (var y = first.y; y <= last.y; y++) {
        for (var x = first.x; x <= last.x; x++) {
            depth = min(depth, world_depth[offset + y * dims.x + x]);
        }
    }
    if depth <= 0.0 {
        return reach;
    }
    let at_origin = scene.view_proj[3];
    let forward = (at_origin.z - depth * at_origin.w) / depth;
    var min_slope = 1e6;
    var max_length = 0.0;
    for (var corner = 0u; corner < 4u; corner++) {
        let pixel = select(lo, hi, vec2((corner & 1u) != 0u, (corner & 2u) != 0u));
        let ray = unnormalized_atmosphere_ray(vec2<f32>(pixel) + 0.5, size);
        min_slope = min(min_slope, (scene.view_proj * vec4(ray, 0.0)).w);
        max_length = max(max_length, length(ray));
    }
    return min(reach, forward * max_length / max(min_slope, 1e-8));
}

fn rain_cover_boundary(dir: vec3<f32>, a: f32, b: f32, exposed_end: bool) -> f32 {
    var low = a;
    var high = b;
    for (var i = 0u; i < 5u; i++) {
        let middle = (low + high) * 0.5;
        if rain_exposed(scene, dir * middle) == exposed_end {
            high = middle;
        } else {
            low = middle;
        }
    }
    // Keep the side known to contain rain.
    return select(low, high, exposed_end);
}

// A sample also feeds its neighboring pixels during bilinear reconstruction.
fn rain_sample_can_light_bow(s: Sky, dir: vec3<f32>, size: vec2<u32>) -> bool {
    let sun = s.axes[2].xyz;
    if s.cloud_sun.w <= 0.0 || sun.y <= 0.0 {
        return false;
    }
    let axes = mat(sky[0]);
    let reach = f32(RAIN_STRIDE) * 2.0 / vec2<f32>(size);
    let margin = (reach.x * length(axes[0]) + reach.y * length(axes[1])) / max(length(axes[2]), 1e-6) + 1e-5;
    let cosine = -dot(dir, sun);
    return cosine + margin > RAINBOW_COS_MIN && cosine - margin < RAINBOW_COS_MAX;
}

@compute @workgroup_size(8, 8)
fn cache_rain_media(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = scene.atmosphere_extent.xy;
    let grid = (size + (RAIN_STRIDE - 1u)) / RAIN_STRIDE;
    if any(id.xy >= grid) {
        return;
    }
    let p = atmosphere_sample_pixel(id.xy, size, RAIN_STRIDE);
    let pixel = vec2<f32>(p) + 0.5;
    let dir = atmosphere_ray(pixel, size);
    let s = scene.cloud;
    let reach = min(1536.0, scene.environment.z);
    let ceiling = rain_ceiling(s);
    var enter = 36.0;
    var leave = select(reach, min(reach, scene.params.y), scene.environment.w >= 1.0);
    leave = rain_visible_range(p, size, leave);
    if dir.y > 0.0 {
        leave = min(leave, ceiling / dir.y);
    } else if dir.y < 0.0 {
        enter = max(enter, ceiling / dir.y);
    } else if ceiling < 0.0 {
        leave = 0.0;
    }
    let may_show_rainbow = rain_sample_can_light_bow(s, dir, size);
    let offset = (id.y * grid.x + id.x) * (RAIN_SLICES + 1u);
    var rain = vec4(0.0);
    var first_wet = leave;
    for (var layer = 1u; layer <= RAIN_SLICES; layer += 1u) {
        let start = f32(layer - 1u) * RAIN_SLICE;
        let end = min(start + RAIN_SLICE, leave);
        for (var t = start; t < end && rain.a < 0.88; t += 32.0) {
            var a = max(t, enter);
            var b = min(t + 32.0, end);
            if b > a {
                let middle = (a + b) * 0.5;
                if !rain_exposed(scene, dir * middle) {
                    if rain_exposed(scene, dir * a) {
                        b = rain_cover_boundary(dir, a, middle, false);
                    } else if rain_exposed(scene, dir * b) {
                        a = rain_cover_boundary(dir, middle, b, true);
                    } else {
                        continue;
                    }
                } else {
                    if !rain_exposed(scene, dir * a) {
                        a = rain_cover_boundary(dir, a, middle, true);
                    }
                    if !rain_exposed(scene, dir * b) {
                        b = rain_cover_boundary(dir, middle, b, false);
                    }
                }
                let medium = rain_coefficients(s, dir, (a + b) * 0.5, reach, may_show_rainbow, scene);
                if medium.a <= 0.0 || b <= a {
                    continue;
                }
                first_wet = min(first_wet, a);
                let absorption = (1.0 - rain.a) * (1.0 - exp(-medium.a * (b - a)));
                rain += vec4(medium.rgb, 1.0) * absorption;
            }
        }
        rain_depth_out[offset + layer] = vec2(pack2x16float(rain.rg), pack2x16float(rain.ba));
    }
    rain_depth_out[offset] = bitcast<vec2<u32>>(vec2(first_wet, leave));
}

// Vanilla's sky is a disc 16 blocks above the eye fading into fog with distance.
fn gradient(dir: vec3<f32>, s: Sky) -> vec4<f32> {
    if dir.y <= 0.0 {
        // Under the horizon: fog, and from below sea level the dark of vanilla's void.
        let dark = s.uv.x * smoothstep(0.0, 0.08, -dir.y);
        return vec4(scene.fog_color.rgb * (1.0 - dark) * s.color.a, s.color.a);
    }
    return vec4(mix(s.color.rgb, scene.fog_color.rgb, clamp(0.0625 / dir.y, 0.0, 1.0)) * s.color.a, s.color.a);
}

// Vanilla's sunset fan: an oval lying on the horizon on the side of the body.
fn glow(dir: vec3<f32>, s: Sky) -> vec4<f32> {
    let toward = dot(dir, s.axes[0].xyz);
    if toward <= 0.0 {
        return vec4(0.0);
    }
    let x = dot(dir, s.axes[2].xyz) / toward / 1.2;
    let y = dir.y / toward / max(0.4 * s.color.a, 0.001);
    let a = s.color.a * max(0.0, 1.0 - sqrt(x * x + y * y));
    return vec4(s.color.rgb, a);
}

// Panorama: longitude and latitude. Box: three by two faces; first row down, up, south; second
// west, north, east.
fn dome_uv(l: vec3<f32>, s: Sky) -> vec2<f32> {
    if s.uv.x == 0.0 {
        return vec2(atan2(l.x, -l.z) / 6.2831853 + 0.5, 0.5 - asin(clamp(l.y, -1.0, 1.0)) / 3.1415927);
    }
    let m = abs(l);
    var face: vec2<f32>;
    var cell: vec2<f32>;
    if m.y >= m.x && m.y >= m.z {
        face = vec2(l.x, select(-l.z, l.z, l.y > 0.0)) / m.y;
        cell = vec2(select(0.0, 1.0, l.y > 0.0), 0.0);
    } else if m.z >= m.x {
        face = vec2(select(l.x, -l.x, l.z > 0.0), -l.y) / m.z;
        cell = select(vec2(1.0, 1.0), vec2(2.0, 0.0), l.z > 0.0);
    } else {
        face = vec2(select(-l.z, l.z, l.x > 0.0), -l.y) / m.x;
        cell = select(vec2(0.0, 1.0), vec2(2.0, 1.0), l.x > 0.0);
    }
    return (cell + face * 0.5 + 0.5) / vec2(3.0, 2.0);
}

@fragment
fn fs_lightning(in: VOut) -> @location(0) vec4<f32> {
    let s = sky[in.index];
    let edge = abs(in.uv.x * 2.0 - 1.0);
    let core = 1.0 - smoothstep(0.15, 0.39, edge);
    let halo = (1.0 - smoothstep(0.12, 1.0, edge)) * 0.32;
    let alpha = min(s.color.a * (core * 1.9 + halo), 1.0);
    if alpha < 0.002 {
        return vec4(0.0);
    }
    var color = mix(s.color.rgb, vec3(0.96, 0.98, 1.0), core);
    if scene.atmosphere_extent.x != 0u {
        // The channel is already over the composited sky. Sample only the air in front of it.
        let range = length(in.at);
        let dir = in.at / max(range, 0.001);
        let cloud = trace_cloud(scene.cloud, dir, range, scene);
        let front = resolve_rain(scene.cloud, dir, range, cloud, scene, in.clip.xy);
        color = front.rgb + color * (1.0 - front.a);
    }
    return vec4(color * alpha, alpha);
}

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    let s = sky[in.index];
    if s.ids.y == SKY_OVERLAY {
        // Vanilla's: four tiles across the screen, sliding with the look. A texture with
        // animation frames shows its first.
        if s.extra.y != 0.0 && in.at.y > -0.25 {
            return vec4(0.0);
        }
        let tile = fract((in.at.xy * vec2(0.5, -0.5) + 0.5) * s.extra.x + s.uv.xy) * s.uv.zw + vec2(0.0, s.extra.z);
        let c = textureSampleLevel(textures[s.ids.x], nearest_clamp, tile, 0.0) * s.color;
        return vec4(c.rgb * c.a, c.a);
    }
    var dir = in.at;
    if s.ids.y == SKY_GRADIENT || s.ids.y == SKY_GLOW || s.ids.y == SKY_DOME {
        dir = mat(sky[0]) * in.at;
    }
    dir = normalize(dir);
    let mist = scene.environment.w;
    if s.ids.y == SKY_GRADIENT {
        let c = gradient(dir, s);
        return vec4(mix(c.rgb, scene.fog_color.rgb * c.a, mist), c.a);
    }
    if s.ids.y == SKY_GLOW {
        let c = glow(dir, s);
        let a = c.a * (1.0 - mist);
        return vec4(c.rgb * a, a);
    }
    let local = dir * mat(s);
    var uv = in.uv;
    var albedo = vec4(1.0);
    if s.ids.y == SKY_DOME {
        uv = dome_uv(local, s);
        if s.ids.x != 0xffffffffu {
            albedo = textureSampleLevel(textures[s.ids.x], linear_clamp, uv, 0.0);
        }
    } else if s.ids.x != 0xffffffffu {
        albedo = textureSampleLevel(textures[s.ids.x], nearest_clamp, uv, 0.0);
    }
    let c = sky_shade(s.ids.z, SkyPixel(dir, local, uv, albedo, vec4(s.color.rgb, 1.0), scene.params.z, scene.params.w));
    let a = c.a * s.color.a * (1.0 - mist);
    return vec4(c.rgb * a, select(a, 0.0, s.ids.w != 0u));
}
