//! Mobs: zombies and spiders at night or in the dark, pigs and cows on grass by day.

use weft_sdk::ai;
use weft_sdk::*;

use base_api::entities::*;
use base_api::items::{ARROW as ARROW_ITEM, BOW, EGG};
/// Ticks of whatever a mob counts: a skeleton's reload, a chicken's next egg.
static FUSE: Component<u32> = Component::named("fuse");

use base_api::entities::COW;

pub fn register(r: &mut Registry) {
    // The engine's player kind, drawn as the same humanoid as the zombie.
    r.set_model(&PLAYER, humanoid(false));
    r.edit(&PLAYER).hitboxes(Hitboxes::humanoid());
    r.entity(&ZOMBIE)
        .hitboxes(Hitboxes::humanoid().reaching())
        .tag("hostile")
        // Undead, as vanilla's: poison and regeneration take no hold.
        .tag("immune_effect/base:poison")
        .tag("immune_effect/base:regeneration")
        .equipment()
        .sound("ambient")
        .sound("hurt")
        .sound("death")
        .size(0.6, 1.95)
        .max_health(20.0)
        .walk_speed(2.6)
        .hostile()
        .burns_in_sun()
        .attack_damage(3.0)
        .reach(1.3)
        .brain(hostile(ai::Brain::new().aggressive(35.0)));
    r.entity(&SPIDER)
        .hitboxes(spider_parts())
        .tag("hostile")
        .sound("ambient")
        .sound("hurt")
        .sound("death")
        .size(1.3, 0.9)
        .max_health(16.0)
        .walk_speed(3.6)
        .hostile()
        .attack_damage(2.0)
        // Vanilla's reach goes by a mob's width: a body this wide stops further off.
        .reach(2.4)
        // In daylight it only fights back.
        .brain(hostile(
            ai::Brain::new()
                .sight_when(35.0, |w, st| !in_daylight(w, st))
                .neutral(),
        ));
    r.entity_component(&FUSE, 0u32, 0);
    r.entity(&SKELETON)
        .hitboxes(Hitboxes::humanoid().slim(2.0).reaching())
        .tag("hostile")
        .tag("immune_effect/base:poison")
        .tag("immune_effect/base:regeneration")
        .equipment()
        .size(0.6, 1.99)
        .max_health(20.0)
        .walk_speed(2.5)
        .hostile()
        .burns_in_sun()
        .brain(skeleton());
    r.on_event(&actions::SPAWN, |batch| {
        for spawned in batch.iter() {
            if spawned.kind == SKELETON.id() {
                batch
                    .world()
                    .inventory(spawned.entity)
                    .set(0, ItemStack::new(BOW.id(), 1));
            }
        }
    });
    // Drawn as long as the use button is held, shot when it is let go.
    r.item(&BOW)
        .durability(384)
        .handheld()
        .use_until_released()
        .on_release(shoot_bow)
        .tab(&crate::tabs::COMBAT);
    // An arrow in the head hurts half as much again, a bow's critical hit: `part` is the part
    // of the hitbox it went into.
    r.modify(&chain::PROJECTILE_DAMAGE, &phase::MULTIPLY, |m| {
        let q = *m.query();
        let arrow = m
            .world()
            .entity(q.entity)
            .is_some_and(|e| e.kind == ARROW.id());
        if arrow && q.part == hitbox_part::HEAD {
            m.value *= 2.0;
        }
    });
    // Drawing it narrows the view, more the further it is drawn (vanilla's 15% at full draw).
    r.modify(&attribute::FOV, &phase::MULTIPLY, |m| {
        let Some(s) = m.world().entity(m.query().entity) else { return };
        if s.using == BOW.id() {
            let drawn = (s.using_ticks as f64 / 20.0).min(1.0);
            m.value *= 1.0 - drawn * drawn * 0.15;
        }
    });
    r.shaped(&crate::recipes::CRAFTING, "bow", 1, &[" s#", "s #", " s#"], &[('s', "#stick"), ('#', "#string")]);
    // Vanilla's arrow: 0.05 a tick down, stuck a minute where it hits a block.
    r.entity(&ARROW)
        .no_egg()
        .size(0.5, 0.5)
        .unpushable()
        .max_health(1.0)
        .walk_speed(0.0)
        .attribute(&attribute::GRAVITY, 0.625)
        .projectile(Projectile::new().sticks(1200));
    r.entity(&SHEEP)
        .hitboxes(sheep_parts())
        .tag("livestock")
        .size(0.9, 1.3)
        .max_health(8.0)
        .walk_speed(2.3)
        .passive()
        .brain(passive());
    r.entity(&CHICKEN)
        .hitboxes(chicken_parts())
        .tag("livestock")
        .size(0.4, 0.7)
        .max_health(4.0)
        .walk_speed(2.5)
        .passive()
        .attach(&FUSE)
        .brain(passive_chicken());
    r.entity(&PIG)
        .hitboxes(pig_parts())
        .tag("livestock")
        .sound("ambient")
        .sound("hurt")
        .sound("death")
        .size(0.9, 0.9)
        .max_health(10.0)
        .walk_speed(2.5)
        .passive()
        .brain(passive());
    r.entity(&COW)
        .hitboxes(Hitboxes::quadruped())
        .tag("livestock")
        .sound("ambient")
        .sound("hurt")
        .sound("death")
        .size(0.9, 1.3)
        .max_health(10.0)
        .walk_speed(2.2)
        .passive()
        .brain(passive());
    // Vanilla amounts; meat comes out cooked when the animal burns.
    r.loot(&ZOMBIE)
        .pool(Pool::new().items("rotten_flesh", 0, 2));
    r.loot(&SPIDER)
        .pool(Pool::new().items("string", 0, 2))
        .pool(
            Pool::new()
                .when(Cond::killed_by_player())
                .when(Cond::chance(0.33))
                .item("spider_eye"),
        );
    r.loot(&SKELETON)
        .pool(Pool::new().items("arrow", 0, 2))
        .pool(Pool::new().items("bone", 0, 2));
    r.loot(&SHEEP).pool(Pool::new().item("wool")).pool(
        Pool::new().entry(
            Entry::item("mutton")
                .count(1, 2)
                .smelted(&crate::recipes::SMELTING),
        ),
    );
    r.loot(&CHICKEN)
        .pool(Pool::new().items("feather", 0, 2))
        .pool(
            Pool::new().entry(
                Entry::item("chicken")
                    .count(1, 1)
                    .smelted(&crate::recipes::SMELTING),
            ),
        );
    r.loot(&PIG).pool(
        Pool::new().entry(
            Entry::item("porkchop")
                .count(1, 3)
                .smelted(&crate::recipes::SMELTING),
        ),
    );
    r.loot(&COW)
        .pool(
            Pool::new().entry(
                Entry::item("beef")
                    .count(1, 3)
                    .smelted(&crate::recipes::SMELTING),
            ),
        )
        .pool(Pool::new().items("leather", 0, 2));
    // Natural spawning, as rules the engine runs: monsters in the dark (spiders more in deep
    // caves), animals in small herds on grass by day, rarely once the world is loaded.
    r.natural_spawn(&ZOMBIE).light(0, 6).weight(2);
    r.natural_spawn(&SKELETON).light(0, 6).weight(2);
    r.natural_spawn(&SPIDER).light(0, 6);
    r.natural_spawn(&SPIDER)
        .light(0, 6)
        .biome("#cave")
        .weight(4);
    for animal in [&PIG, &COW, &SHEEP, &CHICKEN] {
        r.natural_spawn(animal)
            .biome("#animals")
            .ground("#animal_spawn")
            .light(7, 15)
            .pack_size(2, 3)
            .chance(0.05);
    }
}

