// Terrain on the GPU: the same integer math as `weft_noise::fixed` and `server::terrain`, line
// by line, so every block comes out as the CPU makes it. One workgroup per cube: columns, then
// the cave grid, then the blocks.

// The prelude (`gpu_layout::terrain`) declares the bindings, the description's layout (`D_*`),
// record sizes (`WORDS_*`), outputs (`OUT_*`), expression step kinds (`EXPR_*`) and the cave
// seeds (`S_*`).

fn seed(i: u32) -> u32 {
    return bitcast<u32>(desc[D_SEEDS + i]);
}

// One workgroup per distant tile. Share the samples at the corners, make each
// step once, and leave both the faces and their draw counts in device memory.
struct FarSample {
    y: i32,
    land: u32,
    water: u32,
}

var<workgroup> far_solid_count: atomic<u32>;
var<workgroup> far_water_count: atomic<u32>;

fn far_rgba(c: u32) -> vec4<u32> {
    return (vec4(c) >> vec4(0u, 8u, 16u, 24u)) & vec4(255u);
}

fn far_pack(c: vec4<u32>) -> u32 {
    return c.x | (c.y << 8u) | (c.z << 16u) | (c.w << 24u);
}

fn far_color(block: u32, biome: u32, palette: u32) -> u32 {
    let entry = palette + 2u + block * 2u;
    let color = u32(desc[entry]);
    let tint = u32(desc[entry + 1u]);
    if tint == 0u {
        return color;
    }
    let kinds = u32(desc[palette]);
    let colors = u32(desc[palette + 1u]);
    let shade = far_rgba(u32(desc[colors + biome * kinds + tint - 1u]));
    let rgba = far_rgba(color);
    return far_pack(vec4((rgba.rgb * shade.rgb + vec3(127u)) / 255u, rgba.a));
}

fn far_corner(x: i32, z: i32, palette: u32) -> FarSample {
    let c = column(x, z);
    let y = clamp(c.height, -32766, 32766);
    var top = desc[D_STONE];
    var water = desc[D_WATER];
    if u32(c.rule) < u32(desc[D_RULES]) {
        let rule = u32(desc[D_RULE_OFF]) + u32(c.rule) * WORDS_RULE;
        let bare = desc[rule + 13u] != 0 && c.height > desc[rule + 14u];
        if !bare {
            if desc[rule + 5u] >= 0 {
                top = desc[rule + 5u];
            }
            if desc[rule + 10u] >= 0 && c.height < desc[rule + 11u] {
                top = desc[rule + 10u];
            }
        }
        if desc[rule + 12u] >= 0 {
            water = desc[rule + 12u];
        }
    }
    var surface = 0u;
    if desc[D_HAS_SEA] != 0 && y < desc[D_SEA] {
        surface = far_color(u32(water), u32(c.biome), palette);
        if surface >> 24u == 255u {
            return FarSample(clamp(desc[D_SEA], -32766, 32766), surface, 0u);
        }
    }
    return FarSample(y, far_color(u32(top), u32(c.biome), palette), surface);
}

fn far_sample(i: u32) -> FarSample {
    let at = FAR_LOD_SCRATCH + i * 3u;
    return FarSample(bitcast<i32>(outp[at]), outp[at + 1u], outp[at + 2u]);
}

fn far_y(i: u32) -> i32 {
    return bitcast<i32>(outp[FAR_LOD_SCRATCH + i * 3u]);
}

fn far_top(sum: i32) -> i32 {
    return ((sum + 2) >> 2u) + 1;
}

fn far_face(at: u32, x: u32, z: u32, dir: u32, lo: i32, hi: i32, color: u32) {
    outp[at] = x | (z << 5u) | (dir << 10u);
    outp[at + 1u] = (u32(lo) & 65535u) | (u32(hi) << 16u);
    outp[at + 2u] = color;
}

fn far_wall(x: u32, z: u32, dir: u32, lo: i32, hi: i32, color: u32) {
    if lo < hi {
        let at = atomicAdd(&far_solid_count, 1u);
        far_face(FAR_LOD_HEADER + at * FAR_LOD_WORDS, x, z, dir, lo, hi, color);
    }
}

