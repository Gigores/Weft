//! The ground and the things growing on it have different silhouettes. Bake the mod's
//! scatter shapes from several directions once. A tile only owns their roots and records.
use std::hash::{DefaultHasher, Hash};

use glam::{IVec2, IVec3, Vec2, Vec3};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use weft_noise::{hash, mix, unit};

use super::{Columns, TileKey, WORDS, sample_offset};
use crate::client::textures::GameTextures;
use crate::content::Content;
use crate::features::{Shape, grid_layout};
use crate::generation::Snapshot;
use crate::gpu_layout::forest::{ELEVATIONS, FRAME, VIEWS};
use crate::server::lod_tops::impostor_block;

const VARIANTS: usize = 4;
const GROVE: i32 = 4; // Scatter cells per side of a baked group.
const EXTENT: f32 = 1.7320508; // Radius of the normalized [-1, 1] object box.
const MIN_SHADOW_MARGIN: i32 = 12;

pub(super) fn shadow_margin(step: i32) -> i32 {
    MIN_SHADOW_MARGIN.max(2 * step)
}
type Voxels = Vec<(IVec3, u32)>;
type Colors = FxHashMap<u32, [FacePixels; 6]>;

struct FacePixels {
    side: usize,
    rgba: Box<[[u8; 4]]>,
}
pub(super) type Atlases = Vec<Vec<Vec<u8>>>;

#[derive(Clone, Copy, Hash)]
struct Model {
    atlas: u32,
    radius: i32,
    height: i32,
    canopy_center: IVec2,
    canopy_radius: i32,
}

struct Biome {
    chance: f32,
    trees: Vec<Option<Model>>,
    groves: [Option<Model>; VARIANTS],
}

struct Scatter {
    feature: usize,
    seed: u64,
    spacing: i32,
    template: bool,
    biomes: Vec<Option<Biome>>,
}

pub(super) struct Forest {
    dim: u32,
    scatter: Vec<Scatter>,
}

pub(super) struct TileForest {
    pub(super) instances: Vec<[u32; 4]>,
    pub(super) shadow: [[i32; super::GRID]; super::GRID],
    pub(super) y: (i32, i32),
}

