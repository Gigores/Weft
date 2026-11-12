//! Base's worldgen as data: biomes, the overworld's terrain, ores, trees (a scatter the engine
//! places in every dimension whose biomes ask for them) and plants (a decoration).

use weft_sdk::*;

use crate::blocks::*;
use crate::tree_kinds::{TREE_KINDS, TreeKinds, register_tree, tree_place};

use weft_sdk::noise::*;
use weft_sdk::terrain::*;

use std::collections::BTreeMap;
use std::collections::BTreeSet;

pub const SEA: i32 = 63;

static PLANT_SOIL: Tag = Tag::named("plant_soil");
static CACTUS_SOIL: Tag = Tag::named("cactus_soil");
static SUGAR_CANE_SOIL: Tag = Tag::named("sugar_cane_soil");
/// What a biome grows, as data any mod can set on any biome: trees per 5x5 cell (0..1), share of
/// grass on plantable soil, which trees and flowers (block lists the engine checks), and tags `frozen` (nothing grows, set by
/// `.frozen(..)`), `dry` (cacti).
pub static TREES: Component<f32> = Component::named("trees");
pub static GRASS_COVER: Component<f32> = Component::named("grass");
pub static TALL_GRASS_COVER: Component<f32> = Component::named("tall_grass");
pub static BOULDER_COVER: Component<f32> = Component::named("boulder");
pub static RASPBERRY_COVER: Component<f32> = Component::named("raspberry");
pub static SNOWBERRY_COVER: Component<f32> = Component::named("snowberry");
/// Flowers a biome grows, picked evenly (`"base:dandelion base:rose"` where unset).
pub static FLOWERS: Component<BlockList> = Component::named("flowers");
/// On a log, the tree it grows into: `round <leaves>` (like oaks, 4-6 high) or `cone <leaves>`
/// (like spruces, 6-9 high).
pub static TREE_LEAVES: Component<BlockRef> = Component::named("tree_leaves");
/// Logs with this tag grow into cones (spruces), the others round (oaks, birches).
pub static CONE_TREE: Tag = Tag::named("cone_tree");
static FROZEN: Tag = Tag::named("frozen");
static DRY: Tag = Tag::named("dry");

pub static PLAINS: Biome = Biome::named("plains");
pub static FOREST: Biome = Biome::named("forest");
pub static BIRCH_FOREST: Biome = Biome::named("birch_forest");
pub static TAIGA: Biome = Biome::named("taiga");
pub static SNOWY: Biome = Biome::named("snowy_plains");
pub static DESERT: Biome = Biome::named("desert");
pub static MOUNTAINS: Biome = Biome::named("mountains");
pub static OCEAN: Biome = Biome::named("ocean");
pub static BEACH: Biome = Biome::named("beach");
pub static DEEP_CAVES: Biome = Biome::named("deep_caves");