/// Vanilla's skeleton: keeps to about 10 blocks and shoots once in two seconds.
fn skeleton() -> ai::Brain {
    ai::Brain::new()
        .aggressive(16.0)
        .rule(ai::watch_player(16.0))
        .rule(ai::Rule::action("keep_shooting_distance", ai::MOVE, |ctx| {
            ai::follow_memory(ctx, &ai::ANGER, 4)?;
            let w = ctx.world();
            let st = w.entity(ctx.id())?;
            let (id, p) = ai::target(w, ctx.id(), &st, 16.0)?;
            let d2 = (p.pos.x - st.pos.x).powi(2) + (p.pos.z - st.pos.z).powi(2);
            Some(ai::Offer::new(id, 50.0).toward(p.pos, if d2 > 100.0 { 1.0 } else { 0.0 }))
        }, ai::navigate).reads(&ai::ANGER))
        .rule(ai::Rule::action("shoot", ai::USE, |ctx| {
            ai::follow_memory(ctx, &ai::ANGER, 4)?;
            let w = ctx.world();
            let st = w.entity(ctx.id())?;
            let (id, _) = ai::target(w, ctx.id(), &st, 15.0)?;
            Some(ai::Offer::new(id, 50.0))
        }, |ctx, phase, offer| {
            let w = ctx.world();
            if phase == ai::Phase::Step
                && let (Some(st), Some(target)) = (w.entity(ctx.id()), w.entity(offer.target()))
            {
                shoot(w, ctx.id(), &st, &target);
            }
        }).reads(&ai::ANGER).step_every(40))
        .rule(ai::wander(0.6, 10.0))
}

