//! Distant terrain beyond the view: made on the GPU straight from the dimension's terrain at a
//! coarser step (`shaders/terrain.wgsl`'s `lod`), so it reaches as far as wanted for a few
//! kilobytes a tile. A tile is 32x32 cells of 2^level blocks; each cell is the column at its
//! middle: its top block, the one under it, its biome, and any water over it. Where saved
//! cubes made a column's top other than the terrain's (a town, a crater), the server says so
//! (`server::lod_tops`) and that column is taken instead. What stands above the ground is drawn
//! as its own spans, so a building keeps its roof and its walls.
//! Tiles split by projected cell size, using the current camera and render resolution; one is
//! shown once made, its parent or children standing in until then. Where a column of real cubes
//! shows its ground the tiles' pixels are dropped (`fs_lod`, `Lod::mask`), so they fill in for
//! cubes not made or meshed yet (flying fast), and nowhere stand through them.

use std::collections::{BTreeMap, BinaryHeap};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use glam::{DVec3, IVec2, IVec3, Mat4};
use rustc_hash::{FxHashMap, FxHashSet};
use weft_abi::{BlockModel, FACE_DIRS, entry_kind as ek};

use crate::background;
use crate::config;
use crate::content::{Content, StateInfo, content};
use crate::gpu_layout::terrain::lod as lod_gpu;
use crate::gpu_layout::{CORNERS, face, lod_box, lod_face, lod_record};
use crate::server::lod_tops::{LodColumn, Span, shows};

mod cache;
mod cover;
mod detail;
mod forest;

/// Columns that differ from the terrain, as the server said.
type Columns = FxHashMap<IVec2, Arc<LodColumn>>;
// A failed warm-cache probe goes back through normal coarse-first generation.
enum BuiltTile {
    Meshed(Arc<LodMesh>),
    Far(TileKey),
}

type TileResult = Result<BuiltTile, TileKey>;
type SavedColumns = BTreeMap<(i32, i32), Arc<LodColumn>>;

/// Cells a tile side.
pub const CELLS: i32 = 32;
/// Columns a tile is made from: its cells and one around them (for the walls at its edges).
pub const SAMPLES: usize = lod_gpu::SAMPLES as usize;
const SIDE: i32 = lod_gpu::SIDE as i32;
const BORDER: i32 = lod_gpu::BORDER as i32;
const GRID: usize = (CELLS + 2) as usize;
/// Words per sample and per tile coming back from the GPU.
const WORDS: usize = lod_gpu::WORDS as usize;
pub const LOD_WORDS_PER_TILE: usize = SAMPLES * WORDS;
const MAX_LEVEL: u32 = i32::BITS - 2 - CELLS.ilog2();
/// At 32 blocks per cell, fine scenery is left to the more detailed levels.
const FAR_LEVEL: u32 = 5;
const FAR_TILES_PER_FRAME: usize = 32;
/// Blocks under the land from where distant terrain is left out.
const UNDERGROUND: i32 = 32;
/// Tiles asked for and not back yet, at most.
const IN_FLIGHT: usize = 256;
/// Quads a tile may hold. Past this the index buffer does not cover the draw.
const MAX_QUADS: usize = 48_000;
/// More model boxes than this become one box around all of them.
const MAX_MODEL_BOXES: usize = 6;
/// Time taken to dither between a parent tile and its four children.
const LOD_FADE: Duration = Duration::from_millis(250);
/// Extra face records kept on the GPU when a tile leaves the selected tree (32 MiB).
const SPARE_QUADS: usize = (32 << 20) / 16;
/// After this far below the cave mouth the diffuse sky is gone.
const CAVE_LIGHT_REACH: i32 = 15;

/// A tile: its level and place in the grid of its level's tiles.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct TileKey {
    pub level: u32,
    pub x: i32,
    pub z: i32,
}

impl TileKey {
    /// Width in blocks.
    pub fn size(self) -> i32 {
        CELLS << self.level
    }

    /// Lowest block corner (x, z).
    pub fn origin(self) -> IVec2 {
        IVec2::new(self.x, self.z) * self.size()
    }

    fn scale(self) -> i32 {
        1 << self
            .level
            .saturating_sub(crate::gpu_layout::lod_draw::PACKED_LEVEL)
    }

    fn parent(self) -> TileKey {
        TileKey { level: self.level + 1, x: self.x.div_euclid(2), z: self.z.div_euclid(2) }
    }

    fn children(self) -> [TileKey; 4] {
        std::array::from_fn(|i| TileKey { level: self.level - 1, x: self.x * 2 + (i as i32 & 1), z: self.z * 2 + (i as i32 >> 1) })
    }

    /// Nearest and farthest horizontal distance from `p` (x, z) to it.
    fn distance(self, p: glam::DVec2) -> (f64, f64) {
        let lo = self.origin().as_dvec2();
        let hi = lo + glam::DVec2::splat(self.size() as f64);
        let near = (lo - p).max(p - hi).max(glam::DVec2::ZERO).length();
        let far = (p - lo).abs().max((p - hi).abs()).length();
        (near, far)
    }
}

/// A tile for the GPU to make.
pub struct LodJob {
    pub dim: u32,
    pub seed: u64,
    pub key: TileKey,
    done: Sender<TileResult>,
    terrain: Arc<TileTerrain>,
    cache: cache::Entry,
    /// What the server said of columns, taken over the terrain's where the tile samples them.
    columns: Columns,
    edits: Vec<crate::server::area::AreaFill>,
}

impl LodJob {
    pub(super) fn description(&self) -> (u64, &[i32]) {
        (self.terrain.signature, &self.terrain.description)
    }
}

struct TileTerrain {
    forest: forest::Forest,
    cover: Option<cover::Cover>,
    description: Vec<i32>,
    faces: Vec<[(u32, u32); 6]>,
    ctm: Arc<super::ctm::Ctm>,
    signature: u64,
    stone: u32,
    detail_margin: i32,
}

impl TileTerrain {
    fn new(
        c: &Content,
        dim: u32,
        seed: u64,
        shader: u64,
        textures: &super::textures::GameTextures,
    ) -> Option<(Self, Receiver<forest::Atlases>)> {
        let description = super::terrain_gpu::describe(c, dim, seed)?;
        let looks = super::looks::current();
        let ctm = super::ctm::current();
        let mut hash = DefaultHasher::new();
        (shader, &description).hash(&mut hash);
        for m in &c.mods {
            (&m.id, &m.version, m.revision).hash(&mut hash);
        }
        let mut faces = Vec::with_capacity(c.states.len());
        for (id, state) in c.states.iter().enumerate() {
            let id = id as u32;
            (
                state.block,
                state.flags,
                state.model as u32,
                state.layer as u32,
                c.tight[id as usize].opacity,
                shows(c, id),
                ctm.hides(state.flags, id),
            )
                .hash(&mut hash);
            let drawn = looks.drawn(id, state);
            let sides: [_; 6] = std::array::from_fn(|dir| {
                let first = super::mesher::face_layers(c, state, drawn.textures[dir] & face::TEX_MASK, dir)[0];
                (first.tex, first.tint as u32)
            });
            sides.hash(&mut hash);
            faces.push(sides);
            if state.model == BlockModel::Boxes {
                model_boxes(state).hash(&mut hash);
            }
        }
        let (forest, atlases) = forest::Forest::new(c, dim, seed, &faces, textures, &mut hash);
        let cover = cover::Cover::new(c, dim, &faces, textures, &mut hash);
        let stone = c.dims[dim as usize].terrain.as_ref()?.stone;
        let detail_margin = c.dims[dim as usize]
            .features
            .iter()
            .filter(|f| f.impostor)
            .map(|f| f.reach[0] + 1)
            .max()
            .unwrap_or(1);
        Some((Self { forest, cover, description, faces, ctm, signature: hash.finish(), stone, detail_margin }, atlases))
    }

    fn far_description(&self, c: &Content, dim: u32, textures: &super::textures::GameTextures) -> Vec<i32> {
        let terrain = c.dims[dim as usize]
            .terrain
            .as_ref()
            .expect("LOD terrain description");
        let states = std::iter::once(terrain.stone)
            .chain(terrain.sea.map(|(_, state)| state))
            .chain(terrain.rules.iter().flat_map(|r| {
                [r.top, r.deep_top.map(|(b, _)| b), r.ice]
                    .into_iter()
                    .flatten()
            }));
        let mut used = FxHashSet::default();
        let mut pixels = FxHashMap::default();
        let linear = |byte: u8| ((f32::from(byte) / 255.0).powf(2.2) * 255.0).round() as u8;
        let mut words = self.description.clone();
        let palette = words.len();
        let colors = palette + 2 + c.states.len() * 2;
        words.extend_from_slice(&[c.tint_kinds.len() as i32, colors as i32]);
        words.resize(colors, 0);
        for state in states.filter(|&state| used.insert(state)) {
            let (tex, tint) = self.faces[state as usize][3];
            let [r, g, b, a] = *pixels
                .entry(tex)
                .or_insert_with(|| textures.impostor_texels(tex, 1)[0]);
            let alpha = if c.states[state as usize].layer == weft_abi::RenderLayer::Translucent { a.clamp(32, 254) } else { 255 };
            let at = palette + 2 + state as usize * 2;
            words[at] = i32::from_le_bytes([linear(r), linear(g), linear(b), alpha]);
            words[at + 1] = tint as i32;
        }
        let color_word = |rgb: u32| {
            let [b, g, r, _] = rgb.to_le_bytes();
            i32::from_le_bytes([linear(r), linear(g), linear(b), 255])
        };
        if c.biomes.is_empty() {
            words.extend(c.tint_kinds.iter().map(|&(_, rgb)| color_word(rgb)));
        } else {
            words.extend(
                c.biomes
                    .iter()
                    .flat_map(|b| b.tints.iter().copied())
                    .map(color_word),
            );
        }
        words
    }
}

/// A made tile's quads (`mesh`), how many of each part and direction, and the heights they span.
pub struct LodMesh {
    pub key: TileKey,
    pub quads: Vec<[u32; 4]>,
    pub counts: Counts,
    pub y: (i32, i32),
}

/// How many quads a tile has, per part (`ALWAYS`, `BESIDE_CUBES`, `SURFACE`, `FOREST`) and per direction
/// rank (`mesher::DIR_RANK`), in that order.
pub type Counts = [[u32; 6]; PARTS];
/// All a tile shows that is opaque.
pub const ALWAYS: usize = 0;
/// Skirts beside a neighbor as high: only seen where that neighbor's pixels give way to cubes.
pub const BESIDE_CUBES: usize = 1;
/// A see-through surface over the ground (a sea), drawn after everything opaque.
pub const SURFACE: usize = 2;
/// Diagonal cutout faces that the GPU culls by their actual orientation.
pub const PLANTS: usize = 3;
/// View-facing scatter instances, all in direction bucket zero.
pub const FOREST: usize = 4;
const PARTS: usize = 5;

/// What came back for `job`, meshed on a worker.
pub fn made(job: LodJob, mut words: Vec<u32>) {
    background().spawn(move || {
        if !job.columns.is_empty() || !job.edits.is_empty() {
            let (s, o) = (1 << job.key.level, job.key.origin());
            let c = content();
            let dim = &c.dims[job.dim as usize];
            let heights = lod_edit_heights(dim.min_cube_y, dim.max_cube_y);
            for k in 0..SAMPLES {
                let (i, j) = ((k as i32 % SIDE) - BORDER, (k as i32 / SIDE) - BORDER);
                let at = o + IVec2::new(i * s + s / 2, j * s + s / 2);
                let w = k * WORDS;
                if let Some(col) = job.columns.get(&at).filter(|col| s <= 2 && col.top != 0) {
                    let (y, top, under) = (col.y, col.top, col.under);
                    let natural_y = (words[w] as u16) as i16 as i32;
                    if y != natural_y {
                        if words[w + 2] >> 16 == 0 {
                            words[w + 2] = 0;
                        }
                        let gap = words[w + 3];
                        let depth_mask = lod_gpu::CAVE_DEPTH_MASK;
                        let floor = (gap & depth_mask) as i32;
                        let roof = ((gap >> lod_gpu::CAVE_ROOF_SHIFT) & depth_mask) as i32;
                        let shift = y.saturating_sub(natural_y);
                        words[w + 3] &= !0xFFFF;
                        if floor > roof + 1 && shift >= -roof && shift <= depth_mask as i32 - floor {
                            words[w + 3] |= (floor + shift) as u32
                                | ((roof + shift) as u32) << lod_gpu::CAVE_ROOF_SHIFT
                                | gap & lod_gpu::CAVE_OPEN_FLOOR;
                        }
                    }
                    words[w] = (y as u32 & 0xFFFF) | (top << 16);
                    words[w + 1] = (words[w + 1] & 0xFFFF) | (under << 16);
                } else if !job.edits.is_empty() {
                    apply_surface_edits(c, &job.edits, at, heights.clone(), job.terrain.stone, &mut words[w..w + WORDS]);
                }
            }
        }
        let snapshot = crate::generation::Snapshot::new(content(), job.dim, job.seed, u32::MAX);
        let (quads, counts, y) = mesh(snapshot, job.key, &words, &job.columns, &job.terrain);
        let mesh = Arc::new(LodMesh { key: job.key, quads, counts, y });
        let _ = job.done.send(Ok(BuiltTile::Meshed(mesh.clone())));
        job.cache.store(&mesh);
    });
}