fn place_trunk(pos: (i32, i32, i32), rng: &mut Rng, put: &mut dyn FnMut(i32, i32, i32, u32, bool), height: u16, log_block: &Block, allow_branches: bool) {
    let (x, y, z) = pos;
    let top = y + height as i32;
    let log = log_block.id();
    for y in y..top {
        put(x, y, z, log, true);
        if !allow_branches { continue }
        let left = top - y;
        if left < 5 && left > 1 && (rng.next() % (left as u64 + 8u64)) <= 2 {
            let roll = rng.float();
            if roll < 0.25 {
                put(x + 1, y, z, log_block.with(&[("axis", "x")]), true);
            } else if roll < 0.5 {
                put(x - 1, y, z, log_block.with(&[("axis", "x")]), true);
            } else if roll < 0.75 {
                put(x, y, z + 1, log_block.with(&[("axis", "z")]), true);
            } else {
                put(x, y, z - 1, log_block.with(&[("axis", "z")]), true);
            }
        }
    }
    let mut grow_root = |dx: i32, dz: i32, tall: bool| {
        let x = x + dx;
        let z = z + dz;
        if tall {
            put(x, y + 1, z, log, true);
        }
        put(x, y, z, log, true);
        put(x, y - 1, z, log, true);
    };
    let root_positions = [(1, 0), (-1, 0), (0, 1), (0, -1)];
    let mut root_count = 0;
    for dpos in root_positions {
        if rng.float() < 0.666 {
            grow_root(dpos.0, dpos.1, rng.float() < 0.5);
            root_count += 1;
        }
    }
    if root_count == 0 {
        let root_pos = root_positions[rng.next() as usize % root_positions.len()];
        grow_root(root_pos.0, root_pos.1, rng.float() < 0.5);
    }
    put(x, y - 1, z, log, true);
    put(x, y - 2, z, DIRT.id(), true);
}
fn place_cone_foliage(pos: (i32, i32, i32), put: &mut dyn FnMut(i32, i32, i32, u32, bool), height: u16, leaves: u32) {
    let (x, y, z) = pos;
    for dy in 2..=(height as i32) + 1 {
        let y = y + dy;
        let r = if dy == (height as i32) + 1 { 0 } else { 1 + (((height as i32) + 1 - dy) % 3).min(2) };
        for dz in -r..=r {
            for dx in -r..=r {
                if dx * dx + dz * dz <= r * r + 1 {
                    put(x + dx, y, z + dz, leaves, false);
                }
            }
        }
    }
}
fn place_oak(pos: (i32, i32, i32), rng: &mut Rng, put: &mut dyn FnMut(i32, i32, i32, u32, bool)) {
    let height = 6u16 + (rng.next() as u16) % 6u16;
    place_trunk(pos, rng, put, height, &OAK_LOG, true);
}
fn place_birch(pos: (i32, i32, i32), rng: &mut Rng, put: &mut dyn FnMut(i32, i32, i32, u32, bool)) {
    let height = 6u16 + (rng.next() as u16) % 4u16;
    place_trunk(pos, rng, put, height, &BIRCH_LOG, true);
}
fn place_spruce(pos: (i32, i32, i32), rng: &mut Rng, put: &mut dyn FnMut(i32, i32, i32, u32, bool)) {
    let height = 6u16 + (rng.next() as u16) % 4u16;
    place_trunk(pos, rng, put, height, &SPRUCE_LOG, false);
    place_cone_foliage(pos, put, height, SPRUCE_LEAVES.id());
}

/// Whether the ground within `r` blocks of a start is flat enough for it.
fn gentle(p: &Plan, r: i32, max_range: i32) -> bool {
    let at = p.pos();

    let mut lo = i32::MAX;
    let mut hi = i32::MIN;

    for dz in -r..=r {
        for dx in -r..=r {
            let h = p.ground(at.x + dx, at.z + dz).height;
            if h == i32::MIN {
                return false;
            }
            lo = lo.min(h);
            hi = hi.max(h);
        }
    }

    hi - lo <= max_range
}