@compute @workgroup_size(256)
fn lod_far(@builtin(local_invocation_index) t: u32) {
    let job = params.cubes * 4u;
    let step = 1 << u32(jobs[job + 2u]);
    let palette = u32(jobs[job + 3u]);
    for (var k = t; k < FAR_LOD_SAMPLES; k += 256u) {
        let x = jobs[job] + (i32(k % FAR_LOD_SIDE) - 1) * step;
        let z = jobs[job + 1u] + (i32(k / FAR_LOD_SIDE) - 1) * step;
        let sample = far_corner(x, z, palette);
        let at = FAR_LOD_SCRATCH + k * 3u;
        outp[at] = bitcast<u32>(sample.y);
        outp[at + 1u] = sample.land;
        outp[at + 2u] = sample.water;
    }
    if t == 0u {
        atomicStore(&far_solid_count, FAR_LOD_CELLS * FAR_LOD_CELLS);
        atomicStore(&far_water_count, 0u);
    }
    storageBarrier();
    workgroupBarrier();
    for (var k = t; k < FAR_LOD_CELLS * FAR_LOD_CELLS; k += 256u) {
        let x = k % FAR_LOD_CELLS;
        let z = k / FAR_LOD_CELLS;
        let stride = FAR_LOD_SIDE;
        let i = (z + 1u) * stride + x + 1u;
        let a = far_sample(i);
        let b = far_sample(i + 1u);
        let c = far_sample(i + stride);
        let d = far_sample(i + stride + 1u);
        let h = far_top(a.y + b.y + c.y + d.y);
        let color = far_pack((far_rgba(a.land) + far_rgba(b.land) + far_rgba(c.land) + far_rgba(d.land) + vec4(2u)) / 4u);
        far_face(FAR_LOD_HEADER + k * FAR_LOD_WORDS, x, z, 3u, h, h, color);
        // Each shared edge is owned by its higher cell. The border skirts
        // cover the difference in sampling between neighboring LOD levels.
        let skirt = max(-32768, h - min(step, 256));
        let left = far_top(a.y + c.y + far_y(i - 1u) + far_y(i + stride - 1u));
        let right = far_top(b.y + d.y + far_y(i + 2u) + far_y(i + stride + 2u));
        let back = far_top(a.y + b.y + far_y(i - stride) + far_y(i - stride + 1u));
        let front = far_top(c.y + d.y + far_y(i + stride * 2u) + far_y(i + stride * 2u + 1u));
        far_wall(x, z, 0u, select(left, min(left, skirt), x == 0u), h, color);
        far_wall(x, z, 1u, select(right, min(right, skirt), x + 1u == FAR_LOD_CELLS), h, color);
        far_wall(x, z, 4u, select(back, min(back, skirt), z == 0u), h, color);
        far_wall(x, z, 5u, select(front, min(front, skirt), z + 1u == FAR_LOD_CELLS), h, color);
        let sea = clamp(desc[D_SEA], -32766, 32766) + 1;
        if desc[D_HAS_SEA] != 0 && h < sea {
            let count = countOneBits(select(0u, 1u, a.water != 0u) | select(0u, 2u, b.water != 0u)
                | select(0u, 4u, c.water != 0u) | select(0u, 8u, d.water != 0u));
            if count > 0u {
                let at = atomicAdd(&far_water_count, 1u) + FAR_LOD_SOLID_MAX;
                let wet = far_pack((far_rgba(a.water) + far_rgba(b.water) + far_rgba(c.water) + far_rgba(d.water)) / count);
                far_face(FAR_LOD_HEADER + at * FAR_LOD_WORDS, x, z, 3u, sea, sea, wet);
            }
        }
    }
    workgroupBarrier();
    if t == 0u {
        outp[0] = atomicLoad(&far_solid_count) * 6u;
        outp[1] = 1u;
        outp[2] = 0u;
        outp[3] = 0u;
        outp[4] = atomicLoad(&far_water_count) * 6u;
        outp[5] = 1u;
        outp[6] = FAR_LOD_SOLID_MAX * 6u;
        outp[7] = 0u;
    }
}

fn fmix(h0: u32) -> u32 {
    var h = h0;
    h ^= h >> 16u;
    h *= 0x85EBCA6Bu;
    h ^= h >> 13u;
    h *= 0xC2B2AE35u;
    return h ^ (h >> 16u);
}

fn hash(s: u32, x: i32, y: i32, z: i32) -> u32 {
    return fmix(s ^ fmix(bitcast<u32>(x) ^ fmix(bitcast<u32>(y) ^ fmix(bitcast<u32>(z) ^ 0x27D4EB2Fu))));
}

fn div_floor(p: i32, d: i32) -> vec2<i32> {
    var q = p / d;
    if (p % d != 0 && p < 0) {
        q -= 1;
    }
    return vec2<i32>(q, p - q * d);
}

fn lattice(p: i32, scale: i32) -> vec2<i32> {
    let s = clamp(scale, 1, (1 << 20) - 1);
    let d = div_floor(p, s);
    return vec2<i32>(d.x, i32((u32(d.y) * 4096u) / u32(s)));
}

fn fade(t: i32) -> i32 {
    let t3 = (((t * t) >> 12u) * t) >> 12u;
    return (t3 * (((t * (t * 6 - 15 * ONE)) >> 12u) + 10 * ONE)) >> 12u;
}

fn lerp(a: i32, b: i32, t: i32) -> i32 {
    return a + (((b - a) * t) >> 12u);
}

fn grad2(h: u32, x: i32, y: i32) -> i32 {
    switch (h & 7u) {
        case 0u: { return x + y; }
        case 1u: { return x - y; }
        case 2u: { return -x + y; }
        case 3u: { return -x - y; }
        case 4u: { return x; }
        case 5u: { return -x; }
        case 6u: { return y; }
        default: { return -y; }
    }
}