fn lod_edit_heights(min_cube_y: i32, max_cube_y: i32) -> std::ops::RangeInclusive<i32> {
    // Open-ended dimensions use i32 extrema. LOD samples only store i16 heights.
    let min_y = min_cube_y.saturating_mul(16).max(i16::MIN as i32);
    let max_y = max_cube_y
        .saturating_mul(16)
        .saturating_add(15)
        .min(i16::MAX as i32);
    min_y..=max_y
}

/// Apply a fill to the same column that the distant mesh samples, including columns that were
/// never loaded. The tile already approximates undisplayed layers with its under-surface block.
fn apply_surface_edits(
    c: &Content,
    edits: &[crate::server::area::AreaFill],
    at: IVec2,
    limits: std::ops::RangeInclusive<i32>,
    stone: u32,
    words: &mut [u32],
) {
    let (min_y, max_y) = limits.into_inner();
    let y = (words[0] as u16) as i16 as i32;
    let top = words[0] >> 16;
    let under = words[1] >> 16;
    let soil = (words[3] >> lod_gpu::SOIL_DEPTH_SHIFT) as i32;
    let liquid = words[2] >> 16;
    let liquid_y = words[2] as u16 as i16 as i32;
    let at_y = |y| DVec3::new(at.x as f64 + 0.5, y as f64 + 0.5, at.y as f64 + 0.5);
    let mut lo = max_y;
    let mut hi = min_y;
    let mut touched = false;
    for edit in edits {
        let (min, max) = edit.shape.bounds();
        let start = ((min.y - 0.5).ceil() as i32).max(min_y);
        let end = ((max.y - 0.5).floor() as i32).min(max_y);
        if start > end || !edit.shape.test_box(at_y(start), at_y(end)).1 {
            continue;
        }
        touched = true;
        lo = lo.min(start);
        if shows(c, edit.state) {
            hi = hi.max(end);
        }
    }
    if !touched {
        return;
    }
    let read = |height: i32| {
        let original = match height.cmp(&y) {
            std::cmp::Ordering::Less if y - height <= soil => under,
            std::cmp::Ordering::Less => stone,
            std::cmp::Ordering::Equal => top,
            std::cmp::Ordering::Greater if liquid != 0 && height <= liquid_y => liquid,
            std::cmp::Ordering::Greater => 0,
        };
        let p = IVec3::new(at.x, height, at.y);
        edits
            .iter()
            .fold(original, |state, edit| edit.state_at(p, state))
    };
    let low = (lo - 1).min(y).max(min_y);
    let high = hi.max(y).max(if liquid == 0 { y } else { liquid_y });
    let mut water = None;
    let mut ground = (low, 0);
    for height in (low..=high).rev() {
        let state = read(height);
        if !shows(c, state) {
            continue;
        }
        if c.states[state as usize].liquid.is_some() {
            water.get_or_insert((height, state));
        } else {
            ground = (height, state);
            break;
        }
    }
    let (new_y, new_top) = ground;
    if ground != (y, top) {
        words[0] = (new_y as u16 as u32) | (new_top << 16);
        words[1] = (words[1] & 0xFFFF) | (read(new_y - 1) << 16);
        let depth = (soil + new_y - y).clamp(0, u16::MAX as i32);
        words[3] = (depth as u32) << lod_gpu::SOIL_DEPTH_SHIFT;
    }
    if liquid != 0 || water.is_some() || ground != (y, top) {
        words[2] = water.map_or(0, |(height, state)| (height as u16 as u32) | (state << 16));
    }
}

/// A record of `world.wgsl`'s `vs_lod`: the `dir` face of a box at (x, y, z) in the tile, size
/// (sx, sy, sz) blocks, drawn with texture `tex`, tinted for `biome` when `tint`, corners in `ao`,
/// lit by block light `light` (`lod_tops::Span::light`) with how much of the sky's it lacks in
/// the top byte.
fn quad(
    dir: u32,
    x: i32,
    y: i32,
    z: i32,
    size: (i32, i32, i32),
    tex: u32,
    tint: u32,
    biome: u32,
    ao: u32,
    light: u32,
    scale: i32,
) -> [u32; 4] {
    let rgb = [0, 8, 16].map(|s| (light >> s) as u8);
    let sky = 15u8.saturating_sub((light >> 24) as u8);
    let extent = |n: i32| (n + scale - 1) / scale;
    let mut r = lod_record(dir, [x / scale, y, z / scale], [extent(size.0), size.1, extent(size.2)], tex, tint, biome, sky, rgb);
    if scale > 1 && (x % scale != 0 || z % scale != 0 || size.0 % scale != 0 || size.2 % scale != 0) {
        let (min, max) = scaled_edges(x, z, scale, [0; 3], [16; 3]);
        r = lod_box(r, min, max, true);
    }
    r[3] |= (ao & 0xFF) << 24;
    r
}

fn scaled_edges(x: i32, z: i32, scale: i32, mut min: [u32; 3], mut max: [u32; 3]) -> ([u32; 3], [u32; 3]) {
    for (axis, at) in [(0, x), (2, z)] {
        let offset = (at % scale) as u32 * 16;
        let scale = scale as u32;
        min[axis] = (offset + min[axis]) / scale;
        max[axis] = ((offset + max[axis] + scale - 1) / scale)
            .max(min[axis] + 1)
            .min(16);
    }
    (min, max)
}

fn sample_offset(i: i32, j: i32) -> usize {
    ((j + BORDER) * SIDE + i + BORDER) as usize * WORDS
}

fn corner_y(words: &[u32], gi: i32, gj: i32) -> i32 {
    let gi = gi.clamp(-1, CELLS);
    let gj = gj.clamp(-1, CELLS);
    let k = sample_offset(gi, gj);
    (words[k] & 0xFFFF) as u16 as i16 as i32
}

/// Empty blocks between the first solid floor and roof, as a half-open interval.
fn cave_gap(words: &[u32], i: i32, j: i32) -> Option<(i32, i32)> {
    let w = words[sample_offset(i, j) + 3];
    let floor = (w & lod_gpu::CAVE_DEPTH_MASK) as i32;
    let roof = ((w >> lod_gpu::CAVE_ROOF_SHIFT) & lod_gpu::CAVE_DEPTH_MASK) as i32;
    (floor > roof + 1).then(|| {
        let y = corner_y(words, i, j);
        (y - floor + 1, y - roof)
    })
}

fn cave_has_floor(words: &[u32], i: i32, j: i32) -> bool {
    words[sample_offset(i, j) + 3] & lod_gpu::CAVE_OPEN_FLOOR == 0
}

fn cave_cut(words: &[u32], i: i32, j: i32) -> bool {
    let w = words[sample_offset(i, j) + 2];
    w != 0 && w >> 16 == 0
}

fn sample_y(words: &[u32], gx: f32, gz: f32) -> i32 {
    let i0 = gx.floor().clamp(-1.0, (CELLS - 1) as f32) as i32;
    let j0 = gz.floor().clamp(-1.0, (CELLS - 1) as f32) as i32;
    let fx = gx - i0 as f32;
    let fz = gz - j0 as f32;
    let h = |x: i32, z: i32| corner_y(words, x, z) as f32;
    let x = |z: i32| h(i0, z) * (1.0 - fx) + h(i0 + 1, z) * fx;
    (x(j0) * (1.0 - fz) + x(j0 + 1) * fz).round() as i32
}