impl Forest {
    pub(super) fn new(
        c: &Content,
        dim: u32,
        seed: u64,
        faces: &[[(u32, u32); 6]],
        textures: &GameTextures,
        signature: &mut DefaultHasher,
    ) -> (Self, crossbeam_channel::Receiver<Atlases>) {
        let mut scatter = Vec::new();
        let mut models = Vec::new();
        let mut texels = FxHashMap::default();
        let def = &c.dims[dim as usize];
        let leaves = c.components.id("leaves");
        // Mod callbacks can inspect their actual substrate. Take representative surface sites
        // from this seed, preferring the requested biome and a ground accepted by the feature.
        let sites: Vec<_> = (0..512)
            .filter_map(|i| {
                let h = mix(i);
                let x = (h as i16) as i32;
                let z = ((h >> 32) as i16) as i32;
                let col = def.column(seed, x, z)?;
                if col.height == i32::MIN {
                    return None;
                }
                Some((col.biome, IVec3::new(x, col.height + 1, z), def.surface_block(seed, x, z)))
            })
            .collect();
        for (feature, f) in def.features.iter().enumerate() {
            let Some((spacing, chance)) = f.impostor_grid() else { continue };
            let template = matches!(f.shape, Shape::Template(_));
            let variants = if template { f.transforms.len() } else { VARIANTS };
            let snapshot = Snapshot::new(c, dim, seed, f.stage);
            let mut biomes = Vec::new();
            for b in 0..c.biomes.len() as u32 {
                let chance = chance.in_biome(c, b);
                if chance <= 0.0
                    || f.biome
                        .as_ref()
                        .is_some_and(|mask| !crate::refs::has(mask, b))
                {
                    biomes.push(None);
                    continue;
                }
                let fits = |p: IVec3, ground| {
                    (f.y[0]..=f.y[1]).contains(&p.y)
                        && (f.fit.over_water || def.sea_level().is_none_or(|sea| p.y > sea))
                        && f.fit.ground.is_none_or(|m| c.matchers.has(m, ground))
                };
                let mut roots: Vec<_> = sites
                    .iter()
                    .filter(|&&(biome, p, g)| biome == b && fits(p, g))
                    .map(|s| s.1)
                    .collect();
                roots.extend(
                    sites
                        .iter()
                        .filter(|s| s.0 != b && fits(s.1, s.2))
                        .map(|s| s.1),
                );
                let mut samples: Vec<_> = (0..variants)
                    .map(|variant| {
                        let transform = f.transforms[variant % f.transforms.len()];
                        roots
                            .iter()
                            .cycle()
                            .skip(variant)
                            .take(48.min(roots.len()))
                            .enumerate()
                            .find_map(|(attempt, &root)| {
                                let random = mix((variant * 48 + attempt) as u64);
                                let mut blocks =
                                    crate::features::sample(snapshot, feature, root - IVec3::Y * f.fit.buried, b, random, transform);
                                // A coating such as moss should not expand the silhouette by a full block.
                                blocks.retain(|&(_, state)| impostor_block(c, state));
                                (!blocks.is_empty()).then_some(blocks)
                            })
                            .unwrap_or_default()
                    })
                    .collect();
                let Some(example) = samples.iter().find(|shape| !shape.is_empty()).cloned() else {
                    biomes.push(None);
                    continue;
                };
                if !template {
                    for shape in samples.iter_mut().filter(|shape| shape.is_empty()) {
                        *shape = example.clone();
                    }
                }
                let mut colors = Colors::default();
                for &(_, state) in samples.iter().flatten() {
                    colors.entry(state).or_insert_with(|| {
                        let foliage = leaves.is_some_and(|tag| c.components.has(crate::components::BLOCK, state, tag));
                        std::array::from_fn(|dir| {
                            let (tex, tint) = faces[state as usize][dir];
                            // Keep individual leaf holes in the bake instead of averaging the whole face.
                            let side = if foliage { 16 } else { 4 };
                            let face = texels
                                .entry((tex, side))
                                .or_insert_with(|| textures.impostor_texels(tex, side));
                            let tint = c.kind_color(tint as u8, Some(b as u16));
                            let shade = [0.6, 0.6, 0.5, 1.0, 0.8, 0.8][dir];
                            let rgba = face
                                .iter()
                                .map(|&[r, g, b, a]| {
                                    let rgb = [r, g, b];
                                    let rgb: [u8; 3] = std::array::from_fn(|i| {
                                        let tint = ((tint >> (16 - 8 * i)) & 255) as f32 / 255.0;
                                        (rgb[i] as f32 * tint * shade) as u8
                                    });
                                    [rgb[0], rgb[1], rgb[2], a]
                                })
                                .collect();
                            FacePixels { side, rgba }
                        })
                    });
                }
                let palette = std::sync::Arc::new(colors);
                let mut register = |blocks: Voxels| {
                    let radius = blocks
                        .iter()
                        .map(|(p, _)| (-p.x).max(p.x + 1).max(-p.z).max(p.z + 1))
                        .max()?;
                    let height = blocks.iter().map(|(p, _)| p.y + 1).max()?;
                    let canopy: FxHashSet<_> = blocks
                        .iter()
                        .filter(|(p, _)| p.y >= height / 2)
                        .map(|(p, _)| IVec2::new(p.x, p.z))
                        .collect();
                    let area = canopy.len() as f32;
                    let center = canopy.iter().map(|p| p.as_vec2() + 0.5).sum::<Vec2>() / area;
                    let model = Model {
                        radius,
                        height,
                        atlas: models.len() as u32,
                        canopy_center: center.round().as_ivec2(),
                        canopy_radius: (area / std::f32::consts::PI).sqrt().ceil() as i32,
                    };
                    model.hash(signature);
                    models.push((model, blocks, palette.clone()));
                    Some(model)
                };
                let trees: Vec<_> = samples.iter().cloned().map(&mut register).collect();
                let groves = std::array::from_fn(|v| {
                    if spacing > (1 << super::MAX_LEVEL) / 2 {
                        return None;
                    }
                    let mut blocks = Vec::new();
                    for z in 0..GROVE {
                        for x in 0..GROVE {
                            let h = hash(mix(v as u64), x as i64, 0, z as i64);
                            if unit(h.rotate_left(17)) >= chance as f64 {
                                continue;
                            }
                            let ((dx, dz), _, random, _) = grid_layout(c, f, (x, z), spacing, h);
                            let v = variant(random, template, samples.len());
                            let offset = IVec3::new(dx - GROVE * spacing / 2, 0, dz - GROVE * spacing / 2);
                            blocks.extend(samples[v].iter().map(|&(p, s)| (p + offset, s)));
                        }
                    }
                    register(blocks)
                });
                (b, chance.to_bits(), &trees, groves).hash(signature);
                biomes.push(Some(Biome { chance, trees, groves }));
            }
            if biomes.iter().any(Option::is_some) {
                let ground = f.fit.ground.map(|m| c.matchers.set(m));
                (feature, f.salt, spacing, f.y, f.fit.buried, f.fit.over_water, ground).hash(signature);
                scatter.push(Scatter { feature, seed: seed ^ f.salt, spacing, template, biomes });
            }
        }
        let (tx, rx) = crossbeam_channel::bounded(1);
        crate::background().spawn(move || {
            let atlases = models
                .into_par_iter()
                .map(|(model, blocks, colors)| bake(model, &blocks, &colors))
                .collect();
            let _ = tx.send(atlases);
        });
        (Self { dim, scatter }, rx)
    }