fn grad3(h: u32, x: i32, y: i32, z: i32) -> i32 {
    switch (h % 12u) {
        case 0u: { return x + y; }
        case 1u: { return -x + y; }
        case 2u: { return x - y; }
        case 3u: { return -x - y; }
        case 4u: { return x + z; }
        case 5u: { return -x + z; }
        case 6u: { return x - z; }
        case 7u: { return -x - z; }
        case 8u: { return y + z; }
        case 9u: { return -y + z; }
        case 10u: { return y - z; }
        default: { return -y - z; }
    }
}

fn perlin2(s: u32, x: i32, y: i32, scale: i32) -> i32 {
    let lx = lattice(x, scale);
    let ly = lattice(y, scale);
    let u = fade(lx.y);
    let v = fade(ly.y);
    let g00 = grad2(hash(s, lx.x, ly.x, 0), lx.y, ly.y);
    let g10 = grad2(hash(s, lx.x + 1, ly.x, 0), lx.y - ONE, ly.y);
    let g01 = grad2(hash(s, lx.x, ly.x + 1, 0), lx.y, ly.y - ONE);
    let g11 = grad2(hash(s, lx.x + 1, ly.x + 1, 0), lx.y - ONE, ly.y - ONE);
    let a = lerp(g00, g10, u);
    let b = lerp(g01, g11, u);
    return (lerp(a, b, v) * 2867) >> 12u;
}

fn perlin3(s: u32, x: i32, y: i32, z: i32, sx: i32, sy: i32, sz: i32) -> i32 {
    let lx = lattice(x, sx);
    let ly = lattice(y, sy);
    let lz = lattice(z, sz);
    let u = fade(lx.y);
    let v = fade(ly.y);
    let w = fade(lz.y);
    let x00 = lerp(grad3(hash(s, lx.x, ly.x, lz.x), lx.y, ly.y, lz.y), grad3(hash(s, lx.x + 1, ly.x, lz.x), lx.y - ONE, ly.y, lz.y), u);
    let x10 = lerp(grad3(hash(s, lx.x, ly.x + 1, lz.x), lx.y, ly.y - ONE, lz.y), grad3(hash(s, lx.x + 1, ly.x + 1, lz.x), lx.y - ONE, ly.y - ONE, lz.y), u);
    let x01 = lerp(grad3(hash(s, lx.x, ly.x, lz.x + 1), lx.y, ly.y, lz.y - ONE), grad3(hash(s, lx.x + 1, ly.x, lz.x + 1), lx.y - ONE, ly.y, lz.y - ONE), u);
    let x11 = lerp(grad3(hash(s, lx.x, ly.x + 1, lz.x + 1), lx.y, ly.y - ONE, lz.y - ONE), grad3(hash(s, lx.x + 1, ly.x + 1, lz.x + 1), lx.y - ONE, ly.y - ONE, lz.y - ONE), u);
    return lerp(lerp(x00, x10, v), lerp(x01, x11, v), w);
}

fn fbm(s: u32, x: i32, z: i32, scale: i32, octaves: u32) -> i32 {
    var sum = 0;
    var norm = 0;
    for (var o = 0u; o < min(octaves, 12u); o++) {
        let amp = ONE >> o;
        sum += (perlin2(s + o * 7919u, x, z, max(scale >> o, 1)) * amp) >> 12u;
        norm += amp;
    }
    if (norm == 0) {
        return 0;
    }
    return sum * ONE / norm;
}

fn ramp(v: i32, lo: i32, hi: i32) -> i32 {
    if (hi == lo) {
        return select(0, ONE, v >= hi);
    }
    return clamp(clamp(v - lo, -(1 << 18), 1 << 18) * ONE / (hi - lo), 0, ONE);
}

// `weft_noise::fixed::site`.
fn site(s: u32, x: i32, z: i32, spacing: i32, chance: i32, radius: i32) -> i32 {
    let sp = clamp(spacing, 16, 512 * 16);
    let cx = div_floor(x, sp).x;
    let cz = div_floor(z, sp).x;
    let bx = x >> 4u;
    let bz = z >> 4u;
    var nearest = 2 * ONE;
    for (var dz = -1; dz <= 1; dz++) {
        for (var dx = -1; dx <= 1; dx++) {
            let h = hash(s, cx + dx, 0, cz + dz);
            let h2 = fmix(h);
            if (i32(h & 4095u) > chance) {
                continue;
            }
            let r = ((clamp(radius, 16, 512 * 16) * (2048 + i32((h >> 12u) & 4095u))) >> 12u) >> 4u;
            let r2 = max(r * r, 1);
            let px = ((cx + dx) * sp + ((i32(h2 & 4095u) * sp) >> 12u)) >> 4u;
            let pz = ((cz + dz) * sp + ((i32((h2 >> 12u) & 4095u) * sp) >> 12u)) >> 4u;
            let d2 = (bx - px) * (bx - px) + (bz - pz) * (bz - pz);
            if (d2 >= r2 * 2) {
                continue;
            }
            nearest = min(nearest, i32((u32(d2) * 4096u) / u32(r2)));
        }
    }
    return nearest;
}

