use crate::gpu_layout::sun_shadow::{FAR_DEPTH, FAR_HALF, FAR_SIZE, NEAR_DEPTH, NEAR_HALF, NEAR_SIZE};
use glam::{DMat3, DVec3, IVec3, Mat4, Vec3, Vec4};
use weft_render::{
    Backend, Blend, BufferId, Cmd, CommandList, Compare, DepthState, Format, PipelineDesc, PipelineId, Shader, TextureDesc, TextureId,
    Topology, usage,
};

pub(super) const MAP_COUNT: usize = 3;
pub(super) const FAR_SUN_TICKS: f64 = 8.0;

const FAR_MAP_BYTES: u64 = (FAR_SIZE * FAR_SIZE * 4) as u64;
const FAR_OFFSET: u64 = (NEAR_SIZE * NEAR_SIZE * 4) as u64;
const NEAR_ANCHOR_STEP: f64 = 4.0;
const FAR_ANCHOR_STEP: f64 = 16.0;
const PIVOT_REACH: f64 = 64.0;

pub(super) struct SunShadow {
    texture: TextureId,
    far_texture: Option<TextureId>,
    pub depth: BufferId,
    pub cached: bool,
    keys: [Option<SunKey>; MAP_COUNT],
    pivot: Option<DVec3>,
    phase: DVec3,
    far_grids: [Option<(Vec3, DVec3)>; 2],
}

#[derive(Clone, Copy, PartialEq)]
struct SunKey {
    direction: Vec3,
    cell: [i64; 3],
}

#[derive(Clone, Copy)]
pub(super) struct SunView {
    pub matrix: Mat4,
    pub direction: Vec3,
    pub size: Vec3,
    pub range: [IVec3; 2],
    pub grid: [f32; 4],
    key: SunKey,
}

impl SunView {
    pub fn direction(sun: [f32; 4]) -> Option<Vec3> {
        let direction = Vec3::from_array([sun[0], sun[1], sun[2]]);
        (sun[3] > 0.0 && direction.y > 0.04).then_some(direction)
    }

    fn new(camera: DVec3, sun: [f32; 4], basis: DMat3, coordinates: DVec3, far: bool, stable: bool) -> Self {
        let (half, depth, behind, resolution) =
            if far { (FAR_HALF as f32, FAR_DEPTH as f32, 375.0, FAR_SIZE) } else { (NEAR_HALF as f32, NEAR_DEPTH as f32, 40.0, NEAR_SIZE) };
        let [right, up, direction] = [basis.x_axis, basis.y_axis, basis.z_axis].map(|axis| axis.as_vec3());
        let step =
            if stable { if far { FAR_ANCHOR_STEP } else { NEAR_ANCHOR_STEP } } else { 2.0 * f64::from(half) / f64::from(resolution) };
        let cell = (coordinates / step).floor();
        let mut center = (coordinates - cell * step - DVec3::splat(step * 0.5)).as_vec3();
        if !stable {
            center.z = 0.0;
        }
        let size = Vec3::new(half, half, depth * 0.5);
        let center_world = camera + (direction * (depth * 0.5 - behind - center.z) - right * center.x - up * center.y).as_dvec3();
        let extent = right.abs() * size.x + up.abs() * size.y + direction.abs() * size.z;
        // Include quads which extend a little past the cube they are stored in.
        let extent = extent.as_dvec3() + DVec3::splat(2.0);
        let range = [center_world - extent, center_world + extent].map(|p| p.floor().as_ivec3() >> 4);
        let rows = Mat4::from_cols(
            (right / half).extend(center.x / half),
            (up / half).extend(center.y / half),
            (direction / depth).extend((behind + center.z) / depth),
            Vec4::W,
        );
        let phase = camera - camera.floor();
        let fade = ((sun[1] - 0.04) / 0.24).clamp(0.0, 1.0);
        let strength = fade * fade * (3.0 - 2.0 * fade) * (sun[3] / sun[1]).clamp(0.0, 1.0);
        Self {
            matrix: rows.transpose(),
            direction,
            size,
            range,
            grid: phase.as_vec3().extend(strength).to_array(),
            key: SunKey { direction, cell: cell.to_array().map(|value| value as i64) },
        }
    }

    pub fn contains(self, point: Vec3, radius: f32) -> bool {
        let clip = self.matrix.project_point3(point);
        let margin = radius / self.size;
        clip.x.abs() <= 1.0 + margin.x && clip.y.abs() <= 1.0 + margin.y && clip.z >= -margin.z && clip.z <= 1.0 + margin.z
    }
}

impl SunShadow {
    pub fn new(gpu: &mut dyn Backend, cached: bool) -> Self {
        let texture = make_texture(gpu, NEAR_SIZE);
        let size = FAR_OFFSET + if cached { 2 * FAR_MAP_BYTES } else { 0 };
        let depth = gpu.create_buffer(size, usage::STORAGE, "sun shadow samples");
        Self { texture, far_texture: None, depth, cached, keys: [None; MAP_COUNT], pivot: None, phase: DVec3::ZERO, far_grids: [None; 2] }
    }

