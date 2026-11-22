# Modifiers

A **modifier** changes an **attribute**: a number an entity has, like walking speed, reach or armor points.

```rust
r.item(&RUBY_BOOTS)
    .modifier(Modifier::add(&attribute::MOVEMENT_SPEED, 0.2).worn());
```

The list of attributes the engine reads (`attribute::*`) is in [attributes](details/attributes.md). *base* adds `ARMOR` and `ARMOR_TOUGHNESS` (`base_api::attributes`).

## The four kinds

```rust
Modifier::add(&attribute::ATTACK_DAMAGE, 3.0)       // +3
Modifier::mul(&attribute::MINING_SPEED, 0.5)        // x0.5
Modifier::percent(&attribute::MOVEMENT_SPEED, 0.2)  // +20%
Modifier::set(&attribute::GRAVITY, 0.125)           // becomes 0.125
```

- `percent(&attribute, share)`: Percents **add up** instead of multiplying: speed (+20%) and slowness (-15%) together are +5%, not 1.2 x 0.85. This is what base's potions do.

Each attribute comes with its min and max value.

For your own math, see [Represent new value with code](#represent-new-value-with-code).

## What can have modifiers

```rust
// Item in the main hand (the off hand doesn't count).
r.item(&RUBY_SWORD).modifier(Modifier::add(&attribute::ATTACK_DAMAGE, 3.0));
// `.worn()`: only when in an equipment slot
r.item(&RUBY_HELMET).modifier(Modifier::add(&ARMOR, 3.0).worn());

// Stack (useful for enchantments), replaces the item's own modifiers
// so anything not listed again is gone, and `.worn()` has to be repeated for armor.
// An empty list gives the item's modifiers back.
let sword = stack.with_modifiers(&[Modifier::add(&attribute::ATTACK_DAMAGE, 5.0)]);

// Effect
r.entity_effect(&SPEED).modifier(Modifier::percent(&attribute::MOVEMENT_SPEED, 0.2));

// Pose or gamemode
r.pose(&SIT).modifier(Modifier::mul(&attribute::MOVEMENT_SPEED, 0.3));
r.game_mode(&GHOST).modifier(Modifier::set(&attribute::VISIBILITY, 0.0));

// Player or kind of entity
r.edit(&PLAYER).modifier(Modifier::add(&attribute::CAN_SPRINT, -1.0).when(&HUNGRY));
r.entity(&COW).modifier(Modifier::add(&attribute::MAX_HEALTH, 100.0));

// One specific entity, `200` is ticks (can be 0 to stay until manually removed)
w.attribute_add_modifier(entity, "spell", &Modifier::mul(&attribute::MOVEMENT_SPEED, 0.5), 200);
// Remove it manually
w.attribute_remove_modifier(entity, &attribute::MOVEMENT_SPEED, "spell");
```

## Order

Modifiers don't run in the order you registered them. Each belongs to a **phase**:

| # | Phase | Used by |
|---|---|---|
| 1 | `phase::BASE` | a weapon's own damage |
| 2 | `phase::ADD` | `add` |
| 3 | `phase::PERCENT` | `percent` |
| 4 | `phase::MULTIPLY` | `mul` |
| 5 | `phase::RULES` | where questions are answered (not modifiers) |
| 6 | `phase::OVERRIDE` | `set` |
| 7 | `phase::FINAL` | what has to see the finished value |


`.phase(&phase::..)` moves one modifier to another phase. `r.phase` makes your own phases.

## Only while a condition holds

`.when(cond)` makes a modifier count only while a [condition](details/conditions.md). Multiple conditions should be all true at once (`AND`).

```rust
r.condition(&HUNGRY, |c| /* ... */);

r.edit(&PLAYER)
    .modifier(Modifier::add(&attribute::CAN_SPRINT, -1.0).when(&HUNGRY));
```

## Your own attribute

```rust
static MANA: Attribute = Attribute::named("mana");

fn init(r: &mut Registry) {
    r.attribute(&MANA, 100.0, 0.0, 1000.0); // default, min, max
}
```

- `.rule()`: base value can be changed as a world rule with `/rule`, or set for a single entity with `/attribute`.

## Reading a value

```rust
let speed = w.attribute(entity, &attribute::MOVEMENT_SPEED);

let (speed, why) = w.attribute_explain(entity, &attribute::MOVEMENT_SPEED);
```

`attribute_explain` also returns the steps that led to the number, which is the first thing to print when a value isn't what you expected.

## Represent new value with code

Add, multiply and set cover most things. For the rest, create a **step**:

```rust
static CAPPED: Step = Step::named("capped");

fn init(r: &mut Registry) {
    r.step(&CAPPED).phase(&phase::FINAL).run(|m| {
        m.value = m.value.min(m.arg);
    });
}

// A ring that never lets speed go past 1.5:
r.item(&RING).modifier(Modifier::step(&attribute::MOVEMENT_SPEED, &CAPPED, 1.5).worn());
```

`m` is a `Modify`:

- `m.value`: the value so far.
- `m.base`: where the chain began (an attribute's base, or what the caller passed in).
- `m.start`: the value when this phase began (what `percent` takes its share of).
- `m.arg`: the modifier's number. It is 0 in a step that runs on a chain (see below).
- `m.world()`, `m.query()`, `m.chain()`: where, about whom and which chain it runs in.

