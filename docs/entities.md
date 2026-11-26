# Entities

`r.entity(&ENTITY)` and then the calls below. What a mob thinks is in `weft_sdk::ai`, how it looks in the [models guide](models.md), and every method in the [reference](reference/entities.md).

## Basics, looks and sounds

- every entity gets a spawn egg by itself, `<name>_spawn_egg`, called after it and in the creative tab of eggs. Draw `item/<name>_spawn_egg.png` or it looks generic; `no_egg()` for what isn't a creature
- `size(w, h)`, with `eye_height(blocks)` when its eyes aren't at 85% of that, `max_health`, `walk_speed`
- `hostile()` or `passive()` or `category(..)`
- `texture` (by default `entity/<name>`)
- `model(humanoid(..))`, or `r.set_model(&KIND, "thing")` for a Blockbench geo.json (any item's or entity's, also someone else's, `"#tag"` for many items). A kind with neither is drawn by `models/<name>.geo.json` if there is one, which a resource pack can replace (base's mobs are files). `weft export-models <dir>` writes every model made in code, any mod's, as a geo.json to go on with in Blockbench. Files in your `assets/minecraft/` go into the pack made from the player's Minecraft, so a model for vanilla's texture layout goes there (base's sheep)
- `r.edit(&KIND)` changes any of this on someone else's kind, the engine's too (`r.edit(&PLAYER).attach(&MANA)`), and `r.replace(&KIND)` starts it over
- `r.bone_model(..)`, `.wearable_model(..)` on an item: a model on a bone. `r.bone_motion`, `c.bone_turn`, `c.bone_position`: bones moving by themselves or by code (`docs/models.md`)
- `light(..)`, `light_rgb(..)` and `light_at(x, y, z)` for where on it the light comes from (a lantern on its head). One mob's own light changes with `.attach(&LIGHT)` then `w.set_component(mob, &LIGHT, [r, g, b])`; effects give light too (`r.entity_effect(&E).light(..)`), and `shown_slots(&[0])` makes a kind's slots seen by everyone, glowing if what's there glows (a pedestal holding a torch)
- `sound("hurt")`, `sound("death")`, `sound("ambient")`: files from `sounds/entity/<name>/`
- `.tag("no_breath_bubbles")` disables the ambient breathing particles under liquids
- `animations(..)`

## Callbacks

- `on_tick`: every entity of the kind at once, once per tick
- `on_interact`: a player right clicked one (after `actions::INTERACT_ENTITY`); true if it did something, which swings the player's arm

## Brains

```rust
r.entity(&MOB).brain(ai::Brain::new()
    .aggressive(16.0)
    .rule(ai::chase(20.0, 1.0))
    .rule(ai::melee())
    .rule(ai::wander(0.6, 10.0)));
```

`aggressive` remembers nearby visible players and anyone the preset retaliates against.
`neutral` reacts to being hit by a player. `timid` records a threat for `panic` to act on.
Anger and fear are ordinary saved instance components. Supply your own condition to
`sight_when`, for example to make a spider hunt in the dark.

Add your own named `ai::Rule::sensor` to update facts or `ai::Rule::action` to return an
`Offer` from those facts. The engine wakes rules when components declared with
`.reads(&COMPONENT)` change. A mod event can call `w.wake_brain(id, "my_rule")`
for a specific target. Use `.every(ticks)` for facts that need periodic sampling,
or call `ctx.next_in(ticks)` to schedule one check. For a held item trigger, subscribe
to `ai::HELD_CHANGED` and query either hand with `w.hands(id)`.

Actions can own `ai::MOVE`, `ai::LOOK` and `ai::USE` independently or together. An
offer must beat the current owner's priority on every control it needs.
`Offer::keep_at` sets the priority for continuing a running action. Calculate it
from the current facts, such as distance to the target or the stage of an attack.
Keep the same target key while pursuing it so a change of priority alone preserves
the action. The action callback receives `Start`, `Update`, `Stop`, and optional
`Step` calls from `.step_every(ticks)`. An action callback can call
`w.navigate` or use `ai::navigate` for standard movement offers.