    pub fn views(&mut self, camera: DVec3, solar: [f32; 4], direction: Vec3, far_directions: [Vec3; 2]) -> (SunView, Option<[SunView; 2]>) {
        let basis = light_basis(direction);
        let to_light = basis.transpose();
        let mut pivot = *self.pivot.get_or_insert_with(|| camera.floor());
        let mut rebase = DVec3::ZERO;
        if (camera - pivot).length_squared() > PIVOT_REACH * PIVOT_REACH {
            let next = camera.floor();
            // Rebase by whole anchor steps so both maps keep the same texel grid.
            rebase = next - pivot;
            self.phase = (self.phase + to_light * rebase).rem_euclid(DVec3::splat(FAR_ANCHOR_STEP));
            self.pivot = Some(next);
            self.keys.fill(None);
            pivot = next;
        }
        let coordinates = to_light * (camera - pivot) + self.phase;
        let near = SunView::new(camera, solar, basis, coordinates, false, self.cached);
        let far = self.cached.then(|| {
            std::array::from_fn(|slot| {
                let direction = far_directions[slot];
                let basis = light_basis(direction);
                let to_light = basis.transpose();
                // An endpoint keeps its own grid phase if the common pivot moves mid-blend.
                let phase = self.far_grids[slot]
                    .filter(|&(old, _)| old == direction)
                    .map_or(self.phase, |(_, phase)| (phase + to_light * rebase).rem_euclid(DVec3::splat(FAR_ANCHOR_STEP)));
                self.far_grids[slot] = Some((direction, phase));
                let coordinates = to_light * (camera - pivot) + phase;
                SunView::new(camera, solar, basis, coordinates, true, true)
            })
        });
        (near, far)
    }

    pub fn needs_map(&self, view: SunView, map: usize) -> bool {
        self.keys[map] != Some(view.key)
    }

    pub fn record(
        &mut self,
        gpu: &mut dyn Backend,
        list: &mut CommandList,
        draws: &mut CommandList,
        view: SunView,
        map: usize,
        reusable: bool,
    ) {
        self.keys[map] = reusable.then_some(view.key);
        let (texture, offset) = if map == 0 {
            (self.texture, 0)
        } else {
            let texture = *self
                .far_texture
                .get_or_insert_with(|| make_texture(gpu, FAR_SIZE));
            (texture, FAR_OFFSET + (map - 1) as u64 * FAR_MAP_BYTES)
        };
        list.push(Cmd::Barrier);
        list.push(Cmd::BeginPass { color: None, depth: Some((texture, Some(0.0))) });
        list.append(draws);
        list.push(Cmd::EndPass);
        list.push(Cmd::CopyDepthToBuffer { depth: texture, buffer: self.depth, offset });
        list.push(Cmd::Barrier);
    }

    pub fn destroy(self, gpu: &mut dyn Backend) {
        gpu.destroy_texture(self.texture);
        if let Some(texture) = self.far_texture {
            gpu.destroy_texture(texture);
        }
        gpu.destroy_buffer(self.depth);
    }
}

fn light_basis(direction: Vec3) -> DMat3 {
    let right = Vec3::Y.cross(direction).normalize_or_zero();
    let right = if right == Vec3::ZERO { Vec3::X } else { right };
    let up = direction.cross(right);
    DMat3::from_cols(right.as_dvec3(), up.as_dvec3(), direction.as_dvec3())
}

fn make_texture(gpu: &mut dyn Backend, size: u32) -> TextureId {
    gpu.create_texture(
        &TextureDesc { width: size, height: size, mips: 1, format: Format::Depth32, render_target: true },
        "sun shadow depth",
    )
}
pub(super) fn uses_depth(shader: &Shader) -> bool {
    shader.module.global_variables.iter().any(|(handle, var)| {
        var.name.as_deref() == Some("sun_depth")
            && shader
                .module
                .entry_points
                .iter()
                .enumerate()
                .any(|(index, _)| !shader.info.get_entry_point(index)[handle].is_empty())
    })
}

pub(super) fn pipeline(
    gpu: &mut dyn Backend,
    shader: &Shader,
    label: &str,
    vertex: &str,
    fragment: Option<&str>,
    cull_back: bool,
) -> Result<PipelineId, String> {
    gpu.create_pipeline(&PipelineDesc {
        label,
        shader,
        vertex,
        fragment,
        color: None,
        depth: Some(DepthState { write: true, compare: Compare::GreaterEqual }),
        blend: Blend::None,
        cull_back,
        topology: Topology::Triangles,
    })
    .map_err(|e| e.0)
}
