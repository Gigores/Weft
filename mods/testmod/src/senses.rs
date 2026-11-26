use weft_sdk::*;

pub(super) static FISH: EntityKind = EntityKind::named("shoal_fish");
pub(super) static SENTRY: EntityKind = EntityKind::named("touch_sentry");
pub(super) static AGGRESSION: Component<f32> = Component::named("testmod:aggression");
const PROBE: &str = "probe";
pub(super) static SHOAL_LEADER: Component<u64> = Component::named("testmod:shoal_leader");
static ALARM: Component<bool> = Component::named("testmod:gaze_alarm");
static SPOTTED: Component<bool> = Component::named("testmod:spotted");

pub fn register(r: &mut Registry) {
    r.entity_component(&AGGRESSION, 30.0, component_flags::SAVE);
    r.entity_component(&ALARM, false, 0);
    r.entity_component(&SHOAL_LEADER, 0, 0);
    r.entity_component(&SPOTTED, false, 0);
    r.entity(&FISH)
        .size(0.35, 0.3)
        .walk_speed(2.4)
        .unpushable()
        .tag("no_breath_bubbles")
        .set_text(&SWIMS_IN, "#water")
        .modifier(Modifier::set(&attribute::GRAVITY, 0.0).when(Cond::in_liquid()))
        .attach(&SHOAL_LEADER)
        .texture("shoal_fish")
        .model(vec![
            ModelPart::new("body", [0.0; 3])
                .cuboid([-2.0, 0.0, -4.0], [4.0, 4.0, 8.0], [0.0, 0.0])
                .cuboid([-0.5, 0.0, 4.0], [1.0, 5.0, 4.0], [0.0, 16.0]),
        ])
        .brain(
            ai::Brain::new()
                .rule(ai::Rule::action("school", ai::MOVE, school, |ctx, phase, offer| swim(ctx, phase, offer, 1.0)).every(5))
                .rule(ai::Rule::action("flee_aggro", ai::MOVE, flee, |ctx, phase, offer| swim(ctx, phase, offer, 1.15)).every(8)),
        );
    r.entity(&SENTRY)
        .size(0.65, 1.5)
        .walk_speed(2.6)
        .unpushable()
        .attach(&AGGRESSION)
        .attach(&ALARM)
        .attach(&SPOTTED)
        .texture("touch_sentry")
        .model(
            Model::from(vec![
                ModelPart::new("body", [0.0; 3])
                    .cuboid([-7.0, 0.0, -7.0], [14.0, 4.0, 14.0], [0.0, 0.0])
                    .cuboid([-5.0, 4.0, -4.0], [10.0, 13.0, 8.0], [0.0, 0.0])
                    .cuboid([-8.0, 4.0, -17.0], [16.0, 4.0, 13.0], [0.0, 32.0])
                    .cuboid([-7.0, 17.0, -6.0], [14.0, 7.0, 12.0], [0.0, 0.0]),
            ])
            .uv_size(64, 64),
        )
        .hitboxes(
            Hitboxes::new()
                .part(PROBE, [-8.0, 4.0, -17.0], [8.0, 8.0, -4.0])
                .part("head", [-7.0, 17.0, -6.0], [7.0, 24.0, 6.0])
                .part("base", [-5.0, 0.0, -4.0], [5.0, 17.0, 4.0]),
        )
        .brain(
            ai::Brain::new()
                .rule(
                    ai::Rule::action("probe_contact", ai::USE, contact, |ctx, phase, offer| {
                        if phase == ai::Phase::Start {
                            ctx.world()
                                .damage(offer.target(), Damage::new(1.0, &damage_causes::ATTACK).by(ctx.id()));
                        }
                    })
                    .every(1),
                )
                .rule(
                    ai::Rule::vision_sensor(
                        "spot_in_cone",
                        ai::Vision { blind_near: 1.15, ..ai::Vision::new(8.0, 80.0, 65.0) },
                        spot_in_cone,
                    )
                    .every(5),
                )
                .rule(
                    ai::Rule::action(
                        "head_gaze",
                        ai::LOOK,
                        |ctx| gaze(ctx, ai::Gaze::at(ctx.id(), 12.0).part("head").for_ticks(0)),
                        |ctx, phase, _| {
                            if phase == ai::Phase::Start {
                                flash(ctx, 0xffc048, 12);
                            }
                        },
                    )
                    .every(20),
                )
                .rule(
                    ai::Rule::action(
                        "held_head_gaze",
                        ai::USE,
                        |ctx| {
                            gaze(
                                ctx,
                                ai::Gaze::at(ctx.id(), 12.0)
                                    .part("head")
                                    .for_real_time(std::time::Duration::from_secs(1)),
                            )
                        },
                        |ctx, phase, _| match phase {
                            ai::Phase::Start => {
                                ctx.world().set_component(ctx.id(), &ALARM, true);
                                flash(ctx, 0xff4131, 24);
                            }
                            ai::Phase::Step => flash(ctx, 0xff4131, 8),
                            ai::Phase::Stop => {
                                ctx.world().set_component(ctx.id(), &ALARM, false);
                            }
                            ai::Phase::Update => {}
                        },
                    )
                    .every(20)
                    .step_every(8),
                ),
        );
    r.command("senseaggro", "<entity:int> <value:float>", |cmd| {
        let w = cmd.world();
        let Some(id) = cmd.int("entity").and_then(|id| u64::try_from(id).ok()) else { return };
        let Some(value) = cmd.float("value") else { return };
        if let Some(aggression) = w.component_mut(id, &AGGRESSION) {
            *aggression = value as f32;
            cmd.reply("command.testmod.senseaggro", &[Arg::Float(value, 1)]);
        }
    });
}

