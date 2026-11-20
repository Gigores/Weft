
var<private> TRI_FLIP: array<u32, 6> = array<u32, 6>(1u, 2u, 3u, 1u, 3u, 0u);
var<private> AO: array<f32, 4> = array<f32, 4>(0.42, 0.62, 0.8, 1.0);
const WORLD_LIQUID: u32 = 1u;
const COVER_WALL: u32 = 2u;
const LOD_DIR_SHIFT: u32 = 2u;
const LOD_BOX: u32 = 32u;
const LOD_CUTOUT: u32 = 64u;
const LOD_LEAVES: u32 = 128u;
const COVER_SIZE_SHIFT: u32 = 8u;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    // Textures (bindless indices) of this frame and the next.
    @location(1) @interpolate(flat) layer: u32,
    @location(2) color: vec3<f32>,
    @location(3) light: vec4<f32>,
    @location(4) dist: f32,
    @location(5) @interpolate(flat) layer2: u32,
    @location(6) @interpolate(flat) blend: f32,
    @location(7) clip_cube: vec3<f32>,
    // The face's tint, also in `color`; alone it colors emission.
    @location(8) tint: vec3<f32>,
    // Glow mask texture (LabPBR emission), all ones: none.
    @location(9) @interpolate(flat) glow: u32,
    // Block shader slot of the texture, 0 none, and the block coordinates for it.
    @location(10) @interpolate(flat) slot: u32,
    @location(11) pos: vec3<f32>,
    @location(12) @interpolate(flat) size: f32,
    @location(13) @interpolate(flat) lod_fade: f32,
    @location(14) rel: vec3<f32>,
    @location(15) @interpolate(flat) flags: u32,
}

fn shader_of(tex: u32) -> u32 {
    if tex >= arrayLength(&tex_shader) {
        return 0u;
    }
    return tex_shader[tex];
}

fn glow_of(tex: u32) -> u32 {
    if tex >= arrayLength(&glow_map) {
        return 0xffffffffu;
    }
    return glow_map[tex];
}

fn unpack_rgb(c: u32) -> vec3<f32> {
    return vec3<f32>(f32((c >> 16u) & 255u), f32((c >> 8u) & 255u), f32(c & 255u)) / 255.0;
}

fn column_tint(x: i32, z: i32, tint: u32) -> u32 {
    let t = i32(g.ids.w);
    let tx = ((x >> 4u) % t + t) % t;
    let tz = ((z >> 4u) % t + t) % t;
    let i = g.ids.z + u32(tz * t + tx) * 256u + u32((z & 15) * 16 + (x & 15));
    return biomes[i * g.extra.z + tint - 1u];
}

// Tint at a block corner: average of the four columns touching it, falling back to the face's
// own biome where colors are not uploaded yet.
fn tint_color(tint: u32, biome: u32, x: i32, z: i32) -> vec3<f32> {
    if tint == 0u {
        return vec3(1.0);
    }
    var sum = vec3(0.0);
    var n = 0.0;
    for (var k = 0; k < 4; k++) {
        let c = column_tint(x - 1 + (k & 1), z - 1 + (k >> 1u), tint);
        if c != 0u {
            sum += unpack_rgb(c);
            n += 1.0;
        }
    }
    if n > 0.0 {
        return sum / n;
    }
    return unpack_rgb(biomes[biome * g.extra.z + tint - 1u]);
}

struct Frame {
    a: u32,
    b: u32,
    blend: f32,
    size: f32,
}

fn texture_frame(name: u32) -> Frame {
    let base = tex_info[name * 4u];
    let info = tex_info[name * 4u + 1u];
    // A size of 0 would make every derivative NaN, and the sampler the smallest mip.
    let size = f32(max(tex_info[name * 4u + 2u], 1u));
    if info == 0u {
        return Frame(base, base, 0.0, size);
    }
    let count = info & 65535u;
    let total = f32((info >> 16u) & 32767u);
    let ticks = scene.light.z;
    let t = ticks - floor(ticks / total) * total;
    var acc = 0.0;
    for (var i = 0u; i < count; i++) {
        let e = frames[base + i];
        let d = f32(e >> 16u);
        if t < acc + d || i + 1u == count {
            let next = frames[base + (i + 1u) % count];
            var f = 0.0;
            if (info >> 31u) == 1u {
                f = (t - acc) / d;
            }
            return Frame(e & 65535u, next & 65535u, f, size);
        }
        acc += d;
    }
    return Frame(base, base, 0.0, size);
}

