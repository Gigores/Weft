use std::f64::consts::PI;

use weft_sdk::*;

use super::senses::{FISH, SENTRY, SHOAL_LEADER};

static AMBER: EntityKind = EntityKind::named("amber_hunter");
static CYAN: EntityKind = EntityKind::named("cyan_hunter");
static WIDE_FISH: EntityKind = EntityKind::named("wide_shoal_fish");
static TORCH_SHY: EntityKind = EntityKind::named("torch_shy");
static ALERT: Event<u64> = Event::named("testmod:showcase_alert");
static VICTIM: Component<u64> = Component::named("testmod:showcase_victim");
static GLASS: Block = Block::named("base:glass");
static STONE: Block = Block::named("base:stone");
static WATER: Block = Block::named("base:water");
static GRASS: Block = Block::named("base:short_grass");
const RADIUS: f64 = 6.0;
const NOTICE_RANGE: f64 = 16.0;

pub fn register(r: &mut Registry) {
    r.entity_component(&VICTIM, 0, 0);
    r.entity(&WIDE_FISH)
        .size(1.4, 1.1)
        .walk_speed(2.4)
        .unpushable()
        .tag("no_breath_bubbles")
        .set_text(&SWIMS_IN, "#water")
        .modifier(Modifier::set(&attribute::GRAVITY, 0.0).when(Cond::in_liquid()))
        .texture("cyan_hunter")
        .model(vec![
            ModelPart::new("body", [0.0; 3])
                .cuboid([-9.0, 0.0, -6.0], [18.0, 13.0, 12.0], [0.0, 0.0])
                .cuboid([-1.0, 0.0, 6.0], [2.0, 12.0, 6.0], [0.0, 0.0]),
        ]);
    for (kind, texture, side) in [(&AMBER, "amber_hunter", 0.0), (&CYAN, "cyan_hunter", PI)] {
        r.entity(kind)
            .size(0.65, 1.8)
            .walk_speed(3.2)
            .attack_damage(2.0)
            .texture(texture)
            .model(humanoid(false))
            .attach(&VICTIM)
            .brain(
                ai::Brain::new()
                    .rule(
                        ai::Rule::action("circle", ai::MOVE | ai::LOOK, move |ctx| orbit(ctx, side), circle)
                            .player_notice_range(NOTICE_RANGE)
                            .every(2)
                            .step_every(2),
                    )
                    .rule(
                        ai::Rule::action("close_attack", ai::USE, close_attack, |ctx, phase, offer| {
                            if matches!(phase, ai::Phase::Start | ai::Phase::Step) {
                                ctx.world().attack(ctx.id(), offer.target());
                            }
                        })
                        .every(3)
                        .step_every(10),
                    ),
            );
    }
    r.command("brainshowcase", "[scene:word(gaze,shoal,gap,routes,hunt,torch)]", showcase);
    r.on_event(&ALERT, |events| {
        let w = events.world();
        for &id in events.iter() {
            if let Some(state) = w.entity(id) {
                w.set_component(id, &super::senses::AGGRESSION, 55.0);
                w.spawn_particles(&super::looks::SPARK, state.eye())
                    .color(0xff4131)
                    .count(28)
                    .spread(0.4)
                    .speed(1.0);
            }
        }
    });
}

fn target(ctx: &ai::Context<'_>) -> Option<(u64, EntityState)> {
    let w = ctx.world();
    let mob = w.entity(ctx.id())?;
    if let Some(id) = w.component(ctx.id(), &VICTIM).filter(|id| *id != 0) {
        return w
            .entity(id)
            .filter(|e| e.health > 0.0 && (e.pos - mob.pos).length_squared() < 24.0 * 24.0)
            .map(|e| (id, e));
    }
    ai::nearest_player(w, &mob, NOTICE_RANGE)
}

fn orbit_point(center: Vec3, angle: f64) -> Vec3 {
    center.offset(angle.cos() * RADIUS, 0.0, angle.sin() * RADIUS)
}

fn orbit(ctx: &mut ai::Context<'_>, side: f64) -> Option<ai::Offer> {
    let (id, target) = target(ctx)?;
    let w = ctx.world();
    let pos = w.entity(ctx.id())?.pos;
    let radius = |v: Vec3| v.x.hypot(v.z);
    let previous = ctx
        .current()
        .filter(|o| o.target() == id)
        .map(ai::Offer::point)
        .filter(|&p| (radius(p - target.pos) - RADIUS).abs() < 0.6 && (p.y - target.pos.y).abs() < 0.5);
    let point = match previous {
        Some(p) if w.navigating(ctx.id()) && radius(p - pos) > 0.55 => p,
        _ => {
            let radial = previous.unwrap_or(pos) - target.pos;
            let angle = if radius(radial) < 0.1 { side } else { radial.z.atan2(radial.x) };
            orbit_point(target.pos, angle + 0.26)
        }
    };
    Some(ai::Offer::new(id, 10.0).toward(point, 0.6))
}

fn circle(ctx: &mut ai::Context<'_>, phase: ai::Phase, offer: ai::Offer) {
    if matches!(phase, ai::Phase::Start | ai::Phase::Update | ai::Phase::Step) {
        let w = ctx.world();
        if let (Some(st), Some(other)) = (w.entity(ctx.id()), w.entity(offer.target())) {
            w.entity_modify(ctx.id()).yaw(yaw_toward(st.pos, other.pos));
        }
    }
    ai::navigate(ctx, phase, offer);
}

