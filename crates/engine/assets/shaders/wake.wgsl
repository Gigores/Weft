var<private> CORNER: array<vec2<f32>, 6> = array<vec2<f32>, 6>(
    vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(1.0, 1.0), vec2(0.0, 0.0), vec2(1.0, 1.0), vec2(0.0, 1.0),
);

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) rel: vec3<f32>,
    @location(1) wake: vec4<f32>,
    @location(2) @interpolate(flat) style: vec2<u32>,
    @location(3) @interpolate(flat) noise_origin: vec2<f32>,
}

@vertex
fn vs(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VOut {
    let strip = strips[ii];
    let uv = CORNER[vi];
    let start_edge = mix(strip.left_from, strip.right_from, uv.x);
    let end_edge = mix(strip.left_to, strip.right_to, uv.x);
    let position = mix(start_edge, end_edge, uv.y);
    let distance = mix(strip.course.x, strip.course.y, uv.y);
    let age = mix(strip.course.z, strip.course.w, uv.y);
    return VOut(scene.view_proj * vec4(position.xyz, 1.0), position.xyz, vec4(uv.x * 2.0 - 1.0, distance, age, position.w), strip.style.xy, strip.course.xy);
}

fn grain(p: vec2<f32>) -> f32 {
    return fract(sin(dot(p, vec2(47.27, 153.61))) * 43758.5453);
}

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    let across = in.wake.x;
    let along = in.wake.y - in.wake.z * 0.6;
    let noise = grain(floor(vec2(across * 18.0, along * 11.0)));
    let foam = smoothstep(0.17, 0.83, noise);
    let ridge = 0.5 + 0.5 * sin(along * 6.3 + across * across * 4.6);
    let edge = 1.0 - smoothstep(0.55, 1.0, abs(across) + 0.15 * (noise - 0.5));
    var alpha = edge * (0.16 + 0.38 * foam + 0.14 * ridge);
    var water_white = mix(vec3(0.69, 0.85, 0.92), vec3(0.96, 1.0, 1.0), foam);
    if in.style.x != 0u {
        let across_sheet = in.wake.z;
        let life = bitcast<f32>(in.style.y);
        // Sample the same size cells along straights and corners, with a stable origin as the camera moves.
        let cell = vec2<i32>(floor((in.rel.xz + in.noise_origin) * 16.0)) & vec2<i32>(4095);
        let spray = grain(vec2<f32>(cell));
        let outer = smoothstep(0.36, 0.60, across_sheet);
        let tears = smoothstep(0.32, 0.83, spray);
        let breakup = mix(1.0, tears, outer * (0.56 + 0.44 * life));
        let edge_fade = smoothstep(0.0, 0.12, across_sheet) * (1.0 - smoothstep(0.62, 1.0, across_sheet));
        let lip = smoothstep(0.40, 0.64, across_sheet) * (1.0 - smoothstep(0.69, 0.86, across_sheet));
        let rim_foam = lip * smoothstep(0.25, 0.85, spray);
        alpha = breakup * edge_fade * (0.24 + 0.12 * spray + 0.21 * rim_foam);
        water_white = mix(vec3(0.43, 0.69, 0.88), vec3(0.95, 0.99, 1.0), 0.14 + 0.62 * rim_foam);
    }
    let light = lightmap(unpack_light(15u), scene.params.z, scene.light.x, scene.light.y, scene.light.w, scene.environment.x);
    let fog = fog_amount(length(in.rel), scene.params.x, scene.params.y);
    let color = mix(water_white * pow(light, vec3(2.2)), scene.fog_color.rgb, fog);
    return through_cloud(vec4(color, alpha * in.wake.w), in.rel, scene);
}