/// The tile's top faces and the walls down to lower neighbors: the top block for its own
/// height, then the block under it for the rest, lit as the column the cell is sampled at; then
/// what stands on the ground (`add_structures`). Sorted by part and direction (`Counts`).
fn mesh(
    snapshot: crate::generation::Snapshot<'_>,
    key: TileKey,
    words: &[u32],
    columns: &Columns,
    terrain: &TileTerrain,
) -> (Vec<[u32; 4]>, Counts, (i32, i32)) {
    let c = snapshot.c;
    let s = 1 << key.level;
    let scale = key.scale();
    let origin = key.origin();
    let at = |i: i32, j: i32| {
        let k = sample_offset(i, j);
        let (a, b) = (words[k], words[k + 1]);
        let col = IVec2::new(origin.x + i * s + s / 2, origin.y + j * s + s / 2);
        (a >> 16, b & 0xFFFF, b >> 16, columns.get(&col).map_or(0, |c| c.light))
    };
    // What stands over a cell's ground: its top and block.
    let over = |i: i32, j: i32| {
        let w = words[sample_offset(i, j) + 2];
        ((w & 0xFFFF) as u16 as i16 as i32, w >> 16)
    };
    let cave_mouth = |i: i32, j: i32| {
        let (depth, surface) = over(i, j);
        (surface == 0 && depth > 0).then(|| corner_y(words, i, j) + depth)
    };
    // Under a surface (a sea) the sky's light on the ground after `depth` blocks of it, as in the
    // cubes.
    static SKY: std::sync::LazyLock<[([u8; 17], Option<i32>); 16]> = std::sync::LazyLock::new(|| {
        std::array::from_fn(|opacity| {
            let mut sky = [15; 17];
            for depth in 1..sky.len() {
                sky[depth] = crate::world::light::step(sky[depth - 1], opacity as u8, true);
            }
            let dark = sky.iter().position(|&v| v == 0).map(|d| d as i32);
            (sky, dark)
        })
    });
    let sky_under = |surface: u32| &SKY[c.tight[surface as usize].opacity.min(15) as usize];
    let light_of = |i: i32, j: i32, y: i32, light: u32| match over(i, j) {
        (top_y, surface) if surface != 0 => light | ((15 - sky_under(surface).0[(top_y - y).clamp(0, 16) as usize]) as u32) << 24,
        _ => light,
    };
    let mut h = height_grid(words, key.level == 0);
    // Ground deeper than the sky's light reaches is as dark at any depth: it is laid as high as
    // that, a flat floor of few quads and no walls.
    let deepest = h.iter().flatten().copied().min().unwrap_or(0);
    for j in -1..=CELLS {
        for i in -1..=CELLS {
            let (top_y, surface) = over(i, j);
            if s <= 2 {
                let p = origin + IVec2::new(i * s + s / 2, j * s + s / 2);
                if columns
                    .get(&p)
                    .is_some_and(|col| col.top != 0 || !col.spans.is_empty())
                {
                    h[(i + 1) as usize][(j + 1) as usize] = corner_y(words, i, j);
                }
            }
            if surface != 0
                && let Some(dark) = sky_under(surface).1
            {
                let y = &mut h[(i + 1) as usize][(j + 1) as usize];
                *y = (*y).max(top_y - dark);
            }
        }
    }
    // Far off a face is its first layer.
    let face_of = |state: u32, dir| terrain.faces[state as usize][dir];
    let cover = terrain
        .cover
        .as_ref()
        .map(|cover| cover.colors(c, snapshot.seed, key, words, columns));
    let mut forest = terrain.forest.instances(c, key, words, &h, columns);
    let mut detail = (key.level == 0).then(|| detail::Detail::new(snapshot));
    let generated = detail
        .as_mut()
        .map(|detail| detail.structures(key, columns, &h, &mut forest));
    let ground_light = |i: i32, j: i32, y, light| shade_sky(light_of(i, j, y, light), forest.shadow[(i + 1) as usize][(j + 1) as usize]);
    let mut faces = Vec::new();
    let (mut lo, mut hi) = (i32::MAX, i32::MIN);
    let mut plant_mesh = Vec::new();
    let mut cover = cover;
    if let (Some(detail), Some(cells)) = (&mut detail, &mut cover) {
        for j in 0..CELLS {
            for i in 0..CELLS {
                let cell = &mut cells[(i + 1) as usize][(j + 1) as usize];
                let y = column(&h, i, j) + 1;
                let at_pos = origin + IVec2::new(i, j);
                if over(i, j) != (0, 0)
                    || columns
                        .get(&at_pos)
                        .is_some_and(|col| col.spans.iter().any(|s| s.bottom <= y && s.top >= y))
                {
                    continue;
                }
                let (_, biome, _, light) = at(i, j);
                let p = IVec3::new(at_pos.x, y, at_pos.y);
                let top = detail.plant(key, p, biome, ground_light(i, j, y, light), &mut plant_mesh);
                if top > y {
                    hi = hi.max(top);
                    *cell = None;
                }
            }
        }
    }
    let plants = |i: i32, j: i32| {
        cover
            .as_ref()
            .and_then(|cells| cells[(i + 1) as usize][(j + 1) as usize])
    };
    for j in 0..CELLS {
        for i in 0..CELLS {
            let y = column(&h, i, j);
            let (top, biome, _, light) = at(i, j);
            let lit = ground_light(i, j, y, light);
            let lit = cave_mouth(i, j).map_or(lit, |mouth| {
                let loss = if key.level == 0 && cave_has_floor(words, i, j) { 0 } else { mouth - y };
                shade_sky(lit, loss)
            });
            let (tex, tint) = face_of(top, 3);
            if cave_mouth(i, j).is_none() || cave_has_floor(words, i, j) {
                faces.push(Face {
                    dir: 3,
                    at: [i * s, y, j * s],
                    size: [s, 1, s],
                    tex,
                    tint,
                    biome,
                    ao: ao_bits(&h, i, j, 3, y),
                    light: lit,
                    part: ALWAYS,
                    cover: plants(i, j).map(|cell| cover::Appearance { gain: cell.gain, pattern: None }),
                });
            }
            (lo, hi) = (lo.min(y), hi.max(y + 1));
            if let Some((floor, roof)) = cave_gap(words, i, j) {
                for (dir, by) in [(3, floor - 1), (2, roof)] {
                    if dir == 3 && !cave_has_floor(words, i, j) {
                        continue;
                    }
                    let (tex, tint) = face_of(terrain.stone, dir as usize);
                    faces.push(Face {
                        dir,
                        at: [i * s, by, j * s],
                        size: [s, 1, s],
                        tex,
                        tint,
                        biome,
                        ao: 0xFF,
                        light: shade_sky(light, CAVE_LIGHT_REACH),
                        part: ALWAYS,
                        cover: None,
                    });
                    lo = lo.min(by);
                }
            }
            // A sea's surface, unshaded as a liquid's.
            let (top_y, surface) = over(i, j);
            if surface != 0 && top_y > y {
                let (tex, tint) = face_of(surface, 3);
                let part = if c.states[surface as usize].layer == weft_abi::RenderLayer::Translucent { SURFACE } else { ALWAYS };
                faces.push(Face {
                    dir: 3,
                    at: [i * s, top_y, j * s],
                    size: [s, 1, s],
                    tex,
                    tint,
                    biome,
                    ao: 0xFF,
                    light,
                    part,
                    cover: None,
                });
                hi = hi.max(top_y + 1);
            }
        }
    }
    for j in 0..CELLS {
        for i in 0..CELLS {
            let (top, biome, under, light) = at(i, j);
            let y = column(&h, i, j);
            let gap = cave_gap(words, i, j);
            let soil = (words[sample_offset(i, j) + 3] >> lod_gpu::SOIL_DEPTH_SHIFT) as i32;
            let light = ground_light(i, j, y, light);
            for (dir, (di, dj)) in [(0usize, (-1, 0)), (1, (1, 0)), (4, (0, -1)), (5, (0, 1))] {
                // At the tile's edge the tile beside may be of another level, its ground not
                // where this one's sample says: the wall goes on down below it (a skirt), or
                // the sky shows through the crack.
                let (ni, nj) = (i + di, j + dj);
                let ny = column(&h, ni, nj);
                let edge = !((0..CELLS).contains(&ni) && (0..CELLS).contains(&nj));
                let other_gap = cave_gap(words, ni, nj);
                let mouth = cave_mouth(ni, nj).or_else(|| cave_mouth(i, j));
                if gap.is_some() || other_gap.is_some() || mouth.is_some() {
                    let (floor, roof) = gap.unwrap_or((y + 1, y + 1));
                    let bottom = if edge && other_gap.is_none() {
                        ny.min(y)
                            .saturating_sub(s + 2)
                            .max(y - lod_face::HEIGHT_MASK as i32)
                    } else {
                        ny + 1
                    };
                    let open_sky = key.level == 0 && cave_cut(words, ni, nj) && cave_has_floor(words, ni, nj);
                    let sky = (bottom, y + 1, if open_sky { None } else { mouth });
                    let below_roof = other_gap.map(|(a, b)| (a, b, Some(b + CAVE_LIGHT_REACH / 2)));
                    let mut emit = |from: i32, to: i32, state: u32, mouth: Option<i32>| {
                        if from >= to {
                            return;
                        }
                        let (tex, tint) = face_of(state, dir);
                        let mut start = from;
                        while start < to {
                            let end = mouth.map_or(to, |h| {
                                if start < h - CAVE_LIGHT_REACH {
                                    to.min(h - CAVE_LIGHT_REACH)
                                } else if start < h {
                                    to.min(start + 3)
                                } else {
                                    to
                                }
                            });
                            let lit = mouth.map_or(light, |h| shade_sky(light, h - (start + (end - start) / 2)));
                            let ao = ao_span(&h, i, j, dir, end - 1, start);
                            faces.push(Face {
                                dir: dir as u32,
                                at: [i * s, start, j * s],
                                size: [s, end - start, s],
                                tex,
                                tint,
                                biome,
                                ao,
                                light: lit,
                                part: ALWAYS,
                                cover: None,
                            });
                            lo = lo.min(start);
                            start = end;
                        }
                    };
                    let under_floor = if cave_has_floor(words, i, j) { i16::MIN as i32 } else { floor };
                    for (a, b, shadow) in std::iter::once(sky).chain(below_roof) {
                        for (solid_lo, solid_hi) in [(under_floor, floor), (roof, y + 1)] {
                            let (lo, hi) = (a.max(solid_lo), b.min(solid_hi));
                            emit(lo, hi.min(y - soil), terrain.stone, shadow);
                            emit(lo.max(y - soil), hi.min(y), under, shadow);
                            emit(lo.max(y), hi, top, shadow);
                        }
                    }
                    continue;
                }
                // Past a neighbor as high, real cubes may stand in for it lower than its sample
                // says (it is interpolated): a skirt down below that, or the sky shows through
                // between them.
                if ny > y && !edge {
                    continue;
                }
                let mut skirt = ny.min(y - 1) - s - 2;
                // Under a sea the wall goes on down to where the ground really is, the tile's
                // deepest for all (their rows stay alike): seen from the cubes' water at the edge
                // of the view, a look goes under the floor laid higher.
                if over(i, j).1 != 0 {
                    skirt = skirt.min(deepest - s - 2);
                }
                // Beside one as high inside the tile it is only ever seen where that one's
                // pixels give way to cubes.
                let part = if ny == y && !edge { BESIDE_CUBES } else { ALWAYS };
                let has_top = ny < y || edge;
                let top_y = if has_top { y - 1 } else { y };
                skirt = skirt
                    .max(top_y - lod_face::HEIGHT_MASK as i32)
                    .max(i16::MIN as i32);
                let mut bottom = skirt + 1;
                lo = lo.min(bottom);
                let (under_tex, under_tint) = face_of(under, dir);
                if ny < y
                    && over(i, j).1 == 0
                    && light & 0xFF_FFFF == 0
                    && let Some(cell) = plants(ni, nj)
                {
                    // A taller grass canopy hides the foot of the next riser. A coarse
                    // cell stands for several steps, so scale the band with the cell.
                    let start = (ny + 1).max(bottom);
                    let end = (start + (cell.height * s as f32).round() as i32).min(y + 1);
                    if end > start {
                        if start > bottom {
                            faces.push(Face {
                                dir: dir as u32,
                                at: [i * s, bottom, j * s],
                                size: [s, start - bottom, s],
                                tex: under_tex,
                                tint: under_tint,
                                biome,
                                ao: 0xFF,
                                light,
                                part: if edge { ALWAYS } else { BESIDE_CUBES },
                                cover: None,
                            });
                        }
                        faces.push(Face {
                            dir: dir as u32,
                            at: [i * s, start, j * s],
                            size: [s, end - start, s],
                            tex: cell.texture,
                            tint: 0,
                            biome,
                            ao: 0xFF,
                            light,
                            part,
                            cover: Some(cover::Appearance { gain: cell.gain, pattern: Some(cell.pattern) }),
                        });
                        bottom = end;
                    }
                }
                if has_top && bottom <= y {
                    let (tex, tint) = face_of(top, dir);
                    faces.push(Face {
                        dir: dir as u32,
                        at: [i * s, y, j * s],
                        size: [s, 1, s],
                        tex,
                        tint,
                        biome,
                        ao: ao_bits(&h, i, j, dir, y),
                        light,
                        part,
                        cover: None,
                    });
                }
                if bottom < y {
                    faces.push(Face {
                        dir: dir as u32,
                        at: [i * s, bottom, j * s],
                        size: [s, y - bottom, s],
                        tex: under_tex,
                        tint: under_tint,
                        biome,
                        ao: ao_span(&h, i, j, dir, top_y, (ny + 1).max(bottom).min(top_y)),
                        light,
                        part,
                        cover: None,
                    });
                }
            }
        }
    }
    let record = |f: &Face| {
        let mut q = quad(f.dir, f.at[0], f.at[1], f.at[2], (f.size[0], f.size[1], f.size[2]), f.tex, f.tint, f.biome, f.ao, f.light, scale);
        if let Some(cover) = f.cover {
            crate::gpu_layout::lod_set_gain(&mut q, cover.gain);
            if let Some(tex) = cover.pattern {
                crate::gpu_layout::lod_set_cover_pattern(&mut q, tex);
            }
        }
        q
    };
    let faces = merged(faces);
    let mut parts: [Vec<[u32; 4]>; PARTS] = std::array::from_fn(|p| faces.iter().filter(|f| f.part == p).map(record).collect());
    add_structures(&mut parts[ALWAYS], &mut lo, &mut hi, c, key, words, columns, generated.as_ref(), &h, &face_of, &terrain.ctm);
    let room = MAX_QUADS.saturating_sub(parts.iter().map(Vec::len).sum());
    plant_mesh.truncate(room);
    let rank = |q: &[u32; 4]| super::mesher::DIR_RANK[((q[0] >> lod_face::DIR_SHIFT) & face::DIR_MASK) as usize] as usize;
    let mut counts = [[0; 6]; PARTS];
    for (part, n) in parts.iter_mut().zip(&mut counts) {
        part.sort_by_key(rank);
        for q in part.iter() {
            n[rank(q)] += 1;
        }
    }
    counts[PLANTS][0] = plant_mesh.len() as u32;
    parts[PLANTS] = plant_mesh;
    if key.level == 0 {
        let mask = lod_face::POS_MASK;
        for q in parts.iter_mut().flatten() {
            let x = ((q[0] & mask) + lod_face::DETAIL_PAD) & mask;
            let z = ((q[0] >> lod_face::Z_SHIFT) + lod_face::DETAIL_PAD) & mask;
            q[0] = (q[0] & !(mask | (mask << lod_face::Z_SHIFT))) | x | (z << lod_face::Z_SHIFT);
        }
    }
    let space = MAX_QUADS.saturating_sub(parts.iter().map(Vec::len).sum());
    forest.instances.truncate(space);
    if !forest.instances.is_empty() {
        (lo, hi) = (lo.min(forest.y.0), hi.max(forest.y.1));
        parts[FOREST] = forest.instances;
    }
    counts[FOREST][0] = parts[FOREST].len() as u32;
    (parts.concat(), counts, (lo, hi))
}

fn shade_sky(light: u32, loss: i32) -> u32 {
    let lost = (loss.clamp(0, 15) as u32).max(light >> 24);
    light & 0xFF_FFFF | (lost << 24)
}

/// A face of a tile before it is packed.
#[derive(Clone, Copy, PartialEq)]
struct Face {
    dir: u32,
    at: [i32; 3],
    size: [i32; 3],
    tex: u32,
    tint: u32,
    biome: u32,
    ao: u32,
    light: u32,
    cover: Option<cover::Appearance>,
    /// Which of `Counts`' parts.
    part: usize,
}

/// `faces` with neighbors alike made one face: a flat stretch of ground is a few quads, a straight
/// cliff a few strips, not one a cell. Runs along a row first (walls facing x along z, the others
/// along x), then tops' runs alike side by side into rectangles; walls stop at strips, their
/// rows being of other heights. A face stretches only where its shading would (the corners at
/// both ends shaded alike).
fn merged(faces: Vec<Face>) -> Vec<Face> {
    let rows = merged_along(faces, |f| Some(if f.dir <= 1 { 2 } else { 0 }));
    merged_along(rows, |f| (f.dir == 2 || f.dir == 3).then_some(2))
}

/// Runs of faces alike but for where they are along the axis `along` gives (none: kept as they
/// are) made one face.
fn merged_along(mut faces: Vec<Face>, along: impl Fn(&Face) -> Option<usize>) -> Vec<Face> {
    // Alike but for where along the row: sorted together, in order along it.
    let key = |f: &Face| {
        let a = along(f);
        let mut pos = f.at;
        if let Some(a) = a {
            pos[a] = 0;
        }
        (f.dir, f.part, pos, f.size, f.tex, f.tint, f.biome, f.ao, f.light, f.cover, a.map_or(0, |a| f.at[a]))
    };
    faces.sort_unstable_by_key(key);
    let mut out: Vec<Face> = Vec::with_capacity(faces.len());
    for f in faces {
        if let Some(last) = out.last_mut()
            && let Some(a) = along(&f)
        {
            let (mut a_pos, mut f_pos) = (last.at, f.at);
            (a_pos[a], f_pos[a]) = (0, 0);
            let mut a_size = last.size;
            a_size[a] = f.size[a];
            let alike = Face { at: f_pos, ..f } == Face { at: a_pos, size: a_size, ..*last };
            if alike && last.at[a] + last.size[a] == f.at[a] && stretches(f.dir, f.ao, a) {
                last.size[a] += f.size[a];
                continue;
            }
        }
        out.push(f);
    }
    out
}

/// Whether a face's shading holds when it is stretched along axis `along`: each corner shaded
/// like the one at the other end of the face.
fn stretches(dir: u32, ao: u32, along: usize) -> bool {
    let corners = &CORNERS[dir as usize];
    let shade = |k: usize| (ao >> (2 * k)) & 3;
    (0..4).all(|k| {
        let other = (0..4).find(|&m| m != k && (0..3).all(|ax| ax == along || corners[m][ax] == corners[k][ax]));
        other.is_none_or(|m| shade(m) == shade(k))
    })
}

