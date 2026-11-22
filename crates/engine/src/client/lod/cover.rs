//! Low vegetation contributes to the surface color without more geometry.
use std::hash::{DefaultHasher, Hash};

use glam::{IVec2, Vec3};
use rustc_hash::FxHashMap;
use weft_abi::{Callback, ModId, RenderLayer, SurfaceCoverArgs, SurfaceCoverSample};

use super::{CELLS, Columns, GRID, TileKey, corner_y, sample_offset};
use crate::client::textures::GameTextures;
use crate::client::{looks, mesher};
use crate::content::Content;
use crate::gpu_layout::lod_color_gain;

pub(super) type Colors = [[Option<Cell>; GRID]; GRID];

#[derive(Clone, Copy)]
pub(super) struct Cell {
    pub texture: u32,
    pub gain: u16,
    pub height: f32,
    pub pattern: u16,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Appearance {
    pub gain: u16,
    pub pattern: Option<u16>,
}

#[derive(Clone, Copy)]
struct Material {
    texture: u32,
    rgb: Vec3,
    opacity: f32,
    tint: u8,
    height: f32,
}

impl Material {
    fn color(self, c: &Content, biome: u16) -> Vec3 {
        let tint = c.kind_color(self.tint, Some(biome));
        self.rgb * Vec3::from_array([16, 8, 0].map(|s| ((tint >> s) & 255) as f32 / 255.0))
    }
}

pub(super) struct Cover {
    hooks: Vec<(ModId, Callback)>,
    ground: FxHashMap<u32, Material>,
    plants: Vec<Option<Material>>,
}

impl Cover {
    pub(super) fn new(
        c: &Content,
        dim: u32,
        faces: &[[(u32, u32); 6]],
        textures: &GameTextures,
        signature: &mut DefaultHasher,
    ) -> Option<Self> {
        let def = &c.dims[dim as usize];
        let hooks: Vec<_> = def
            .features
            .iter()
            .filter_map(|f| f.surface_cover)
            .collect();
        if hooks.is_empty() {
            return None;
        }
        let terrain = def.terrain.as_ref()?;
        for biome in &c.biomes {
            biome.tints.hash(signature);
        }
        let mut cache = FxHashMap::default();
        let mut material = |(tex, tint)| {
            let (rgb, opacity) = *cache.entry(tex).or_insert_with(|| {
                let pixels = textures.impostor_texels(tex, 4);
                pixels.hash(signature);
                let mut rgb = Vec3::ZERO;
                let mut alpha = 0.0;
                for [r, g, b, a] in pixels {
                    rgb += Vec3::new(r as f32, g as f32, b as f32) * a as f32;
                    alpha += a as f32;
                }
                (rgb / (255.0 * alpha.max(1.0)), alpha / (16.0 * 255.0))
            });
            Material { texture: tex, rgb, opacity, tint: tint as u8, height: 0.0 }
        };
        let mut ground = FxHashMap::default();
        for state in std::iter::once(terrain.stone).chain(
            terrain
                .rules
                .iter()
                .flat_map(|r| [r.top, r.deep_top.map(|v| v.0)].into_iter().flatten()),
        ) {
            ground
                .entry(state)
                .or_insert_with(|| material(faces[state as usize][3]));
        }
        let looks = looks::current();
        let projection = c.components.id(weft_abi::comp::feature::SURFACE_PROJECTION);
        let plants = c
            .states
            .iter()
            .enumerate()
            .map(|(id, state)| {
                let opted_in = projection.is_some_and(|tag| c.components.has(crate::components::BLOCK, id as u32, tag));
                if !opted_in && (state.layer != RenderLayer::Cutout || state.solid()) {
                    return None;
                }
                // For a tall plant use the canopy, whose texture can differ from its stem.
                let model = looks.drawn(id as u32, state);
                let (height, tex, dir) = model
                    .boxes
                    .iter()
                    .flat_map(|b| {
                        (0..6).filter_map(move |dir| {
                            mesher::box_face(b, dir)?;
                            Some((b.max[1], b.textures[dir], dir))
                        })
                    })
                    .max_by(|a, b| a.0.total_cmp(&b.0))?;
                let first = mesher::face_layers(c, state, tex & crate::gpu_layout::face::TEX_MASK, dir)[0];
                Some(Material { height: height.clamp(0.0, 3.0), ..material((first.tex, first.tint as u32)) })
            })
            .collect();
        Some(Self { hooks, ground, plants })
    }

