use weft_sdk::*;

static MOB: EntityKind = EntityKind::named("torch_shy");
static LIGHT: Tag = Tag::named("testmod:torch_shy_light");
static FLEE_PRIORITY: Component<f32> = Component::named("testmod:torch_flee_priority");
const FLEE: &str = "run_from_torch";
const RANGE: f64 = 16.0;

pub fn register(r: &mut Registry) {
    r.tag("testmod:torch_shy_light", &["base:torch"]);
    r.entity_component(&FLEE_PRIORITY, 110.0, component_flags::SAVE);
    r.entity(&MOB)
        .size(0.6, 1.95)
        .max_health(20.)
        .walk_speed(3.)
        .attack_damage(2.0)
        .hostile()
        .texture("base:zombie")
        .model(humanoid(true))
        .attach(&FLEE_PRIORITY)
        .brain(ai::Brain::new()
            .aggressive(RANGE)
            .rule(ai::chase(RANGE, 1.0))
            .rule(ai::melee())
            .rule(ai::Rule::action(FLEE, ai::MOVE | ai::USE, flee, ai::navigate)
                .reads(&FLEE_PRIORITY).every(5)));
    r.on_event(&ai::HELD_CHANGED, |events| {
        let w = events.world();
        for change in events.iter() {
            let Some(source) = w.entity(change.entity) else { continue };
            for id in w.entities_near(source.pos, RANGE + 2.0, Some(&MOB)) {
                w.wake_brain(id, FLEE);
            }
        }
    });
}

fn flee(ctx: &mut ai::Context<'_>) -> Option<ai::Offer> {
    let w = ctx.world();
    let st = w.entity(ctx.id())?;
    let (source, player) = ai::nearest_player(w, &st, RANGE)?;
    if !w.hands(source).into_iter().any(|stack| !stack.is_empty() && LIGHT.on_item(stack.item)) {
        return None;
    }
    let priority = w.component(ctx.id(), &FLEE_PRIORITY)?;
    let away = ai::flee_from(&st, player.pos, 8.0);
    let point = ctx.current().filter(|o| o.target() == source && w.navigating(ctx.id()))
        .and_then(|o| {
            let path = o.point() - st.pos;
            let direction = away - st.pos;
            (path.x * direction.x + path.z * direction.z > 0.0).then_some(o.point())
        }).unwrap_or(away);
    Some(ai::Offer::new(source, priority).toward(point, 1.45))
}