fn height_grid(words: &[u32], exact: bool) -> [[i32; GRID]; GRID] {
    let mut h = [[0i32; GRID]; GRID];
    for j in -1..=CELLS {
        for i in -1..=CELLS {
            // Interpolating across an open cut lowers intact roof columns into pillars.
            let cave = (j..=(j + 1).min(CELLS))
                .any(|z| (i..=(i + 1).min(CELLS)).any(|x| cave_cut(words, x, z) || cave_gap(words, x, z).is_some()));
            let y = if exact || cave {
                corner_y(words, i, j)
            } else if i < CELLS && j < CELLS {
                sample_y(words, i as f32 + 0.5, j as f32 + 0.5)
            } else {
                let (i0, fx) = if i < CELLS { (i, 0.5) } else { (CELLS, 0.0) };
                let (j0, fz) = if j < CELLS { (j, 0.5) } else { (CELLS, 0.0) };
                let s = |x: i32, z: i32| corner_y(words, x, z) as f32;
                let a = s(i0, j0) * (1.0 - fx) + s((i0 + 1).min(CELLS), j0) * fx;
                let b = s(i0, (j0 + 1).min(CELLS)) * (1.0 - fx) + s((i0 + 1).min(CELLS), (j0 + 1).min(CELLS)) * fx;
                (a * (1.0 - fz) + b * fz).round() as i32
            };
            h[(i + 1) as usize][(j + 1) as usize] = y;
        }
    }
    h
}

fn column(h: &[[i32; GRID]; GRID], i: i32, j: i32) -> i32 {
    h[(i + 1).clamp(0, CELLS + 1) as usize][(j + 1).clamp(0, CELLS + 1) as usize]
}

fn ao_bits(h: &[[i32; GRID]; GRID], i: i32, j: i32, dir: usize, by: i32) -> u32 {
    let nrm = FACE_DIRS[dir];
    let axis = dir / 2;
    let (ta, tb) = match axis {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    };
    let base = [i + nrm[0], by + nrm[1], j + nrm[2]];
    let mut bits = 0u32;
    for (k, corner) in CORNERS[dir].iter().enumerate() {
        let sa = corner[ta] * 2 - 1;
        let sb = corner[tb] * 2 - 1;
        let mut p1 = base;
        let mut p2 = base;
        let mut p3 = base;
        p1[ta] += sa;
        p2[tb] += sb;
        p3[ta] += sa;
        p3[tb] += sb;
        let solid = |p: [i32; 3]| column(h, p[0], p[2]) >= p[1];
        let (s1, s2, s3) = (solid(p1), solid(p2), solid(p3));
        let ao = if s1 && s2 { 0 } else { 3 - u32::from(s1) - u32::from(s2) - u32::from(s3) };
        bits |= ao << (2 * k);
    }
    bits
}

fn ao_span(h: &[[i32; GRID]; GRID], i: i32, j: i32, dir: usize, top: i32, bot: i32) -> u32 {
    let hi = ao_bits(h, i, j, dir, top);
    let lo = ao_bits(h, i, j, dir, bot);
    let mut bits = 0u32;
    for (k, c) in CORNERS[dir].iter().enumerate() {
        let src = if c[1] == 1 { hi } else { lo };
        bits |= ((src >> (2 * k)) & 3) << (2 * k);
    }
    bits
}

/// `lo..=hi` with the ranges in `cover` taken out.
fn exposed(lo: i32, hi: i32, cover: &[Span]) -> Vec<(i32, i32)> {
    let mut segs = vec![(lo, hi)];
    for span in cover {
        let (a, b) = (span.bottom, span.top);
        let mut next = Vec::new();
        for (s, e) in segs {
            if b < s || a > e {
                next.push((s, e));
            } else {
                if s < a {
                    next.push((s, a - 1));
                }
                if e > b {
                    next.push((b + 1, e));
                }
            }
        }
        segs = next;
    }
    segs
}

/// A model's boxes in sixteenths, or one box around them when there are many. Tilt is dropped:
/// the distant shape is the box as it was placed.
fn model_boxes(st: &StateInfo) -> Vec<([u32; 3], [u32; 3], [u32; 6])> {
    let mut raw: Vec<([u32; 3], [u32; 3], [u32; 6])> = st
        .boxes
        .iter()
        .map(|b| {
            let edge = |v: f32| (v * 16.0).round().clamp(0.0, 16.0) as u32;
            (b.min.map(edge), b.max.map(edge), b.textures)
        })
        .filter(|(mn, mx, _)| mn.iter().zip(mx).all(|(a, c)| a < c))
        .collect();
    if raw.len() > MAX_MODEL_BOXES {
        let mut mn = [16u32; 3];
        let mut mx = [0u32; 3];
        for (a, c, _) in &raw {
            for i in 0..3 {
                mn[i] = mn[i].min(a[i]);
                mx[i] = mx[i].max(c[i]);
            }
        }
        let tex = raw[0].2;
        raw = vec![(mn, mx, tex)];
    }
    raw
}