pub fn register(r: &mut Registry) {
    // Tags say what a biome is (spawning, decoration and mods ask these, not names).
    let (grass, foliage, water) = (&tints::GRASS, &tints::FOLIAGE, &tints::WATER);
    register_tree("base:oak", place_oak);
    register_tree("base:birch", place_birch);
    register_tree("base:spruce", place_spruce);
    r.biome(&PLAINS)
        .surface("grass", "dirt", 3)
        .tag("animals")
        .tag("spawn_biome")
        .set(&TREES, 0.025)
        .set(&GRASS_COVER, 0.7)
        .set(&TALL_GRASS_COVER, 0.2)
        .set(&BOULDER_COVER, 0.03)
        .set(&RASPBERRY_COVER, 0.02)
        .set(&SNOWBERRY_COVER, 0.01);
    r.biome(&FOREST)
        .humidity(0.25..)
        .surface("grass", "dirt", 3)
        .tint(grass, 0x79c05a)
        .tint(foliage, 0x59ae30)
        .tag("forest")
        .tag("animals")
        .set(&TREES, 0.4)
        .set(&GRASS_COVER, 0.12)
        .set(&TREE_KINDS, TreeKinds::from([("base:oak", 4), ("base:birch", 1)]));
    r.biome(&BIRCH_FOREST)
        .temperature(..0.1)
        .humidity(0.05..)
        .surface("grass", "dirt", 3)
        .tint(grass, 0x88bb67)
        .tint(foliage, 0x6ba941)
        .tag("forest")
        .tag("animals")
        .set(&TREES, 0.45)
        .set(&GRASS_COVER, 0.12)
        .set(&TREE_KINDS, TreeKinds::from([("base:birch", 1)]));
    r.biome(&TAIGA)
        .temperature(..-0.15)
        .surface("grass", "dirt", 3)
        .tint(grass, 0x86b783)
        .tint(foliage, 0x68a464)
        .tint(water, 0x3d57d6)
        .tag("forest")
        .tag("cold")
        .tag("animals")
        .set(&TREES, 0.45)
        .set(&GRASS_COVER, 0.08)
        .set(&TREE_KINDS, TreeKinds::from([("base:spruce", 1)]));
    r.biome(&SNOWY)
        .snows()
        .temperature(..-0.25)
        .surface("snowy_grass", "dirt", 3)
        .frozen("ice")
        .tint(grass, 0x80b497)
        .tint(foliage, 0x60a17b)
        .tint(water, 0x3938c9)
        .tag("cold")
        .tag("animals")
        .set(&TREES, 0.03)
        .set(&TREE_KINDS, TreeKinds::from([("base:spruce", 1)]));
    r.biome(&DESERT)
        .temperature(0.25..)
        .humidity(..0.1)
        .surface("sand", "sand", 3)
        .under("sandstone", 1)
        .tint(grass, 0xbfb755)
        .tint(foliage, 0xaea42a)
        .tag("dry")
        .tag("hot");
    r.biome(&MOUNTAINS)
        .surface("grass", "dirt", 3)
        .bare_above(130)
        .tint(grass, 0x8ab689)
        .tint(foliage, 0x6da36b)
        .tag("mountain")
        .tag("animals")
        .set(&TREES, 0.06)
        .set(&GRASS_COVER, 0.05)
        .set(&TREE_KINDS, TreeKinds::from([("base:spruce", 1)]));
    r.biome(&OCEAN)
        .surface("sand", "sand", 3)
        .deep_top("gravel", SEA - 8)
        .tag("ocean");
    r.biome(&BEACH).surface("sand", "sand", 3).tag("beach");
    // Underground (3D): whole cubes deep under the surface; spiders like it there.
    r.biome(&DEEP_CAVES).tag("cave").tag("dark");

    // The last: share left out where it would touch a cave, vanilla's.
    for (ore, max_y, veins, size, exposed) in [
        (&COAL_ORE, 128, 8, 12, 0.0),
        (&IRON_ORE, 64, 6, 8, 0.0),
        (&GOLD_ORE, 32, 2, 7, 0.5),
        (&crate::redstone::REDSTONE_ORE, 16, 4, 8, 0.0),
        (&DIAMOND_ORE, 16, 1, 6, 0.5),
    ] {
        r.ore(ore)
            .y(i32::MIN, max_y)
            .per_cube(veins)
            .vein_size(size)
            .replaces("#stone_ore_replaceables")
            .exposed(exposed);
    }

    // The overworld's shape. Other mods add to it by these names (`base:desert`, `base:mountains`).
    r.terrain(&crate::OVERWORLD)
        .stone("stone")
        .base_height(SEA as f64 - 8.0)
        .sea(SEA, "water")
        .field("temperature", Expr::fbm(0x22, 2200.0, 3) * Expr::value(1.4))
        .field("humidity", Expr::fbm(0x33, 1600.0, 3) * Expr::value(1.4))
        .landform(preset::continents(18.0))
        // Biomes shape the land through their climate, not their borders, so nothing seams:
        // the cold flattens hills into tundra, deserts get dunes.
        .landform(preset::hills(16.0, 110.0).mask(Expr::temperature().ramp(-0.35, -0.6).not()))
        .landform(
            preset::dunes(4.0, 26.0).mask(
                Expr::temperature()
                    .ramp(0.25, 0.45)
                    .and(Expr::humidity().ramp(0.1, -0.05)),
            ),
        )
        // How mountainous a place is: the mountains' strength, and where the mountains biome goes.
        .field(
            "base:mountains",
            Expr::fbm(0x44, 700.0, 3)
                .ramp(-0.1, 0.3)
                .and(Expr::continent().ramp(0.0, 0.2)),
        )
        .landform(
            preset::mountains(150.0, 260.0)
                .mask(Expr::field("base:mountains"))
                .named("mountains"),
        )
        // Biomes bring their climate and surface; rules only add where else they must hold.
        .rule(BiomeRule::new("ocean").height(..=SEA - 2))
        .rule(
            BiomeRule::new("snowy_plains")
                .named("snowy_coast")
                .height(..=SEA + 1)
                .continent(..0.1),
        )
        .rule(BiomeRule::new("beach").height(..=SEA + 1).continent(..0.3))
        .rule(BiomeRule::new("mountains").field("base:mountains", 0.6..))
        .rule(BiomeRule::new("snowy_plains"))
        .rule(BiomeRule::new("taiga"))
        .rule(BiomeRule::new("desert"))
        .rule(BiomeRule::new("forest"))
        .rule(BiomeRule::new("birch_forest"))
        .rule(BiomeRule::new("plains"))
        .cave_biome("deep_caves", 24, 96.0, 0.25)
        .caves(Caves::new("lava", i32::MIN, 11));

    // Trees wherever a biome asks for them, in any dimension.
    r.scatter(&TREE_SITES)
        .spacing(5)
        .reach(3)
        .density(&TREES)
        .ground("#plant_soil")
        .plan(|s| {
            let BlockPos { x, y, z } = s.pos();
            let kinds = TREE_KINDS.on_biome(s.biome());
            let mut rng = Rng(s.random());
            // Biomes that ask for trees without naming kinds grow oaks, as they always did.
            let name = match &kinds {
                Some(kinds) => kinds.pick(rng.next()),
                None => Some("base:oak"),
            };
            let Some(name) = name else { return };
            let Some(place) = tree_place(name) else { return };

            let mut put = |x: i32, y: i32, z: i32, state: u32, force: bool| {
                if force {
                    s.set([x, y, z], state);
                } else {
                    s.put([x, y, z], state);
                }
            };

            place((x, y, z), &mut rng, &mut put);
        });
    r.scatter(&BOULDER_SITES)
        .spacing(5)
        .reach(3)
        .density(&BOULDER_COVER)
        .plan(|s| {
            let BlockPos { x, y, z } = s.pos();
            if y < 70 || !gentle(s, 2, 2) {
                return;
            }
            let mut rng = Rng(s.random());
            let mut cells = Vec::new();
            place_boulder(s, &mut rng, x, y, z, &mut cells);
            if rng.float() < 0.5 {
                let dx = x + (rng.float() * 2.0) as i32;
                let dy = y + (rng.float() * 2.0 - 0.5) as i32;
                let dz = z + (rng.float() * 2.0) as i32;
                place_boulder(s, &mut rng, dx, dy, dz, &mut cells);
            }
            let blocks: BTreeSet<[i32; 3]> = cells.into_iter().collect();
            let mut carpets: BTreeMap<[i32; 3], u8> = BTreeMap::new();
            for b in &blocks {
                for &d in &Dir::ALL {
                    if d == Dir::Down {
                        continue;
                    }
                    let o = d.offset();
                    let n = [b[0] + o.x, b[1] + o.y, b[2] + o.z];
                    if blocks.contains(&n) {
                        continue;
                    }
                    let face = d.opposite().face() as u8;
                    *carpets.entry(n).or_insert(0) |= 1u8 << face;
                }
            }
            for (p, mask) in carpets {
                if rng.float() > 0.2 {
                    continue;
                }
                let props: Vec<(&str, &str)> = Dir::ALL
                    .iter()
                    .map(|&d| {
                        let on = mask & (1u8 << d.face() as u8) != 0;
                        (d.name(), if on { "true" } else { "false" })
                    })
                    .collect();
                let state = MOSS.with(&props);
                if state != u32::MAX {
                    s.put(p, state);
                }
            }
        });
    r.scatter(&RASPBERRY_SITES)
        .spacing(5)
        .reach(3)
        .density(&RASPBERRY_COVER)
        .plan(|s| {
            place_bush(
                s,
                |r| if r.float() < 0.5 { RASPBERRY_LEAVES.id() } else { RASPBERRY_LEAVES.with(&[("berried", "true")]) },
                BUSH_STEM.id(),
            )
        });
    r.scatter(&SNOWBERRY_SITES)
        .spacing(5)
        .reach(3)
        .density(&SNOWBERRY_COVER)
        .plan(|s| {
            place_bush(
                s,
                |r| if r.float() < 0.5 { SNOWBERRY_LEAVES.id() } else { SNOWBERRY_LEAVES.with(&[("berried", "true")]) },
                BUSH_STEM.id(),
            )
        });
    r.decoration(decorate);
}

