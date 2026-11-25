// Entity models: boxes and free-form triangles of every kind in two static tables; each entity
// layer (skin or something worn) is one instance pointing at its bone matrices, posed on the CPU
// (hierarchy, keyframes, vanilla's limb swing) and uploaded per frame.

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec3<f32>,
    @location(2) @interpolate(flat) tex: u32,
    @location(3) dist: f32,
    @location(4) @interpolate(flat) shader: u32,
    @location(5) @interpolate(flat) glow: u32,
    @location(6) @interpolate(flat) effect: u32,
    @location(7) @interpolate(flat) strength: f32,
    @location(8) rel: vec3<f32>,
    @location(9) @interpolate(flat) sky: f32,
    @location(10) ambient: vec3<f32>,
    @location(11) @interpolate(flat) normal: vec3<f32>,
}

// A layer draws the parts of its roles, and those covering the bones it names.
fn drawn(inst: Instance, role: u32, bone: u32) -> bool {
    return ((inst.roles >> role) & 1u) != 0u || (bone < 32u && ((inst.bone_mask >> bone) & 1u) != 0u);
}

fn place(bone: u32, p: vec3<f32>) -> vec3<f32> {
    let b = bones[bone];
    let q = vec4(p, 1.0);
    return vec3(dot(b.rows[0], q), dot(b.rows[1], q), dot(b.rows[2], q));
}

// A local normal in world space, through the bone's cofactors so stretched and flattened bones
// shade right too.
fn turn_normal(bone: u32, n: vec3<f32>) -> vec3<f32> {
    let b = bones[bone];
    let x = vec3(b.rows[0].x, b.rows[1].x, b.rows[2].x);
    let y = vec3(b.rows[0].y, b.rows[1].y, b.rows[2].y);
    let z = vec3(b.rows[0].z, b.rows[1].z, b.rows[2].z);
    let m = mat3x3(cross(y, z), cross(z, x), cross(x, y));
    return normalize(m * n * sign(dot(x, cross(y, z))) + vec3(0.0, 1e-6, 0.0));
}

fn vertex(vi: u32, ii: u32, shadow: bool) -> VOut {
    var inst = instances[ii];
    var out: VOut;
    if shadow {
        if inst.shadow_roles == MODEL_SHADOW_SKIP {
            out.clip = vec4(0.0, 0.0, 0.0, 1.0);
            return out;
        }
        inst.roles |= inst.shadow_roles;
    }
    var world: vec3<f32>;
    var shade: f32;
    let boxes = inst.cuboid_count * 36u;
    if vi < boxes {
        let cub = cuboids[inst.cuboids + vi / 36u];
        if !drawn(inst, cub.role, cub.cover) {
            out.clip = vec4(0.0, 0.0, 0.0, 1.0);
            return out;
        }
        let face = (vi % 36u) / 6u;
        let c = TRI[vi % 6u];
        let grow = cub.inflate + inst.inflate;
        let local = cub.origin - vec3(grow) + CORNERS[face * 4u + c] * (cub.size + vec3(2.0 * grow));
        world = place(inst.bones + cub.bone, local);
        // Constant indices: SPIRV-Cross cannot copy a runtime-indexed array out of a storage buffer to Metal.
        var r: vec4<f32>;
        switch face {
            case 0u: { r = cub.faces[0]; }
            case 1u: { r = cub.faces[1]; }
            case 2u: { r = cub.faces[2]; }
            case 3u: { r = cub.faces[3]; }
            case 4u: { r = cub.faces[4]; }
            default: { r = cub.faces[5]; }
        }
        out.uv = (r.xy + UVS[c] * r.zw) / inst.tex_size;
        // The face's direction after the bone turns it, as the triangles below and the world.
        out.normal = turn_normal(inst.bones + cub.bone, FACE_DIRS[face]);
        shade = normal_shade(out.normal);
    } else {
        let k = vi - boxes;
        let v = corners[inst.vertices + k];
        if !drawn(inst, v.role, v.cover) {
            out.clip = vec4(0.0, 0.0, 0.0, 1.0);
            return out;
        }
        world = place(inst.bones + v.bone, v.pos);
        out.uv = v.uv / inst.tex_size;
        // Face shading from the triangle's normal, like the world's free-form quads.
        let t = inst.vertices + k - k % 3u;
        let a = place(inst.bones + corners[t].bone, corners[t].pos);
        let b = place(inst.bones + corners[t + 1u].bone, corners[t + 1u].pos);
        let c2 = place(inst.bones + corners[t + 2u].bone, corners[t + 2u].pos);
        let n = normalize(cross(b - a, c2 - a) + vec3(0.0, 1e-6, 0.0));
        out.normal = n;
        shade = normal_shade(n);
    }
    out.clip = select(scene.view_proj * vec4(world, 1.0), scene.sun_view * vec4(world, 1.0), shadow);
    out.dist = length(world);
    out.rel = world;
    out.tex = inst.tex;
    var lit = vec3(1.0);
    if inst.light != 0xffffffffu {
        let light = unpack_light(inst.light);
        let flash = lightning_ground(scene, world, light.x);
        lit = lightmap(light, scene.params.z, scene.light.x, scene.light.y, scene.light.w, scene.environment.x);
        lit = min(vec3(1.0), lit + flash);
        if scene.sun_grid.w > 0.0 || CUSTOM_SUN_VISIBILITY {
            out.sky = light.x;
            let ambient = lightmap(light, scene.params.z, scene.light.x, scene.light.y, scene.light.w, 0.0);
            out.ambient = pow(inst.color * min(vec3(1.0), ambient + flash) * shade, vec3(2.2));
        }
    }
    out.color = pow(inst.color * lit * shade, vec3(2.2));
    out.shader = inst.shader;
    out.glow = inst.glow;
    out.effect = inst.effect;
    out.strength = inst.strength;
    return out;
}

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    let albedo = textureSample(textures[in.tex], nearest_clamp, in.uv);
    let sun = sun_visibility(scene, in.rel, in.sky, in.normal);
    var light = mix(in.ambient, in.color, sun);
    if in.glow != 0xffffffffu {
        let e = textureSampleLevel(textures[in.glow], nearest_clamp, in.uv, 0.0).r;
        light = max(light, vec3(pow(e, 2.2)));
    }
    let p = EntityPixel(in.uv, albedo, light, scene.params.w);
    let c = entity_effect(in.effect, p, entity_shade(in.shader, p), in.strength);
    if c.a < 0.5 {
        discard;
    }
    let f = fog_amount(in.dist, scene.params.x, scene.params.y);
    return vec4(mix(c.rgb, scene.fog_color.rgb, f), 1.0);
}

@vertex
fn vs(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VOut {
    return vertex(vi, ii, false);
}

// The first person's arms: depth squeezed into the near half (reverse Z), in front of the world
// (nothing the player collides with comes closer than ~0.2 blocks) but sorted within themselves.
@vertex
fn vs_hand(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VOut {
    var out = vertex(vi, ii, false);
    out.clip.z = (0.5 + 0.5 * out.clip.z / out.clip.w) * out.clip.w;
    return out;
}

struct SunModelOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) tex: u32,
}

@vertex
fn vs_sun_model(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> SunModelOut {
    let v = vertex(vi, ii, true);
    return SunModelOut(v.clip, v.uv, v.tex);
}

@fragment
fn fs_sun_model(in: SunModelOut) {
    if textureSampleLevel(textures[in.tex], nearest_clamp, in.uv, 0.0).a < 0.5 {
        discard;
    }
}