Use `ai::share` in a group alert to write a fact to nearby members selected by your
predicate. Rules watching that fact then decide how each member responds.

`ai::Nearby::new(origin, radius)` finds living bodies. Use `.kind(&SPECIES)`,
`.excluding(ctx.id())`, and `visit`, `nearest`, `list` or `count`. Local queries
in one brain pass can share candidates while still testing the exact requested
origin and radius. Filter by tag with `TAG.on_entity(entry.kind)` in a visitor.
Use `w.sensed_block` or `w.nearest_block` for nearby terrain, `w.looked_at`
for a direction, and `w.block_path_clear` for a straight sweep of the current
body box against blocks.

`ai::Vision` controls range, angles, near blind distance and block occlusion.
Use several profiles with different yaw or pitch offsets for separate fields of
view. `ai::Scent` scores the strength of a local source after distance falloff.
`ai::Touch` checks bodies in range against the collision box or a named hitbox
part in its current pose. Use any of these in your own sensor or action offer.

Read a source entity's component with `ctx.observe(source, &COMPONENT)` while
sampling or offering. A change through SDK or system writes, or the source
leaving the region, wakes the rule. Each sample refreshes the subscriptions. Use periodic
sampling where a source can enter or leave a sensor's range by moving.

Use `.without_brain_rule("core:chase")` to edit the kind. Use
`w.enable_brain_rule(id, name, enabled)` and `w.install_brain_rule(id, &SOURCE, name)` to
edit the rules of a single entity. Instance edits are saved by name. Inspect a running rule
with `w.brain_action(id, name)` or log transitions with `--log brain=debug`.

Game masters can use `/gizmo mob_brain` to see active rules and their keep
priorities above nearby mobs, and `/gizmo mob_path` to see remaining route
segments and target markers. Use `ai::Rule::vision_sensor(name, profile, sample)`
to pass an `ai::Vision` profile to a sensor callback and expose its geometric
limits to `/gizmo mob_vision`. The orange boundary marks the near blind zone.
The outline shows the profile before per-target obstruction checks.
The same group also shows the nominal player detection distance as a sphere.
The built-in sight and watch-player rules expose their range. Custom rules can
declare theirs with `.player_notice_range(range)`. From inside a volume, the
overlay tints the view at 20% opacity.

### Watching someone's gaze

Call `ctx.watch_gaze(viewer, ai::Gaze::at(ctx.id(), range).part("head").for_ticks(20))`
from a sensor or offer. The rule wakes on entry, exit and threshold crossing.
`hit()` returns the current hit, `held()` says whether it has reached the
threshold, and `entered()`, `left()` and `reached_threshold()` are consumed
on read. Transitions can coalesce. A threshold crossing expires when that
focus ends. Leaving off the part filter keeps the same target in focus even if
the viewer moves between its parts. Use a named eye hitbox for a tighter check.

Use `for_real_time(Duration::from_secs(1))` for monotonic server time. A gap
over 250 ms or two configured tick periods, whichever is longer, resets
real-time watches. Simulation-time watches count sampled server ticks. The server shares a single block-occluded entity ray per
watched viewer each tick. Use a local entity query on an interval to pick new
viewers and refresh a rule's watches by reading them when it runs.

## Navigation

Pass `ai::Navigation::walking()` or `ai::Navigation::swimming()` to
`w.navigate_with(id, target, speed, &profile)`. A swimmer uses the liquids
declared in `SWIMS_IN`. It changes depth freely, checks the whole body against
solids and liquid surfaces, and follows a clear straight leg before trying a
cell search. Long straight routes are checked in successive legs.
On the ground, clear supported routes and final approaches can use fractional
coordinates. The cell search still handles terrain detours, steps and drops.

