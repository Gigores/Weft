//! The stern sweeps a trail in world space. Bow waves and impact sheets use the same strip mesh.

use std::collections::VecDeque;

use glam::DVec3;
use rustc_hash::FxHashMap;
use weft_render::*;

use super::Camera;
use crate::assets::Assets;
use crate::gpu_layout::GpuWake;

const LIFE: f64 = 70.0;
const SPLASH_LIFE: f64 = 13.0;
const IMPACT_GAP: f64 = 2.25;
const MAX_TRAILS: usize = 24;
const CREST_LIP: f64 = 0.65;
const SPRAY_TIMES: [f64; 4] = [0.8, 3.0, 6.0, 9.0];

pub(super) struct Stern {
    pub center: DVec3,
    pub half_width: f64,
    pub clearance: f64,
    pub velocity: DVec3,
}

#[derive(Clone, Copy)]
pub(super) struct Crest {
    pub at: DVec3,
    pub outward: DVec3,
    pub velocity: DVec3,
    pub power: f64,
    pub size: f64,
    pub droplets: u32,
}

impl Crest {
    fn between(&self, other: &Self, t: f64) -> Self {
        Self {
            at: self.at.lerp(other.at, t),
            outward: self.outward.lerp(other.outward, t).normalize_or_zero(),
            velocity: self.velocity.lerp(other.velocity, t),
            power: self.power + (other.power - self.power) * t,
            size: self.size + (other.size - self.size) * t,
            droplets: self.droplets,
        }
    }

    fn profile(&self, now: f64, age: f64, impact: bool) -> impl Fn(f64) -> DVec3 {
        let energy = self.power.sqrt() * self.size;
        let texture = 0.91 + 0.09 * (self.at.dot(DVec3::new(1.5, 0.0, 2.3)) + now * 0.65).sin();
        let (rise, expansion) = if impact {
            let rise = (0.54 + 0.45 * (age * std::f64::consts::PI).sin()) * (1.0 - age).powf(1.4);
            (rise, 0.82 + 1.35 * age)
        } else {
            (1.0, 1.0)
        };
        let scale = self.size.min(1.0);
        let height = (0.18 * energy + 0.025 * self.power * scale).min(2.4) * rise * texture;
        let width = (0.50 * scale + 0.28 * energy).min(3.1) * expansion;
        move |across| {
            let arc = (across / CREST_LIP).min((1.0 - across) / (1.0 - CREST_LIP));
            let arched = arc * arc * (3.0 - 2.0 * arc);
            self.at + self.outward * (across * width) + DVec3::Y * (arched * height)
        }
    }
}

#[derive(Default)]
pub(super) struct SurfaceLine {
    pub stern: Option<Stern>,
    pub bow: Vec<[Crest; 2]>,
    pub impact: Vec<(u16, Crest)>,
}

struct Point {
    stern: Stern,
    born: f64,
    distance: f32,
    joins: bool,
}

struct Impact {
    body: u64,
    liquid: u16,
    height: f64,
    born: f64,
    last_contact: f64,
    outline: Vec<Crest>,
    contour: Vec<[Crest; 2]>,
    spray_step: usize,
}

impl Impact {
    fn rebuild(&mut self, on_water: &impl Fn(DVec3, u16) -> bool) {
        keep_outline(&mut self.outline);
        self.contour.clear();
        let hull = &self.outline;
        if hull.len() < 3 {
            return;
        }
        let normal = |a: Crest, b: Crest| DVec3::Y.cross(b.at - a.at).normalize_or_zero();
        for i in 0..hull.len() {
            let mut a = hull[i];
            let mut b = hull[(i + 1) % hull.len()];
            let outward = normal(a, b);
            let before = normal(hull[(i + hull.len() - 1) % hull.len()], a);
            let after = normal(b, hull[(i + 2) % hull.len()]);
            a.outward = (before + outward).normalize_or_zero();
            b.outward = (outward + after).normalize_or_zero();
            let steps = (a.at.distance(b.at) / 1.5).ceil().clamp(1.0, 16.0) as u32;
            let mut from = a;
            for step in 1..=steps {
                let to = if step == steps { b } else { Crest { outward, ..a.between(&b, f64::from(step) / f64::from(steps)) } };
                if on_water((from.at + to.at) * 0.5, self.liquid) {
                    self.contour.push([from, to]);
                }
                from = to;
            }
        }
    }
}