/// Saved spans in this tile, one box per run of block, at the column itself. The ground is left
/// where the tile has it, so a roof is not smoothed into the neighbors.
#[allow(clippy::too_many_arguments)]
fn add_structures(
    out: &mut Vec<[u32; 4]>,
    lo: &mut i32,
    hi: &mut i32,
    c: &Content,
    key: TileKey,
    words: &[u32],
    columns: &Columns,
    generated: Option<&detail::Structures>,
    h: &[[i32; GRID]; GRID],
    face_of: &impl Fn(u32, usize) -> (u32, u32),
    ctm: &super::ctm::Ctm,
) {
    let s = 1 << key.level;
    let scale = key.scale();
    let origin = key.origin();
    let (x0, z0) = (origin.x, origin.y);
    let (x1, z1) = (x0 + CELLS * s, z0 + CELLS * s);
    let inside = |p: IVec2| (x0..x1).contains(&p.x) && (z0..z1).contains(&p.y);
    let columns = generated.map_or(columns, |g| &g.columns);
    let mut here: Vec<IVec2> = columns
        .iter()
        .filter(|(p, col)| !col.spans.is_empty() && inside(**p))
        .map(|(p, _)| *p)
        .collect();
    if let Some(g) = generated {
        here.extend(g.outside.keys().copied());
    }
    let (cx, cz) = (x0 + CELLS * s / 2, z0 + CELLS * s / 2);
    here.sort_by_key(|p| (p.x - cx).abs() + (p.y - cz).abs());
    let hides = |block: u32, by: &Span| super::mesher::face_hidden(c, ctm, block, by.block);
    let leaves = c.components.id("leaves");
    for p in here {
        if out.len() >= MAX_QUADS {
            return;
        }
        let Some(col) = columns.get(&p) else { continue };
        let (i, j) = ((p.x - x0) / s, (p.y - z0) / s);
        let outside = generated.and_then(|g| g.outside.get(&p));
        let (biome, ground, spans) = if let Some((biome, spans)) = outside {
            (*biome, i16::MIN as i32, spans.as_slice())
        } else {
            (words[sample_offset(i, j) + 1] & 0xFFFF, column(h, i, j), col.spans.as_slice())
        };
        // How high the ground beside covers this column's sides.
        let skirt_hi = |ni: i32, nj: i32| {
            if outside.is_some() {
                return i32::MIN;
            }
            let n = column(h, ni, nj);
            if ni == i && nj == j || n < ground {
                i32::MIN
            } else if n > ground {
                n
            } else {
                n - 1
            }
        };
        let (lx, lz) = (p.x - x0, p.y - z0);
        for span in spans {
            let (bottom, span_hi, block, light) = (span.bottom.max(ground + 1), span.top, span.block, span.light);
            if bottom > span_hi {
                continue;
            }
            if c.states[block as usize].model == BlockModel::Boxes {
                for (mn, mx, textures) in model_boxes(&c.states[block as usize]) {
                    let height = span_hi - bottom + 1;
                    let extrude = mn[1] == 0 && mx[1] == 16 && height > 1;
                    let mut y = bottom;
                    while y <= span_hi {
                        let top = if extrude { span_hi } else { y };
                        for dir in 0..6 {
                            let at_edge = [mn[0] == 0, mx[0] == 16, mn[1] == 0, mx[1] == 16, mn[2] == 0, mx[2] == 16][dir];
                            let hidden = at_edge
                                && match dir {
                                    2 => {
                                        y <= ground + 1
                                            || col
                                                .spans
                                                .iter()
                                                .any(|s| s.bottom <= y - 1 && s.top >= y - 1 && hides(block, s))
                                    }
                                    3 => {
                                        (y <= ground && mx[1] == 16)
                                            || col
                                                .spans
                                                .iter()
                                                .any(|s| s.bottom <= top + 1 && s.top >= top + 1 && hides(block, s))
                                    }
                                    _ => {
                                        let (dx, dz) = [(-1, 0), (1, 0), (0, 0), (0, 0), (0, -1), (0, 1)][dir];
                                        let np = IVec2::new(p.x + dx, p.y + dz);
                                        let hide = skirt_hi((np.x - x0).div_euclid(s), (np.y - z0).div_euclid(s));
                                        let beside = columns.get(&np).map_or(&[][..], |n| n.spans.as_slice());
                                        (hide != i32::MIN && top <= hide)
                                            || beside
                                                .iter()
                                                .any(|s| s.bottom <= y && s.top >= top && hides(block, s))
                                    }
                                };
                            if hidden {
                                continue;
                            }
                            let (fb, tint) = face_of(block, dir);
                            let own = textures[dir] & face::TEX_MASK;
                            let tex = if own == 0 { fb } else { own };
                            let r = quad(
                                dir as u32,
                                lx,
                                y,
                                lz,
                                (1, if extrude { height } else { 1 }, 1),
                                tex,
                                tint,
                                biome,
                                ao_bits(h, i, j, dir, top),
                                light,
                                scale,
                            );
                            let (mn, mx) = scaled_edges(lx, lz, scale, mn, mx);
                            let mut r = lod_box(r, mn, mx, extrude);
                            if c.states[block as usize].layer == weft_abi::RenderLayer::Cutout {
                                let foliage =
                                    key.level > 0 && leaves.is_some_and(|tag| c.components.has(crate::components::BLOCK, block, tag));
                                let rgb = c.kind_color(tint as u8, Some(biome as u16));
                                let gain = crate::gpu_layout::lod_color_gain([16, 8, 0].map(|s| ((rgb >> s) & 255) as f32 / 255.0));
                                crate::gpu_layout::lod_set_gain(&mut r, gain);
                                if foliage {
                                    r[0] |= lod_face::FILLED_LEAVES << lod_face::TINT_SHIFT;
                                }
                            }
                            out.push(r);
                        }
                        if extrude {
                            break;
                        }
                        y += 1;
                    }
                }
            } else {
                if !col
                    .spans
                    .iter()
                    .any(|s| s.bottom == span_hi + 1 && hides(block, s))
                {
                    let (tex, tint) = face_of(block, 3);
                    out.push(quad(3, lx, span_hi, lz, (1, 1, 1), tex, tint, biome, ao_bits(h, i, j, 3, span_hi), light, scale));
                }
                let held = col
                    .spans
                    .iter()
                    .any(|s| s.top + 1 == bottom && hides(block, s));
                if !held && bottom > ground + 1 {
                    let (tex, tint) = face_of(block, 2);
                    out.push(quad(2, lx, bottom, lz, (1, 1, 1), tex, tint, biome, ao_bits(h, i, j, 2, bottom), light, scale));
                }
                for (dir, dx, dz) in [(0usize, -1, 0), (1, 1, 0), (4, 0, -1), (5, 0, 1)] {
                    let (tex, tint) = face_of(block, dir);
                    let np = IVec2::new(p.x + dx, p.y + dz);
                    let beside = columns.get(&np).map_or(&[][..], |n| n.spans.as_slice());
                    let mut cover: Vec<Span> = beside.iter().copied().filter(|s| hides(block, s)).collect();
                    let n_top = skirt_hi((np.x - x0).div_euclid(s), (np.y - z0).div_euclid(s));
                    if n_top != i32::MIN {
                        cover.push(Span { bottom: n_top.saturating_sub(4096), top: n_top, block: 0, light: 0 });
                    }
                    for (s0, e0) in exposed(bottom, span_hi, &cover) {
                        let ao = ao_span(h, i, j, dir, e0 + 1, s0);
                        out.push(quad(dir as u32, lx, s0, lz, (1, e0 - s0 + 1, 1), tex, tint, biome, ao, light, scale));
                    }
                }
            }
            *lo = (*lo).min(bottom);
            *hi = (*hi).max(span_hi + 1);
            if out.len() >= MAX_QUADS {
                return;
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
struct LodView {
    pos: DVec3,
    projection: Mat4,
    tiles_per_screen: f64,
    hide: i32,
    reach: i32,
}

impl LodView {
    fn similar(self, other: Self) -> bool {
        self.hide == other.hide
            && self.reach == other.reach
            && self.pos.distance_squared(other.pos) < 2.0 * 2.0
            && (self.tiles_per_screen - other.tiles_per_screen).abs() <= self.tiles_per_screen * 0.02
            && self.projection.abs_diff_eq(other.projection, 0.02)
    }

    fn area(self, min: glam::Vec3, max: glam::Vec3, planes: &[glam::Vec4; 4]) -> f64 {
        let center = (min + max) * 0.5;
        let half = (max - min) * 0.5;
        if !planes
            .iter()
            .all(|plane| plane.dot(center.extend(1.0)) + plane.truncate().abs().dot(half) >= 0.0)
        {
            return 0.0;
        }
        const NEAR: f32 = 0.05;
        let corners: [_; 8] = std::array::from_fn(|corner| {
            let at = glam::Vec3::new([min.x, max.x][corner & 1], [min.y, max.y][(corner >> 1) & 1], [min.z, max.z][corner >> 2]);
            self.projection * at.extend(1.0)
        });
        let (mut low, mut high) = (glam::Vec2::splat(1.0), glam::Vec2::splat(-1.0));
        let mut include = |point: glam::Vec2| {
            low = low.min(point);
            high = high.max(point);
        };
        for clip in corners.iter().filter(|clip| clip.w >= NEAR) {
            include(glam::Vec2::new(clip.x, clip.y) / clip.w);
        }
        // A box spanning the near plane can still cover only a sliver of the screen.
        for (i, behind) in corners.iter().enumerate().filter(|(_, clip)| clip.w < NEAR) {
            for axis in [1, 2, 4] {
                let ahead = corners[i ^ axis];
                if ahead.w >= NEAR {
                    let at = behind.lerp(ahead, (NEAR - behind.w) / (ahead.w - behind.w));
                    include(glam::Vec2::new(at.x, at.y) / NEAR);
                }
            }
        }
        let span = (high.min(glam::Vec2::ONE) - low.max(-glam::Vec2::ONE)).max(glam::Vec2::ZERO);
        span.x as f64 * span.y as f64 * self.tiles_per_screen * 0.25
    }
}

/// The client's distant terrain: which tiles to show, asking for missing ones, taking made ones.
pub struct Lod {
    /// The land's height at the camera's column, kept while it stays in it.
    ground: Option<((u32, IVec2), i32)>,
    jobs: Sender<LodJob>,
    done: (Sender<TileResult>, Receiver<TileResult>),
    dim: u32,
    seed: u64,
    shader: Option<u64>,
    cache_dir: PathBuf,
    terrain: Option<Arc<TileTerrain>>,
    baking: Option<Receiver<forest::Atlases>>,
    /// Drawn height bounds guide priorities, never conservative visibility bounds.
    ready: FxHashMap<TileKey, ReadyTile>,
    cached_heights: cache::Heights,
    cache_misses: FxHashSet<TileKey>,
    generation: u64,
    pending: FxHashSet<TileKey>,
    /// Tiles wanted, coarse first then nearest, and the view that selected them.
    wanted: Vec<TileKey>,
    wanted_at: Option<LodView>,
    // Ordinary-view tiles protected from eviction, and where they were last seen.
    remembered: FxHashMap<TileKey, DVec3>,
    /// Ready or pending tiles requested exclusively for a magnified view.
    zoom_only: FxHashSet<TileKey>,
    zooming: bool,
    dirty: bool,
    max_level: u32,
    /// A bit per cube column around the camera's (`MASK_MARGIN` past the view, addressed
    /// modulo the window's width so it does not move with the camera): its cubes show its
    /// ground. Its half width and center, and whether it changed since uploaded.
    mask: Vec<u32>,
    half: i32,
    center: IVec2,
    mask_changed: bool,
    /// What the server said of columns of `columns_dim`.
    columns: Arc<SavedColumns>,
    columns_dim: u32,
    edits: Arc<Vec<crate::server::area::AreaFill>>,
    edits_dim: u32,
    /// Made tiles to make again for columns that changed, shown as they are till then.
    stale: FxHashSet<TileKey>,
    /// A coarse tile and the complete finer cover sharing its dither.
    transitions: FxHashMap<TileKey, Fade>,
    solid: Vec<TileKey>,
    frontier: FxHashSet<TileKey>,
    hidden: bool,
}

#[derive(Clone, Copy, Default)]
struct ReadyTile {
    y: (i32, i32),
    quads: usize,
    last_used: u64,
    far: bool,
}

struct Fade {
    fine: Vec<TileKey>,
    refining: bool,
    start: Instant,
}

/// Columns the mask reaches past the view radius (cubes are kept that far).
const MASK_MARGIN: i32 = 3;

impl Lod {
    pub fn new(jobs: Sender<LodJob>, cache_dir: PathBuf, shader: Option<u64>) -> Lod {
        Lod {
            ground: None,
            jobs,
            done: unbounded(),
            dim: u32::MAX,
            seed: 0,
            shader,
            cache_dir,
            terrain: None,
            baking: None,
            ready: FxHashMap::default(),
            cached_heights: cache::Heights::default(),
            cache_misses: FxHashSet::default(),
            generation: 0,
            pending: FxHashSet::default(),
            wanted: Vec::new(),
            wanted_at: None,
            remembered: FxHashMap::default(),
            zoom_only: FxHashSet::default(),
            zooming: false,
            dirty: false,
            max_level: 1,
            mask: Vec::new(),
            half: 0,
            center: IVec2::ZERO,
            mask_changed: false,
            columns: Arc::default(),
            columns_dim: u32::MAX,
            edits: Arc::default(),
            edits_dim: u32::MAX,
            stale: FxHashSet::default(),
            transitions: FxHashMap::default(),
            solid: Vec::new(),
            frontier: FxHashSet::default(),
            hidden: false,
        }
    }

    /// `ToClient::LodColumns`: tiles with those columns are made again.
    pub fn set_columns(&mut self, dim: u32, all: bool, cols: &[LodColumn]) {
        if all || dim != self.columns_dim {
            self.columns = Arc::default();
            self.columns_dim = dim;
            // One still being made has the columns from before this, so it is made again too.
            self.stale.extend(self.ready.keys().copied());
            self.stale.extend(self.pending.iter().copied());
        }
        let map = Arc::make_mut(&mut self.columns);
        for col in cols {
            let (x, z) = (col.x, col.z);
            let new = (col.top != u32::MAX).then_some(col);
            if map.get(&(z, x)).map(Arc::as_ref) == new {
                continue;
            }
            if let Some(col) = new {
                map.insert((z, x), Arc::new(col.clone()));
            } else {
                map.remove(&(z, x));
            }
            if all || self.ready.is_empty() && self.pending.is_empty() {
                continue;
            }
            for level in 0..=MAX_LEVEL {
                let side = (CELLS as i64) << level;
                let border = i64::from(sample_margin(level, self.terrain.as_deref().map_or(1, |t| t.detail_margin)));
                let range = |p: i32| (p as i64 - border).div_euclid(side) as i32..=(p as i64 + border).div_euclid(side) as i32;
                for z in range(z) {
                    for x in range(x) {
                        let key = TileKey { level, x, z };
                        if self.ready.contains_key(&key) || self.pending.contains(&key) {
                            self.stale.insert(key);
                        }
                    }
                }
            }
        }
        if !self.stale.is_empty() {
            self.dirty = true;
        }
    }

    pub(super) fn set_edits(&mut self, dim: u32, all: bool, edits: &[crate::server::area::AreaFill]) {
        let reset = all || dim != self.edits_dim;
        let detail_margin = self.terrain.as_deref().map_or(1, |t| t.detail_margin);
        let affected = self
            .ready
            .keys()
            .chain(&self.pending)
            .copied()
            .filter(|&key| {
                reset
                    || edits
                        .iter()
                        .any(|edit| tile_meets_edit(key, edit, detail_margin))
            });
        self.stale.extend(affected);
        self.dirty |= !self.stale.is_empty();
        if reset {
            self.edits = Arc::default();
            self.edits_dim = dim;
        }
        Arc::make_mut(&mut self.edits).extend_from_slice(edits);
    }

    pub fn reload(&mut self) {
        self.save_view();
        self.dim = u32::MAX;
    }

    pub(super) fn save_view(&self) {
        let (Some(terrain), Some(view)) = (&self.terrain, self.wanted_at) else { return };
        if self.dim == u32::MAX {
            return;
        }
        let mut ordinary: Vec<_> = self
            .remembered
            .keys()
            .filter_map(|&key| self.ready.get(&key).filter(|t| !t.far).map(|t| (key, t.y)))
            .collect();
        if ordinary.is_empty() {
            return;
        }
        let pos = glam::DVec2::new(view.pos.x, view.pos.z);
        ordinary.sort_unstable_by_key(|(key, _)| (key.distance(pos).0.to_bits(), std::cmp::Reverse(key.level), *key));
        let previous = self
            .cached_heights
            .iter()
            .filter(|(key, _)| !self.remembered.contains_key(key) || !self.ready.contains_key(key))
            .map(|(&key, &y)| (key, y));
        cache::write_heights(&self.cache_dir, self.dim, self.seed, terrain.signature, ordinary.into_iter().chain(previous));
    }

    pub fn columns_len(&self) -> usize {
        self.columns.len()
    }

    /// Tiles asked for and not back yet.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    pub(super) fn ready_for_entry(&self, dim: u32, seed: u64) -> bool {
        Self::distance() == 0
            || (self.dim, self.seed) == (dim, seed)
                && (self.terrain.is_none()
                    || self.hidden
                    || self.wanted_at.is_some() && self.wanted.iter().all(|key| self.ready.contains_key(key)))
    }

    /// Distance in blocks it reaches (`core:lod_distance` cubes), 0 when off.
    pub fn distance() -> i32 {
        let c = content();
        c.game
            .catalog
            .ids(ek::CONFIG)
            .get("core:lod_distance")
            .map_or(0, |&id| config::get(id) as i32)
            * 16
    }

    /// Picks, asks for and hands the renderer this frame's tiles. `view`: the view radius in
    /// cubes (inside it the real cubes are drawn).
    pub fn update(
        &mut self,
        gpu: &mut dyn weft_render::Backend,
        render: &mut super::world_render::WorldRenderer,
        world: &mut super::world::ClientWorld,
        textures: &super::textures::GameTextures,
        seed: u64,
        camera: &super::world_render::Camera,
        frame: &weft_render::FrameInfo,
        early: &mut weft_render::CommandList,
        far_compute: Option<weft_render::PipelineId>,
        normal_fov: f32,
        view: i32,
        upload_budget: Duration,
        animate: bool,
    ) {
        let cam = camera.pos;
        let c = content();
        let dim = world.dim;
        let reach = Self::distance();
        let world_changed = (dim, seed) != (self.dim, self.seed);
        if (reach != 0 && world_changed) || (reach == 0 && self.terrain.is_some()) {
            self.save_view();
            self.cached_heights.clear();
            self.cache_misses.clear();
            for (key, _) in self.ready.drain() {
                render.remove_lod(key);
            }
            render.far_lod.invalidate_description(gpu);
            self.pending.clear();
            self.done = unbounded();
            self.stale.clear();
            self.wanted.clear();
            self.frontier.clear();
            self.transitions.clear();
            self.solid.clear();
            self.wanted_at = None;
            self.remembered.clear();
            self.zoom_only.clear();
            self.zooming = false;
            self.ground = None;
            self.mask.clear();
            render.lod_draw.clear();
            render.set_forest(gpu, Vec::new());
        }
        render.far_lod.collect(gpu);
        if reach == 0 {
            self.dim = u32::MAX;
            self.terrain = None;
            self.baking = None;
            world.take_changed_columns();
            return;
        }
        if world_changed {
            (self.dim, self.seed) = (dim, seed);
            (self.terrain, self.baking) = self
                .shader
                .and_then(|s| TileTerrain::new(c, dim, seed, s, textures))
                .map(|(terrain, rx)| (Arc::new(terrain), rx))
                .unzip();
            if let Some(terrain) = &self.terrain {
                self.cached_heights = cache::read_heights(&self.cache_dir, dim, seed, terrain.signature);
            }
        }
        if let Some(images) = self.baking.as_ref().and_then(|rx| rx.try_recv().ok()) {
            render.set_forest(gpu, images);
            self.baking = None;
        }
        if self.terrain.is_none() {
            world.take_changed_columns();
            return;
        }

        self.update_mask(gpu, render, world, seed, cam, view);
        let land = self.land_height(c, cam);
        let hidden = land.is_some_and(|h| cam.y < (h - UNDERGROUND) as f64);
        // Above the loaded cubes the tiles also fill the middle of the view.
        let spill = self
            .terrain
            .as_deref()
            .map_or(0, |t| t.detail_margin.min(lod_face::DETAIL_PAD as i32));
        let hide = if land.is_some_and(|h| cam.y > (h + view * 8) as f64) { 0 } else { (view / 2 * 16 - spill).max(0) };
        let upload_start = Instant::now();
        // An eased FOV can settle a few float steps below its target at high frame rates.
        let zooming = camera.fov < normal_fov - 0.01;
        let released = self.zooming && !zooming;
        self.zooming = zooming;
        let mut far_built = 0;
        while self.baking.is_none()
            && let Ok(result) = self.done.1.try_recv()
        {
            let key = match &result {
                Ok(BuiltTile::Meshed(m)) => m.key,
                Ok(BuiltTile::Far(key)) => *key,
                Err(key) => *key,
            };
            self.pending.remove(&key);
            self.dirty = true;
            if !zooming && self.zoom_only.contains(&key) {
                if !self.ready.contains_key(&key) {
                    self.zoom_only.remove(&key);
                }
                continue;
            }
            if self.stale.contains(&key) {
                continue;
            }
            let tile = match result {
                Ok(BuiltTile::Meshed(m)) => {
                    render.upload_lod(gpu, textures, m.key, &m.quads, m.counts, m.y);
                    ReadyTile { y: m.y, quads: m.quads.len(), last_used: self.generation, far: false }
                }
                Ok(BuiltTile::Far(key)) => {
                    far_built += 1;
                    render.remove_lod(key);
                    render.far_lod.add(gpu, key);
                    // The height envelope is only a priority hint. Geometry stays on the GPU.
                    let y = land.unwrap_or(0);
                    let quads = super::world_render::far_lod::TILE_WORDS.div_ceil(4) as usize;
                    ReadyTile { y: (y.saturating_sub(128), y.saturating_add(128)), quads, last_used: self.generation, far: true }
                }
                Err(_) => {
                    self.cache_misses.insert(key);
                    continue;
                }
            };
            self.cache_misses.remove(&key);
            self.ready.insert(key, tile);
            if far_built >= FAR_TILES_PER_FRAME || upload_start.elapsed() >= upload_budget {
                break;
            }
        }
        let viewport = (frame.width, frame.height);
        let far_compute = far_compute.filter(|_| render.far_lod.enabled());
        let quality = config::by_name("core:lod_detail").map_or(4.0, config::get);
        let aspect = viewport.0.max(1) as f32 / viewport.1.max(1) as f32;
        let guard = super::world_render::Camera { fov: (camera.fov * 1.1).min(175.0), ..*camera };
        let selection = LodView {
            pos: cam,
            projection: guard.proj(aspect) * guard.view(),
            tiles_per_screen: viewport.0.max(1) as f64 * viewport.1.max(1) as f64 / (CELLS as f64 * 24.0 / quality).powi(2),
            hide,
            reach,
        };
        let changed = self.dirty
            || released
            || self
                .wanted_at
                .is_none_or(|previous| !selection.similar(previous));
        let now = Instant::now();
        if changed {
            self.dirty = false;
            // After a cut to the opposite view, its resident detail can show in the first frame.
            let cut = self.wanted_at.is_some_and(|v| {
                v.projection
                    .row(3)
                    .truncate()
                    .dot(selection.projection.row(3).truncate())
                    < 0.0
            });
            if self
                .wanted_at
                .is_some_and(|v| v.tiles_per_screen != selection.tiles_per_screen || v.reach != reach || v.hide != hide)
            {
                self.remembered.clear();
            }
            self.wanted_at = Some(selection);
            let d = &c.dims[dim as usize];
            let heights = ((d.min_cube_y as f64 * 16.0).max(i16::MIN as f64), (d.max_cube_y as f64 * 16.0 + 16.0).min(i16::MAX as f64));
            let normal = zooming.then(|| {
                let wide = super::world_render::Camera { fov: (normal_fov * 1.1).min(175.0), ..guard };
                LodView { projection: wide.proj(aspect) * wide.view(), ..selection }
            });
            if released {
                self.frontier.clear();
                self.transitions.clear();
            }
            let mut needed = self.pick(selection, heights, normal);
            if released {
                self.release_zoom(|key| render.remove_lod(key));
            }
            let mut next = Vec::new();
            for &root in needed.iter().filter(|k| k.level == self.max_level) {
                self.cover(root, &needed, &mut next);
            }
            if cut {
                self.frontier.clear();
                self.transitions.clear();
            }
            if animate {
                self.change_frontier(next.into_iter().collect(), now);
            } else {
                // The world preview already provides the transition into this view.
                self.transitions.clear();
                self.frontier = next.into_iter().collect();
            }
            needed.extend(&self.frontier);
            for (&parent, fade) in &self.transitions {
                needed.insert(parent);
                needed.extend(&fade.fine);
            }
            self.trim(&needed, render);
            self.request(far_compute.is_some());
            log::debug!(target: "load", "lod fov {:.2}, range {}, tiles {}, levels {}..{}, shown {} (finest {:?}), zoom-only resident {}, far GPU {}",
                camera.fov, reach, self.wanted.len(), self.wanted.iter().map(|k| k.level).min().unwrap_or(0), self.max_level,
                self.frontier.len(), self.frontier.iter().map(|k| k.level).min(), self.zoom_only.iter().filter(|k| self.ready.contains_key(k)).count(),
                self.frontier.iter().filter(|key| self.ready.get(key).is_some_and(|t| t.far)).count());
        }
        if changed || !self.transitions.is_empty() || self.hidden != hidden {
            self.hidden = hidden;
            if self.transitions.values().all(|f| now - f.start >= LOD_FADE) {
                self.transitions.clear();
            }
            render.lod_draw.clear();
            if !hidden {
                self.draw_frontier(now, &mut render.lod_draw);
            }
        }
        if let (Some(compute), Some(terrain)) = (far_compute, &self.terrain) {
            render
                .far_lod
                .prepare(gpu, early, frame.slot, compute, terrain.description.len(), || terrain.far_description(c, dim, textures));
        }
    }

    fn change_frontier(&mut self, next: FxHashSet<TileKey>, now: Instant) {
        if self.frontier == next {
            return;
        }
        let mut old = std::mem::take(&mut self.transitions);
        old.retain(|_, fade| now - fade.start < LOD_FADE);
        let mut groups: FxHashMap<TileKey, (bool, Vec<TileKey>)> = FxHashMap::default();
        let top = self.max_level.max(
            old.keys()
                .chain(&self.frontier)
                .map(|k| k.level)
                .max()
                .unwrap_or(0),
        );
        let ancestor = |mut key: TileKey, has: &dyn Fn(&TileKey) -> bool| {
            let mut found = None;
            while key.level < top {
                key = key.parent();
                if has(&key) {
                    found = Some(key);
                }
            }
            found
        };
        for &key in &next {
            if let Some(parent) = ancestor(key, &|k| self.frontier.contains(k) || old.contains_key(k)) {
                groups
                    .entry(parent)
                    .or_insert_with(|| (true, Vec::new()))
                    .1
                    .push(key);
            }
        }
        for &key in self.frontier.difference(&next) {
            if let Some(parent) = ancestor(key, &|k| next.contains(k)) {
                groups
                    .entry(parent)
                    .or_insert_with(|| (false, Vec::new()))
                    .1
                    .push(key);
            }
        }
        for (parent, fade) in old
            .iter()
            .filter(|(p, f)| !f.refining && next.contains(p) && now - f.start < LOD_FADE)
        {
            groups
                .entry(*parent)
                .or_insert_with(|| (false, fade.fine.clone()));
        }
        let area = |key: TileKey| 1u64 << (key.level * 2);
        let mut solid = next.clone();
        let mut groups: Vec<_> = groups.into_iter().collect();
        groups.sort_unstable_by_key(|(k, _)| std::cmp::Reverse(k.level));
        for (parent, (refining, fine)) in groups {
            // At the inner/outer radius only part of the parent may belong to either view.
            if fine.iter().map(|&k| area(k)).sum::<u64>() != area(parent)
                || ancestor(parent, &|k| self.transitions.contains_key(k)).is_some()
            {
                continue;
            }
            let start = match old.get(&parent) {
                Some(f) if f.refining == refining => f.start,
                Some(f) => now - LOD_FADE.saturating_sub(now - f.start),
                None => now,
            };
            if now - start >= LOD_FADE {
                continue;
            }
            if refining {
                for k in &fine {
                    solid.remove(k);
                }
            } else {
                solid.remove(&parent);
            }
            self.transitions
                .insert(parent, Fade { fine, refining, start });
        }
        self.solid.clear();
        self.solid.extend(solid);
        self.frontier = next;
    }

    fn draw_frontier(&self, now: Instant, out: &mut Vec<(TileKey, f32)>) {
        if self.transitions.is_empty() {
            out.extend(self.frontier.iter().map(|&key| (key, 2.0)));
            return;
        }
        out.extend(self.solid.iter().map(|&key| (key, 2.0)));
        for (&parent, fade) in &self.transitions {
            let progress = ((now - fade.start).as_secs_f32() / LOD_FADE.as_secs_f32()).min(1.0);
            let detail = if fade.refining { progress } else { 1.0 - progress };
            if detail < 1.0 {
                out.push((parent, detail - 1.0));
            }
            if detail > 0.0 {
                out.extend(fade.fine.iter().map(|&key| (key, detail)));
            }
        }
    }

    fn trim(&mut self, needed: &FxHashSet<TileKey>, render: &mut super::world_render::WorldRenderer) {
        self.generation += 1;
        let mut spare = Vec::new();
        let mut quads = 0;
        for (&key, tile) in &mut self.ready {
            if needed.contains(&key) || self.remembered.contains_key(&key) {
                tile.last_used = self.generation;
            } else {
                quads += tile.quads;
                spare.push((tile.last_used, key, tile.quads));
            }
        }
        let mut count = spare.len();
        // Room for the other directions around the camera, including empty tiles.
        let limit = 3 * self.wanted.len();
        if count <= limit && quads <= SPARE_QUADS {
            return;
        }
        spare.sort_unstable();
        for (_, key, size) in spare {
            self.ready.remove(&key);
            if !self.pending.contains(&key) {
                self.zoom_only.remove(&key);
            }
            render.remove_lod(key);
            quads -= size;
            count -= 1;
            if count <= limit && quads <= SPARE_QUADS {
                break;
            }
        }
    }

    fn release_zoom(&mut self, mut remove: impl FnMut(TileKey)) {
        self.zoom_only.retain(|key| {
            if self.ready.remove(key).is_some() {
                remove(*key);
            }
            // Ignore a late completion unless a subsequent ordinary view needs it.
            self.pending.contains(key)
        });
    }

    /// How high the land is at the camera's column.
    fn land_height(&mut self, c: &Content, cam: DVec3) -> Option<i32> {
        let at = IVec2::new(cam.x.floor() as i32, cam.z.floor() as i32);
        if self.ground.is_none_or(|g| g.0 != (self.dim, at)) {
            let height = c.dims[self.dim as usize]
                .column(self.seed, at.x, at.y)
                .map_or(i32::MIN, |col| col.height);
            self.ground = Some(((self.dim, at), height));
        }
        self.ground.map(|g| g.1).filter(|&h| h != i32::MIN)
    }

    /// Every possible ground-mask lookup for this tile, including the neighbor across a wall,
    /// resolves to a shown cube. Partial coverage is still tested per pixel.
    pub(super) fn ground_covered(&self, key: TileKey, heights: (i32, i32), spill: i32, camera: IVec3) -> bool {
        let center = IVec2::new(camera.x, camera.z);
        if self.mask.is_empty() || self.center != center || heights.0 < i32::from(i16::MIN) || heights.1 > i32::from(i16::MAX) {
            return false;
        }
        let origin = key.origin().as_i64vec2();
        let lo = origin - i64::from(spill);
        let hi = origin + i64::from(key.size()) + i64::from(spill);
        let magnitude = lo
            .abs()
            .max(hi.abs())
            .max_element()
            .max(i64::from(heights.0).abs())
            .max(i64::from(heights.1).abs());
        // Packed box edges, the quarter-block wall offset, and interpolation in world-space f32.
        let margin = 2 * i64::from(key.scale()) + (magnitude as f64 * 4.0 * f64::from(f32::EPSILON)).ceil() as i64;
        if magnitude + margin + i64::from(lod_face::DETAIL_PAD) > i64::from(i32::MAX) {
            return false;
        }
        let first = ((lo - margin) >> 4u32).as_ivec2();
        let last = ((hi + margin) >> 4u32).as_ivec2();
        if (first - center)
            .abs()
            .max((last - center).abs())
            .max_element()
            > self.half
        {
            return false;
        }
        let width = 2 * self.half + 1;
        let header = 2 * (width as usize * width as usize).div_ceil(32);
        let base_y = self.mask[header] as i32;
        let stride = self.mask[header + 1] as usize;
        let bottom = ((i64::from(heights.0) - margin) >> 4) as i32 - base_y;
        let top = ((i64::from(heights.1) + margin) >> 4) as i32 - base_y;
        if bottom < 0 || top < bottom || top as usize / 32 >= stride {
            return false;
        }
        for z in first.y..=last.y {
            for x in first.x..=last.x {
                let i = (z.rem_euclid(width) * width + x.rem_euclid(width)) as usize;
                if self.mask[i / 32] & (1 << (i % 32)) == 0 {
                    return false;
                }
                let column = header + 2 + i * stride;
                for word in bottom / 32..=top / 32 {
                    let from = (bottom - word * 32).max(0);
                    let to = (top - word * 32).min(31);
                    let needed = (u32::MAX << from) & (u32::MAX >> (31 - to));
                    if self.mask[column + word as usize] & needed != needed {
                        return false;
                    }
                }
            }
        }
        true
    }

    /// Keeps the mask of columns shown by cubes current: columns that changed, and the ones
    /// coming into the window when the camera moved to another column. A bit per column for
    /// its ground (seen through what is clear), then a bit per column for its top (a sea's
    /// surface): a sea with its floor not drawn yet shows the tile's floor under the cubes' water.
    fn update_mask(
        &mut self,
        gpu: &mut dyn weft_render::Backend,
        render: &mut super::world_render::WorldRenderer,
        world: &mut super::world::ClientWorld,
        seed: u64,
        cam: DVec3,
        view: i32,
    ) {
        let half = view + MASK_MARGIN;
        let w = 2 * half + 1;
        let center = IVec2::new((cam.x / 16.0).floor() as i32, (cam.z / 16.0).floor() as i32);
        let slot = |col: IVec2| (col.y.rem_euclid(w) * w + col.x.rem_euclid(w)) as usize;
        let words = ((w * w) as usize).div_ceil(32);
        let (low, high) = world.lod_range();
        let layers = (high.saturating_sub(low).saturating_add(1).max(0) as usize).div_ceil(32);
        let header = 2 * words;
        let info = [low as u32, layers as u32];
        let changed = world.take_changed_columns();
        let world = &*world;
        let set = |mask: &mut Vec<u32>, col: IVec2| {
            let i = slot(col);
            let bit = 1u32 << (i % 32);
            let ground = world.column_shown(col, seed, true);
            let surface = world.column_shown(col, seed, false);
            let mut changed = false;
            for (part, shown) in [(0, ground), (1, surface)] {
                let word = part * words + i / 32;
                let old = mask[word];
                mask[word] = if shown { old | bit } else { old & !bit };
                changed |= old != mask[word];
            }
            let offset = header + info.len() + i * layers;
            for (word, stored) in mask[offset..offset + layers].iter_mut().enumerate() {
                let mut shown = 0u32;
                if ground || surface {
                    let first = low + word as i32 * 32;
                    for y in first..=high.min(first + 31) {
                        if world.cube_shown(IVec3::new(col.x, y, col.y)) {
                            shown |= 1 << (y - first);
                        }
                    }
                }
                changed |= *stored != shown;
                *stored = shown;
            }
            changed
        };
        let inside = |col: IVec2, at: IVec2| (col - at).abs().max_element() <= half;
        if half != self.half || self.mask.get(header..header + info.len()) != Some(&info) {
            self.half = half;
            self.mask = vec![0; header + info.len() + (w * w) as usize * layers];
            self.mask[header..header + info.len()].copy_from_slice(&info);
            self.center = center;
            for z in -half..=half {
                for x in -half..=half {
                    let col = center + IVec2::new(x, z);
                    set(&mut self.mask, col);
                }
            }
            self.mask_changed = true;
        } else if center != self.center {
            let old = self.center;
            self.center = center;
            for z in -half..=half {
                for x in -half..=half {
                    let col = center + IVec2::new(x, z);
                    if !inside(col, old) {
                        self.mask_changed |= set(&mut self.mask, col);
                    }
                }
            }
        }
        for col in changed {
            if inside(col, center) {
                self.mask_changed |= set(&mut self.mask, col);
            }
        }
        render.lod_half = half as u32;
        if self.mask_changed {
            self.mask_changed = false;
            render.set_lod_mask(gpu, &self.mask);
        }
    }

    fn select(&self, view: LodView, heights: (f64, f64)) -> FxHashSet<TileKey> {
        let top = CELLS << self.max_level;
        let p = glam::DVec2::new(view.pos.x, view.pos.z);
        let reach = view.reach as f64;
        let (lo, hi) = (((p - reach) / top as f64).floor().as_ivec2(), ((p + reach) / top as f64).floor().as_ivec2());
        let planes = super::world_render::projection_planes(view.projection);
        // Allow overlap from slopes, walls and conservative height envelopes.
        let budget = (view.tiles_per_screen * 8.0).ceil().max(16.0) as usize;
        let mut tree = FxHashSet::default();
        let mut queue = BinaryHeap::new();
        let in_range = |key: TileKey| {
            let (near, far) = key.distance(p);
            near <= reach && far >= view.hide as f64
        };
        let priority = |key: TileKey| {
            if key.level == 0 {
                return None;
            }
            let (low, high) = self
                .ready
                .get(&key)
                .map(|t| t.y)
                .or_else(|| self.cached_heights.get(&key).copied())?;
            let origin = key.origin();
            let bounds = |height: (f64, f64)| {
                let min = (DVec3::new(origin.x as f64, height.0, origin.y as f64) - view.pos).as_vec3();
                let max =
                    (DVec3::new(origin.x as f64 + key.size() as f64, height.1, origin.y as f64 + key.size() as f64) - view.pos).as_vec3();
                (min, max)
            };
            let (min, max) = bounds(heights);
            let possible = view.area(min, max, &planes);
            if possible == 0.0 {
                return None;
            }
            let (min, max) = bounds((low as f64, high as f64));
            let sampled = view.area(min, max, &planes);
            // Drawn bounds prioritize visible geometry. An unsampled peak remains eligible within
            // the conservative height envelope, below visible geometry that exceeds its budget.
            if sampled > 1.0 { Some((true, sampled.to_bits(), key)) } else { (possible > 1.0).then_some((false, possible.to_bits(), key)) }
        };
        for z in lo.y..=hi.y {
            for x in lo.x..=hi.x {
                let key = TileKey { level: self.max_level, x, z };
                if in_range(key) {
                    tree.insert(key);
                    if let Some(candidate) = priority(key) {
                        queue.push(candidate);
                    }
                }
            }
        }
        let mut leaves = tree.len();
        while let Some((_, _, key)) = queue.pop() {
            let children = key.children().map(|child| in_range(child).then_some(child));
            let count = children.iter().flatten().count();
            if leaves + count - 1 > budget {
                continue;
            }
            leaves = leaves + count - 1;
            for child in children.into_iter().flatten() {
                tree.insert(child);
                if let Some(candidate) = priority(child) {
                    queue.push(candidate);
                }
            }
        }
        tree
    }

    fn pick(&mut self, view: LodView, heights: (f64, f64), normal: Option<LodView>) -> FxHashSet<TileKey> {
        self.max_level = (view.reach as u32)
            .next_power_of_two()
            .ilog2()
            .saturating_sub(CELLS.ilog2())
            .clamp(1, MAX_LEVEL);
        let (mut needed, wide) = match normal {
            Some(wide) if !view.similar(wide) => (self.select(view, heights), Some(self.select(wide, heights))),
            _ => (self.select(normal.unwrap_or(view), heights), None),
        };
        let p = glam::DVec2::new(view.pos.x, view.pos.z);
        let reach = view.reach as f64;
        self.remembered.retain(|&key, at| {
            let (near, far) = key.distance(p);
            near <= reach && far >= view.hide as f64 && view.pos.distance_squared(*at) <= (2.0 * key.size() as f64).powi(2)
        });
        self.remembered.extend(
            wide.as_ref()
                .unwrap_or(&needed)
                .iter()
                .map(|&key| (key, view.pos)),
        );
        self.zoom_only
            .retain(|key| !self.remembered.contains_key(key));
        if needed
            .iter()
            .any(|k| k.level == self.max_level && !self.ready.contains_key(k))
        {
            for &key in self.frontier.iter().filter(|k| k.level <= self.max_level) {
                let (near, far) = key.distance(p);
                if near > reach || far < view.hide as f64 {
                    continue;
                }
                let mut ancestor = key;
                loop {
                    needed.insert(ancestor);
                    if ancestor.level == self.max_level {
                        break;
                    }
                    ancestor = ancestor.parent();
                }
            }
        }
        self.wanted.clear();
        self.wanted.extend(needed.iter().copied());
        if let Some(wide) = wide {
            self.wanted.extend(wide.difference(&needed).copied());
        }
        let mut fallback = FxHashSet::default();
        for mut key in self
            .cache_misses
            .iter()
            .copied()
            .filter(|k| needed.contains(k))
        {
            loop {
                if !fallback.insert(key) || key.level == self.max_level {
                    break;
                }
                key = key.parent();
            }
        }
        self.wanted.sort_by_cached_key(|key| {
            let distance = key.distance(p).0.to_bits();
            let cached_leaf = self.cached_heights.contains_key(key)
                && needed.contains(key)
                && (key.level == 0 || !key.children().iter().any(|k| needed.contains(k)));
            let coarse = self.max_level - key.level;
            if fallback.contains(key) {
                (0, coarse, distance)
            } else if cached_leaf {
                (1, key.level, distance)
            } else {
                (2, coarse, distance)
            }
        });
        needed
    }

    fn cover(&self, key: TileKey, needed: &FxHashSet<TileKey>, shown: &mut Vec<TileKey>) -> bool {
        let start = shown.len();
        if key.level > 0 {
            let children = key.children();
            let mut required = false;
            let mut complete = true;
            for child in children.into_iter().filter(|k| needed.contains(k)) {
                required = true;
                complete &= self.cover(child, needed, shown);
            }
            if required && complete {
                return true;
            }
            if !required && !self.ready.contains_key(&key) && children.iter().all(|k| self.ready.contains_key(k)) {
                shown.extend(children);
                return true;
            }
        }
        if self.ready.contains_key(&key) {
            // A complete cached leaf cover needs no intermediate meshes. Only replace partial
            // coverage with a parent when a branch really is missing.
            shown.truncate(start);
            shown.push(key);
            return true;
        }
        false
    }

    /// Asks for the nearest wanted tiles not made yet, or made before columns changed.
    fn request(&mut self, far: bool) {
        let Some(terrain) = &self.terrain else { return };
        let columns = (self.columns_dim == self.dim).then_some(&self.columns);
        let edits = (self.edits_dim == self.dim).then_some(&self.edits);
        // Released zoom jobs may finish, but no longer occupy ordinary request slots.
        let retired = if self.zooming { 0 } else { self.zoom_only.len() };
        for &key in &self.wanted {
            if self.pending.len() >= IN_FLIGHT + retired {
                break;
            }
            // Leave the mark: the one in flight still has the old columns.
            if self.pending.contains(&key) {
                continue;
            }
            let again = self.stale.remove(&key);
            if self.ready.contains_key(&key) && !again {
                continue;
            }
            self.pending.insert(key);
            if self.zooming && !self.remembered.contains_key(&key) && !self.ready.contains_key(&key) {
                self.zoom_only.insert(key);
            }
            let (dim, seed) = (self.dim, self.seed);
            let (done, terrain, columns) = (self.done.0.clone(), terrain.clone(), columns.cloned());
            let edits = edits.cloned();
            let dir = self.cache_dir.clone();
            let jobs = self.jobs.clone();
            let probe = !again && key.level < self.max_level && self.cached_heights.contains_key(&key) && !self.cache_misses.contains(&key);
            background().spawn(move || {
                let (columns, column_hash) = tile_columns(key, columns.as_deref(), terrain.detail_margin);
                let edits: Vec<_> = edits
                    .iter()
                    .flat_map(|edits| edits.iter())
                    .filter(|edit| tile_meets_edit(key, edit, terrain.detail_margin))
                    .cloned()
                    .collect();
                if far && key.level >= FAR_LEVEL && columns.is_empty() && edits.is_empty() {
                    let _ = done.send(Ok(BuiltTile::Far(key)));
                    return;
                }
                let mut hash = DefaultHasher::new();
                column_hash.hash(&mut hash);
                for edit in &edits {
                    edit.spec(content()).hash(&mut hash);
                }
                let edits_hash = if edits.is_empty() { column_hash } else { hash.finish() };
                let mut cache = cache::Entry::new(&dir, dim, seed, key, terrain.signature, edits_hash);
                if let Some(mesh) = cache.load(key) {
                    let _ = done.send(Ok(BuiltTile::Meshed(Arc::new(mesh))));
                } else if probe {
                    let _ = done.send(Err(key));
                } else {
                    let _ = jobs.send(LodJob { dim, seed, key, done, terrain, cache, columns, edits });
                }
            });
        }
        self.stale
            .retain(|k| self.ready.contains_key(k) || self.pending.contains(k));
    }
}

fn sample_margin(level: u32, detail_margin: i32) -> i32 {
    forest::shadow_margin(1 << level).max(if level == 0 { detail_margin } else { 0 })
}

fn tile_meets_edit(key: TileKey, edit: &crate::server::area::AreaFill, detail_margin: i32) -> bool {
    let (lo, hi) = edit.shape.bounds();
    let at = key.origin().as_dvec2();
    let margin = sample_margin(key.level, detail_margin).max((BORDER + 1) << key.level) as f64;
    let lower = at - margin;
    let upper = at + (key.size() as f64 + margin);
    hi.x >= lower.x && hi.z >= lower.y && lo.x <= upper.x && lo.z <= upper.y
}

/// Snapshot only columns in the tile and its sampling border, in a stable order for the disk key.
fn tile_columns(key: TileKey, saved: Option<&SavedColumns>, detail_margin: i32) -> (Columns, u64) {
    let mut columns = Columns::default();
    let mut hash = DefaultHasher::new();
    if let Some(saved) = saved.filter(|s| !s.is_empty()) {
        let o = key.origin();
        let border = sample_margin(key.level, detail_margin);
        let bounds = |v: i32| v.saturating_sub(border)..=v.saturating_add(key.size()).saturating_add(border - 1);
        let (x_range, z_range) = (bounds(o.x), bounds(o.y));
        for (&(z, x), col) in saved.range((*z_range.start(), i32::MIN)..=(*z_range.end(), i32::MAX)) {
            if x_range.contains(&x) {
                col.hash(&mut hash);
                columns.insert(IVec2::new(x, z), col.clone());
            }
        }
    }
    (columns, hash.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selection(fov: f32, height: u32, reach: i32) -> LodView {
        let camera = super::super::world_render::Camera {
            pos: DVec3::new(0.0, 100.0, 0.0),
            yaw: 0.0,
            pitch: 0.0,
            fov,
            bob: (glam::Vec3::ZERO, 0.0, 0.0),
        };
        LodView {
            pos: camera.pos,
            projection: camera.proj(16.0 / 9.0) * camera.view(),
            tiles_per_screen: height as f64 * height as f64 * (16.0 / 9.0) / (CELLS as f64 * 6.0).powi(2),
            hide: 64,
            reach,
        }
    }

    fn settle(lod: &mut Lod, view: LodView, heights: (f64, f64)) {
        for _ in 0..=MAX_LEVEL {
            lod.pick(view, heights, None);
            if lod.wanted.iter().all(|k| lod.ready.contains_key(k)) {
                return;
            }
            lod.ready.extend(
                lod.wanted
                    .iter()
                    .map(|&key| (key, ReadyTile { y: (heights.0 as i32, heights.1 as i32), ..ReadyTile::default() })),
            );
        }
        panic!("selection did not settle");
    }

    #[test]
    fn tile_coverage_requires_every_ground_column_and_cube_in_the_height_span() {
        let mut lod = Lod::new(unbounded().0, PathBuf::new(), None);
        let key = TileKey { level: 0, x: -1, z: 0 };
        let camera = IVec3::new(-1, 16, 0);
        lod.center = IVec2::new(camera.x, camera.z);
        lod.half = 4;
        let width = 2 * lod.half + 1;
        let columns = (width * width) as usize;
        let header = 2 * columns.div_ceil(32);
        lod.mask = vec![u32::MAX; header + 2 + columns * 2];
        lod.mask[header..header + 2].copy_from_slice(&[(-16i32) as u32, 2]);
        let covered = |lod: &Lod| lod.ground_covered(key, (238, 273), 0, camera);
        assert!(covered(&lod));
        assert!(!lod.ground_covered(key, (238, 273), 0, camera + IVec3::X));
        // The negative-X neighbor can be sampled by a wall. Its span crosses two Y words.
        let column = ((-1i32).rem_euclid(width) * width + (-3i32).rem_euclid(width)) as usize;
        let at = header + 2 + column * 2;
        for (index, bit) in [(column / 32, column % 32), (at, 31), (at + 1, 0)] {
            lod.mask[index] &= !(1 << bit);
            assert!(!covered(&lod));
            lod.mask[index] |= 1 << bit;
        }
        lod.mask[at + 1] &= !(1 << 2);
        assert!(covered(&lod));
    }

    #[test]
    fn edit_heights_fit_lod_samples_in_open_ended_dimensions() {
        let min = i16::MIN as i32;
        let max = i16::MAX as i32;
        assert_eq!(lod_edit_heights(i32::MIN, i32::MAX), min..=max);
        assert_eq!(lod_edit_heights(i32::MIN, 4), min..=79);
        assert_eq!(lod_edit_heights(-4, i32::MAX), -64..=max);
        assert_eq!(lod_edit_heights(-4, 4), -64..=79);
        assert!(lod_edit_heights(2048, i32::MAX).is_empty());
        assert!(lod_edit_heights(i32::MIN, -2049).is_empty());
    }

    #[test]
    fn tile_crossing_near_plane_can_cover_only_the_bottom_of_the_view() {
        let view = selection(70.0, 1080, 4096);
        let planes = super::super::world_render::projection_planes(view.projection);
        let min = glam::Vec3::new(-32.0, -800.0, -1024.0);
        let max = glam::Vec3::new(32.0, -650.0, 8.0);
        let area = view.area(min, max, &planes);
        assert!(area > 0.0 && area < view.tiles_per_screen * 0.1, "projected area: {area}");
    }

    #[test]
    fn projected_detail_and_range() {
        let view = selection(70.0, 1080, 4096);
        let planes = super::super::world_render::projection_planes(view.projection);
        assert_eq!(view.area(glam::Vec3::new(-32.0, -100.0, -32.0), glam::Vec3::new(32.0, -99.0, 32.0), &planes), 0.0);
        let mut lod = Lod::new(unbounded().0, PathBuf::new(), None);
        let level_at = |lod: &Lod, x: i32, z: i32| {
            lod.wanted
                .iter()
                .filter(|k| {
                    let o = k.origin();
                    x >= o.x && x < o.x + k.size() && z >= o.y && z < o.y + k.size()
                })
                .map(|k| k.level)
                .min()
                .unwrap()
        };
        settle(&mut lod, selection(70.0, 1080, 4096), (0.0, 256.0));
        let normal = lod.wanted.len();
        let normal_level = level_at(&lod, 0, -2048);
        settle(&mut lod, selection(70.0, 2160, 4096), (0.0, 256.0));
        assert!(level_at(&lod, 0, -2048) < normal_level);
        settle(&mut lod, selection(14.0, 1080, 4096), (0.0, 256.0));
        assert!(level_at(&lod, 0, -2048) < normal_level);
        assert!(level_at(&lod, 0, 2048) > normal_level);
        settle(&mut lod, selection(70.0, 1080, 16 * 1_048_576), (0.0, 256.0));
        assert!(lod.max_level > 5);
        assert!(lod.wanted.len() < normal * 8, "{} -> {} tiles", normal, lod.wanted.len());
        assert!(
            lod.wanted
                .windows(2)
                .all(|pair| pair[0].level >= pair[1].level)
        );
        let far = lod.wanted.len();
        settle(&mut lod, selection(1.4, 1080, 16 * 1_048_576), (0.0, 256.0));
        assert!(lod.wanted.len() < 8192, "extreme zoom requests {} tiles", lod.wanted.len());
        eprintln!("adaptive LOD nodes normal={normal}, million-range={far}, 50x={}", lod.wanted.len());
    }

    #[test]
    fn turning_keeps_normal_detail_ready() {
        let mut lod = Lod::new(unbounded().0, PathBuf::new(), None);
        let view = selection(70.0, 1080, 4096);
        let heights = (0.0, 256.0);
        settle(&mut lod, view, heights);
        let original = lod.pick(view, heights, None);
        let back = LodView { projection: view.projection * Mat4::from_rotation_y(std::f32::consts::PI), ..view };
        let away = lod.pick(back, heights, None);
        assert!(!original.is_subset(&away));
        assert!(original.iter().all(|key| lod.remembered.contains_key(key)));
        assert_eq!(lod.pick(view, heights, None), original);
    }

    #[test]
    fn zoom_sweep_only_retains_ordinary_detail() {
        let mut lod = Lod::new(unbounded().0, PathBuf::new(), None);
        let normal = selection(70.0, 1080, 4096);
        let zoom = selection(14.0, 1080, 4096);
        let heights = (0.0, 256.0);
        let mut expected = FxHashSet::default();
        for yaw in [0.0, 1.0, 2.0, 3.0, 4.0, 5.0] {
            let turn = Mat4::from_rotation_y(yaw);
            let wide = LodView { projection: normal.projection * turn, ..normal };
            let narrow = LodView { projection: zoom.projection * turn, ..zoom };
            for _ in 0..=MAX_LEVEL {
                let tiles = lod.pick(narrow, heights, Some(wide));
                expected.extend(lod.select(wide, heights));
                if lod.wanted.iter().all(|k| lod.ready.contains_key(k)) {
                    assert!(!tiles.is_subset(&lod.select(wide, heights)));
                    break;
                }
                for &key in &lod.wanted {
                    if !lod.remembered.contains_key(&key) {
                        lod.zoom_only.insert(key);
                    }
                    lod.ready
                        .insert(key, ReadyTile { y: (0, 256), ..ReadyTile::default() });
                }
            }
        }
        assert_eq!(lod.remembered.keys().copied().collect::<FxHashSet<_>>(), expected);
        assert_eq!(lod.pick(normal, heights, None), lod.select(normal, heights));
        let late = *lod.zoom_only.iter().next().unwrap();
        lod.pending.insert(late);
        lod.release_zoom(|_| {});
        assert_eq!(lod.ready.keys().copied().collect::<FxHashSet<_>>(), expected);
        assert_eq!(lod.zoom_only, FxHashSet::from_iter([late]));
    }

    #[test]
    fn cached_view_selects_leaves_before_meshes_arrive() {
        let view = selection(70.0, 1080, 4096);
        let heights = (0.0, 256.0);
        let mut lod = Lod::new(unbounded().0, PathBuf::new(), None);
        settle(&mut lod, view, heights);
        let expected = lod.select(view, heights);
        lod.cached_heights
            .extend(lod.ready.drain().map(|(key, tile)| (key, tile.y)));
        let wanted = lod.pick(view, heights, None);
        assert_eq!(wanted, expected);
        let leaf = lod.wanted[0];
        assert!(leaf.level < lod.max_level);
        assert!(leaf.level == 0 || !leaf.children().iter().any(|k| wanted.contains(k)));
        lod.cache_misses.insert(leaf);
        assert_eq!(lod.pick(view, heights, None), expected);
        assert_eq!(lod.wanted[0].level, lod.max_level);
    }

    #[test]
    fn coverage_waits_for_complete_children() {
        let mut lod = Lod::new(unbounded().0, PathBuf::new(), None);
        let parent = TileKey { level: 3, x: -1, z: 0 };
        let children = parent.children();
        let needed = [parent].into_iter().chain(children).collect();
        lod.ready.extend(
            [parent, children[0]]
                .into_iter()
                .map(|k| (k, ReadyTile::default())),
        );
        let mut shown = Vec::new();
        lod.cover(parent, &needed, &mut shown);
        assert_eq!(shown, [parent]);
        lod.ready
            .extend(children.into_iter().map(|k| (k, ReadyTile::default())));
        shown.clear();
        lod.cover(parent, &needed, &mut shown);
        assert_eq!(shown, children);
        lod.ready.remove(&parent);
        shown.clear();
        lod.cover(parent, &[parent].into_iter().collect(), &mut shown);
        assert_eq!(shown, children);

        let fine: FxHashSet<_> = children.into_iter().flat_map(TileKey::children).collect();
        lod.ready.clear();
        lod.ready.insert(parent, ReadyTile::default());
        lod.ready
            .extend(fine.iter().map(|&key| (key, ReadyTile::default())));
        shown.clear();
        lod.cover(parent, &needed.union(&fine).copied().collect(), &mut shown);
        assert_eq!(shown.into_iter().collect::<FxHashSet<_>>(), fine);
        let coarse: FxHashSet<_> = [parent].into_iter().collect();
        lod.frontier = coarse.clone();
        let start = Instant::now();
        lod.change_frontier(fine.clone(), start);
        let middle = start + LOD_FADE / 4;
        let draw = |lod: &Lod, now| {
            let mut out = Vec::new();
            lod.draw_frontier(now, &mut out);
            assert_eq!(out.len(), fine.len() + 1);
            out.into_iter().collect::<FxHashMap<_, _>>()
        };
        let before = draw(&lod, middle);
        assert_eq!(before[&parent], -0.75);
        assert!(fine.iter().all(|k| before[k] == 0.25));
        lod.change_frontier(coarse, middle);
        assert_eq!(draw(&lod, middle), before);
    }

    #[test]
    fn saved_tile_inputs_follow_the_sampling_border() {
        let key = TileKey { level: 2, x: -1, z: 1 };
        let mut saved = SavedColumns::new();
        for (x, z) in [(-141, 127), (-140, 116), (-4, 258), (11, 267), (12, 267), (-2, 268)] {
            saved.insert(
                (z, x),
                Arc::new(LodColumn { x, z, y: 40, top: 1, under: 2, light: 0, spans: Vec::new(), removed_scatter: Vec::new() }),
            );
        }
        let (cols, id) = tile_columns(key, Some(&saved), 1);
        assert_eq!(cols.len(), 3);
        for p in [(-140, 116), (-4, 258), (11, 267)] {
            assert!(cols.contains_key(&IVec2::new(p.0, p.1)));
        }
        saved.remove(&(267, 12));
        assert_eq!(id, tile_columns(key, Some(&saved), 1).1);
        Arc::make_mut(saved.get_mut(&(267, 11)).unwrap()).light = 10;
        assert_ne!(id, tile_columns(key, Some(&saved), 1).1);
    }

    #[test]
    fn scaled_faces_preserve_sub_block_bounds() {
        for level in [0, 1, 5, 6, 10, 19, MAX_LEVEL] {
            let key = TileKey { level, x: -1, z: 1 };
            let scale = key.scale();
            let width = key.size();
            let r = quad(3, 0, 80, 0, (width, 1, width), 9, 3, 2, 255, 0, scale);
            assert_eq!(r[3] & lod_face::POS_MASK, (width / scale - 1) as u32);
            let at = width - 1;
            let r = quad(3, at, 80, at, (1, 1, 1), 9, 3, 2, 255, 0, scale);
            assert_eq!(r[0] & lod_face::POS_MASK, (at / scale) as u32);
            assert_eq!((r[0] >> lod_face::DIR_SHIFT) & face::DIR_MASK, 3);
            let (min, max) = scaled_edges(at, at, scale, [0, 3, 0], [16, 13, 16]);
            assert!(max[0] > min[0] && max[2] > min[2]);
            let r = lod_box(r, min, max, false);
            assert_eq!((r[0] >> lod_face::EXTRUDE_SHIFT) & 1, 0);
            assert_eq!((r[2] >> lod_face::HALF_SHIFT) & lod_face::EDGE_MASK, 3);
        }
    }
}