// The expression of `len` steps from word `at` at (x, z), as `server::terrain::Expr::eval`;
// `fv` holds the fields computed so far.
fn eval(at: u32, len: u32, x: i32, z: i32, fv: ptr<function, array<i32, LIMIT_FIELDS>>) -> i32 {
    var st: array<i32, LIMIT_DEPTH>;
    var sp = 0u;
    for (var i = 0u; i < len; i++) {
        let m = at + i * WORDS_STEP;
        let kind = u32(desc[m]);
        var v = 0;
        switch (kind) {
            case EXPR_CONST: { v = desc[m + 2u]; }
            case EXPR_X: { v = x; }
            case EXPR_Z: { v = z; }
            case EXPR_FIELD: { v = (*fv)[u32(desc[m + 2u])]; }
            case EXPR_ABS: {
                sp -= 1u;
                v = abs(st[sp]);
            }
            case EXPR_RAMP: {
                sp -= 1u;
                v = ramp(st[sp], desc[m + 2u], desc[m + 3u]);
            }
            case EXPR_NOISE: {
                sp -= 2u;
                v = fbm(bitcast<u32>(desc[m + 1u]), st[sp], st[sp + 1u], desc[m + 2u], u32(desc[m + 3u]));
            }
            case EXPR_SITE: {
                sp -= 2u;
                v = site(bitcast<u32>(desc[m + 1u]), st[sp], st[sp + 1u], desc[m + 2u], desc[m + 3u], desc[m + 4u]);
            }
            default: {
                sp -= 2u;
                let l = st[sp];
                let r = st[sp + 1u];
                switch (kind) {
                    case EXPR_ADD: { v = l + r; }
                    case EXPR_SUB: { v = l - r; }
                    case EXPR_MUL: { v = (clamp(l, -46340, 46340) * clamp(r, -46340, 46340)) >> 12u; }
                    case EXPR_DIV: {
                        if (r != 0) {
                            v = l / r;
                        }
                    }
                    case EXPR_REM: { v = div_floor(l, max(r, 1)).y; }
                    case EXPR_MIN: { v = min(l, r); }
                    default: { v = max(l, r); }
                }
            }
        }
        st[sp] = v;
        sp += 1u;
    }
    if (sp == 0u) {
        return 0;
    }
    return st[0];
}

struct Column {
    height: i32,
    biome: i32,
    rule: i32,
}

// Column at block (`bx`, `bz`).
fn column(bx: i32, bz: i32) -> Column {
    let x = bx * 16;
    let z = bz * 16;
    var fv: array<i32, LIMIT_FIELDS>;
    let nfi = u32(desc[D_FIELDS]);
    let fio = u32(desc[D_FIELD_OFF]);
    for (var i = 0u; i < nfi; i++) {
        let f = fio + i * WORDS_FIELD;
        fv[i] = eval(u32(desc[f]), u32(desc[f + 1u]), x, z, &fv);
    }
    var h = desc[D_BASE];
    let nf = u32(desc[D_FEATURES]);
    let fo = u32(desc[D_FEAT_OFF]);
    for (var i = 0u; i < nf; i++) {
        let f = fo + i * WORDS_FEATURE;
        var w = ONE;
        if (desc[f + 3u] > 0) {
            w = min(eval(u32(desc[f + 2u]), u32(desc[f + 3u]), x, z, &fv), ONE);
        }
        if (w > 0) {
            h += (w * clamp(eval(u32(desc[f]), u32(desc[f + 1u]), x, z, &fv), -262144, 262144)) >> 12u;
        }
    }
    let height = h >> 4u;
    let nr = u32(desc[D_RULES]);
    let ro = u32(desc[D_RULE_OFF]);
    var rule = i32(nr) - 1;
    if (nr == 0u) {
        rule = 0;
    }
    for (var i = 0u; i < nr; i++) {
        let r = ro + i * WORDS_RULE;
        var ok = height >= desc[r + 1u] && height <= desc[r + 2u];
        for (var k = 0u; ok && k < u32(desc[r + 4u]); k++) {
            let c = u32(desc[r + 3u]) + k * WORDS_COND;
            let v = fv[u32(desc[c])];
            ok = v >= desc[c + 1u] && v < desc[c + 2u];
        }
        if (ok) {
            rule = i32(i);
            break;
        }
    }
    var biome = 0;
    if (u32(rule) < nr) {
        biome = desc[ro + u32(rule) * WORDS_RULE];
    }
    return Column(height, biome, rule);
}