/// Vanilla's bow: full strength after a second's draw, the curve easing in; an arrow from the
/// inventory, none taken in creative.
fn shoot_bow(u: &mut ItemUse) -> bool {
    let (w, id) = (u.world(), u.entity());
    let t = u.held_ticks() as f32 / 20.0;
    let power = ((t * t + t * 2.0) / 3.0).min(1.0) as f64;
    if power < 0.1 {
        return false;
    }
    let creative = w.creative(id);
    let inv = w.inventory(id);
    let arrow = (0..inv.len()).find(|&s| inv.get(s).item == ARROW_ITEM.id());
    if arrow.is_none() && !creative {
        return false;
    }
    let Some(s) = w.entity(id) else { return false };
    // Three blocks a tick at full draw.
    let v = look_dir(s.yaw, s.pitch) * (power * 3.0 * 20.0);
    w.spawn_with(
        &ARROW,
        s.eye(),
        &Spawn::new()
            .owner(id)
            .velocity(v)
            .inherit_velocity()
            .yaw(s.yaw),
    );
    if !creative {
        if let Some(slot) = arrow {
            inv.set(slot, consume_one(inv.get(slot)));
        }
        u.set_stack(u.stack().add_wear(1));
    }
    true
}

fn shoot(w: &World, id: u64, from: &EntityState, at: &EntityState) {
    let eye = from.eye().y;
    let d = [at.pos.x - from.pos.x, at.pos.y + at.size.eye as f64 * 0.7 - eye, at.pos.z - from.pos.z];
    let flat = (d[0] * d[0] + d[2] * d[2]).sqrt();
    let len = (flat * flat + d[1] * d[1]).sqrt().max(0.1);
    // A little above the target for the drop on the way.
    const SPEED: f64 = 32.0;
    let v = [d[0] / len * SPEED, d[1] / len * SPEED + flat * 0.4, d[2] / len * SPEED];
    w.spawn_with(
        &ARROW,
        [from.pos.x, eye, from.pos.z],
        &Spawn::new()
            .owner(id)
            .velocity(v)
            .inherit_velocity()
            .yaw(yaw_toward([0.0; 3], d)),
    );
}

/// A chicken: flutters down instead of falling, and lays an egg now and then.
fn passive_chicken() -> ai::Brain {
    passive()
        .rule(ai::Rule::sensor("flutter", |ctx| {
            let w = ctx.world();
            if let Some(st) = w.entity(ctx.id()).filter(|s| s.on_ground == 0 && s.vel.y < 0.0) {
                w.entity_modify(ctx.id()).velocity(Vec3 { y: st.vel.y * 0.6, ..st.vel });
            }
        }).every(1))
        .rule(ai::Rule::sensor("lay_egg", |ctx| {
            let w = ctx.world();
            let id = ctx.id();
            if w.component(id, &FUSE).unwrap_or_default() != 0
                && let Some(st) = w.entity(id)
            {
                w.spawn_item(st.pos, ItemStack::new(EGG.id(), 1));
            }
            w.set_component(id, &FUSE, 1);
            let delay = 6000 + w.random_below(6000) as u32;
            ctx.next_in(delay);
        }))
}

