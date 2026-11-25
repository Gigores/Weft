// Sample the sun at the center of the receiver's world pixel.
const SUN_FAR_FADE_START2: f32 = 220.0 * 220.0;
const SUN_FAR_FADE_END2: f32 = 320.0 * 320.0;
const SUN_OCCLUSION: f32 = 0.72;

fn sun_visibility(view: Scene, relative: vec3<f32>, sky: f32, normal: vec3<f32>) -> f32 {
    if view.sun_grid.w <= 0.0 || sky <= 0.05 {
        return 1.0;
    }
    let incidence = dot(normal, view.sun.xyz);
    // Keep diffuse skylight on faces that receive little or no direct sunlight.
    let direct = smoothstep(0.015, 0.30, incidence);
    let strength = view.sun_grid.w * direct;
    let unoccluded = 1.0 - SUN_OCCLUSION * view.sun_grid.w * (1.0 - direct);
    if direct <= 0.0 {
        return unoccluded;
    }
    let distance2 = dot(relative, relative);
    let has_far = view.sun_far_view[3].w != 0.0;
    let far_distance2 = dot(relative.xz, relative.xz);
    if select(distance2, far_distance2, has_far) >= select(11664.0, SUN_FAR_FADE_END2, has_far) {
        return unoccluded;
    }
    if !has_far {
        let point = near_sun_point(view, relative, normal);
        let near = sun_map(view.sun_view, point, normal, incidence, 0u);
        let fade = 1.0 - smoothstep(0.8, 1.0, max(abs(point.x), abs(point.y)));
        return unoccluded - near * fade * strength;
    }
    if distance2 < 11664.0 {
        let point = near_sun_point(view, relative, normal);
        let edge = sun_camera_edge(view.sun_view, point);
        let depth = point.z - view.sun_view[3].z + 40.0 / f32(SUN_SHADOW_NEAR_DEPTH);
        if depth > 0.02 && depth < 0.98 && edge < 0.93 {
            let near = sun_map(view.sun_view, point, normal, incidence, 0u);
            if edge <= 0.78 && depth >= 0.06 && depth <= 0.94 {
                return unoccluded - near * strength;
            }
            let depth_fade = max(1.0 - smoothstep(0.02, 0.06, depth), smoothstep(0.94, 0.98, depth));
            let far = far_sun_visibility(view, relative, normal, far_distance2);
            return unoccluded - mix(near, far, max(depth_fade, smoothstep(0.78, 0.93, edge))) * strength;
        }
    }
    return unoccluded - far_sun_visibility(view, relative, normal, far_distance2) * strength;
}

fn near_sun_point(view: Scene, relative: vec3<f32>, normal: vec3<f32>) -> vec3<f32> {
    let phase = view.sun_grid.xyz;
    let center = (floor((relative + phase) * 16.0) + 0.5) / 16.0 - phase;
    let snapped = center - normal * (dot(center - relative, normal) / dot(normal, normal));
    return (view.sun_view * vec4(snapped, 1.0)).xyz;
}

fn far_sun_visibility(view: Scene, relative: vec3<f32>, normal: vec3<f32>, distance2: f32) -> f32 {
    let range = 1.0 - smoothstep(SUN_FAR_FADE_START2, SUN_FAR_FADE_END2, distance2);
    let a = far_sun_map(view.sun_far_view, relative, normal, 1u);
    if view.sun_far_blend.x <= 0.0 {
        return a * range;
    }
    let b = far_sun_map(view.sun_far_other_view, relative, normal, 2u);
    return mix(a, b, view.sun_far_blend.x) * range;
}

fn far_sun_map(matrix: mat4x4<f32>, relative: vec3<f32>, normal: vec3<f32>, map: u32) -> f32 {
    let point = (matrix * vec4(relative, 1.0)).xyz;
    let fade = 1.0 - smoothstep(0.80, 0.93, sun_camera_edge(matrix, point));
    let direction = vec3(matrix[0].z, matrix[1].z, matrix[2].z) * f32(SUN_SHADOW_FAR_DEPTH);
    let incidence = max(0.015, dot(direction, normal));
    return sun_map(matrix, point, normal, incidence, map) * fade;
}

