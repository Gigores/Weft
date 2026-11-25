//! Event-driven mob behavior. Sensors update facts and actions compete for controls.

use crate::{Component, EntityState, Event, ItemStack, Vec3, World, abi};
use abi::brain as b;

pub const MOVE: u32 = b::MOVE;
pub const LOOK: u32 = b::LOOK;
pub const USE: u32 = b::USE;

pub static HELD_CHANGED: Event<abi::HeldChange> = Event::named(b::HELD_CHANGED);
pub static LAST_HURT: Component<abi::BrainMemory> = Component::named(b::HURT);
pub static ANGER: Component<abi::BrainMemory> = Component::named(b::ANGER);
pub static FEAR: Component<abi::BrainMemory> = Component::named(b::FEAR);

/// One candidate action. Keep its target key stable while it pursues the same intent.
#[derive(Clone, Copy, Debug)]
pub struct Offer(pub(crate) abi::BrainOffer);

impl Offer {
    pub fn new(target: u64, priority: f32) -> Self {
        Self(abi::BrainOffer { target, priority, keep: priority, valid: 1, ..Default::default() })
    }

    pub fn toward(mut self, point: impl Into<Vec3>, speed: f32) -> Self {
        self.0.point = point.into();
        self.0.speed = speed;
        self
    }

    pub fn keep_at(mut self, priority: f32) -> Self {
        self.0.keep = priority;
        self
    }

    pub fn target(self) -> u64 {
        self.0.target
    }
    pub fn point(self) -> Vec3 {
        self.0.point
    }
    pub fn speed(self) -> f32 {
        self.0.speed
    }
    pub fn priority(self) -> f32 {
        self.0.priority
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Start,
    Update,
    Step,
    Stop,
}

pub struct Context<'a> {
    world: &'a World,
    actor: &'a mut abi::BrainActor,
}

impl Context<'_> {
    pub fn world(&self) -> &World {
        self.world
    }
    pub fn id(&self) -> u64 {
        self.actor.id
    }
    pub fn current(&self) -> Option<Offer> {
        (self.actor.current.valid != 0).then_some(Offer(self.actor.current))
    }
    /// Ask for another check or step. Zero waits for a signal.
    pub fn next_in(&mut self, ticks: u32) {
        self.actor.next = ticks;
    }
}

pub struct Rule {
    pub(crate) name: String,
    pub(crate) callback: abi::Callback,
    pub(crate) settings: abi::BrainSettings,
}