// A body's blocks have their own light and are lit as under open sky; where it is, the world has
// its say.
fn on_body(l: vec4<f32>, k: i32) -> vec4<f32> {
    if k == 0 {
        return l;
    }
    let w = unpack_light(bitcast<u32>(offsets[k].w));
    return vec4(min(l.x, w.x), max(l.yzw, w.yzw));
}

fn finish(local: vec3<f32>, origin: vec4<i32>) -> vec4<f32> {
    var p = vec3<f32>(origin.xyz) * 16.0 + local;
    if origin.w != 0 {
        p = quat_vector(body_rotations[origin.w], p);
    }
    let pos = p - g.cam_frac.xyz + offsets[origin.w].xyz;
    return vec4(pos, 1.0);
}

@vertex
fn vs_face(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VOut {
    let f = vi / 4u;
    let w0 = records[f * 4u];
    let w1 = records[f * 4u + 1u];
    let w2 = records[f * 4u + 2u];
    let w3 = records[f * 4u + 3u];
    let a = face_ao(w0);
    let flip = a.x + a.z < a.y + a.w;
    // Indexed as (0 1 2)(0 2 3); starting one corner later puts the diagonal on 1-3.
    var c = vi % 4u;
    if flip {
        c = (c + 1u) % 4u;
    }
    let dir = face_dir(w0);
    let local = face_pos(w0) + CORNERS[dir * 4u + c];
    let pos = finish(local, origins[ii]);
    var out: VOut;
    out.clip = scene.view_proj * pos;
    out.lod_fade = 0.0;
    out.dist = length(pos.xyz);
    out.rel = pos.xyz;
    out.flags = 0u;
    out.uv = UVS[(c + 4u - ((w0 >> FACE_TURN_SHIFT) & FACE_TURN_MASK)) % 4u];
    let tex_id = w1 & FACE_TEX_MASK;
    let fr = texture_frame(tex_id);
    out.size = fr.size;
    out.layer = fr.a;
    out.layer2 = fr.b;
    out.blend = fr.blend;
    out.glow = glow_of(fr.a);
    out.light = on_body(corner_light(w2, w3, c), origins[ii].w);
    let o = origins[ii] + g.cam_cube;
    out.slot = shader_of(tex_id);
    out.pos = pos.xyz + vec3<f32>(g.cam_cube.xyz * 16) + g.cam_frac.xyz;
    let tint_kind = (w0 >> FACE_TINT_SHIFT) & TINT_MASK;
    var tint = vec3(1.0);
    if tint_kind != 0u {
        tint = tint_color(tint_kind, w1 >> FACE_BIOME_SHIFT, o.x * 16 + i32(local.x), o.z * 16 + i32(local.z));
    }
    out.tint = tint;
    var shade = SHADE[dir];
    if origins[ii].w != 0 {
        shade = normal_shade(quat_vector(body_rotations[origins[ii].w], FACE_DIRS[dir]));
    }
    out.color = tint * shade * AO[a[c]];
    return out;
}

@vertex
fn vs_quad(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VOut {
    let q = vi / 4u;
    let c = vi % 4u;
    let base = q * 8u;
    let p0 = quad_pos(records[base]);
    let p1 = quad_pos(records[base + 1u]);
    let p2 = quad_pos(records[base + 2u]);
    let local = quad_pos(records[base + c]);
    let w4 = records[base + 4u];
    let uvw = records[base + 5u];
    let uv = quad_uv_fine(uvw, records[base], records[base + 1u], records[base + 2u], records[base + 3u]);
    var uvs = array<vec2<f32>, 4>(vec2(uv.x, uv.w), vec2(uv.z, uv.w), vec2(uv.z, uv.y), vec2(uv.x, uv.y));
    var n = normalize(cross(p1 - p0, p2 - p1));
    if origins[ii].w != 0 {
        n = quat_vector(body_rotations[origins[ii].w], n);
    }
    // Face shading blended by normal; plants opt out like vanilla's crossed squares.
    var shade = normal_shade(n);
    if ((uvw >> QUAD_TINT_SHIFT) & QUAD_FLAT) != 0u {
        shade = 1.0;
    }
    let pos = finish(local, origins[ii]);
    var out: VOut;
    out.clip = scene.view_proj * pos;
    out.lod_fade = 0.0;
    out.dist = length(pos.xyz);
    out.rel = pos.xyz;
    let light0 = records[base + 6u];
    var light1 = records[base + 7u];
    if ((uvw >> QUAD_TINT_SHIFT) & QUAD_CLIP) != 0u {
        let planes = vec3<u32>(light1) >> vec3<u32>(0u, 2u, 4u);
        let min_pos = -f32(QUAD_POS_OFFSET) / 16.0;
        let max_pos = f32(QUAD_POS_MASK - QUAD_POS_OFFSET) / 16.0;
        let lo = select(vec3(min_pos), vec3(0.0), (planes & vec3(1u)) != vec3(0u));
        let hi = select(vec3(max_pos), vec3(16.0), (planes & vec3(2u)) != vec3(0u));
        out.clip_cube = (local - lo) / (hi - lo) - 0.5;
        light1 = light0;
    }
    out.flags = select(0u, WORLD_LIQUID, origins[ii].w == 0 && ((uvw >> QUAD_TINT_SHIFT) & QUAD_LIQUID) != 0u);
    out.uv = uvs[c];
    let tex_id = w4 & FACE_TEX_MASK;
    let fr = texture_frame(tex_id);
    out.size = fr.size;
    out.layer = fr.a;
    out.layer2 = fr.b;
    out.blend = fr.blend;
    out.glow = glow_of(fr.a);
    out.light = on_body(corner_light(light0, light1, c), origins[ii].w);
    let o = origins[ii] + g.cam_cube;
    out.slot = shader_of(tex_id);
    out.pos = pos.xyz + vec3<f32>(g.cam_cube.xyz * 16) + g.cam_frac.xyz;
    let tint_kind = (uvw >> QUAD_TINT_SHIFT) & TINT_MASK;
    var tint = vec3(1.0);
    if tint_kind != 0u {
        tint = tint_color(tint_kind, w4 >> FACE_BIOME_SHIFT, o.x * 16 + i32(round(local.x)), o.z * 16 + i32(round(local.z)));
    }
    out.tint = tint;
    out.color = tint * shade;
    return out;
}

fn shade(in: VOut, albedo: vec4<f32>) -> vec4<f32> {
    if any(abs(in.clip_cube) > vec3(0.5)) { discard; }
    var light = in.color * lightmap(in.light, scene.params.z, scene.light.x, scene.light.y, scene.light.w, scene.environment.x);
    // Glowing texels are lifted towards full brightness by their mask: lit = albedo * max(light,
    // emission), so they stay their own (tinted, unshaded) color in the dark.
    if in.glow != 0xffffffffu && (in.flags & COVER_WALL) == 0u {
        let e = textureSampleLevel(textures[in.glow], nearest_clamp, in.uv, 0.0).r;
        light = max(light, in.tint * e);
    }
    var linear_light: vec3<f32>;
    if light.r == light.g && light.r == light.b {
        linear_light = vec3(pow(light.r, 2.2));
    } else {
        linear_light = pow(light, vec3(2.2));
    }
    var lit: vec4<f32>;
    if in.slot == 0u {
        lit = vec4(albedo.rgb * linear_light, albedo.a);
    } else {
        lit = block_shade(in.slot, BlockPixel(in.uv, albedo, linear_light, in.pos, scene.params.w));
    }
    let fog = fog_amount(in.dist, scene.params.x, scene.params.y);
    return vec4(mix(lit.rgb, scene.fog_color.rgb, fog), lit.a);
}

// Nearest texels with antialiased borders (vanilla 26.1's sampleNearest): up close each texel
// keeps its color but its edge blends over one screen pixel, so blocks look clean, not jagged.
fn nearest_uv(size: f32, uv: vec2<f32>, du: vec2<f32>, dv: vec2<f32>) -> vec2<f32> {
    let inv_s = 1.0 / size;
    let t = uv * size;
    let r = round(t);
    let inv_pp = inverseSqrt(max(du * du + dv * dv, vec2(1e-10))) * inv_s;
    let off = clamp((t - r) * inv_pp + 0.5, vec2(0.0), vec2(1.0));
    return (r - 0.5 + off) * inv_s;
}

fn sample(in: VOut, nuv: vec2<f32>, du: vec2<f32>, dv: vec2<f32>) -> vec4<f32> {
    let a = textureSampleGrad(textures[in.layer], linear_mipped, nuv, du, dv);
    if in.blend <= 0.0 {
        return a;
    }
    let b = textureSampleGrad(textures[in.layer2], linear_mipped, nuv, du, dv);
    return mix(a, b, in.blend);
}

@fragment
fn fs_opaque(in: VOut) -> @location(0) vec4<f32> {
    let du = dpdx(in.uv);
    let dv = dpdy(in.uv);
    let nuv = nearest_uv(in.size, in.uv, du, dv);
    let albedo = sample(in, nuv, du, dv);
    clip_dry_liquid(in);
    return vec4(shade(in, albedo).rgb, 1.0);
}

// Mip level from the smaller screen derivative: a plant quad seen almost edge-on would otherwise
// pick a tiny mip whose averaged texels cover the whole quad and draw it as a solid strip.
fn sample_cutout(in: VOut, nuv: vec2<f32>, du: vec2<f32>, dv: vec2<f32>) -> vec4<f32> {
    let size = in.size;
    let t = in.uv * size;
    let lod = max(log2(max(min(length(du * size), length(dv * size)), 1e-6)), 0.0);
    let a = textureSampleLevel(textures[in.layer], linear_mipped, nuv, lod);
    if in.blend <= 0.0 {
        return a;
    }
    let b = textureSampleLevel(textures[in.layer2], linear_mipped, nuv, lod);
    return mix(a, b, in.blend);
}

@fragment
fn fs_cutout(in: VOut) -> @location(0) vec4<f32> {
    let du = dpdx(in.uv);
    let dv = dpdy(in.uv);
    let nuv = nearest_uv(in.size, in.uv, du, dv);
    let albedo = sample_cutout(in, nuv, du, dv);
    clip_dry_liquid(in);
    // Vanilla's alpha test (GL_GREATER 0.1).
    if albedo.a <= 0.1 {
        discard;
    }
    return vec4(shade(in, albedo).rgb, 1.0);
}

fn translucent(in: VOut) -> vec4<f32> {
    let du = dpdx(in.uv);
    let dv = dpdy(in.uv);
    let nuv = nearest_uv(in.size, in.uv, du, dv);
    let albedo = sample(in, nuv, du, dv);
    return shade(in, albedo);
}

fn in_dry_hull(rel: vec3<f32>) -> bool {
    let count = u32(dry_volumes[0].origin.x);
    for (var i = 1u; i <= count; i++) {
        let hull = dry_volumes[i];
        let local = quat_vector(hull.inverse, rel - hull.origin.xyz);
        if any(local < hull.lo.xyz) || any(local >= hull.hi.xyz) {
            continue;
        }
        for (var j = u32(hull.origin.w); j < u32(hull.lo.w); j++) {
            let bounds = dry_volumes[j];
            if all(local >= bounds.lo.xyz) && all(local < bounds.hi.xyz) {
                return true;
            }
        }
    }
    return false;
}

fn clip_dry_liquid(in: VOut) {
    if (in.flags & WORLD_LIQUID) != 0u && in_dry_hull(in.rel) {
        discard;
    }
}

@fragment
fn fs_translucent(in: VOut) -> @location(0) vec4<f32> {
    let color = translucent(in);
    clip_dry_liquid(in);
    return through_cloud(color, in.rel, scene);
}

@fragment
fn fs_layers(in: VOut) -> @location(0) vec4<f32> {
    let color = translucent(in);
    clip_dry_liquid(in);
    return color;
}

// Distant terrain (`client::lod`): a record is the `dir` face of a box in its tile, textured and
// tinted like a near face, full sky light. Its pixels in columns whose cubes show their ground
// are dropped (`lod_mask`); it sits a little lower so the cubes win along the edges.
fn lod_uv(dir: u32, p: vec3<f32>) -> vec2<f32> {
    if dir == 2u || dir == 3u {
        return p.xz;
    }
    if dir == 0u {
        return vec2(p.z, -p.y);
    }
    if dir == 1u {
        return vec2(-p.z, -p.y);
    }
    if dir == 4u {
        return vec2(-p.x, -p.y);
    }
    return vec2(p.x, -p.y);
}

@vertex
fn vs_lod(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VOut {
    let f = vi / 4u;
    let w0 = records[f * 4u];
    let w1 = records[f * 4u + 1u];
    let w2 = records[f * 4u + 2u];
    let w3 = records[f * 4u + 3u];
    let dir = lod_dir(w0);
    let covered = ((w3 >> LOD_FACE_COVER_SHIFT) & 1u) != 0u;
    let partial = ((w0 >> LOD_FACE_PARTIAL_SHIFT) & 1u) != 0u;
    let kind = lod_tint(w0);
    let plant = partial && covered && kind == LOD_FACE_PLANT;
    let grass_wall = covered && !partial && dir != 3u;
    let a = select(lod_ao(w3), vec4(3u), grass_wall);
    var c = vi % 4u;
    if a.x + a.z < a.y + a.w {
        c = (c + 1u) % 4u;
    }
    let data = u32(origins[ii].w);
    let scale = exp2(f32((data >> LOD_DRAW_SCALE_SHIFT) & LOD_DRAW_SCALE_MASK));
    let corner = CORNERS[dir * 4u + c];
    var shape = lod_corner(w0, w2, w3, corner);
    let u = select(corner.x, corner.z, dir < 2u);
    if plant {
        // The packed X/Z edge pairs are the endpoints of the plane.
        let edge = vec4<f32>(vec4<u32>(w3, w3 >> 5u, w3 >> 10u, w3 >> 15u) & vec4(LOD_FACE_EDGE_MASK)) / 16.0;
        shape = vec3(mix(edge.x, edge.y, u), shape.y, mix(edge.z, edge.w, u));
    }
    let local = (lod_pos(w0, w2) + shape) * vec3(scale, 1.0, scale) - vec3(0.0, 0.125, 0.0);
    let pos = finish(local, vec4<i32>(origins[ii].xyz, 0));
    var out: VOut;
    out.clip = scene.view_proj * pos;
    // Reverse-Z: a fallback wall sharing a plane with loaded geometry stays behind it.
    out.clip.z -= abs(out.clip.z) * 0.00001;
    out.lod_fade = f32(data & LOD_DRAW_FADE_MASK) / f32(LOD_DRAW_FADE_MASK) * 3.0 - 1.0;
    out.dist = length(pos.xyz);
    out.rel = pos.xyz;
    out.flags = (dir << LOD_DIR_SHIFT) | select(0u, LOD_BOX, partial) | select(0u, LOD_CUTOUT, partial && covered);
    let o = origins[ii] + g.cam_cube;
    let world = vec3<f32>(o.xyz * 16) + local + vec3(0.0, 0.125, 0.0);
    out.uv = lod_uv(dir, world);
    // A wall lies on the line between two columns: it is of the one its box stands in, a little
    // behind it, or the mask's pick between them flickers along it.
    out.pos = world - vec3(0.0, 0.125, 0.0) - FACE_DIRS[dir] * 0.25;
    if plant {
        out.uv = vec2(select(1.0 - u, u, dir == 0u || dir == 5u), 1.0 - corner.y);
        let owner = vec3<f32>(o.xyz * 16) + lod_pos(w0, w2) + vec3(0.5, 0.0, 0.5);
        out.pos = vec3(owner.x, world.y, owner.z);
    }
    let tex_id = w1 & FACE_TEX_MASK;
    let fr = texture_frame(tex_id);
    out.size = fr.size;
    out.layer = fr.a;
    out.layer2 = fr.b;
    out.blend = fr.blend;
    out.glow = 0xffffffffu;
    out.slot = 0u;
    out.light = lod_light(w0, w2, w3);
    if partial && covered && kind == LOD_FACE_FILLED_LEAVES {
        out.flags |= LOD_LEAVES;
    }
    var tint = vec3(1.0);
    var shade = select(SHADE[dir] * AO[a[c]], 1.0, plant);
    if covered {
        let rgb = w1 >> FACE_BIOME_SHIFT;
        tint = f32(LOD_FACE_COVER_GAIN) * vec3(f32(rgb >> 11u) / 31.0, f32((rgb >> 5u) & 63u) / 63.0, f32(rgb & 31u) / 31.0);
        if grass_wall {
            // The texture represents the grass emerging from the lower step.
            shade = mix(SHADE[dir], 1.0, 0.65);
            let pattern_id = (w3 >> LOD_FACE_PATTERN_LOW_SHIFT)
                | (((w2 >> LOD_FACE_LIGHT_RG_SHIFT) & LOD_FACE_PATTERN_MID_MASK) << 8u)
                | (((w3 >> LOD_FACE_LIGHT_B_SHIFT) & LOD_FACE_PATTERN_HIGH_MASK) << 14u);
            let pattern = texture_frame(pattern_id);
            out.flags |= COVER_WALL | (u32(pattern.size) << COVER_SIZE_SHIFT);
            out.glow = pattern.a;
            out.light = vec4(out.light.x, 0.0, 0.0, 0.0);
        }
    } else if kind != 0u {
        tint = unpack_rgb(biomes[(w1 >> FACE_BIOME_SHIFT) * g.extra.z + kind - 1u]);
    }
    out.tint = tint;
    out.color = tint * shade;
    return out;
}

// Whether cubes show the column at `pos`, distant terrain giving way to them there: its ground
// (part 0) or its top (part 1, a sea's surface).
fn shown_by_cubes(pos: vec3<f32>, part: u32) -> bool {
    let half = i32(g.extra.w);
    let col = vec2<i32>(floor(pos.xz / 16.0));
    let d = col - g.cam_cube.xz;
    if abs(d.x) > half || abs(d.y) > half {
        return false;
    }
    let w = 2 * half + 1;
    let i = u32(((col.y % w) + w) % w * w + ((col.x % w) + w) % w);
    let words = (u32(w * w) + 31u) / 32u;
    let shown = ((lod_mask[i / 32u + part * words] >> (i % 32u)) & 1u) != 0u;
    if !shown {
        return false;
    }
    let mask_header = 2u * words;
    let layer = i32(floor(pos.y / 16.0)) - bitcast<i32>(lod_mask[mask_header]);
    let stride = lod_mask[mask_header + 1u];
    if layer < 0 || u32(layer) / 32u >= stride {
        return false;
    }
    let bit = u32(layer);
    return ((lod_mask[mask_header + 2u + i * stride + bit / 32u] >> (bit % 32u)) & 1u) != 0u;
}

fn lod_fade_visible(fade: f32, pixel: vec2<f32>) -> bool {
    // Normal LOD tiles are always visible.
    if fade > 1.0 {
        return true;
    }

    let bayer = array<u32, 16>(
         0u,  8u,  2u, 10u,
        12u,  4u, 14u,  6u,
         3u, 11u,  1u,  9u,
        15u,  7u, 13u,  5u
    );

    let x = u32(pixel.x) & 3u;
    let y = u32(pixel.y) & 3u;
    let threshold = (f32(bayer[y * 4u + x]) + 0.5) / 16.0;

    if fade < 0.0 {
        return threshold >= fade + 1.0;
    }

    return threshold < fade;
}

@vertex
fn vs_forest(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VOut {
    let f = (vi / 4u) * 4u;
    let xz = records[f];
    let y = records[f + 1u];
    let radius = bitcast<f32>(records[f + 2u]);
    let data = u32(origins[ii].w);
    let scale = exp2(f32((data >> LOD_DRAW_SCALE_SHIFT) & LOD_DRAW_SCALE_MASK));
    // Records keep tile-local x/z, signed ground height, model height, horizontal radius and atlas.
    let half = vec3(radius, f32(y >> 16u) * 0.5, radius);
    let center = vec3(f32(xz & 65535u) * scale, f32(i32(y << 16u) >> 16u) + half.y - 0.125, f32(xz >> 16u) * scale);
    let rel = finish(center, vec4<i32>(origins[ii].xyz, 0)).xyz;
    // Select the view in object space. Coarse groves are wider but keep their physical height.
    let normal = normalize(-rel / half);
    let yaw = atan2(normal.x, select(1.0, normal.z, dot(normal.xz, normal.xz) > 1e-12));
    let right = vec3(cos(yaw), 0.0, -sin(yaw));
    let up = cross(normal, right);
    let corner = array<vec2<f32>, 4>(vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(1.0, 1.0), vec2(-1.0, 1.0))[vi % 4u];
    let offset = (right * corner.x + up * corner.y) * sqrt(3.0) * half;
    var out: VOut;
    out.rel = rel + offset;
    out.clip = scene.view_proj * vec4(out.rel, 1.0);
    out.uv = corner * vec2(0.5, -0.5) + 0.5;
    out.layer = records[f + 3u];
    out.pos = vec3<f32>((origins[ii].xyz + g.cam_cube.xyz) * 16) + center + offset;
    out.dist = length(out.rel);
    out.lod_fade = f32(data & LOD_DRAW_FADE_MASK) / f32(LOD_DRAW_FADE_MASK) * 3.0 - 1.0;
    out.light = vec4(1.0, 0.0, 0.0, 0.0);
    out.color = vec3(1.0);
    out.glow = 0xffffffffu;
    let pi = 3.14159265;
    // Both coordinates are constant across the primitive. Interpolate the four enclosing views.
    out.blend = fract(yaw / (2.0 * pi) + 1.0) * f32(FOREST_VIEWS);
    out.size = (asin(clamp(normal.y, -1.0, 1.0)) / pi + 0.5) * f32(FOREST_ELEVATIONS - 1u);
    return out;
}

fn forest_view(layer: u32, local: vec2<f32>, frame: vec2<u32>, level: f32) -> vec4<f32> {
    let edge = 0.5 * exp2(ceil(level)) / f32(FOREST_FRAME);
    let uv = (vec2<f32>(frame) + clamp(local, vec2(edge), vec2(1.0 - edge))) / vec2(f32(FOREST_VIEWS), f32(FOREST_ELEVATIONS));
    let p = textureSampleLevel(textures[layer], linear_mipped, uv, level);
    return vec4(p.rgb * p.a, p.a);
}

// Only views near an angular boundary need blending. This preserves small branches at
// magnification and skips the other texture reads when a single view describes the tree.
fn forest_row(layer: u32, uv: vec2<f32>, frame: vec2<u32>, weight: f32, level: f32) -> vec4<f32> {
    let first = forest_view(layer, uv, frame, level);
    if weight <= 0.0 {
        return first;
    }
    let next = vec2((frame.x + 1u) % FOREST_VIEWS, frame.y);
    return mix(first, forest_view(layer, uv, next, level), weight);
}

@fragment
fn fs_forest(in: VOut) -> @location(0) vec4<f32> {
    let du = dpdx(in.uv);
    let dv = dpdy(in.uv);
    let footprint = max(length(du), length(dv)) * f32(FOREST_FRAME);
    let level = clamp(log2(max(1.0, footprint)), 0.0, log2(f32(FOREST_FRAME)));
    let uv = nearest_uv(f32(FOREST_FRAME), in.uv, du, dv);
    if !lod_fade_visible(in.lod_fade, in.clip.xy) || shown_by_cubes(in.pos, 0u) {
        discard;
    }
    let angles = vec2(in.blend, in.size);
    var weight = smoothstep(vec2(0.25), vec2(0.75), fract(angles));
    let base = vec2<u32>(angles) + select(vec2(0u), vec2(1u), weight == vec2(1.0));
    weight = select(weight, vec2(0.0), weight == vec2(1.0));
    let frame = vec2(base.x % FOREST_VIEWS, min(base.y, FOREST_ELEVATIONS - 1u));
    var p = forest_row(in.layer, uv, frame, weight.x, level);
    if weight.y > 0.0 {
        let next = vec2(frame.x, min(frame.y + 1u, FOREST_ELEVATIONS - 1u));
        p = mix(p, forest_row(in.layer, uv, next, weight.x, level), weight.y);
    }
    if p.a < 0.32 {
        discard;
    }
    return vec4(shade(in, vec4(p.rgb / p.a, 1.0)).rgb, 1.0);
}

@fragment
fn fs_lod(in: VOut) -> @location(0) vec4<f32> {
    let du = dpdx(in.uv);
    let dv = dpdy(in.uv);
    if !lod_fade_visible(in.lod_fade, in.clip.xy) {
        discard;
    }
    if shown_by_cubes(in.pos, 0u) {
        let dir = (in.flags >> LOD_DIR_SHIFT) & FACE_DIR_MASK;
        // A near column may omit a side against its neighbor. Keep the wall
        // across the boundary until that neighboring column also draws cubes.
        if (in.flags & LOD_BOX) != 0u || dir == 2u || dir == 3u || shown_by_cubes(in.pos + FACE_DIRS[dir] * 0.5, 0u) {
            discard;
        }
    }
    let uv = fract(in.uv);
    let nuv = nearest_uv(in.size, uv, du, dv);
    var albedo = sample(in, nuv, du, dv);
    if (in.flags & LOD_CUTOUT) != 0u {
        if (in.flags & LOD_LEAVES) != 0u {
            albedo = vec4(albedo.rgb * albedo.a, 1.0);
        } else if albedo.a <= 0.1 {
            discard;
        }
        return vec4(shade(in, albedo).rgb, 1.0);
    }
    let cover = smoothstep(0.1, 0.6, albedo.a);
    let tinted = any(in.tint != vec3(1.0));
    albedo = vec4(mix(select(albedo.rgb, vec3(1.0), tinted), albedo.rgb, cover), 1.0);
    if (in.flags & COVER_WALL) != 0u {
        let leaf_uv = nearest_uv(f32(in.flags >> COVER_SIZE_SHIFT), uv, du, dv);
        let plant = textureSampleGrad(textures[in.glow], linear_mipped, leaf_uv, du, dv);
        let leaf = dot(plant.rgb, vec3(0.2126, 0.7152, 0.0722));
        albedo = vec4(albedo.rgb * mix(0.75, 1.25 + 0.35 * leaf, plant.a), 1.0);
    }
    return vec4(shade(in, albedo).rgb, 1.0);
}

// A see-through surface far off (a sea), as a near liquid's top is drawn: its picture's own
// alpha over the ground under it.
@fragment
fn fs_lod_surface(in: VOut) -> @location(0) vec4<f32> {
    let du = dpdx(in.uv);
    let dv = dpdy(in.uv);
    if !lod_fade_visible(in.lod_fade, in.clip.xy) {
        discard;
    }
    if shown_by_cubes(in.pos, 1u) {
        discard;
    }
    let nuv = nearest_uv(in.size, fract(in.uv), du, dv);
    return through_cloud(shade(in, sample(in, nuv, du, dv)), in.rel, scene);
}

struct FarDraw {
    cube: vec3<i32>,
    step: i32,
    fade: f32,
}

var<immediate> far_draw: FarDraw;

struct FarOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) rel: vec3<f32>,
    @location(1) @interpolate(flat) color: vec4<f32>,
    @location(2) @interpolate(flat) dir: u32,
}

@vertex
fn vs_lod_far(@builtin(vertex_index) vi: u32) -> FarOut {
    let at = 8u + (vi / 6u) * 3u;
    let face = records[at];
    let y = records[at + 1u];
    let rgba = records[at + 2u];
    let dir = face >> 10u;
    let tri = array<u32, 6>(0u, 1u, 2u, 0u, 2u, 3u);
    let corner = CORNERS[dir * 4u + tri[vi % 6u]];
    let lo = f32(bitcast<i32>(y << 16u) >> 16u);
    let hi = f32(bitcast<i32>(y) >> 16u);
    let local = vec3(
        (f32(face & 31u) + corner.x) * f32(far_draw.step),
        mix(lo, hi, corner.y) - 0.125,
        (f32((face >> 5u) & 31u) + corner.z) * f32(far_draw.step)
    );
    let rel = vec3<f32>(far_draw.cube) * 16.0 + local - g.cam_frac.xyz;
    var clip = scene.view_proj * vec4(rel, 1.0);
    clip.z -= abs(clip.z) * 0.00001;
    let color = vec4<f32>((vec4(rgba) >> vec4(0u, 8u, 16u, 24u)) & vec4(255u)) / 255.0;
    return FarOut(clip, rel, color, dir);
}

fn far_lit(in: FarOut) -> vec4<f32> {
    let light = lightmap(vec4(1.0, 0.0, 0.0, 0.0), scene.params.z, scene.light.x, scene.light.y, scene.light.w, scene.environment.x);
    let lit = in.color.rgb * pow(light * SHADE[in.dir], vec3(2.2));
    let fog = fog_amount(length(in.rel), scene.params.x, scene.params.y);
    return vec4(mix(lit, scene.fog_color.rgb, fog), in.color.a);
}

@fragment
fn fs_lod_far(in: FarOut) -> @location(0) vec4<f32> {
    let p = in.rel + vec3<f32>(g.cam_cube.xyz * 16) + g.cam_frac.xyz - FACE_DIRS[in.dir] * 0.25;
    if !lod_fade_visible(far_draw.fade, in.clip.xy) {
        discard;
    }
    if shown_by_cubes(p, 0u) && (in.dir == 3u || shown_by_cubes(p + FACE_DIRS[in.dir] * 0.5, 0u)) {
        discard;
    }
    return vec4(far_lit(in).rgb, 1.0);
}

@fragment
fn fs_lod_far_water(in: FarOut) -> @location(0) vec4<f32> {
    let p = in.rel + vec3<f32>(g.cam_cube.xyz * 16) + g.cam_frac.xyz - vec3(0.0, 0.25, 0.0);
    if !lod_fade_visible(far_draw.fade, in.clip.xy) || shown_by_cubes(p, 1u) {
        discard;
    }
    return through_cloud(far_lit(in), in.rel, scene);
}