    pub(super) fn instances(
        &self,
        c: &Content,
        key: TileKey,
        words: &[u32],
        heights: &[[i32; super::GRID]; super::GRID],
        columns: &Columns,
    ) -> TileForest {
        let mut result = TileForest { instances: Vec::new(), shadow: [[0; super::GRID]; super::GRID], y: (i32::MAX, i32::MIN) };
        if key.level == 0 {
            return result;
        }
        let origin = key.origin();
        let step = 1 << key.level;
        let margin = shadow_margin(step);
        let def = &c.dims[self.dim as usize];
        for rule in &self.scatter {
            if !rule.template
                && !words.chunks_exact(WORDS).any(|w| {
                    rule.biomes
                        .get((w[1] & 0xffff) as usize)
                        .is_some_and(Option::is_some)
                })
            {
                continue;
            }
            let f = &def.features[rule.feature];
            // A coarse tile samples at most one grove per terrain cell, even at the horizon.
            let groups = if step / 2 < rule.spacing { 1 } else { GROVE << (step / (GROVE * rule.spacing)).max(1).ilog2() };
            let stride = groups * rule.spacing;
            let end = origin + IVec2::splat(key.size() - 1);
            // Refine only patches containing saved columns. An edited forest then uses
            // individual sites beside the edits while the rest keeps its ordinary draw cost.
            let mut edited = FxHashSet::default();
            if groups > 1 {
                for (site, _) in columns
                    .iter()
                    .filter(|(_, col)| col.top != 0 || col.removed_scatter.contains(&f.id))
                {
                    let mut n = groups;
                    while n >= GROVE {
                        let width = n * rule.spacing;
                        edited.insert((n, site.x.div_euclid(width), site.y.div_euclid(width)));
                        n /= 2;
                    }
                }
            }
            let mut patches = Vec::new();
            // Include the crowns just across the tile edge. The sampled terrain border
            // supplies their height and biome even if the adjacent mesh isn't ready.
            for z in origin.y.saturating_sub(margin).div_euclid(stride)..=end.y.saturating_add(margin).div_euclid(stride) {
                for x in origin.x.saturating_sub(margin).div_euclid(stride)..=end.x.saturating_add(margin).div_euclid(stride) {
                    patches.push((groups, x, z));
                }
            }
            while let Some((groups, x, z)) = patches.pop() {
                if edited.contains(&(groups, x, z)) {
                    let div = if groups == GROVE { GROVE } else { 2 };
                    for dz in 0..div {
                        for dx in 0..div {
                            patches.push((groups / div, x * div + dx, z * div + dz));
                        }
                    }
                    continue;
                }
                let stride = groups * rule.spacing;
                let h = hash(rule.seed, x as i64, 0, z as i64);
                let (site, foot, random) = if groups == 1 {
                    let ((wx, wz), foot, random, _) = grid_layout(c, f, (x, z), stride, h);
                    (IVec2::new(wx, wz), foot, random)
                } else {
                    (IVec2::new(x * stride + stride / 2, z * stride + stride / 2), IVec3::ZERO, hash(h, 1, 2, 3))
                };
                let local = site - origin;
                if local.min_element() < -margin || local.max_element() >= key.size() + margin {
                    continue;
                }
                let cell = local
                    .div_euclid(IVec2::splat(step))
                    .clamp(IVec2::splat(-1), IVec2::splat(super::CELLS));
                let k = sample_offset(cell.x, cell.y);
                if columns
                    .get(&site)
                    .is_some_and(|col| col.removed_scatter.contains(&f.id))
                {
                    continue;
                }
                let seed = rule.seed ^ f.salt;
                let (model, base) = if groups == 1 && rule.template {
                    let Some(start) = crate::features::start_at(c, def, self.dim, seed, rule.feature, (x, z)) else { continue };
                    if !start
                        .lod_blocks(Snapshot::new(c, self.dim, seed, f.stage))
                        .iter()
                        .any(|&(_, state)| impostor_block(c, state))
                    {
                        continue;
                    }
                    let mid = start.pos + foot / 2;
                    let Some(col) = def.column(seed, mid.x, mid.z) else { continue };
                    let Some(b) = rule.biomes.get(col.biome as usize).and_then(Option::as_ref) else { continue };
                    (b.trees[variant(random, rule.template, b.trees.len())], start.pos.y)
                } else {
                    let biome = words[k + 1] & 0xffff;
                    let Some(b) = rule.biomes.get(biome as usize).and_then(Option::as_ref) else { continue };
                    let base = super::column(heights, cell.x, cell.y) + 1;
                    let surface = words[k + 2];
                    let blocked = if surface >> 16 != 0 { !f.fit.over_water } else { surface != 0 && !f.fit.over_caves };
                    if blocked
                        || !(f.y[0]..=f.y[1]).contains(&base)
                        || f.fit
                            .ground
                            .is_some_and(|m| !c.matchers.has(m, words[k] >> 16))
                    {
                        continue;
                    }
                    let model = if groups == 1 {
                        if unit(h.rotate_left(17)) >= b.chance as f64 {
                            continue;
                        }
                        b.trees[variant(random, false, b.trees.len())]
                    } else {
                        b.groves[variant(random, false, VARIANTS)]
                    };
                    (model, base - f.fit.buried)
                };
                let Some(model) = model else { continue };
                let magnification = if groups == 1 { 1.0 } else { groups as f32 / GROVE as f32 };
                let radius = model.radius as f32 * magnification;
                let offset = model.canopy_center.as_vec2() * magnification;
                let canopy = (model.canopy_radius as f32 * magnification).min(margin as f32 - offset.abs().max_element());
                if canopy > 0.0 {
                    result.shade(local.as_vec2() + offset, step, canopy, base + model.height, heights);
                }
                if local.min_element() < 0 || local.max_element() >= key.size() || result.instances.len() >= super::MAX_QUADS {
                    continue;
                }
                let p = local / key.scale();
                result.instances.push([
                    p.x as u32 | (p.y as u32) << 16,
                    base as u16 as u32 | (model.height as u32) << 16,
                    radius.to_bits(),
                    model.atlas,
                ]);
                result.y.0 = result.y.0.min(base);
                result.y.1 = result.y.1.max(base + model.height);
            }
        }
        result
    }
}