fn cave_node(x0: i32, y0: i32, z0: i32) -> vec2<i32> {
    let x = x0 * 16;
    let y = y0 * 16;
    let z = z0 * 16;
    let a = perlin3(seed(S_C1), x, y, z, 48 * 16, 32 * 16, 48 * 16);
    let b = perlin3(seed(S_C2), x, y, z, 48 * 16, 32 * 16, 48 * 16);
    let spaghetti = 328 - (abs(a) + abs(b));
    let cheese = perlin3(seed(S_C3), x, y, z, 90 * 16, 50 * 16, 90 * 16) - 1720;
    return vec2<i32>(spaghetti, cheese);
}

fn carves(tunnel: bool, cavern: i32, closed: i32, depth: i32) -> bool {
    let d = clamp(depth, -(1 << 16), 1 << 16);
    let near = max(ONE - d * ONE / (12 * 16), 0);
    let pit = max(ONE - d * ONE / (24 * 16), 0);
    let need = ((((closed * 1024) >> 12u) * near) >> 12u) - (((((ONE - closed) * 492) >> 12u) * pit) >> 12u);
    return tunnel || cavern > need;
}

// 0 nothing, 1 lava, 2 stone.
fn lake(wy: i32) -> i32 {
    let band = div_floor(wy, 64).x;
    let h = hash(seed(S_LAKE), 0, band, 0);
    if ((h & 1u) == 0u) {
        return 0;
    }
    let level = band * 64 + 16 + i32((h >> 8u) % 32u);
    let d = level - wy;
    if (d >= 1 && d <= 4) {
        return 1;
    }
    if (d >= 5 && d <= 6) {
        return 2;
    }
    return 0;
}

fn cavern_roof(x: i32, z: i32) -> i32 {
    return ramp(perlin2(seed(S_C4), x * 16, z * 16, 120 * 16), 1638, 1024);
}

var<workgroup> col_h: array<i32, 256>;
var<workgroup> col_biome: array<i32, 256>;
var<workgroup> col_rule: array<i32, 256>;
var<workgroup> biomes: array<i32, 256>;
var<workgroup> corners: array<i32, 25>;
var<workgroup> nodes: array<vec2<i32>, 125>;
var<workgroup> max_h: atomic<i32>;

fn node(gy: u32, gz: u32, gx: u32, k: u32) -> i32 {
    let n = nodes[(gy * 5u + gz) * 5u + gx];
    return select(n.y, n.x, k == 0u);
}

// Trilinear blend at (x, y, z) inside the cube, as `server::terrain::interpolate`.
fn field(x: u32, y: u32, z: u32, k: u32) -> i32 {
    let fx = i32(x % 4u) * 1024;
    let fy = i32(y % 4u) * 1024;
    let fz = i32(z % 4u) * 1024;
    let gx = x / 4u;
    let gy = y / 4u;
    let gz = z / 4u;
    let a = lerp(lerp(node(gy, gz, gx, k), node(gy, gz, gx + 1u, k), fx), lerp(node(gy, gz + 1u, gx, k), node(gy, gz + 1u, gx + 1u, k), fx), fz);
    let b = lerp(lerp(node(gy + 1u, gz, gx, k), node(gy + 1u, gz, gx + 1u, k), fx), lerp(node(gy + 1u, gz + 1u, gx, k), node(gy + 1u, gz + 1u, gx + 1u, k), fx), fz);
    return lerp(a, b, fy);
}

fn surface(x: u32, z: u32) -> i32 {
    let cx = x / 4u;
    let cz = z / 4u;
    let fx = i32(x % 4u);
    let fz = i32(z % 4u);
    let c00 = corners[cz * 5u + cx];
    let c10 = corners[cz * 5u + cx + 1u];
    let c01 = corners[(cz + 1u) * 5u + cx];
    let c11 = corners[(cz + 1u) * 5u + cx + 1u];
    return (c00 * (4 - fx) + c10 * fx) * (4 - fz) + (c01 * (4 - fx) + c11 * fx) * fz;
}

fn lerp2(a: vec2<i32>, b: vec2<i32>, t: i32) -> vec2<i32> {
    return vec2<i32>(lerp(a.x, b.x, t), lerp(a.y, b.y, t));
}

fn cave_plane(grid: vec2<i32>, y: i32, weight: vec2<i32>) -> vec2<i32> {
    let x = grid.x;
    let z = grid.y;
    let a = lerp2(cave_node(x, y, z), cave_node(x + 4, y, z), weight.x);
    let b = lerp2(cave_node(x, y, z + 4), cave_node(x + 4, y, z + 4), weight.x);
    return lerp2(a, b, weight.y);
}