fn close_attack(ctx: &mut ai::Context<'_>) -> Option<ai::Offer> {
    let (id, target) = target(ctx)?;
    let st = ctx.world().entity(ctx.id())?;
    let vision = ai::Vision::new(2.0, 100.0, 90.0);
    vision
        .sees(ctx.world(), &st, target.eye())
        .then(|| ai::Offer::new(id, 15.0))
}

fn spawn_facing(w: &World, kind: &EntityKind, pos: Vec3, target: Vec3) -> u64 {
    w.spawn_with(kind, pos, &Spawn::new().yaw(yaw_toward(pos, target)))
}

fn showcase(cmd: &mut ServerCommand) {
    let w = cmd.world();
    let player = cmd.player();
    let Some(st) = w.entity(player) else { return };
    let scene = match cmd.text("scene").unwrap_or("") {
        "gaze" => {
            let [x, z] = yaw_dir(st.yaw);
            let pos = st.pos.offset(f64::from(x) * 5.0, 0.0, f64::from(z) * 5.0);
            let id = spawn_facing(&w, &SENTRY, pos, st.pos);
            cmd.reply("command.testmod.brainshowcase.gaze", &[Arg::Int(id as i64)]);
            return;
        }
        "torch" => {
            let [x, z] = yaw_dir(st.yaw);
            let pos = st.pos.offset(f64::from(x) * 7.0, 0.0, f64::from(z) * 7.0);
            spawn_facing(&w, &TORCH_SHY, pos, st.pos);
            "command.testmod.brainshowcase.torch"
        }
        "hunt" => {
            for (kind, side) in [(&AMBER, 0.0), (&CYAN, PI)] {
                let pos = orbit_point(st.pos, side);
                let hunter = spawn_facing(&w, kind, pos, st.pos);
                w.set_component(hunter, &VICTIM, player);
            }
            "command.testmod.brainshowcase.hunt"
        }
        "shoal" if shoal(&w, st.block_pos()) => "command.testmod.brainshowcase.shoal",
        "gap" if gap(&w, st.block_pos()) => "command.testmod.brainshowcase.gap",
        "routes" if routes(&w, st.block_pos()) => "command.testmod.brainshowcase.routes",
        "shoal" | "gap" | "routes" => "command.testmod.brainshowcase.space",
        _ => "command.testmod.brainshowcase.unknown",
    };
    cmd.reply(scene, &[]);
}

fn aquarium(w: &World, origin: BlockPos, wall: impl Fn(i32, i32) -> bool) -> bool {
    let cells = || (3..=14).flat_map(|x| (0..5).flat_map(move |y| (-3_i32..=3).map(move |z| (x, y, z))));
    if cells().any(|(x, y, z)| w.block_loaded(origin.offset(x, y, z)) != Some(0)) {
        return false;
    }
    for (x, y, z) in cells() {
        let block = if x == 3 || x == 14 || z.abs() == 3 {
            &GLASS
        } else if x == 8 && wall(y, z) {
            &STONE
        } else {
            &WATER
        };
        w.set_block(origin.offset(x, y, z), block.id());
    }
    true
}

fn shoal(w: &World, origin: BlockPos) -> bool {
    if !aquarium(w, origin, |y, _| y < 2) {
        return false;
    }
    let leader = w.spawn(&FISH, origin.offset(6, 1, 0).center().offset(0.0, -0.25, 0.0));
    w.enable_brain_rule(leader, "testmod:school", false);
    for (x, y, z) in [(6, 0, 0), (6, 1, 1), (6, 1, -1), (7, 1, 0), (11, 0, 0)] {
        let p = origin.offset(x, y, z).center().offset(0.0, -0.25, 0.0);
        let fish = w.spawn(&FISH, p);
        w.set_component(fish, &SHOAL_LEADER, leader);
    }
    let pos = origin.offset(2, 0, 0).center().offset(0.0, -0.5, 0.0);
    let id = spawn_facing(w, &SENTRY, pos, origin.center());
    // The change is visible in the fish without a ray through the glass.
    w.timer(id, 160, &ALERT, id);
    true
}

fn gap(w: &World, origin: BlockPos) -> bool {
    if !aquarium(w, origin, |_, z| z != 0) {
        return false;
    }
    let target = origin.offset(5, 0, 0).center().offset(0.0, -0.25, 0.0);
    for (x, kind) in [(11, &FISH), (12, &WIDE_FISH)] {
        let start = origin.offset(x, 0, 0).center().offset(0.0, -0.25, 0.0);
        let id = spawn_facing(w, kind, start, target);
        if kind.id() == FISH.id() {
            w.enable_brain_rule(id, "testmod:school", false);
        }
        w.navigate_with(id, target, 1.0, &ai::Navigation::swimming());
    }
    true
}

fn routes(w: &World, origin: BlockPos) -> bool {
    let strips = || (6..=9).flat_map(|x| [-4, -3, -2, 2, 3, 4].map(|z| (x, z)));
    if strips().any(|(x, z)| w.block_loaded(origin.offset(x, 0, z)) != Some(0)) {
        return false;
    }
    for (x, z) in strips() {
        w.set_block(origin.offset(x, 0, z), GRASS.id());
    }
    for (kind, z, cost) in [(&AMBER, -3, 10.0), (&CYAN, 3, 0.25)] {
        let p = origin.offset(3, 0, z).center().offset(0.0, -0.5, 0.0);
        let goal = origin.offset(13, 0, z).center().offset(0.0, -0.5, 0.0);
        let id = spawn_facing(w, kind, p, goal);
        w.enable_brain_rule(id, "testmod:circle", false);
        w.enable_brain_rule(id, "testmod:close_attack", false);
        w.navigate_with(id, goal, 0.85, &ai::Navigation::walking().block_cost(&GRASS, cost));
    }
    true
}