fn variant(random: u64, template: bool, count: usize) -> usize {
    let choice = if template { random >> 40 } else { random };
    (choice % count as u64) as usize
}

impl TileForest {
    pub(super) fn shade(&mut self, local: Vec2, step: i32, radius: f32, top: i32, heights: &[[i32; super::GRID]; super::GRID]) {
        let p = local / step as f32;
        let r = radius / step as f32;
        let extent = (r + 0.35).max(0.75);
        let min = (p - extent).floor().as_ivec2().max(IVec2::splat(-1));
        let max = (p + extent)
            .floor()
            .as_ivec2()
            .min(IVec2::splat(super::CELLS));
        let strength = 8.0 * (r / extent).min(1.0);
        for j in min.y..=max.y {
            for i in min.x..=max.x {
                if super::column(heights, i, j) >= top {
                    continue;
                }
                let dist = (IVec2::new(i, j).as_vec2() + 0.5 - p).length() / extent;
                let light = ((1.0 - dist).clamp(0.0, 1.0) * strength).round() as i32;
                let cell = &mut self.shadow[(i + 1) as usize][(j + 1) as usize];
                *cell = (*cell).max(light);
            }
        }
    }
}

/// Raycast the occupied blocks orthographically. Empty pixels between limbs and crowns survive
/// in the atlas. Both the bake and the vertex shader use the normalized object box, so a grove
/// can cover a wider area at lower detail without making the trees taller.
fn bake(model: Model, blocks: &[(IVec3, u32)], colors: &Colors) -> Vec<Vec<u8>> {
    let full_width = model.radius * 2;
    // Sampling above twice the atlas resolution cannot contribute silhouette detail. In
    // particular, empty space between widely spaced trees must not allocate a huge voxel grid.
    let width = full_width.min((2 * FRAME) as i32);
    let height = model.height.min((2 * FRAME) as i32);
    let size = Vec3::new(width as f32, height as f32, width as f32);
    let half = size * 0.5;
    let dims = size.as_ivec3();
    let mut grid = vec![0; width as usize * width as usize * height as usize];
    let index = |p: IVec3| ((p.y * width + p.z) * width + p.x) as usize;
    for &(p, state) in blocks {
        let p = p + IVec3::new(model.radius, 0, model.radius);
        let quantize = |v: i32, actual: i32, target: i32| (v as i64 * target as i64 / actual as i64) as i32;
        grid[index(IVec3::new(quantize(p.x, full_width, width), quantize(p.y, model.height, height), quantize(p.z, full_width, width)))] =
            state;
    }
    let atlas = FRAME * VIEWS;
    let mut pixels = vec![0; (atlas * atlas * 4) as usize];
    for row in 0..ELEVATIONS {
        let pitch = (row as f32 / (ELEVATIONS - 1) as f32 - 0.5) * std::f32::consts::PI;
        for col in 0..VIEWS {
            let yaw = col as f32 * std::f32::consts::TAU / VIEWS as f32;
            let normal = Vec3::new(yaw.sin() * pitch.cos(), pitch.sin(), yaw.cos() * pitch.cos());
            let right = Vec3::new(yaw.cos(), 0.0, -yaw.sin());
            let up = normal.cross(right);
            let ray = -normal * half;
            // Parallel axes must never win a DDA step (including rays on a slab boundary).
            let inv = ray.map(|v| if v.abs() < 1e-6 { 1e20 } else { v.recip() });
            let step = ray.signum().as_ivec3();
            for y in 0..FRAME {
                for x in 0..FRAME {
                    let u = ((x as f32 + 0.5) / FRAME as f32 * 2.0 - 1.0) * EXTENT;
                    let v = (1.0 - (y as f32 + 0.5) / FRAME as f32 * 2.0) * EXTENT;
                    let start = (right * u + up * v + normal * 2.0) * half + half;
                    let a = -start * inv;
                    let b = (size - start) * inv;
                    let near = a.min(b);
                    let t = near.max_element();
                    if t >= a.max(b).min_element() {
                        continue;
                    }
                    let mut axis = if near.x == t {
                        0
                    } else if near.y == t {
                        1
                    } else {
                        2
                    };
                    let mut p = (start + ray * (t + 1e-4))
                        .floor()
                        .as_ivec3()
                        .clamp(IVec3::ZERO, dims - IVec3::ONE);
                    let edge = p.as_vec3() + ray.map(|d| if d > 0.0 { 1.0 } else { 0.0 });
                    let mut next = ((edge - start) * inv).abs();
                    for i in 0..3 {
                        if ray[i].abs() < 1e-6 {
                            next[i] = f32::INFINITY;
                        }
                    }
                    let mut hit = t;
                    let mut rgb = Vec3::ZERO;
                    let mut alpha = 0.0;
                    while p.cmpge(IVec3::ZERO).all() && p.cmplt(dims).all() {
                        let state = grid[index(p)];
                        if state != 0 {
                            let dir = axis * 2 + (step[axis] < 0) as usize;
                            let at = (start + ray * hit) * Vec3::new(full_width as f32, model.height as f32, full_width as f32) / size;
                            let uv = match axis {
                                0 => [at.z, -at.y],
                                1 => [at.x, at.z],
                                _ => [at.x, -at.y],
                            };
                            let face = &colors[&state][dir];
                            let [u, v] = uv.map(|v| ((v.rem_euclid(1.0) * face.side as f32) as usize).min(face.side - 1));
                            let [r, g, b, a] = face.rgba[v * face.side + u];
                            let mut outside = p;
                            outside[axis] -= step[axis];
                            let shade = if outside.cmpge(IVec3::ZERO).all()
                                && outside.cmplt(dims).all()
                                && (1..=3).any(|dy| outside.y + dy < dims.y && grid[index(outside + IVec3::Y * dy)] != 0)
                            {
                                0.78
                            } else {
                                1.0
                            };
                            let cover = (1.0 - alpha) * a as f32 / 255.0;
                            rgb += shade * cover * Vec3::new(r as f32, g as f32, b as f32);
                            alpha += cover;
                            if alpha >= 0.99 {
                                break;
                            }
                        }
                        axis = if next.x < next.y && next.x < next.z {
                            0
                        } else if next.y < next.z {
                            1
                        } else {
                            2
                        };
                        hit = next[axis];
                        p[axis] += step[axis];
                        next[axis] += inv[axis].abs();
                    }
                    if alpha > 0.0 {
                        let pixel = (((row * FRAME + y) * atlas + col * FRAME + x) * 4) as usize;
                        pixels[pixel..pixel + 3].copy_from_slice(&(rgb / alpha).to_array().map(|v| v.round() as u8));
                        pixels[pixel + 3] = (alpha * 255.0).round() as u8;
                    }
                }
            }
        }
    }
    let mut mips = crate::assets::mip_chain(pixels, atlas);
    mips.truncate(FRAME.ilog2() as usize + 1);
    for (level, mip) in mips.iter_mut().enumerate() {
        mip.truncate(((atlas >> level) * ((ELEVATIONS * FRAME) >> level) * 4) as usize);
    }
    mips
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atlas_keeps_directional_silhouettes_and_transparent_faces() {
        let model = Model { radius: 4, height: 5, atlas: 0, canopy_center: IVec2::ZERO, canopy_radius: 4 };
        let fence: Vec<_> = (-4..4)
            .flat_map(|x| (0..5).map(move |y| (IVec3::new(x, y, 0), 1)))
            .collect();
        let mut colors = Colors::default();
        let pixels = |alpha| std::array::from_fn(|_| FacePixels { side: 4, rgba: Box::new([[180, 100, 50, alpha]; 16]) });
        colors.insert(1, pixels(255));
        let mips = bake(model, &fence, &colors);
        let solid = |view: u32| {
            (0..FRAME)
                .flat_map(|y| (0..FRAME).map(move |x| ((2 * FRAME + y) * FRAME * VIEWS + view * FRAME + x) as usize))
                .filter(|&i| mips[0][i * 4 + 3] != 0)
                .count()
        };
        assert!(solid(0) > 4 * solid(VIEWS / 4));
        assert!(solid(VIEWS / 4) > 0);
        for (level, data) in mips.iter().enumerate() {
            assert_eq!(data.len(), (VIEWS * ELEVATIONS * (FRAME >> level).pow(2) * 4) as usize);
        }
        colors.insert(1, pixels(0));
        assert!(
            bake(model, &fence, &colors)[0]
                .chunks_exact(4)
                .all(|p| p[3] == 0)
        );
    }
}