fn place_boulder(s: &mut Plan, rng: &mut Rng, ox: i32, oy: i32, oz: i32, cells: &mut Vec<[i32; 3]>) {
    for (dx, dy, dz) in sphere(1.5) {
        let pos = [ox + dx, oy + dy, oz + dz];
        let block = if rng.float() < 0.5 { COBBLESTONE.id() } else { MOSSY_COBBLESTONE.id() };
        s.put(pos, block);
        cells.push(pos);
    }
}

static SNOWBERRY_SITES: Feature = Feature::named("snowberries");

pub fn place_bush(s: &mut Plan, leaf: impl Fn(&mut Rng) -> u32, stem: u32) {
    let BlockPos { x, y, z } = s.pos();
    let mut rng = Rng(s.random());
    let bush_size = {
        let mut result = rng.float() + 0.7;
        if result < 1.0 {
            result = 1.0;
        }
        result
    };
    s.put([x, y, z], stem);
    let mut place_ball = |mut rng: &mut Rng, ox, oy, oz, size| {
        for (dx, dy, dz) in sphere(size) {
            s.put([ox + dx, oy + dy, oz + dz], leaf(&mut rng));
        }
    };
    place_ball(&mut rng, x, y, z, bush_size);
    if bush_size < 1.5 {
        return;
    }
    let dx = x + (rng.float() * 2.0) as i32;
    let dz = z + (rng.float() * 2.0) as i32;
    place_ball(&mut rng, dx, y, dz, bush_size - 0.5);
}