// Keep the lattice planes while walking down a column. Return the solid floor and roof.
// For an open cut the search stops at the first solid block.
fn lod_cave(x: i32, z: i32, ground: i32, hi: i32, exposed: i32, lo: i32, cut: bool) -> vec3<i32> {
    let grid = vec2(div_floor(x, 4).x, div_floor(z, 4).x) * 4;
    let weight = (vec2(x, z) - grid) * 1024;
    let closed = cavern_roof(x, z);
    let first = div_floor(hi, 4).x * 4 + 4;
    let last = div_floor(lo, 4).x * 4;
    var upper = vec2(0);
    var roof = hi + 1;
    for (var plane = first; plane >= last; plane -= 4) {
        let lower = cave_plane(grid, plane, weight);
        if plane != first {
            for (var y = min(hi, plane + 3); y >= plane && y >= lo; y -= 1) {
                // Once past the lip, only continue tracing a gap that opens above it.
                if y < exposed && roof <= exposed {
                    return vec3(0);
                }
                let n = lerp2(lower, upper, (y - plane) * 1024);
                let carved = carves(n.x > 0, n.y, closed, ground - y * 16);
                if !carved || (y >= desc[D_LAVA_FROM] && y < desc[D_LAVA_TO] && lake(y) != 0) {
                    if cut || roof - y >= 4 {
                        return vec3(y, roof, 1);
                    }
                    roof = y;
                }
            }
        }
        upper = lower;
    }
    return select(vec3(0), vec3(lo, roof, 0), cut || roof - lo >= 4);
}

// The block at local (x, y, z) of column `i`, as `Terrain::generate`.
fn block(x: u32, y: u32, z: u32, ox: i32, oy: i32, oz: i32, cave: bool) -> u32 {
    let i = z * 16u + x;
    let h = col_h[i];
    let nr = u32(desc[D_RULES]);
    let stone = desc[D_STONE];
    var sea = -2147483647 - 1;
    if (desc[D_HAS_SEA] != 0) {
        sea = desc[D_SEA];
    }
    let water = desc[D_WATER];
    var top = stone;
    var filler = stone;
    var depth = 0;
    var under_b = -1;
    var under_n = 0;
    var ice = -1;
    var bare = false;
    let ri = u32(col_rule[i]);
    if (ri < nr) {
        let r = u32(desc[D_RULE_OFF]) + ri * WORDS_RULE;
        if (desc[r + 13u] != 0 && h > desc[r + 14u]) {
            bare = true;
        }
        if (desc[r + 5u] >= 0) {
            top = desc[r + 5u];
        }
        if (desc[r + 10u] >= 0 && h < desc[r + 11u]) {
            top = desc[r + 10u];
        }
        if (desc[r + 6u] >= 0) {
            filler = desc[r + 6u];
        }
        depth = desc[r + 7u];
        under_b = desc[r + 8u];
        under_n = desc[r + 9u];
        ice = desc[r + 12u];
    }
    let wy = oy + i32(y);
    var s = 0;
    if (wy > h) {
        if (wy <= sea) {
            if (wy == sea && ice >= 0) {
                s = ice;
            } else {
                s = water;
            }
        }
    } else if (bare) {
        s = stone;
    } else if (wy == h) {
        s = top;
    } else if (wy >= h - depth) {
        if (under_b >= 0 && wy < h - depth + under_n) {
            s = under_b;
        } else {
            s = filler;
        }
    } else {
        s = stone;
    }
    if (cave && wy <= h && !(h <= sea && wy > h - 4)) {
        let tunnel = field(x, y, z, 0u) > 0;
        let cavern = field(x, y, z, 1u);
        let closed = cavern_roof(ox + i32(x), oz + i32(z));
        if (carves(tunnel, cavern, closed, surface(x, z) - wy * 16)) {
            let l = lake(wy);
            let in_band = wy >= desc[D_LAVA_FROM] && wy < desc[D_LAVA_TO];
            if (l == 1 && in_band) {
                s = desc[D_LAVA];
            } else if (l == 2 && in_band) {
                s = stone;
            } else {
                s = 0;
            }
        }
    }
    return u32(s);
}

