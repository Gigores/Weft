//! Block geometry in a one-block terrain tile, outside the loaded cubes.
use std::sync::Arc;

use glam::{IVec2, IVec3, Vec2};
use rustc_hash::{FxHashMap, FxHashSet};
use weft_abi::RenderLayer;

use super::{CELLS, Columns, GRID, TileKey, forest, quad};
use crate::client::{looks, mesher};
use crate::features::{grid_layout, start_at};
use crate::generation::Snapshot;
use crate::gpu_layout::{CORNERS, face, lod_box, lod_color_gain, lod_face, lod_set_gain};
use crate::server::lod_tops::{LodColumn, Span, impostor_block};

pub(super) struct Structures {
    pub(super) columns: Columns,
    pub(super) outside: FxHashMap<IVec2, (u32, Vec<Span>)>,
}

pub(super) struct Detail<'a> {
    snapshot: Snapshot<'a>,
    cubes: FxHashMap<IVec3, Arc<Vec<u32>>>,
}

impl<'a> Detail<'a> {
    pub(super) fn new(snapshot: Snapshot<'a>) -> Self {
        Self { snapshot, cubes: FxHashMap::default() }
    }

    fn block(&mut self, p: IVec3) -> u32 {
        let side = IVec3::splat(16);
        let cube = p.div_euclid(side);
        let def = self.snapshot.def();
        if !(def.min_cube_y..=def.max_cube_y).contains(&cube.y) {
            return 0;
        }
        self.cubes
            .entry(cube)
            .or_insert_with(|| self.snapshot.cube(cube))[crate::world::local_index(p.rem_euclid(side))]
    }

    pub(super) fn structures(
        &mut self,
        key: TileKey,
        saved: &Columns,
        heights: &[[i32; GRID]; GRID],
        grove: &mut forest::TileForest,
    ) -> Structures {
        let snap = self.snapshot;
        let def = snap.def();
        let origin = key.origin();
        let end = origin + IVec2::splat(CELLS - 1);
        let margin = forest::shadow_margin(1);
        let mut generated = FxHashMap::<IVec2, Vec<Span>>::default();
        for (fi, feature) in def.features.iter().enumerate() {
            let Some((spacing, _)) = feature.impostor_grid() else { continue };
            let s = IVec2::splat(spacing);
            let split = feature.reach[0] >= lod_face::DETAIL_PAD as i32;
            let search = if split { feature.reach[0].max(margin) } else { margin };
            let lo = (origin - search).div_euclid(s);
            let hi = (end + search).div_euclid(s);
            for z in lo.y..=hi.y {
                for x in lo.x..=hi.x {
                    let hash = weft_noise::hash(snap.seed ^ feature.salt, x as i64, 0, z as i64);
                    let ((wx, wz), ..) = grid_layout(snap.c, feature, (x, z), spacing, hash);
                    let root = IVec2::new(wx, wz);
                    if saved
                        .get(&root)
                        .is_some_and(|col| col.removed_scatter.contains(&feature.id))
                    {
                        continue;
                    }
                    let Some(start) = start_at(snap.c, def, snap.dim, snap.seed, fi, (x, z)) else { continue };
                    let own = root.cmpge(origin).all() && root.cmple(end).all();
                    let top = start.hi.y + 1;
                    let middle = start.pos.y + (top - start.pos.y).max(0) / 2;
                    let mut canopy = FxHashSet::default();
                    for &(p, state) in start.lod_blocks(Snapshot { before: feature.stage, ..snap }) {
                        if !impostor_block(snap.c, state) {
                            continue;
                        }
                        let at = IVec2::new(p.x, p.z);
                        if p.y >= middle {
                            canopy.insert(at);
                        }
                        let here = if split { at.cmpge(origin).all() && at.cmple(end).all() } else { own };
                        let local = at - origin;
                        if here
                            && local.min_element() >= -(lod_face::DETAIL_PAD as i32)
                            && local.max_element() <= (lod_face::POS_MASK - lod_face::DETAIL_PAD) as i32
                            && self.block(p) == state
                            && !saved.get(&at).is_some_and(|col| {
                                col.top != 0 && p.y <= col.y || col.spans.iter().any(|s| s.bottom <= p.y && p.y <= s.top)
                            })
                        {
                            generated
                                .entry(at)
                                .or_default()
                                .push(Span { bottom: p.y, top: p.y, block: state, light: 0 });
                        }
                    }
                    if !canopy.is_empty() {
                        let area = canopy.len() as f32;
                        let center = canopy
                            .iter()
                            .map(|&p| (p - origin).as_vec2() + 0.5)
                            .sum::<Vec2>()
                            / area;
                        let radius = (area / std::f32::consts::PI).sqrt().min(margin as f32);
                        grove.shade(center, 1, radius, top, heights);
                    }
                }
            }
        }
        let mut columns = saved.clone();
        let mut outside = FxHashMap::default();
        for (p, mut spans) in generated {
            if p.cmplt(origin).any() || p.cmpgt(end).any() {
                let biome = def.column(snap.seed, p.x, p.y).map_or(0, |c| c.biome);
                outside.insert(p, (biome, spans.clone()));
            }
            let column = columns.entry(p).or_insert_with(|| {
                Arc::new(LodColumn { x: p.x, z: p.y, y: 0, top: 0, under: 0, light: 0, spans: Vec::new(), removed_scatter: Vec::new() })
            });
            let column = Arc::make_mut(column);
            spans.append(&mut column.spans);
            spans.sort_unstable_by_key(|s| (s.bottom, s.block));
            spans.dedup();
            spans.dedup_by(|s, prev| {
                if s.bottom == prev.top + 1 && s.block == prev.block && s.light == prev.light {
                    prev.top = s.top;
                    true
                } else {
                    false
                }
            });
            column.spans = spans;
        }
        Structures { columns, outside }
    }