static RASPBERRY_SITES: Feature = Feature::named("raspberries");

pub fn sphere(radius: f32) -> impl Iterator<Item = (i32, i32, i32)> {
    let extent = radius.floor() as i32;
    let limit = radius as f64 * radius as f64;
    (-extent..=extent).flat_map(move |x| {
        let x2 = x as f64 * x as f64;
        (-extent..=extent).flat_map(move |y| {
            let xy = x2 + y as f64 * y as f64;
            (-extent..=extent)
                .filter(move |&z| xy + z as f64 * z as f64 <= limit)
                .map(move |z| (x, y, z))
        })
    })
}

static BOULDER_SITES: Feature = Feature::named("boulders");

static TREE_SITES: Feature = Feature::named("trees");

struct TreeAt {
    x: i32,
    y: i32,
    z: i32,
    log: u32,
    leaves: u32,
    cone: bool,
    height: i32,
}

/// A tree growing from `log` (its `tree` component), `h` deciding the height.
fn tree_at(log: u32, x: i32, y: i32, z: i32, h: u64) -> Option<TreeAt> {
    let leaves = TREE_LEAVES.on_block(log)?.0;
    let cone = CONE_TREE.on_block(log);
    let height = if cone { 6 + (h % 4) as i32 } else { 6 + (h % 3) as i32 };
    Some(TreeAt { x, y, z, log, leaves, cone, height })
}