/// Monsters chase the player they are after (what `goals` sets them off by) and hit it when in
/// reach (`ai::melee`, once a second). Otherwise they wander.
fn hostile(brain: ai::Brain) -> ai::Brain {
    brain
        .rule(ai::watch_player(8.0))
        .rule(ai::melee())
        .rule(ai::chase(35.0, 1.0))
        .rule(ai::wander(0.6, 10.0))
}

/// A spider's peace: out in daylight.
fn in_daylight(w: &World, st: &EntityState) -> bool {
    let feet = [st.pos.x.floor() as i32, (st.pos.y + 0.5).floor() as i32, st.pos.z.floor() as i32];
    w.daylight() >= 0.5 && w.sky_light(feet) >= 10
}

/// Animals run about for three seconds after being hurt, otherwise wander.
fn passive() -> ai::Brain {
    ai::Brain::new()
        .timid(3.0)
        .rule(ai::watch_player(6.0))
        .rule(ai::panic(1.6))
        .rule(ai::wander(0.5, 10.0))
}

// Where each of base's mobs is hit, part by part, in the pixels of its model: what its body
// looks like, not the model itself (`Hitboxes`).
fn four_legs(h: Hitboxes, x: [f32; 2], tall: f32, front: [f32; 2], back: [f32; 2]) -> Hitboxes {
    h.bone("front_right_leg", "leg3", [-x[1], 0.0, front[0]], [-x[0], tall, front[1]])
        .bone("front_left_leg", "leg2", [x[0], 0.0, front[0]], [x[1], tall, front[1]])
        .bone("back_right_leg", "leg0", [-x[1], 0.0, back[0]], [-x[0], tall, back[1]])
        .bone("back_left_leg", "leg1", [x[0], 0.0, back[0]], [x[1], tall, back[1]])
}

fn sheep_parts() -> Hitboxes {
    let h = Hitboxes::new()
        .bone("head", "head", [-4.0, 14.0, -14.0], [4.0, 22.0, -6.0])
        .bone("body", "body", [-4.5, 10.0, -8.0], [4.5, 18.0, 10.0]);
    four_legs(h, [1.0, 5.0], 12.0, [-7.0, -3.0], [5.0, 9.0])
}

fn pig_parts() -> Hitboxes {
    let h = Hitboxes::new()
        .bone("head", "head", [-4.0, 8.0, -15.0], [4.0, 16.0, -6.0])
        .bone("body", "body", [-5.0, 6.0, -8.0], [5.0, 14.0, 9.0]);
    four_legs(h, [1.0, 5.0], 6.0, [-7.0, -3.0], [5.0, 9.0])
}

fn chicken_parts() -> Hitboxes {
    Hitboxes::new()
        .bone("head", "head", [-2.0, 9.0, -8.0], [2.0, 15.0, -3.0])
        .bone("body", "body", [-3.0, 5.0, -4.0], [3.0, 11.0, 4.0])
        .bone("right_wing", "wing0", [-4.0, 7.0, -3.0], [-3.0, 11.0, 3.0])
        .bone("left_wing", "wing1", [3.0, 7.0, -3.0], [4.0, 11.0, 3.0])
        .bone("right_leg", "rightLeg", [-2.5, 0.0, -1.0], [-1.5, 5.0, 1.0])
        .bone("left_leg", "leftLeg", [0.5, 0.0, -1.0], [1.5, 5.0, 1.0])
}

fn spider_parts() -> Hitboxes {
    Hitboxes::new()
        .head("head", [-4.0, 5.0, -11.0], [4.0, 13.0, -3.0], [0.0, 9.0, -3.0])
        .part("body", [-3.0, 6.0, -3.0], [3.0, 12.0, 3.0])
        .part("abdomen", [-5.0, 5.0, 3.0], [5.0, 13.0, 15.0])
        .part("right_legs", [-15.0, 0.0, -4.0], [-3.0, 9.0, 4.0])
        .part("left_legs", [3.0, 0.0, -4.0], [15.0, 9.0, 4.0])
}