fn school(ctx: &mut ai::Context<'_>) -> Option<ai::Offer> {
    let w = ctx.world();
    let st = w.entity(ctx.id())?;
    let mut count = 0;
    let mut center = Vec3::ZERO;
    let mut leader: Option<ai::Neighbor> = None;
    let mut separation = Vec3::ZERO;
    let mut close = 0;
    let mut stacked = false;
    ai::Nearby::new(st.pos, 8.0)
        .kind(&FISH)
        .excluding(ctx.id())
        .visit(w, |other| {
            center = center + other.pos;
            if leader.is_none_or(|old| other.id < old.id) {
                leader = Some(other);
            }
            count += 1;
            let delta = st.pos - other.pos;
            let distance = delta.x.hypot(delta.z);
            if distance < 0.01 {
                stacked = true;
            } else if distance < 0.8 {
                separation = separation + Vec3::new(delta.x, 0.0, delta.z) * ((0.8 - distance) / distance);
                close += 1;
            }
        });
    let leader = leader?;
    let center = center * (1.0 / f64::from(count));
    let assigned = w
        .component(ctx.id(), &SHOAL_LEADER)
        .filter(|&id| id != 0)
        .and_then(|id| w.entity(id))
        .filter(|leader| (leader.pos - st.pos).length_squared() <= 64.0);
    let gather = if let Some(leader) = assigned {
        leader.pos
    } else if w.in_liquid(center) {
        center
    } else {
        leader.pos
    };
    let cohesion = gather - st.pos;
    let mut direction = cohesion * 0.4 + separation * (1.5 / f64::from(close.max(1)));
    if stacked {
        let angle = (ctx.id() % 1024) as f64 * 2.39996323;
        direction = direction.offset(angle.cos() * 0.7, 0.0, angle.sin() * 0.7);
    }
    let strength = direction.length_squared().sqrt();
    let goal = if cohesion.length_squared() > 2.25 { gather } else { st.pos + direction };
    (strength > 0.1).then(|| ai::Offer::new(0, 10.0).toward(goal, strength.min(0.85) as f32))
}

fn flee(ctx: &mut ai::Context<'_>) -> Option<ai::Offer> {
    let w = ctx.world();
    let st = w.entity(ctx.id())?;
    let scent = ai::Scent { range: 8.0, threshold: 0.1 };
    let threat = ai::Nearby::new(st.pos, scent.range)
        .kind(&SENTRY)
        .nearest(w, |other| {
            let Some(aggression) = ctx.observe(other.id, &AGGRESSION) else { return false };
            let aggression = if ctx.observe(other.id, &ALARM) == Some(true) { aggression.max(50.0) } else { aggression };
            let already_fleeing = ctx
                .current()
                .is_some_and(|offer| offer.target() == other.id);
            (aggression >= 50.0 || already_fleeing && aggression > 30.0) && scent.detect(st.pos, other.pos, 1.0).is_some()
        })?;
    Some(ai::Offer::new(threat.id, 50.0).toward(ai::flee_from(&st, threat.pos, 4.0), 1.3))
}

fn flash(ctx: &ai::Context<'_>, color: u32, count: u32) {
    let w = ctx.world();
    if let Some(st) = w.entity(ctx.id()) {
        w.spawn_particles(&super::looks::SPARK, st.eye().offset(0.0, 0.22, 0.0))
            .color(color)
            .count(count)
            .spread(0.3)
            .speed(1.0);
    }
}

fn spot_in_cone(ctx: &mut ai::Context<'_>, sight: ai::Vision) {
    let w = ctx.world();
    let Some(st) = w.entity(ctx.id()) else { return };
    let sees = ai::Nearby::new(st.pos, sight.range)
        .kind(&PLAYER)
        .nearest(w, |viewer| {
            w.entity(viewer.id)
                .is_some_and(|target| sight.sees(w, &st, target.eye()))
        })
        .is_some();
    if w.component(ctx.id(), &SPOTTED) != Some(sees) {
        w.set_component(ctx.id(), &SPOTTED, sees);
        if sees {
            flash(ctx, 0x66ff9e, 22);
        }
    }
}

fn gaze(ctx: &mut ai::Context<'_>, gaze: ai::Gaze) -> Option<ai::Offer> {
    let w = ctx.world();
    let st = w.entity(ctx.id())?;
    let viewer = ai::Nearby::new(st.pos, 12.0)
        .kind(&PLAYER)
        .nearest(w, |_| true)?;
    ctx.watch_gaze(viewer.id, gaze)
        .held()
        .then(|| ai::Offer::new(viewer.id, 0.5))
}

fn contact(ctx: &mut ai::Context<'_>) -> Option<ai::Offer> {
    let w = ctx.world();
    let st = w.entity(ctx.id())?;
    let touch = ai::Touch { range: 3.0, part: Some(PROBE) };
    let other = touch.nearest(w, ctx.id(), &st, |other| other.kind == PLAYER.id())?;
    Some(ai::Offer::new(other.id, 1.0))
}

fn swim(ctx: &mut ai::Context<'_>, phase: ai::Phase, offer: ai::Offer, open_cost: f32) {
    if matches!(phase, ai::Phase::Start | ai::Phase::Update) {
        ctx.world()
            .navigate_with(ctx.id(), offer.point(), offer.speed(), &ai::Navigation::swimming().open_cost(open_cost));
    }
}