/// Calls `put(x, y, z, state, force)` for every block of the tree.
fn tree_blocks(t: &TreeAt, rng: &mut Rng, mut put: impl FnMut(i32, i32, i32, u32, bool)) {
    let (log, leaves) = (t.log, t.leaves);
    let top = t.y + t.height;
    match t.cone {
        true => {
            for dy in 2..=t.height + 1 {
                let y = t.y + dy;
                let r = if dy == t.height + 1 { 0 } else { 1 + ((t.height + 1 - dy) % 3).min(2) };
                for dz in -r..=r {
                    for dx in -r..=r {
                        if dx * dx + dz * dz <= r * r + 1 {
                            put(t.x + dx, y, t.z + dz, leaves, false);
                        }
                    }
                }
            }
        }
        false => {
            for y in top - 3..=top {
                let r: i32 = if y >= top - 1 { 1 } else { 2 };
                for dz in -r..=r {
                    for dx in -r..=r {
                        let corner = dx.abs() == r && dz.abs() == r;
                        if corner && (y == top || hash(0, (t.x + dx) as i64, y as i64, (t.z + dz) as i64) & 1 == 0) {
                            continue;
                        }
                        put(t.x + dx, y, t.z + dz, leaves, false);
                    }
                }
            }
        }
    }
    for y in t.y..top {
        put(t.x, y, t.z, log, true);
        let left = top - y;
        if rng.next() % (left as u64) <= 2 {
            if rng.float() < 0.5 {
                put(t.x, y, t.z, log, true);
            } else {
                put(t.x, y, t.z, log, true);
            }
        }
    }
    let mut grow_root = |dx: i32, dz: i32, tall: bool| {
        let x = t.x + dx;
        let z = t.z + dz;
        if tall {
            put(x, t.y + 1, z, log, true);
        }
        put(x, t.y, z, log, true);
        put(x, t.y - 1, z, log, true);
    };
    let root_positions = [(1, 0), (-1, 0), (0, 1), (0, -1)];
    let mut root_count = 0;
    for dpos in root_positions {
        if rng.float() < 0.666 {
            grow_root(dpos.0, dpos.1, rng.float() < 0.5);
            root_count += 1;
        }
    }
    if root_count == 0 {
        let root_pos = root_positions[rng.next() as usize % root_positions.len()];
        grow_root(root_pos.0, root_pos.1, rng.float() < 0.5);
    }
    put(t.x, t.y - 1, t.z, log, true);
    put(t.x, t.y - 2, t.z, DIRT.id(), true);
}

/// Places a tree into a live world (saplings). Checks for room first.
pub fn grow_tree(w: World, p: [i32; 3]) {
    let Some(t) = tree_at(OAK_LOG.id(), p[0], p[1], p[2], w.random()) else { return };
    for y in t.y + 1..t.y + t.height {
        if w.block([t.x, y, t.z]) != 0 {
            return;
        }
    }
    let mut rng = Rng((p[0] ^ p[1] ^ p[2]) as u64);
    tree_blocks(&t, &mut rng, |x, y, z, s, overwrite| {
        let cur = w.block([x, y, z]);
        if overwrite || cur == 0 || TALL_GRASS.is(cur) || TALL_TALL_GRASS.is(cur) {
            w.set_block([x, y, z], s);
        }
    });
}
static COVERED: Block = Block::named("core:covered");

