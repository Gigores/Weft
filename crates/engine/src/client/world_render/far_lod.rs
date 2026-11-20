//! The coarsest unedited tiles. Only the tile jobs cross to the GPU. The
//! compute pass generates compact faces and the indirect draws that use them.

use glam::IVec3;
use rustc_hash::FxHashMap;
use weft_render::*;

use crate::client::lod::TileKey;
use crate::gpu_layout::terrain::{binding, far_lod as layout};

pub(in crate::client) const TILE_WORDS: u32 = layout::SCRATCH + layout::SAMPLES * 3;
const TILE_BYTES: u64 = TILE_WORDS as u64 * 4;

#[derive(Default)]
pub(in crate::client) struct FarLod {
    pipes: Option<[PipelineId; 2]>,
    tiles: FxHashMap<TileKey, BufferId>,
    build: Vec<(TileKey, BufferId)>,
    retired: Vec<BufferId>,
    jobs: Vec<(BufferId, u32)>,
    description: Option<BufferId>,
}

impl FarLod {
    pub fn reload(&mut self, gpu: &mut dyn Backend, shader: &Shader) {
        if let Some(old) = self.pipes.take() {
            for p in old {
                gpu.destroy_pipeline(p);
            }
        }
        if ["vs_lod_far", "fs_lod_far", "fs_lod_far_water"]
            .iter()
            .any(|&entry| !shader.module.entry_points.iter().any(|e| e.name == entry))
            || crate::app::api::launch_options().contains_key("legacy-lod")
        {
            return;
        }
        let mut make = |label, fragment, write, blend| {
            gpu.create_pipeline(&PipelineDesc {
                label,
                shader,
                vertex: "vs_lod_far",
                fragment: Some(fragment),
                color: Some(Format::Surface),
                depth: Some(DepthState { write, compare: Compare::GreaterEqual }),
                blend,
                cull_back: true,
                topology: Topology::Triangles,
            })
        };
        match make("far lod ground", "fs_lod_far", true, Blend::None) {
            Ok(land) => match make("far lod water", "fs_lod_far_water", false, Blend::Alpha) {
                Ok(water) => self.pipes = Some([land, water]),
                Err(e) => {
                    log::warn!("far LOD water: {e}");
                    gpu.destroy_pipeline(land);
                }
            },
            Err(e) => log::warn!("far LOD ground: {e}"),
        }
    }

    pub fn enabled(&self) -> bool {
        self.pipes.is_some()
    }

    pub fn add(&mut self, gpu: &mut dyn Backend, key: TileKey) {
        let buffer = gpu.create_buffer(TILE_BYTES, usage::STORAGE | usage::INDIRECT, "far LOD tile");
        self.tiles.insert(key, buffer);
        self.build.push((key, buffer));
    }

    pub fn remove(&mut self, key: TileKey) {
        if let Some(old) = self.tiles.remove(&key) {
            self.retired.push(old);
        }
    }

    pub fn invalidate_description(&mut self, gpu: &mut dyn Backend) {
        if let Some(old) = self.description.take() {
            gpu.destroy_buffer(old);
        }
    }

    pub fn collect(&mut self, gpu: &mut dyn Backend) {
        for old in self.retired.drain(..) {
            gpu.destroy_buffer(old);
        }
    }

    pub fn prepare(
        &mut self,
        gpu: &mut dyn Backend,
        early: &mut CommandList,
        slot: usize,
        compute: PipelineId,
        palette: usize,
        description: impl FnOnce() -> Vec<i32>,
    ) {
        self.build
            .retain(|(key, buffer)| self.tiles.get(key) == Some(buffer));
        if self.build.is_empty() {
            return;
        }
        let desc = *self.description.get_or_insert_with(|| {
            let words = description();
            let desc = gpu.create_buffer((words.len() * 4) as u64, usage::STORAGE, "far LOD terrain and colors");
            gpu.write_buffer(desc, 0, bytemuck::cast_slice(&words));
            desc
        });
        let jobs = crate::gpu_layout::frame_buffer(gpu, &mut self.jobs, slot, self.build.len() as u32, 16, usage::STORAGE, "far LOD jobs");
        {
            let words: &mut [[i32; 4]] = bytemuck::cast_slice_mut(gpu.mapped(jobs));
            for (&(key, _), job) in self.build.iter().zip(words) {
                let o = key.origin();
                *job = [o.x, o.y, key.level as i32, palette as i32];
            }
        }
        early.pipeline(compute);
        early.bind(binding::DESC, desc);
        early.bind(binding::JOBS, jobs);
        log::debug!(target: "load", "far lod dispatch {} tiles", self.build.len());
        for (index, (_, buffer)) in self.build.drain(..).enumerate() {
            early.bind(binding::OUT, buffer);
            early.push_constants(&(index as u32));
            early.push(Cmd::Dispatch { x: 1, y: 1, z: 1 });
        }
        early.push(Cmd::Barrier);
    }

    pub fn draw(
        &self,
        list: &mut CommandList,
        draw: &[(TileKey, f32)],
        camera: IVec3,
        water: bool,
        bind: impl FnOnce(&mut CommandList, PipelineId, BufferId),
    ) {
        let Some(pipes) = self.pipes else { return };
        let Some(&buffer) = self.tiles.values().next() else { return };
        use crate::gpu_layout::bind::world as b;
        bind(list, pipes[water as usize], buffer);
        for &(key, fade) in draw {
            let Some(&buffer) = self.tiles.get(&key) else { continue };
            let o = key.origin();
            let cube = IVec3::new(o.x / 16, 0, o.y / 16) - camera;
            list.bind(b::RECORDS, buffer);
            list.push_constants(&[cube.x, cube.y, cube.z, 1 << key.level, fade.to_bits() as i32, 0, 0, 0]);
            list.push(Cmd::DrawIndirect { buffer, offset: u64::from(water) * 16, count: 1 });
        }
    }

    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    pub fn destroy(&mut self, gpu: &mut dyn Backend) {
        self.collect(gpu);
        self.invalidate_description(gpu);
        for (_, buffer) in self.tiles.drain() {
            gpu.destroy_buffer(buffer);
        }
        for (buffer, _) in self.jobs.drain(..) {
            gpu.destroy_buffer(buffer);
        }
        if let Some(pipes) = self.pipes.take() {
            for pipe in pipes {
                gpu.destroy_pipeline(pipe);
            }
        }
    }
}