impl Rule {
    /// A sensor can update memory and schedule its next sample.
    pub fn sensor(name: &str, sample: impl Fn(&mut Context<'_>) + Send + Sync + 'static) -> Self {
        Self {
            name: name.into(),
            callback: crate::callback(move |args: &mut abi::BrainArgs| {
                for_actor(args, &sample);
            }),
            settings: abi::BrainSettings::default(),
        }
    }

    /// Evaluates a candidate from the current facts. Make changes from the action callback.
    pub fn action(
        name: &str,
        controls: u32,
        offer: impl Fn(&mut Context<'_>) -> Option<Offer> + Send + Sync + 'static,
        act: impl Fn(&mut Context<'_>, Phase, Offer) + Send + Sync + 'static,
    ) -> Self {
        assert!(controls != 0, "an action must own a control");
        Self {
            name: name.into(),
            callback: crate::callback(move |args: &mut abi::BrainArgs| {
                let phase = args.phase;
                for_actor(args, |ctx| {
                    if phase == b::OFFER {
                        ctx.actor.offer = offer(ctx).map_or_else(Default::default, |value| value.0);
                    } else {
                        let (phase, value) = match phase {
                            b::START => (Phase::Start, ctx.actor.offer),
                            b::UPDATE => (Phase::Update, ctx.actor.offer),
                            b::STEP => (Phase::Step, ctx.actor.offer),
                            b::STOP => (Phase::Stop, ctx.actor.current),
                            _ => return,
                        };
                        act(ctx, phase, Offer(value));
                    }
                });
            }),
            settings: abi::BrainSettings { controls, ..Default::default() },
        }
    }

    /// Periodic sampling for facts that have no direct change event.
    pub fn every(mut self, ticks: u32) -> Self {
        self.settings.interval = ticks;
        self
    }

    /// Optional work on a running action, independent from its selection.
    pub fn step_every(mut self, ticks: u32) -> Self {
        self.settings.step = ticks;
        self
    }

    pub fn reads<T: crate::Value>(mut self, component: &Component<T>) -> Self {
        self.settings.watched.push(component.name().to_owned());
        self
    }
}

fn for_actor(args: &mut abi::BrainArgs, f: impl Fn(&mut Context<'_>)) {
    let world = World::from_raw(args.world);
    for actor in unsafe { std::slice::from_raw_parts_mut(args.actors, args.count) } {
        f(&mut Context { world: &world, actor });
    }
}

#[derive(Default)]
pub struct Brain {
    pub(crate) rules: Vec<Rule>,
    pub(crate) anger: bool,
    pub(crate) fear: bool,
}

impl Brain {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn rule(mut self, rule: Rule) -> Self {
        self.rules.push(rule);
        self
    }

    pub fn aggressive(self, range: f64) -> Self {
        self.sight_when(range, |_, _| true).neutral()
    }

    pub fn sight_when(mut self, range: f64, condition: impl Fn(&World, &EntityState) -> bool + Send + Sync + 'static) -> Self {
        self.anger = true;
        self.rule(
            Rule::sensor("core:sight", move |ctx| {
                let w = ctx.world();
                let Some(st) = w.entity(ctx.id()).filter(|s| condition(w, s)) else { return };
                if let Some((source, _)) = nearest_player(w, &st, range) {
                    remember(w, ctx.id(), &ANGER, source, w.game_time().saturating_add(40));
                }
            })
            .every(5),
        )
    }

    pub fn neutral(mut self) -> Self {
        self.anger = true;
        self.rule(hurt_memory("core:anger_on_hurt", &ANGER, 400, |w, who| w.players_iter().any(|id| id == who)))
    }

    pub fn timid(mut self, secs: f64) -> Self {
        self.fear = true;
        self.rule(hurt_memory("core:fear_on_hurt", &FEAR, (secs * 20.0) as u64, |_, _| true))
    }
}

impl World {
    /// Wake one rule after a custom event. Use an empty name to reconsider the whole brain.
    pub fn wake_brain(&self, id: u64, rule: &str) -> bool {
        let name = if rule.is_empty() { String::new() } else { crate::full_name(rule) };
        unsafe { (crate::host().brain_wake)(self.raw(), id, abi::Str::new(&name)) != 0 }
    }

    /// Change the rules of one saved entity. An empty name toggles all its rules.
    pub fn enable_brain_rule(&self, id: u64, rule: &str, enabled: bool) -> bool {
        let name = if rule.is_empty() { String::new() } else { crate::full_name(rule) };
        unsafe { (crate::host().brain_enable)(self.raw(), id, abi::Str::new(&name), enabled as u32) != 0 }
    }

    /// Add a rule from a registered kind to this entity. Names must be unique on the host entity.
    /// Attach the components the rule reads to the receiving kind.
    pub fn install_brain_rule(&self, id: u64, source: &crate::EntityKind, rule: &str) -> bool {
        let name = crate::full_name(rule);
        unsafe { (crate::host().brain_install)(self.raw(), id, source.id(), abi::Str::new(&name)) != 0 }
    }

    pub fn brain_action(&self, id: u64, rule: &str) -> Option<Offer> {
        let name = crate::full_name(rule);
        let mut offer = abi::BrainOffer::default();
        let active = unsafe { (crate::host().brain_status)(self.raw(), id, abi::Str::new(&name), &mut offer) };
        (active != 0).then_some(Offer(offer))
    }

    /// The resolved main and off hand, including the selected slot.
    pub fn hands(&self, id: u64) -> [ItemStack; 2] {
        let mut hands = [ItemStack::EMPTY; 2];
        unsafe { (crate::host().entity_hands)(self.raw(), id, &mut hands) };
        hands
    }
}

fn remember(w: &World, id: u64, memory: &Component<abi::BrainMemory>, source: u64, until: u64) {
    if w.component(id, memory)
        .is_none_or(|old| old.source != source || old.time.saturating_add(10) < until)
    {
        w.set_component(id, memory, abi::BrainMemory { source, time: until });
    }
}

fn hurt_memory(
    name: &str,
    memory: &'static Component<abi::BrainMemory>,
    duration: u64,
    accepts: impl Fn(&World, u64) -> bool + Send + Sync + 'static,
) -> Rule {
    Rule::sensor(name, move |ctx| {
        let w = ctx.world();
        if let Some(hit) = w
            .component(ctx.id(), &LAST_HURT)
            .filter(|h| h.time != 0 && accepts(w, h.source))
        {
            remember(w, ctx.id(), memory, hit.source, hit.time.saturating_add(duration));
        }
    })
    .reads(&LAST_HURT)
}

fn visible(st: &EntityState, target: &EntityState, range: f64, visibility: f64) -> bool {
    let d = st.pos - target.pos;
    target.health > 0.0 && visibility > 0.0 && d.y.abs() <= 8.0 * visibility && d.x * d.x + d.z * d.z <= (range * visibility).powi(2)
}

pub fn nearest_player(w: &World, st: &EntityState, range: f64) -> Option<(u64, EntityState)> {
    w.players_iter()
        .filter_map(|id| w.entity(id).map(|target| (id, target)))
        .filter(|(id, target)| visible(st, target, range, w.attribute(*id, &crate::attribute::VISIBILITY)))
        .min_by(|a, b| {
            (st.pos - a.1.pos)
                .length_squared()
                .total_cmp(&(st.pos - b.1.pos).length_squared())
                .then(a.0.cmp(&b.0))
        })
}

pub fn target(w: &World, id: u64, st: &EntityState, range: f64) -> Option<(u64, EntityState)> {
    let memory = w.component(id, &ANGER).filter(|m| m.time > w.game_time())?;
    let target = w.entity(memory.source)?;
    let visibility =
        if w.players_iter().any(|p| p == memory.source) { w.attribute(memory.source, &crate::attribute::VISIBILITY) } else { 1.0 };
    visible(st, &target, range, visibility).then_some((memory.source, target))
}

/// Follow a remembered fact while it is live, checking no later than its expiry.
pub fn follow_memory(ctx: &mut Context<'_>, memory: &Component<abi::BrainMemory>, interval: u32) -> Option<abi::BrainMemory> {
    let w = ctx.world();
    let now = w.game_time();
    let value = w.component(ctx.id(), memory).filter(|m| m.time > now)?;
    ctx.next_in((value.time - now).min(u64::from(interval)) as u32);
    Some(value)
}

/// The standard action callback for a movement offer.
pub fn navigate(ctx: &mut Context<'_>, phase: Phase, offer: Offer) {
    if matches!(phase, Phase::Start | Phase::Update) {
        ctx.world().navigate(ctx.id(), offer.point(), offer.speed());
    }
}

pub fn chase(range: f64, speed: f32) -> Rule {
    Rule::action(
        "core:chase",
        MOVE,
        move |ctx| {
            follow_memory(ctx, &ANGER, 4)?;
            let w = ctx.world();
            let st = w.entity(ctx.id())?;
            let (id, p) = target(w, ctx.id(), &st, range)?;
            let priority = 25.0 + (1.0 - ((st.pos - p.pos).length_squared().sqrt() / range.max(1.0)).min(1.0)) as f32 * 65.0;
            Some(Offer::new(id, priority).toward(p.pos, speed))
        },
        navigate,
    )
    .reads(&ANGER)
}

pub fn melee() -> Rule {
    Rule::action(
        "core:melee",
        USE,
        |ctx| {
            follow_memory(ctx, &ANGER, 3)?;
            let w = ctx.world();
            let st = w.entity(ctx.id())?;
            let reach = w.attribute(ctx.id(), &crate::attribute::ENTITY_REACH);
            let (id, _) = target(w, ctx.id(), &st, reach + st.size.width as f64 + 2.0)?;
            w.in_reach(ctx.id(), id).then_some(Offer::new(id, 40.0))
        },
        |ctx, phase, action| {
            let w = ctx.world();
            if matches!(phase, Phase::Start | Phase::Step) && w.in_reach(ctx.id(), action.target()) && w.swing_strength(ctx.id()) >= 1.0 {
                w.attack(ctx.id(), action.target());
            }
        },
    )
    .reads(&ANGER)
    .step_every(2)
}

fn random_point(w: &World, st: &EntityState, radius: f64) -> Vec3 {
    let angle = w.random_below(628) as f64 / 100.0;
    st.pos
        .offset(angle.cos() * radius, 0.0, angle.sin() * radius)
}

pub fn wander(speed: f32, radius: f64) -> Rule {
    Rule::action(
        "core:wander",
        MOVE,
        move |ctx| {
            let w = ctx.world();
            let st = w.entity(ctx.id())?;
            if let Some(current) = ctx.current().filter(|_| w.navigating(ctx.id())) {
                return Some(current);
            }
            if w.random_below(12) == 0 { Some(Offer::new(0, 1.0).toward(random_point(w, &st, radius), speed)) } else { None }
        },
        navigate,
    )
    .every(8)
}

pub fn flee_from(st: &EntityState, source: Vec3, distance: f64) -> Vec3 {
    let d = st.pos - source;
    let len = (d.x * d.x + d.z * d.z).sqrt();
    if len < 0.001 {
        st.pos.offset(distance, 0.0, 0.0)
    } else {
        st.pos
            .offset(d.x * distance / len, 0.0, d.z * distance / len)
    }
}

pub fn panic(speed: f32) -> Rule {
    Rule::action(
        "core:panic",
        MOVE,
        move |ctx| {
            let fear = follow_memory(ctx, &FEAR, 5)?;
            let w = ctx.world();
            let st = w.entity(ctx.id())?;
            if let Some(current) = ctx
                .current()
                .filter(|o| o.target() == fear.source && w.navigating(ctx.id()))
            {
                return Some(current);
            }
            let point = w
                .entity(fear.source)
                .map_or_else(|| random_point(w, &st, 8.0), |src| flee_from(&st, src.pos, 8.0));
            Some(Offer::new(fear.source, 120.0).toward(point, speed))
        },
        navigate,
    )
    .reads(&FEAR)
}

pub fn watch_player(range: f64) -> Rule {
    Rule::action(
        "core:watch_player",
        LOOK,
        move |ctx| {
            let w = ctx.world();
            let st = w.entity(ctx.id())?;
            if w.navigating(ctx.id()) {
                return None;
            }
            let (id, seen) = nearest_player(w, &st, range)?;
            Some(Offer::new(id, 1.0).toward(seen.eye(), 0.0))
        },
        |ctx, phase, offer| {
            if !matches!(phase, Phase::Stop)
                && let Some(st) = ctx.world().entity(ctx.id())
            {
                let d = offer.point() - st.eye();
                let horizontal = (d.x * d.x + d.z * d.z).sqrt();
                ctx.world()
                    .entity_modify(ctx.id())
                    .yaw(crate::yaw_toward(st.pos, offer.point()))
                    .pitch(d.y.atan2(horizontal) as f32);
            }
        },
    )
    .every(6)
    .step_every(6)
}

/// Deliver a fact to nearby members of a group. Call from an event or a transition.
pub fn share<T: crate::Value>(
    w: &World,
    position: Vec3,
    radius: f64,
    belongs: impl Fn(u64, &EntityState) -> bool,
    fact: &Component<T>,
    value: T,
) {
    for id in w.entities_near(position, radius, None) {
        if w.entity(id).is_some_and(|state| belongs(id, &state)) {
            w.set_component(id, fact, value);
        }
    }
}