/// Plants and Clay after the terrain and trees filled the cube.
fn decorate(g: &mut Gen) {
    let place_grass = |g: &mut Gen, x, y, z| g.set(x, y, z, TALL_GRASS.id());
    let place_tall_grass = |g: &mut Gen, x, y, z| {
        let upper = COVERED.with(&[("to", "down")]);
        if y + 1 < 16 && g.get(x, y + 1, z) == 0 && upper != u32::MAX {
            g.set(x, y, z, TALL_TALL_GRASS.id());
            g.set(x, y + 1, z, upper);
        } else {
            g.set(x, y, z, TALL_GRASS.id());
        }
    };
    let Some(&cols) = g.columns() else { return };
    let seed = g.seed();
    let BlockPos { x: ox, y: oy, z: oz } = g.origin();
    let max_h = cols.iter().map(|c| c.height).max().unwrap_or(0);
    let min_h = cols.iter().map(|c| c.height).min().unwrap_or(0);

    // Surface decoration only near the surface.
    if oy > max_h + 16 || oy + 15 < min_h - 1 {
        return;
    }
    // Plant decorations
    for z in 0..16usize {
        for x in 0..16usize {
            let col = &cols[z * 16 + x];
            let y = col.height + 1 - oy;
            if !(0..16).contains(&y) || col.height < SEA {
                continue;
            }
            let y = y as usize;
            if g.get(x, y, z) != 0 {
                continue;
            }
            let (wx, wz) = (ox + x as i32, oz + z as i32);
            let h = hash(seed ^ 0xF10, wx as i64, 0, wz as i64);
            let roll = unit(h);
            // Plants go where they could be planted (soil tags); the biome decides how many.
            let b = col.biome;
            if FROZEN.on_biome(b) {
                continue;
            }
            let tall_grass_chance = TALL_GRASS_COVER.on_biome(b).unwrap_or(0.0) as f64;
            let grass_chance = GRASS_COVER.on_biome(b).unwrap_or(0.05) as f64;
            // What is under the lowest layer takes a look at the terrain: only if anything would grow.
            if y == 0 && roll >= grass_chance.max(0.08) && roll >= tall_grass_chance.max(0.08) {
                continue;
            }
            let below = if y == 0 { g.surface_block(wx, wz) } else { g.get(x, y - 1, z) };
            if PLANT_SOIL.on_block(below) {
                // roll ∈ [0; 0.012)
                if roll < 0.012 {
                    let flower = FLOWERS
                        .on_biome(b)
                        .map_or_else(|| [DANDELION.id(), ROSE.id()][(h >> 32) as usize % 2], |l| l.pick(h >> 32).unwrap_or(0));
                    if flower != 0 {
                        g.set(x, y, z, flower);
                    }
                // roll ∈ [0.012; tall_grass_chance)
                } else if roll < tall_grass_chance {
                    let wave = fixed::fbm(fixed::seed32(seed, 0x7A11), wx * 16, wz * 16, 48 * 16, 3);
                    if wave > 0 {
                        place_tall_grass(g, x, y, z);
                    }
                // roll ∈ [tall_grass_chance; grass_chance)
                } else if roll < grass_chance {
                    place_grass(g, x, y, z);
                }
            } else if CACTUS_SOIL.on_block(below) && DRY.on_biome(b) && roll < 0.006 && y + 2 < 16 {
                let n = 1 + (h >> 40) % 3;
                for k in 0..n as usize {
                    g.set(x, y + k, z, CACTUS.id());
                }
            } else if SUGAR_CANE_SOIL.on_block(below)
                && col.height == SEA
                && roll < 0.08
                && y + 2 < 16
                && [(1, 0), (-1, 0), (0, 1), (0, -1)]
                    .iter()
                    .any(|&(dx, dz)| g.column(wx + dx, wz + dz).height < SEA)
            {
                let n = 1 + (h >> 40) % 3;
                for k in 0..n as usize {
                    g.set(x, y + k, z, SUGAR_CANE.id());
                }
            }
        }
    }
    // Clay patches in shallow water (rivers, lakes, ocean shores), replacing the top blocks in
    // bodies of water
    for z in 0..16usize {
        for x in 0..16usize {
            let col = &cols[z * 16 + x];
            // Make sure we only generate clay patches in shallower waters
            if col.height >= SEA || col.height < SEA - 6 {
                continue;
            }
            let y = col.height - oy;
            if !(0..16).contains(&y) {
                continue;
            }
            let y = y as usize;
            let (wx, wz) = (ox + x as i32, oz + z as i32);
            // Clay patch size based on noise scale
            let clay = fixed::fbm(fixed::seed32(seed, 0xC1A7), wx * 16, wz * 16, 18 * 16, 3);
            // Threshold for clay generation
            if clay > fixed::ONE / 4 {
                let surface = g.get(x, y, z);
                if SAND.is(surface) || GRAVEL.is(surface) {
                    g.set(x, y, z, CLAY.id());
                }
            }
        }
    }
}

/// The test dimension: flat, bounded in height, nothing else.
pub fn flat(g: &mut Gen) {
    let BlockPos { y: oy, .. } = g.origin();
    for y in 0..16usize {
        let wy = oy + y as i32;
        let s = match wy {
            ..=-1 => STONE.id(),
            0..=2 => DIRT.id(),
            3 => GRASS.id(),
            _ => 0,
        };
        if s != 0 {
            for z in 0..16 {
                for x in 0..16 {
                    g.set(x, y, z, s);
                }
            }
        }
    }
}
