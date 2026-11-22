# Rules

A **rule** is something a block does by itself: *when* something happens, *if* a condition holds, *then* the block changes. Crops drying, sand falling, grass dying under a block and torches breaking off a wall are all rules.

```rust
r.block(&FARMLAND)
    .prop("moisture", 0..=7)
    .rule(Rule::random(300.0)
        .when(Cond::new("light(..4) & !beside(#water)"))
        .becomes("moisture=0"));
```

A rule has three parts, always in this order: `Rule::<when>().when(<if>).<then>()`. A rule without `.when(..)` always acts.

## When

- `Rule::random(mean_secs)`: every `mean_secs` on average
- `Rule::neighbor()`: when something next to the block changes
- `Rule::after(secs)`: `secs` after something next to the block changed

There are no random ticks. Rules, [spreading](blocks.md#spreading) and scheduled ticks all run on world time, so an unloaded area catches up on the time it missed when it loads.

## If

`.when(condition)` takes a [condition](details/conditions.md) about the block's own cell. Call it again to add more: all of them have to hold. The ones that matter most here:

- `side(down, #soil)`: that side touches a matching block. Without a matcher, it matches any solid block
- `beside(#water)`: a horizontal neighbor is a matching block. `beside(down, #water)` checks around the cell below
- `open(down)`: that side is air or [replaceable](blocks.md#plant)
- `covered`: the cell above is opaque
- `light(9..)`: the light level is in that range
- `state(age=7)`: the block is in a state with these properties

`side` and `open` name a side of the block's own frame, so they turn with the block (`down` of a block that lies on its side is not the world's down).

Conditions combine with `&`, `|`, `!` and parentheses, in text (`Cond::new("night & !covered")`) or in code (`Cond::light(9, 15)`, `!Cond::covered()`).

## Then

- `.becomes("age=0")`: sets some of its own properties
- `.becomes("dirt")`: turns into another block. `[..]` picks a state: `"base:farmland[moisture=0]"`
- `.grows()`: its `on_grow` runs, or `age` goes one higher if there is none ([Plants and growth](blocks.md#plants-and-growth))
- `.breaks()`: breaks off, dropping what it drops
- `.falls()`: falls like sand, as a moving block
- `.sheds("north=false")`: a piece comes off: it becomes that, dropping what the piece drops (a side of an `on_sides` block)

A name without a namespace in `becomes` belongs to your mod as usual.

## Shortcuts

Many of the block builder's calls are rules written for you. You'll often only need these:

- `.needs_below("#soil")`: `Rule::neighbor().when(!Cond::side(Dir::Down, "#soil")).breaks()`
- `.needs(Dir::West, "#wall")`: the same for another side. An empty matcher (`""`) means any solid block
- `.needs_near("#water")`: breaks unless the block it stands on has one of these beside it, or is this same block (sugar cane)
- `.grows(secs, min_light)`: `Rule::random(secs).when(Cond::light(min_light, 15)).grows()`
- `.smothered_into("dirt", secs)`: `Rule::random(secs).when(Cond::covered()).becomes("dirt")`
- `.falling()`: `Rule::after(0.1).when(Cond::open(Dir::Down)).falls()`

A `neighbor` rule that breaks the block also keeps it from being placed where it would break right away, so `needs_below` doubles as a placement check.

[`spreads`](blocks.md#spreading) works on the cells around the block instead and isn't a rule of the block itself.

## Rules and states

`.rule(..)` goes on a block (every state) or inside `.states(..)` (only that state). For a rule that only holds in some states, a condition does the same thing with less code: `.when(Cond::state("age=7"))`.

## Several blocks in one cell

Rules hold for every block of a cell, each by its own: a crop in a cell with snow still grows, and breaks off by itself when its soil goes. See [several blocks in one cell](blocks.md#several-blocks-in-one-cell).