fn keep_outline(points: &mut Vec<Crest>) {
    points.sort_unstable_by(|a, b| {
        a.at.x
            .total_cmp(&b.at.x)
            .then(a.at.z.total_cmp(&b.at.z))
            .then(b.power.total_cmp(&a.power))
    });
    points.dedup_by(|a, b| a.at.x == b.at.x && a.at.z == b.at.z);
    if points.len() < 3 {
        return;
    }
    let mut hull: Vec<Crest> = Vec::with_capacity(points.len());
    let mut append = |point: Crest, start: usize| {
        while hull.len() >= start + 2 {
            let a = hull[hull.len() - 2].at;
            let b = hull[hull.len() - 1].at;
            if (b - a).cross(point.at - a).y < -1e-5 {
                break;
            }
            hull.pop();
        }
        hull.push(point);
        hull.len()
    };
    let mut lower = 0;
    for &point in points.iter() {
        lower = append(point, 0);
    }
    for &point in points.iter().rev().skip(1) {
        append(point, lower - 1);
    }
    hull.pop();
    *points = hull;
}

pub struct Wakes {
    trails: FxHashMap<u64, VecDeque<Point>>,
    pipe: Option<PipelineId>,
    buffers: Vec<(BufferId, u32)>,
    strips: Vec<GpuWake>,
    bow: Vec<[Crest; 2]>,
    bow_tick: f64,
    impacts: Vec<Impact>,
}

impl Wakes {
    pub fn new(gpu: &mut dyn Backend, assets: &Assets) -> Self {
        let mut this = Self {
            trails: FxHashMap::default(),
            pipe: None,
            buffers: Vec::new(),
            strips: Vec::new(),
            bow: Vec::new(),
            bow_tick: 0.0,
            impacts: Vec::new(),
        };
        this.reload(gpu, assets);
        this
    }

    pub fn reload(&mut self, gpu: &mut dyn Backend, assets: &Assets) {
        let made = crate::packs::compose(assets, "wake.wgsl", &[])
            .map_err(|e| e.0)
            .and_then(|(shader, _)| {
                gpu.create_pipeline(&PipelineDesc {
                    label: "water wakes",
                    shader: &shader,
                    vertex: "vs",
                    fragment: Some("fs"),
                    color: Some(Format::Surface),
                    depth: Some(DepthState { write: false, compare: Compare::GreaterEqual }),
                    blend: Blend::Alpha,
                    cull_back: false,
                    topology: Topology::Triangles,
                })
                .map_err(|e| e.0)
            });
        match made {
            Ok(pipe) => {
                if let Some(previous) = self.pipe.replace(pipe) {
                    gpu.destroy_pipeline(previous);
                }
            }
            Err(error) => log::error!("wake shader: {error}"),
        }
    }

    pub fn destroy(&mut self, gpu: &mut dyn Backend) {
        if let Some(pipe) = self.pipe.take() {
            gpu.destroy_pipeline(pipe);
        }
        for (buffer, _) in self.buffers.drain(..) {
            gpu.destroy_buffer(buffer);
        }
    }

    pub fn clear(&mut self) {
        self.trails.clear();
        self.bow.clear();
        self.impacts.clear();
    }

    pub(super) fn advance(&mut self, now: f64) {
        self.bow.clear();
        self.bow_tick = now;
        self.impacts
            .retain(|i| i.born <= now && now - i.born < SPLASH_LIFE);
        self.trails.retain(|_, trail| {
            while trail
                .front()
                .is_some_and(|p| now - p.born >= LIFE || p.born > now + 1.0)
            {
                trail.pop_front();
            }
            !trail.is_empty()
        });
    }

    pub(super) fn waterline(&mut self, id: u64, line: SurfaceLine, now: f64, on_water: impl Fn(DVec3, u16) -> bool) {
        if let Some(stern) = line.stern {
            self.record(id, stern, now);
        }
        for (liquid, point) in line.impact {
            let index = self.impacts.iter().rposition(|i| {
                i.body == id && i.liquid == liquid && (i.height - point.at.y).abs() < 0.27 && now - i.last_contact <= IMPACT_GAP
            });
            let index = index.unwrap_or_else(|| {
                self.impacts.push(Impact {
                    body: id,
                    liquid,
                    height: point.at.y,
                    born: now,
                    last_contact: now,
                    outline: Vec::new(),
                    contour: Vec::new(),
                    spray_step: 0,
                });
                self.impacts.len() - 1
            });
            let impact = &mut self.impacts[index];
            impact.last_contact = now;
            impact.outline.push(point);
        }
        let mut entering = false;
        for impact in self.impacts.iter_mut().filter(|i| i.body == id) {
            if impact.last_contact == now {
                impact.rebuild(&on_water);
            }
            entering |= now - impact.last_contact <= IMPACT_GAP;
        }
        if !entering {
            self.bow.extend(line.bow);
        }
    }