@compute @workgroup_size(256)
fn generate(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) t: u32) {
    let cube = wg.x;
    if (cube >= params.cubes) {
        return;
    }
    let job = cube * 4u;
    let ox = jobs[job] * 16;
    let oy = jobs[job + 1u] * 16;
    let oz = jobs[job + 2u] * 16;
    if (t == 0u) {
        atomicStore(&max_h, -2147483647 - 1);
    }
    workgroupBarrier();
    let c = column(ox + i32(t % 16u), oz + i32(t / 16u));
    col_h[t] = c.height;
    col_biome[t] = c.biome;
    col_rule[t] = c.rule;
    atomicMax(&max_h, c.height);
    if (t < 25u) {
        corners[t] = column(ox + i32(t % 5u) * 4, oz + i32(t / 5u) * 4).height;
    }
    workgroupBarrier();
    let mh = atomicLoad(&max_h);
    var sea = -2147483647 - 1;
    if (desc[D_HAS_SEA] != 0) {
        sea = desc[D_SEA];
    }
    let cave = desc[D_HAS_CAVES] != 0 && oy <= mh;
    if (cave && t < 125u) {
        nodes[t] = cave_node(ox + i32(t % 5u) * 4, oy + i32(t / 25u) * 4, oz + i32((t / 5u) % 5u) * 4);
    }
    workgroupBarrier();
    let out = cube * WORDS_OUT;
    // Biome per column: a cave biome deep enough under it, else the column's.
    var biome = c.biome;
    let ncb = u32(desc[D_CAVE_BIOMES]);
    let co = u32(desc[D_CAVE_OFF]);
    for (var k = 0u; k < ncb; k++) {
        let b = co + k * WORDS_CAVE;
        if (oy + 15 < c.height - desc[b + 1u] &&
            (desc[b + 3u] <= -ONE ||
            perlin3(bitcast<u32>(desc[b + 4u]), (ox + i32(t % 16u)) * 16, (oy + 8) * 16, (oz + i32(t / 16u)) * 16, desc[b + 2u], desc[b + 2u], desc[b + 2u]) > desc[b + 3u])) {
            biome = desc[b];
            break;
        }
    }
    biomes[t] = biome;
    outp[out + OUT_HEIGHTS + t] = bitcast<u32>(c.height);
    workgroupBarrier();
    if (t < 128u) {
        outp[out + OUT_BIOMES + t] = u32(biomes[t * 2u]) | (u32(biomes[t * 2u + 1u]) << 16u);
        outp[out + OUT_COLUMN_BIOMES + t] = u32(col_biome[t * 2u]) | (u32(col_biome[t * 2u + 1u]) << 16u);
        let z = t / 8u;
        let x = (t % 8u) * 2u;
        let empty = oy > max(mh, sea) + 16;
        for (var y = 0u; y < 16u; y++) {
            var a = 0u;
            var b = 0u;
            if (!empty) {
                a = block(x, y, z, ox, oy, oz, cave);
                b = block(x + 1u, y, z, ox, oy, oz, cave);
            }
            outp[out + ((y * 16u + z) * 16u + x) / 2u] = a | (b << 16u);
        }
    }
}

// Different workgroups build different parts of a detailed tile. A dispatch boundary
// makes the full heightfield available to columns which trace a neighboring cave lip.
var<workgroup> lod_corners: array<i32, (LOD_GROUP_SIDE + 1u) * (LOD_GROUP_SIDE + 1u)>;

fn lod_scratch(tile: u32, k: u32) -> u32 {
    return params.cubes * LOD_SAMPLES * LOD_WORDS + (tile * LOD_SAMPLES + k) * LOD_SCRATCH_WORDS;
}

fn lod_height(scratch: u32, k: u32) -> i32 {
    return bitcast<i32>(outp[scratch + k * LOD_SCRATCH_WORDS]);
}