    pub(super) fn plant(&mut self, key: TileKey, p: IVec3, biome: u32, light: u32, out: &mut Vec<[u32; 4]>) -> i32 {
        let state = self.block(p);
        let c = self.snapshot.c;
        let info = &c.states[state as usize];
        if info.layer != RenderLayer::Cutout || info.solid() {
            return p.y;
        }
        let model = looks::current();
        let drawn = model.drawn(state, info);
        let at = IVec2::new(p.x, p.z) - key.origin();
        let mut top = p.y;
        for bx in drawn
            .boxes
            .iter()
            .filter(|b| b.flat && (b.min[0] == b.max[0] || b.min[2] == b.max[2]))
        {
            for dir in [0usize, 1, 4, 5] {
                let Some((corners, uv)) = mesher::box_face(bx, dir) else { continue };
                if uv != [0.0, 0.0, 16.0, 16.0] || bx.turn[dir] != 0 {
                    continue;
                }
                let axis = if dir < 2 { 2 } else { 0 };
                let at_edge = |edge| {
                    let i = CORNERS[dir]
                        .iter()
                        .position(|corner| corner[axis] == edge && corner[1] == 0)
                        .unwrap();
                    corners[i]
                };
                let (a, b) = (at_edge(0), at_edge(1));
                let low = a[1];
                let high = corners.iter().map(|p| p[1]).fold(low, f32::max);
                if a[1] != b[1]
                    || high <= low
                    || !corners
                        .iter()
                        .all(|p| (p[1] == low || p[1] == high) && (0.0..=1.0).contains(&p[0]) && (0.0..=1.0).contains(&p[2]))
                {
                    continue;
                }
                let y = low.floor();
                let snap = |v: f32| (v * 16.0).round().clamp(0.0, 31.0) as u32;
                let pic = mesher::face_layers(c, info, bx.textures[dir] & face::TEX_MASK, dir)[0];
                let rgb = c.kind_color(pic.tint, Some(biome as u16));
                let mut q = quad(dir as u32, at.x, p.y + y as i32, at.y, (1, 1, 1), pic.tex, 0, biome, 0xFF, light, 1);
                q = lod_box(q, [snap(a[0]), snap(low - y), snap(a[2])], [snap(b[0]), snap(high - y), snap(b[2])], false);
                lod_set_gain(&mut q, lod_color_gain([16, 8, 0].map(|n| ((rgb >> n) & 255) as f32 / 255.0)));
                q[0] |= lod_face::PLANT << lod_face::TINT_SHIFT;
                out.push(q);
                top = top.max(p.y + high.ceil() as i32);
            }
        }
        top
    }
}