    pub(super) fn spray(&mut self, now: f64, phase: f64, mut emit: impl FnMut(u32, DVec3, DVec3)) {
        let mut budget = 60;
        let mut shed = |[a, b]: &[Crest; 2], index: usize, age: f64, impact: bool| {
            if budget == 0 {
                return;
            }
            let t = 0.12 + 0.76 * (phase + index as f64 * 0.618).fract();
            let crest = a.between(b, t);
            if crest.power < if impact { 1.8 } else { 2.3 } {
                return;
            }
            let at = crest.profile(now, age, impact)(CREST_LIP);
            let motion = crest.velocity * if impact { 0.17 } else { 0.8 };
            let speed = crest.power.sqrt();
            let vel = motion + crest.outward * (0.7 + speed * 0.75) + DVec3::Y * (0.8 + speed * (0.78 - age * 0.35));
            emit(crest.droplets, at, vel);
            budget -= 1;
        };
        for impact in &mut self.impacts {
            let elapsed = now - impact.born;
            let due = SPRAY_TIMES.partition_point(|&time| time <= elapsed);
            if due <= impact.spray_step {
                continue;
            }
            impact.spray_step = due;
            let age = (elapsed / SPLASH_LIFE).clamp(0.0, 1.0);
            for (i, pair) in impact.contour.iter().enumerate() {
                shed(pair, i, age, true);
            }
        }
        let offset = (now.floor() as usize) % 4;
        for (i, pair) in self.bow.iter().enumerate().skip(offset).step_by(4) {
            shed(pair, i, 0.0, false);
        }
    }

    fn record(&mut self, id: u64, stern: Stern, now: f64) {
        if self.trails.len() >= MAX_TRAILS && !self.trails.contains_key(&id) {
            return;
        }
        let trail = self.trails.entry(id).or_default();
        let mut point = Point { stern, born: now, distance: 0.0, joins: false };
        if let Some(previous) = trail.back() {
            let dt = now - previous.born;
            let shift = point.stern.center - previous.stern.center;
            if dt < 0.75 {
                return;
            }
            let distance = shift.with_y(0.0).length();
            point.distance = previous.distance + distance as f32;
            point.joins = dt <= 12.0 && shift.y.abs() <= 0.27 && distance <= 4.0 && point.stern.velocity.dot(previous.stern.velocity) > 0.0;
            if point.joins {
                // The wet footprint can change abruptly as a hull rolls.
                let predicted = previous.stern.center + point.stern.velocity * (dt * crate::world::TICK);
                point.stern.center = predicted
                    .lerp(point.stern.center, 0.3)
                    .with_y(point.stern.center.y);
                point.stern.half_width = (previous.stern.half_width + point.stern.half_width) * 0.5;
                point.stern.clearance = (previous.stern.clearance + point.stern.clearance) * 0.5;
                let span = (point.stern.center - previous.stern.center)
                    .with_y(0.0)
                    .length();
                if span < 0.12 {
                    return;
                }
                point.distance = previous.distance + span as f32;
            }
        }
        trail.push_back(point);
    }

    pub(super) fn draw(&mut self, gpu: &mut dyn Backend, list: &mut CommandList, slot: usize, camera: &Camera, scene: BufferId, now: f64) {
        let Some(pipe) = self.pipe else { return };
        self.strips.clear();
        for trail in self.trails.values() {
            let Some(last) = trail.back() else { continue };
            let head = last.distance - last.stern.clearance as f32;
            for (i, (from, to)) in trail.iter().zip(trail.iter().skip(1)).enumerate() {
                if !to.joins || from.distance >= head || from.stern.center.distance_squared(camera.pos) > 88.0f64.powi(2) {
                    continue;
                }
                let [left_from, right_from] = section(trail, i, now, camera.pos, head);
                let [left_to, right_to] = section(trail, i + 1, now, camera.pos, head);
                self.strips.push(GpuWake {
                    left_from,
                    right_from,
                    left_to,
                    right_to,
                    course: [from.distance, to.distance, life(from, now) as f32, life(to, now) as f32],
                    style: [0; 4],
                });
            }
        }
        let bow_dt = (now - self.bow_tick).clamp(0.0, 1.25) * crate::world::TICK;
        let active = self.bow.iter().map(|pair| (pair, 0.0, false, bow_dt));
        let impacts = self.impacts.iter().flat_map(|impact| {
            let age = ((now - impact.born) / SPLASH_LIFE).clamp(0.0, 1.0);
            impact
                .contour
                .iter()
                .map(move |pair| (pair, age, true, 0.0))
        });
        for (pair, age, impact, drift) in active.chain(impacts) {
            sheet(&mut self.strips, pair, camera.pos, now, age, impact, drift);
        }
        if self.strips.is_empty() {
            return;
        }
        let n = self.strips.len() as u32;
        let stride = size_of::<GpuWake>() as u64;
        let buf = crate::gpu_layout::frame_buffer(gpu, &mut self.buffers, slot, n, stride, usage::STORAGE, "water wakes");
        gpu.mapped(buf)[..self.strips.len() * stride as usize].copy_from_slice(bytemuck::cast_slice(&self.strips));
        list.pipeline(pipe);
        list.bind(crate::gpu_layout::bind::wake::STRIPS, buf);
        list.bind(crate::gpu_layout::bind::wake::SCENE, scene);
        list.push(Cmd::Draw { vertices: 6, instances: n, first_vertex: 0, first_instance: 0 });
    }
}