@compute @workgroup_size(LOD_GROUP_SIDE, LOD_GROUP_SIDE)
fn lod(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) lane: u32,
       @builtin(global_invocation_id) id: vec3<u32>) {
    let tile = wg.z;
    let job = tile * 4u;
    let s = 1 << u32(jobs[job + 2u]);
    let origin = vec2(jobs[job], jobs[job + 1u]);
    let first = origin + (vec2<i32>(wg.xy * LOD_GROUP_SIDE) - i32(LOD_BORDER)) * s + s / 2;
    let grid = first >> vec2(2u);
    let offset = first & vec2(3);
    let span = ((offset + i32(LOD_GROUP_SIDE - 1u) * min(s, 4)) >> vec2(2u)) + 2;
    let caves = desc[D_HAS_CAVES] != 0;
    // At strides of eight or more each sample already lies on a cave-grid corner.
    if caves && s < 8 {
        for (var n = lane; n < u32(span.x * span.y); n += LOD_GROUP_SIDE * LOD_GROUP_SIDE) {
            let at = (grid + vec2(i32(n) % span.x, i32(n) / span.x)) * 4;
            lod_corners[n] = column(at.x, at.y).height;
        }
    }
    workgroupBarrier();
    if any(id.xy >= vec2(LOD_SIDE)) { return; }
    let k = id.y * LOD_SIDE + id.x;
    let at = origin + (vec2<i32>(id.xy) - i32(LOD_BORDER)) * s + s / 2;
    let bx = at.x;
    let bz = at.y;
    let nr = u32(desc[D_RULES]);
    let stone = desc[D_STONE];
    var top = stone;
    var under = stone;
    var soil = 0u;
    var over = 0u;
    var open_floor = 0u;
    let c = column(bx, bz);
    var y = c.height;
    let biome = c.biome;
    var ice = -1;
    let ri = u32(c.rule);
    if ri < nr {
        let r = u32(desc[D_RULE_OFF]) + ri * WORDS_RULE;
        let bare = desc[r + 13u] != 0 && c.height > desc[r + 14u];
        if !bare {
            if desc[r + 5u] >= 0 {
                top = desc[r + 5u];
            }
            if desc[r + 10u] >= 0 && c.height < desc[r + 11u] {
                top = desc[r + 10u];
            }
            if desc[r + 6u] >= 0 && desc[r + 7u] > 0 {
                under = desc[r + 6u];
                soil = u32(min(desc[r + 7u], 65535));
            }
        }
        ice = desc[r + 12u];
    }
    if desc[D_HAS_SEA] != 0 && y < desc[D_SEA] {
        var surface = desc[D_WATER];
        if ice >= 0 {
            surface = ice;
        }
        over = (u32(desc[D_SEA]) & 65535u) | (u32(surface) << 16u);
    }
    var ground = y * 16;
    if caves && (desc[D_HAS_SEA] == 0 || y > desc[D_SEA]) {
        if s < 8 {
            let cell = (offset + vec2<i32>(id.xy % vec2(LOD_GROUP_SIDE)) * s) >> vec2(2u);
            let n = u32(cell.y * span.x + cell.x);
            let fx = bx & 3;
            let fz = bz & 3;
            let row = u32(span.x);
            ground = (lod_corners[n] * (4 - fx) + lod_corners[n + 1u] * fx) * (4 - fz)
                + (lod_corners[n + row] * (4 - fx) + lod_corners[n + row + 1u] * fx) * fz;
        }
        let lo = y - i32(LOD_CAVE_REACH) * select(1, 2, s == 1);
        let gap = lod_cave(bx, bz, ground, y, lo, lo, true);
        if gap.x < y {
            open_floor = select(LOD_CAVE_OPEN_FLOOR, 0u, gap.z != 0);
            over = u32(y - gap.x);
            y = gap.x;
            top = stone;
            under = stone;
            soil = 0u;
        }
    }
    let o = (tile * LOD_SAMPLES + k) * LOD_WORDS;
    outp[o] = (u32(y) & 65535u) | (u32(top) << 16u);
    outp[o + 1u] = (u32(biome) & 65535u) | (u32(under) << 16u);
    outp[o + 2u] = over;
    outp[o + 3u] = (soil << LOD_SOIL_DEPTH_SHIFT) | open_floor;
    if caves && s <= i32(LOD_CAVE_REACH) {
        let scratch = lod_scratch(tile, k);
        outp[scratch] = bitcast<u32>(y);
        outp[scratch + 1u] = bitcast<u32>(ground);
    }
}

@compute @workgroup_size(LOD_GROUP_SIDE, LOD_GROUP_SIDE)
fn lod_openings(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    let j = id.y;
    if i == 0u || j == 0u || i + 1u >= LOD_SIDE || j + 1u >= LOD_SIDE { return; }
    let tile = id.z;
    let job = tile * 4u;
    let s = 1 << u32(jobs[job + 2u]);
    if s > i32(LOD_CAVE_REACH) { return; }
    let k = j * LOD_SIDE + i;
    let o = (tile * LOD_SAMPLES + k) * LOD_WORDS;
    let scratch = lod_scratch(tile, 0u);
    let y = lod_height(scratch, k);
    if outp[o + 2u] != 0u || (desc[D_HAS_SEA] != 0 && y <= desc[D_SEA]) { return; }
    let low_x = min(lod_height(scratch, k - 1u), lod_height(scratch, k + 1u));
    let low_z = min(lod_height(scratch, k - LOD_SIDE), lod_height(scratch, k + LOD_SIDE));
    var lip = min(low_x, low_z);
    if s == 1 {
        // Follow the opening into adjacent roofed columns, where the nearest
        // surface is the roof rather than the floor visible through the mouth.
        let at = vec2<i32>(i32(i), i32(j));
        let limit = vec2<i32>(i32(LOD_SIDE) - 1);
        let lip_reach = 6;
        for (var d = -lip_reach; d <= lip_reach; d += 1) {
            let x = clamp(at.x + d, 0, limit.x);
            let z = clamp(at.y + d, 0, limit.y);
            lip = min(lip, min(lod_height(scratch, j * LOD_SIDE + u32(x)), lod_height(scratch, u32(z) * LOD_SIDE + i)));
        }
    }
    let lo = max(y - i32(LOD_CAVE_REACH), lip);
    if y - lo < 4 { return; }
    let bx = jobs[job] + (i32(i) - i32(LOD_BORDER)) * s + s / 2;
    let bz = jobs[job + 1u] + (i32(j) - i32(LOD_BORDER)) * s + s / 2;
    let ground = bitcast<i32>(outp[lod_scratch(tile, k) + 1u]);
    let depth = select(lo, y - 2 * i32(LOD_CAVE_REACH), s == 1);
    let gap = lod_cave(bx, bz, ground, y - 1, lo, depth, false);
    if gap.x < gap.y {
        outp[o + 3u] |= u32(y - gap.x) | (u32(y - gap.y) << LOD_CAVE_ROOF_SHIFT)
            | select(LOD_CAVE_OPEN_FLOOR, 0u, gap.z != 0);
    }
}