    pub(super) fn colors(&self, c: &Content, seed: u64, key: TileKey, words: &[u32], columns: &Columns) -> Colors {
        let mut colors = [[None; GRID]; GRID];
        let s = 1 << key.level;
        let side = s.min(2);
        let n = (side * side) as usize;
        let origin = key.origin();
        let mut samples = Vec::new();
        let mut cells = Vec::new();
        for j in -1..=CELLS {
            for i in -1..=CELLS {
                let k = sample_offset(i, j);
                let state = words[k] >> 16;
                let Some(&ground) = self.ground.get(&state) else { continue };
                let p = origin + IVec2::new(i * s, j * s);
                if words[k + 2] != 0 {
                    continue;
                }
                let y = corner_y(words, i, j);
                let biome = words[k + 1] as u16;
                cells.push((i, j, ground, ground.color(c, biome)));
                let stride = s / side;
                for dz in 0..side {
                    for dx in 0..side {
                        let x = p.x + dx * stride;
                        let z = p.y + dz * stride;
                        let h = weft_noise::hash(seed, x as i64, 0, z as i64);
                        let bx = x + (h as i32 & (stride - 1));
                        let bz = z + ((h >> 32) as i32 & (stride - 1));
                        let saved = columns.get(&IVec2::new(bx, bz));
                        let (y, ground) = saved
                            .filter(|col| col.top != 0)
                            .map_or((y, state), |col| (col.y, col.top));
                        let plant_y = y + 1;
                        let occupied = saved.is_some_and(|col| {
                            col.spans
                                .iter()
                                .any(|span| span.bottom <= plant_y && plant_y <= span.top)
                        });
                        let ground = if occupied { 0 } else { ground };
                        samples.push(SurfaceCoverSample { surface: [bx, y, bz].into(), biome: biome as u32, ground, plant: 0 });
                    }
                }
            }
        }
        if samples.is_empty() {
            return colors;
        }
        let mut args = SurfaceCoverArgs { seed, samples: samples.as_mut_ptr(), len: samples.len() };
        for &(owner, cb) in &self.hooks {
            crate::host::invoke(owner, cb, &mut args);
        }
        for ((i, j, material, ground), samples) in cells.into_iter().zip(samples.chunks_exact(n)) {
            let mut delta = Vec3::ZERO;
            let mut height = 0.0;
            let mut pattern = 0;
            let mut thickest = 0.0;
            for sample in samples {
                if let Some(plant) = self.plants.get(sample.plant as usize).copied().flatten() {
                    // A crossed plant shows more than one face from an oblique view.
                    let cover = (plant.opacity * 1.6).min(0.8);
                    delta += (plant.color(c, sample.biome as u16) - ground) * cover;
                    let reach = plant.height * cover;
                    height += reach;
                    if reach > thickest {
                        thickest = reach;
                        pattern = plant.texture as u16;
                    }
                }
            }
            if height > 0.0 {
                // Apply color relative to the original texture so its full contrast survives.
                let color = ground + delta / n as f32;
                let gain = color / material.rgb.max(Vec3::splat(1.0 / 255.0));
                colors[(i + 1) as usize][(j + 1) as usize] = Some(Cell {
                    texture: material.texture,
                    gain: lod_color_gain(gain.to_array()),
                    height: height * 2.0 / n as f32,
                    pattern,
                });
            }
        }
        colors
    }
}