// Fade with the camera, while reads stay on the world-anchored texel grid.
fn sun_camera_edge(matrix: mat4x4<f32>, point: vec3<f32>) -> f32 {
    let edge = abs(point.xy - matrix[3].xy);
    return max(edge.x, edge.y);
}

fn sun_map(matrix: mat4x4<f32>, point: vec3<f32>, normal: vec3<f32>, incidence: f32, map: u32) -> f32 {
    let far = map != 0u;
    if max(abs(point.x), abs(point.y)) >= 1.0 || point.z <= 0.0 || point.z >= 1.0 {
        return 0.0;
    }
    let size = select(SUN_SHADOW_NEAR_SIZE, SUN_SHADOW_FAR_SIZE, far);
    let start = select(0u, SUN_SHADOW_NEAR_SIZE * SUN_SHADOW_NEAR_SIZE + (max(map, 1u) - 1u) * SUN_SHADOW_FAR_SIZE * SUN_SHADOW_FAR_SIZE, far);
    let uv = vec2(point.x + 1.0, 1.0 - point.y) * (f32(size) * 0.5) - 0.5;
    let base = vec2<i32>(floor(uv));
    let weight = fract(uv);
    // Compare at each texel center on the same plane. Blending the comparisons gives
    // each world pixel coverage without blurring the boundary between world pixels.
    let sx = vec3(matrix[0].x, matrix[1].x, matrix[2].x);
    let sy = vec3(matrix[0].y, matrix[1].y, matrix[2].y);
    let scale = select(
        2.0 * f32(SUN_SHADOW_NEAR_HALF * SUN_SHADOW_NEAR_HALF) / f32(SUN_SHADOW_NEAR_SIZE * SUN_SHADOW_NEAR_DEPTH),
        2.0 * f32(SUN_SHADOW_FAR_HALF * SUN_SHADOW_FAR_HALF) / f32(SUN_SHADOW_FAR_SIZE * SUN_SHADOW_FAR_DEPTH),
        far,
    );
    let slope = vec2(-dot(normal, sx), dot(normal, sy)) * (scale / incidence);
    let lo = vec2<u32>(max(base, vec2(0)));
    let hi = vec2<u32>(min(base + 1, vec2(i32(size) - 1)));
    let x = vec2(f32(lo.x), f32(hi.x)) - uv.x;
    let y = vec2(f32(lo.y), f32(hi.y)) - uv.y;
    let depth_range = select(f32(SUN_SHADOW_NEAR_DEPTH), f32(SUN_SHADOW_FAR_DEPTH), far);
    // The coarse silhouette can sit above its corresponding detailed surface.
    let bias = select(0.00012 + 0.004 / (max(0.15, incidence) * depth_range),
        (0.25 + 0.65 / max(0.25, incidence)) / depth_range, far);
    let receiver = point.z + bias;
    let feather = select(12500.0, depth_range * 2.0, far);
    let a = sun_coverage(start + lo.y * size, lo.x, hi.x, receiver + y.x * slope.y + x * slope.x, weight.x, feather);
    let b = sun_coverage(start + hi.y * size, lo.x, hi.x, receiver + y.y * slope.y + x * slope.x, weight.x, feather);
    return mix(a, b, weight.y) * SUN_OCCLUSION;
}

fn sun_coverage(row: u32, x0: u32, x1: u32, receiver: vec2<f32>, weight: f32, feather: f32) -> f32 {
    let depth = vec2(sun_depth[row + x0], sun_depth[row + x1]);
    let covered = select(vec2(0.0), clamp((depth - receiver) * feather, vec2(0.0), vec2(1.0)), depth > vec2(0.0));
    return mix(covered.x, covered.y, weight);
}