The profile can price a block or tag with `block_cost` or `tag_cost`. A cost
of one gives ordinary distance, a larger weight makes travel more expensive,
and a smaller positive weight favors that material. The last matching rule
takes effect. `open_cost` prices cells without a wall beside the body. The
fast straight probe can bypass the search when the segment costs the minimum
allowed by its profile. A profile that prefers a wall can still search across
open water.

Create options from the mob's current components or senses each time the
behavior changes its route. A different profile replaces the existing route.
If the creature must visit a material even when it takes a longer route,
select that material as a waypoint in the behavior.

## Hands

A mob can use the same hand calls as a player. `equipment()` gives it armor
slots and an off hand. `w.entity_modify(id).selected(slot)` chooses a main hand
slot, `w.use_item(id, off_hand)` uses it, and `w.use_item_on` targets a block.
The melee preset uses the same reach, swing strength and attack permission as a
player. `w.start_mining(id, pos)` mines with its tool and `picks_up_items()`
lets it take dropped stacks.

## Projectiles

`projectile(Projectile::new().damage(2.0).sticks(1200))`: it flies as a projectile and the engine moves it. Each tick a ray along its speed decides what it hits first (blocks by what stops a body: through grass), so nothing is flown through however fast, and it sticks right at the face it met: on a moving body it rides along, once its block is gone it falls. It points where it flies, the whole model tipping as it falls. In a mob it stays in its box, held by each client on the body as drawn, turning with the body and not the head, until the mob dies; and off something it did no harm to it bounces back, as vanilla's. `actions::PROJECTILE_HIT` asks (no, and it flies on past them: arrows through your own team) and tells (`owner`, `target`, the block, the point). An entity hit takes `chain::PROJECTILE_DAMAGE` (its damage times its speed, rounded up: vanilla's arrow), going away from where it flew. Shoot one with `w.spawn_with(&ARROW, eye, &Spawn::new().owner(me).velocity(v))`: it won't hit its owner until it has flown clear, and its hits are the owner's. With `.attribute(&attribute::GRAVITY, 0.625)` it falls like vanilla's arrow; that also sets any other attribute's base for the kind

For a shot relative to its shooter, add `.inherit_velocity()` to `Spawn` alongside
`.owner(shooter).velocity(launch_speed)`. This adds the shooter's velocity once at spawn,
including its mount or moving platform; `velocity` alone remains an absolute speed.

## Coming, going and getting hurt

An item with `comps::BLOCKS_ATTACKS` protects its user while held in use. Pair
`.use_until_released()` with a `Blocking` value: `delay_ticks`, the frontal `arc` in
degrees, and `wear_threshold`, `wear_base` and `wear_factor` for durability loss.
Blocked attacks cause no health loss or knockback. The
`damage.bypasses_blocking` tag on a damage cause skips this protection independently
of armor; `DamageCauseBuilder::bypasses_blocking()` sets it. A weapon's
`comps::DISABLE_BLOCKING` value disables blocking for that many seconds. The base
shield activates immediately, has a 180-degree arc, and axes disable it for five seconds.

`comps::CATCH_PROJECTILES` takes a `ProjectileCatch`: a model bone, bounds in its
pixels and an incoming direction. Sticking projectiles blocked by that item become
`comps::EMBEDDED_PROJECTILES` stack data. They stay with the item through hand changes,
drops and saves, and follow its model's bone. Other clients receive held stack data
through the normal entity fields. Up to 32 projectile models are retained per stack.