fn life(p: &Point, now: f64) -> f64 {
    ((now - p.born) / LIFE).clamp(0.0, 1.0)
}

fn section(trail: &VecDeque<Point>, i: usize, now: f64, camera: DVec3, head: f32) -> [[f32; 4]; 2] {
    let point = &trail[i];
    let center = point.stern.center;
    let heading = point.stern.velocity.normalize_or_zero();
    let before = i
        .checked_sub(1)
        .filter(|_| point.joins)
        .map_or(DVec3::ZERO, |j| (center - trail[j].stern.center).with_y(0.0));
    let after = trail
        .get(i + 1)
        .filter(|p| p.joins)
        .map_or(DVec3::ZERO, |p| (p.stern.center - center).with_y(0.0));
    let tangent = (before + after).try_normalize().unwrap_or(heading);
    let turn = before
        .normalize_or_zero()
        .cross(after.normalize_or_zero())
        .y;
    let inner = (before.length() + after.length()) * 0.41 / turn.abs().max(1e-6);
    let side = DVec3::Y.cross(tangent);
    let age = life(point, now);
    let width = point.stern.half_width * (1.0 + age * 0.16);
    let under_hull = ((head - point.distance) / 0.7).clamp(0.0, 1.0);
    let opacity = (point.stern.velocity.length() * 0.2).clamp(0.1, 1.0) * (1.0 - age).powf(1.35);
    [-1.0, 1.0].map(|sign| {
        // Inside a tight turn the full offset would fold the strip through itself.
        let extent = if sign * turn > 0.0 { width.min(inner) } else { width };
        (center + side * (sign * extent) - camera)
            .as_vec3()
            .extend(opacity as f32 * under_hull)
            .to_array()
    })
}

fn sheet(strips: &mut Vec<GpuWake>, [a, b]: &[Crest; 2], camera: DVec3, now: f64, age: f64, impact: bool, drift: f64) {
    const PROFILE: [f64; 7] = [0.0, 0.19, 0.40, CREST_LIP, 0.77, 0.89, 1.0];
    let vertices = |p: &Crest| {
        let profile = p.profile(now, age, impact);
        let offset = p.velocity * drift - camera;
        let opacity = (0.23 * p.power).clamp(0.0, 0.88) * (1.0 - age).powf(1.6);
        PROFILE.map(|across| {
            (profile(across) + offset)
                .as_vec3()
                .extend(opacity as f32)
                .to_array()
        })
    };
    // Only subdivide where the perimeter bends, so expanding corners follow the changing normal.
    let sections = if a.outward.dot(b.outward) < 0.96 { 3 } else { 1 };
    let mut left = vertices(a);
    let origin = [camera.x.rem_euclid(256.0) as f32, camera.z.rem_euclid(256.0) as f32];
    for section in 1..=sections {
        let right = if section == sections { vertices(b) } else { vertices(&a.between(b, section as f64 / sections as f64)) };
        for i in 0..PROFILE.len() - 1 {
            strips.push(GpuWake {
                left_from: left[i],
                right_from: right[i],
                left_to: left[i + 1],
                right_to: right[i + 1],
                course: [origin[0], origin[1], PROFILE[i] as f32, PROFILE[i + 1] as f32],
                style: [1, (age as f32).to_bits(), 0, 0],
            });
        }
        left = right;
    }
}