- comings and goings: `actions::SPAWN` asks before anyone sees a new one (`cause`: `spawn_cause::NATURAL`, `EGG`, `COMMAND`, `MOD`, set with `Spawn::new().cause(..)`, and `DROP` for a dropped stack) and tells after: a zombie in random armor is that event. `ENTITY_REMOVED` tells when one leaves the world and why (`removal::DIED`, `DESPAWNED`, `UNLOADED`, `PICKED_UP`, `HIT` ...): a boss bar gone with its boss. Interactive and moving blocks use the block events. What despawns far from players is its category's; `w.entity_modify(id).persistent(true)` keeps one, and so does naming it or it picking something up, as vanilla's
- `on_hurt(|w, hurt| ..)`, `on_died(|w, death| ..)`: `actions::HURT` and `actions::DIE` for this kind. The damage itself is the `chain::DAMAGE` chain and `actions::HURT` can stop it, for any kind, yours or not. Every hit has a cause (`damage_causes::FALL`, `LAVA`, `PROJECTILE` ...); your own is `r.damage_cause(&RADIATION).bypasses_armor().death_message("death.mymod.radiation")`, and a kind tagged `immune/fall` or `immune/mymod:radiation` takes none of it. From code a hurt is described: `w.damage(id, Damage::new(4.0, &RADIATION).by(who).through(arrow).knock(dir))`, every step seeing who, through what (`direct`), with what and whether it crits. Healing is the same call with a negative amount (`w.heal` is that), going through `chain::HEAL` and the `ENTITY_HEALED` event instead

## Riding

riding: `.seat(Seat::at([0.0, 1.0, 0.0]).steers())` is a place to sit, as many as you like, each with its own point in the mount's frame (+Z ahead) and `.exit(Exit::offset(..))`, `Exit::nearest(2.0)` (the default), `Exit::stay()` or `Exit::call(|w, mount, rider, nearest| ..)` for where its rider gets off. Riders ride riders, as deep as you like, never in a ring. Who sits in a seat that `steers` steers it: a mob with its own AI (a zombie jockey), a player with its keys through `.steering(..)`, which runs on that player's client and gives what the mount does; `steer::walk_turn`, `walk_look`, `strafe`, `fly`, `jump` and `charged_jump` are ready, `.and(..)` puts two together, `Steering::new(|keys, mount| ..)` is your own: `keys.axis(&keys::FORWARD, &keys::BACK)`, `keys.held(..)` and `keys.state(..)` read any action, its `held_ticks` and `released` being a charge (how long it is held, and was before being let go this tick), and `keys.yaw` is where the rider looks. Nobody steering, its own AI moves it. `.rider_rules(RiderRules::new().hits(|w, mount, rider| ..).builds(..).hurt_reaches_riders(0.5))` says what riders may do, by any condition; `.riders_fall()` passes its falls on; `.ride_camera(blocks)` how far back the third-person camera goes; `.rideable()` gets on with a right click. From code `w.mount(rider, mount)`, `w.mount_seat(..)`, `w.dismount(rider)`, `w.vehicle(id)`, `w.riders(mount)`, and `actions::MOUNT` / `DISMOUNT` to refuse or follow them. Players get off with `core:dismount`, Shift unless rebound. A teleport, to another dimension too, takes a mount's riders along and a rider off its mount. Riders sit by their model's `ride` clip (legs forward on any two-legged one) instead of walking. Testmod's steed shows it all

For seats on block-built bodies, `Exit::stay()` looks for the deck at the seat up to half a block above it.
Use `Exit::stay_on_deck(max_rise)` to set the upward search distance for a seat.

## Moving platforms

Moving platforms: `solid()`, `kinematic()`, `carry_height(..)`, and
`body(&DECK)` with a template (`r.template(&DECK).fill("base:oak_planks", from, to)`) makes a flying structure players can build on; its box is what the blocks fill, no `size` needed. `EntityState.size` is an entity's box and eye height as they are now (crouching and `w.set_size` included), so `st.pos.y + st.size.eye` is where its eyes are (or just `st.eye()`).


## Player identity

`w.player_name(entity)` reads a player's display name and `w.player_key(entity)` reads their persistent public ID. Both return `Option<String>` and work across regions and dimensions. A missing or disconnected player returns `None`.

Names can repeat. The key is the same identity used for player saves and permissions, and survives reconnects, respawns and server restarts. Save it with your mod's records to associate permissions or ownership with an offline player. Read the current entity's key again when the player joins to find those records.
