# Blocks

## Your first block

```rust
use weft_sdk::*;

declare_mod!(deps: ["base"], init: init);

static RUBY_ORE: Block = Block::named("ruby_ore");

fn init(r: &mut Registry) {
    r.block(&RUBY_ORE);
}
```

`RUBY_ORE` is a handle, and you use it to reference your block throughout the code. You only write a namespace for blocks of other mods (`Block::named("base:stone")`).

The block then needs two files:

- `assets/textures/block/ruby_ore.png`
- a name in `assets/lang/en_us.lang` (replace `mymod` with your mod id):

```
block.mymod.ruby_ore = Ruby Ore
```

Without more calls it gets:

- the texture `block/<n>` on all six faces
- a new item that places it (`.no_item()` removes it)
- its own item as its drop
- a place in your mod's `misc` creative tab (`.tab(&base_api::tabs::BUILDING)` picks another)

## Quick navigation

Weft lets multiple blocks coexist at the same position (waterlogging, slabs of different blocks). A position in the world (like -14 92 205) is called a *cell*, and a cell holds one or more blocks.

Jump to a section you're interested in, or just read the page top-to-bottom.

| I want a block like... | Section |
|---|---|
| furnace, chest | [Block entities](block_entities.md) |
| flower, grass, crop | [Plants and growth](#plants-and-growth) |
| stairs, slabs, beams | [Stairs, slabs and other shapes](#stairs-slabs-and-other-shapes) |
| fence, pane, pipe, cable | [Connect to neighbors](#connect-to-neighbors) |
| ladder, vine | [Blocks on the sides of a cell](#blocks-on-the-sides-of-a-cell) |
| candles, turtle eggs | [Breaking in pieces](#breaking-in-pieces) |

| I want to customize... | Section |
|---|---|
| texture, tint, layers | [Textures](#textures) |
| mining duration, required tool, drop | [Mining and drops](#mining-and-drops) |
| light, transparency, sounds, particles | [Light, transparency and sounds](#light-transparency-and-sounds) |
| shape, hitbox | [Shape and hitbox](#shape-and-hitbox) |
| multiple looks (on/off, growth stage) | [States](#states-blocks-that-change) |
| what happens on right click, on place, on break | [Events](#events) |
| damage on contact | [Hazards](#hazards) |
| spreading (like grass) | [Spreading](#spreading) |
| what happens by itself (crops drying, sand falling) | [Rules](details/rules.md) |
| waterlogging | [Several blocks in one cell](#several-blocks-in-one-cell) |
| a block from another mod | [Changing something of another mod](details/editing-others.md) |

## Mining and drops

```rust
r.block(&RUBY_ORE)
    .hardness(3.0)
    .mined_by("pickaxe", tool_tier::IRON)
    .drops("ruby", 1);
```

- `hardness(h)`: breaking takes `h` × 1.5 s when the tool is allowed to harvest the block and `h` × 5 s when it is not, divided by the tool's speed. `0.0` is instant
- `mined_by(class, level)`: tools of that class mine it faster, and it drops nothing to a tool below tier `level`
- `drops(item, count)`, `drops_nothing()`: changes the default drop or removes it

**Tiers:** `WOOD` 0, `STONE` 100, `IRON` 200 and `DIAMOND` 300. Other numbers work (testmod has a tier-250 pickaxe). `tool_tier::HAND` drops for any tool or none: dirt is `.mined_by("shovel", tool_tier::HAND)`, so a shovel is faster but not required.

For several possible drops, chances or conditions, use `r.loot`: see [recipes and loot](details/recipes-loot.md).

## Tags and components

A **tag** groups things (`#sand`), and a **component** is a named value on a thing (`fuel = 300`):

```rust
r.block(&RED_SAND)
    .tag("sand")        // cactus and sugar cane accept anything tagged sand
    .set(&FUEL, 300);   // burns in a furnace (base_api::components::FUEL)
```

[How to find out what tags already exist?](details/components-and-tags.md)

## Textures

Start with `texture(t)` to set the texture for all faces, then override some:

| call | faces |
|---|---|
| `texture(t)` | all six |
| `top(t)`, `bottom(t)` | one each |
| `ends(t)` | top and bottom |
| `sides(t)` | the four around |
| `north(t)`, `south(t)`, `west(t)`, `east(t)` | one each |
| `front(t)`, `back(t)` | north and south: the front is where `.facing(..)` turns it ([States](#states-blocks-that-change)) |

When two calls cover the same face, the last one wins. Names are relative to your mod's `textures/block/`, and `"base:stone"` borrows *base*'s.

*base*'s crafting table:

```rust
r.block(&CRAFTING_TABLE)
    .texture("crafting_table_side")
    .bottom("crafting_table_bottom")
    .top("crafting_table_top")
    .north("crafting_table_front")
    .south("crafting_table_front");
```

### Layers and tints

```rust
r.block(&GRASS)  // grass block from base
    .texture("dirt")
    .top("grass_top").tint_kind(&tints::GRASS)
    .sides("dirt").layer("grass_side_overlay").tint_kind(&tints::GRASS);
```

`layer(t)`, `tint(..)` and `tint_kind(..)` apply to the faces picked by the most recent face call (`texture`, `top`, `sides`...).

- `layer(t)`: draws another texture over those faces
- `tint(0xRRGGBB)`: multiplies their top layer by a fixed color
- `tint_kind(&tints::GRASS)`: colors their top layer by the biome instead. The engine provides `tints::GRASS`, `tints::FOLIAGE` and `tints::WATER`. To make your own, see [tint kinds](registration.md#tint-kind).

`tint` makes a tinted copy on load, with its frames and connected tiles, so one white wool texture serves every color:

```rust
r.block(&RED_WOOL).texture("wool").tint(0xB02E26);
r.block(&BLUE_WOOL).texture("wool").tint(0x3C44AA);
```

Each layer is drawn by itself over the one under it, with its own animation, connected textures and tint: see [textures](textures.md) and [CTM](ctm.md).

### Flat item texture

`.item_texture("torch")` uses a flat texture from `textures/item/` instead of the block's model.

## Light, transparency and sounds

### Light

`.light(15)` is full-strength white light. Higher values are allowed and reach further, not brighter. `.light_rgb(r, g, b)` sets a colored light instead.

### Transparency

- `.cutout()`: draws each pixel either fully or not at all (alpha of 0.1 or less is dropped), and by default lets light through (leaves, glass panes). A block made of boxes or a model file is cutout already
- `.translucent()`: draws pixels with partial alpha, so what is behind shows through them, and hides faces between neighbors of the same block (stained glass, ice)
- `.cull_same()`: does only the hiding
- `.light_opacity(n)`: 0 to 15: how much brightness the block removes from light passing through
- `.light_filter(r, g, b)`: instead changes the light's color by absorbing each channel separately (stained glass)

`light_opacity` is 15 for opaque blocks and 0 for everything else by default, so you only need it for in-between cases: leaves is 1, ice is 2.

### Sounds and particles

A **particle** is a small visual effect, like smoke or sparks. By default the engine already throws pieces of the block when it is broken, hit, landed on or run on.

- `.sound_group("base:block.stone")`: plays that group's `break`, `place`, `step` and `hit` sounds. Without a namespace the group is your mod's (`"stone"` means `mymod:block.stone`)
- `.effect(event, sound, particle)`: replaces the sound group's sound for one of those events (`break`, `place`, `step`, `hit`, `land`, `sprint`) with a sound event, and the default particles with a particle kind
- `.ambient_particle("mymod:spark")`: shows that particle around the block now and then, on clients only (torch smoke)

In `.effect`, `""` keeps the default of that part and `"none"` turns it off: `.effect("step", "mymod:squelch", "")` plays your sound and keeps the default particles.

To register your own particles, see [particles and sounds](details/particles-sounds.md). That page also covers the same events for entities.

## Shape and hitbox

A **model** is the block's visible shape. Boxes use 1/16-block units, from `[0, 0, 0]` (west, bottom, north) to `[16, 16, 16]`, and each call adds one box:

```rust
r.block(&CABLE).model_box([5.0, 5.0, 5.0], [11.0, 11.0, 11.0]);
```

Or use a Blockbench (3D model editor) model at `assets/models/block/<n>.json` ([models](models.md#blocks-and-items)).

Two calls change the box you just added:

`.tilted(origin, axis, degrees)` tilts it around a point (in 1/16 units) about an axis. *base*'s torch:

```rust
.model_box([-1.0, 3.5, 7.0], [1.0, 13.5, 9.0])
.tilted([0.0, 3.5, 8.0], Dir::South, -22.5)
```

`.turned(turns)` rotates the texture on each face of the box without moving the box. It takes one number per face, in the order -X, +X, -Y, +Y, -Z, +Z, and each number is a count of quarter turns (90°). *base*'s piston rod turns its four sides and leaves the top and bottom as they are:

```rust
.model_box([6.0, 12.0, 6.0], [10.0, 28.0, 10.0])
.turned([1, 1, 0, 0, 1, 1])
```

**Sharing a model file:** blocks can share one with `.model_file("some_mod:hollow_log")`. The file leaves some textures open as named slots (like `#inside`), and `.model_texture("inside", "stripped_oak_log")` fills the slot for this block. See [models](models.md#blocks-and-items) for how to define slots. Both calls also work inside `.states(..)`.

### Hitbox

The **hitbox** stops walkers. `.hitbox(min, max)` adds one box per call, the parameters are two corners in 1/16 units (`min` is the west, bottom, north corner). A slab:

```rust
.hitbox([0.0, 0.0, 0.0], [16.0, 8.0, 16.0])
```

The default is the model's boxes, else a plant's cross, else the whole block. On a non-solid block it only sets the outline.

The crosshair outline and the break cracks follow the hitbox unless you set `.outline(min, max)`, which takes the same two corners. A hollow log that's stopped by its walls but outlined as a whole cube:

```rust
.outline([0.0, 0.0, 0.0], [16.0, 16.0, 16.0])
```

## States: blocks that change

A **property** is a named value of a block, and a **state** is one combination of values (`facing=north,lit=true`). The first value is the default:

```rust
.prop("lit", ["false", "true"])
.prop("age", 0..=7)
```

`.states(|p, s| ..)` runs once for every combination of the block's properties.

- `p` (properties) holds that combination's values: `p.is("lit", "true")`, `p.get("age")`, `p.int("age")`.
- `s` (state) builds that one state. Anything set on `s` goes on top of the block's own settings.

The *base* furnace:

```rust
r.block(&FURNACE)
    .facing(Facings::HORIZONTAL, Toward::Player)
    .prop("lit", ["false", "true"])
    .states(|p, s| {
        let lit = p.is("lit", "true");
        let s = s.texture("furnace_side").ends("furnace_top")
            .front(if lit { "furnace_front_lit" } else { "furnace_front" });
        if lit { s.light_rgb(13, 11, 8) } else { s }
    });
```

`.states` sees only the block's own properties. To change just the states that match some values, including another mod's properties, use `.state_where(key, |s| ..)`. `key` is `prop=value,..` and picks the states that have every value it names. It goes on top of `.states(..)`:

```rust
r.edit(&base_api::blocks::FURNACE).state_where("lit=true", |s| s.light(15));
```

`r.edit` changes a block registered elsewhere: see [changing something of another mod](details/editing-others.md).

### Reading and changing properties

From code (`w` is the world, see the [world API](world-api.md)):

```rust
w.prop(pos, "lit");                 // Some("true")
w.prop_set(pos, "lit", "false");
```

`base:furnace[lit=true]` picks states by value wherever a block is written as text: structures, `r.set(..)`, lang files, commands, etc.

In r.set:

```rust
r.set("base:furnace[lit=true]", &HARDNESS, 5.0);
```

In a lang file:

```
item.mymod.ruby.tooltip = Smelts in a {block:base:furnace[lit=true]}.
```

In a command:

```
/setblock ~ ~ ~ base:furnace[lit=true]
```

### Facing and axis

`.facing(allowed, toward)` adds a `facing` property that automatically turns the block when it's placed. `allowed` is `Facings::HORIZONTAL`, `Facings::ALL` or `Facings::only(&[Dir::Up, Dir::North])`.

| `toward` | the block's front looks... |
|---|---|
| `Toward::Player` | at the player (furnaces, chests) |
| `Toward::Face` | out of the clicked face (torches, ladders on a wall) |
| `Toward::Away` | where the player looks (stairs) |
| `Toward::Hit` | away from the side of the cell nearest the click (vertical slabs) |

Build the model as if the block faces north, top up.

`.axis()` adds an `axis` property (`y`, `x` or `z`), set from the face the player clicked. Build the model standing up (along `y`) and the engine lays it along the axis automatically (like a log).

### Custom turns

`.facing(..)` and `.axis()` turn the model for you. For any other turn, set the state's **orientation**. You always write the model facing north, top up, and the orientation tells the engine how to turn all of it (boxes, model file, hitboxes, cube faces, liquid walls) from there. *base*'s piston head points its top the way it faces:

```rust
r.block(&PISTON_HEAD)
    .facing(Facings::ALL, Toward::Face)
    .states(|p, s| s.orientation(Orientation::up(p.facing())));
```

A state's orientation replaces the one `.facing(..)` would give, so you only need it where the front is not the part that turns.

`Orientation::...` turns the model so:

- `front(dir)`: its front looks to `dir`, top up (what `.facing(..)` does by default)
- `up(dir)`: its top points to `dir` (a piston head)
- `new(attach, front)`: its bottom is against `attach` and its front looks to `front` (a bed: `new(Dir::Down, p.facing().opposite())`)
- `leaning(attach)`: it stands upright with its west side against `attach` (a torch on a wall)
- `axis("x")`: its top lies along the axis `x`, `y` or `z` (a log)

If `front` is along `attach` in `new`, the engine picks another front. The [reference](reference/blocks.md) lists the rest.

`state_turned(state, o)` returns the state of any block turned by `o`: its `facing`, `axis` or sides are turned for you, and a block that can't turn comes back unchanged. Use it when you place something turned (a structure) or turn a block yourself (a wrench):

```rust
let turned = state_turned(w.block(pos), Orientation::front(Dir::East));
w.set_block(pos, turned);
```

### Placement

`.placement(|place, props| ..)` runs when a player places the block, after `.facing(..)`, and sets the properties `facing` doesn't cover. *base*'s slabs pick the half from where the click landed:

```rust
r.block(&SHELF)
    .prop("half", ["bottom", "top"])
    .placement(|place, props| {
        props.set("half", if place.hit().y > 0.5 { "top" } else { "bottom" });
    });
```

- `place.face()`: the face that was clicked
- `place.hit()`: the point just outside that face, from 0 to 1 from the cell's corner. `hit().y > 0.5` is the upper half whether the click was on a floor, under a ceiling, against a wall or on a slab
- `place.look_dir()`: where the player looks
- `props`: the properties of the block being placed. `props.facing()` already holds the result of `.facing(..)`, and `props.set(name, value)` changes one

*base*'s door reads `hit()` this way to choose its hinge side.

### Merging into a cell

`.merge(|place, p| ..)` runs when a player places the block into a cell that already holds that block. `p` holds the properties of the block already there, and `place.placed("type")` is the value the new item would have had. Setting a property takes the item in, and leaving `p` alone places the item as usual. *base*'s slab:

```rust
.prop("type", ["bottom", "top", "double"])
.merge(|place, p| {
    if place.placed("type") != Some(p.get("type")) {
        p.set("type", "double");
    }
})
```

A top slab placed into a bottom slab's cell makes the block `double`. A bottom slab placed there has the same `type` as what's already in the cell, so it is placed as usual.

## Events

```rust
r.block(&LAMP)
    .prop("lit", ["false", "true"])
    .states(|p, s| if p.is("lit", "true") { s.light(15) } else { s })
    .on_use(|u| {
        let (w, pos) = (u.world(), u.pos());
        let next = if w.prop(pos, "lit") == Some("true") { "false" } else { "true" };
        w.prop_set(pos, "lit", next);
        true
    });
```

- `.on_use(|u| ..)`: when a player right-clicks it
- `.on_place(|w, change| ..)`, `.on_break(..)`: when one of these blocks was placed or broken, handled at the end of the tick (1/20 s)
- `.on_neighbor(..)`, `.on_scheduled(..)`: when a neighbor changed, or a tick you scheduled arrived
- `.on_carried(..)`: when the block is part of a moving body and the body reached a new cell of the world (a drill, a plow)

For more hooks, see [events](events.md). This section lists events exclusive to blocks.

**`on_use` details:**
- `u` gives `u.world()`, `u.pos()`, `u.player()` and `u.sneaking()`.
- Return `true` if you handled the click, `false` to pass it on (a sneak-click that does something else).
- By default a sneaking player can use a block only with empty hands. `.sneak_use(sneak_use::ALWAYS)` sends every sneaking click to `on_use`, and you check `u.sneaking()` yourself. `sneak_use::NEVER` sends none even if sneaking with empty hands.

`on_place` and `on_break` run for this block only: `actions::PLACE_BLOCK` and `actions::BREAK_BLOCK` cover every block.

## Hazards

A **hazard** lets the block deal damage. *base*'s cactus (`PRICKED` is a damage cause registered with `r.damage_cause(..)`):

```rust
.hazard(Hazard::touch(1.0, &PRICKED))
```

| hazard type | triggers when the entity is... |
|---|---|
| `touch` | within a sixteenth of the shape |
| `inside` | in the shape (fire, a berry bush) |
| `on` | on top of it (magma) |
| `head` | eyes inside the shape (like suffocation) |

Another example:

```rust
r.block(&BUSH)
    .hazard(Hazard::inside(1.0, &PRICKED).every(10).when(Cond::sprinting()));
```

- `.every(ticks)`: deals damage every `ticks` ticks (every tick by default)
- `.fire(ticks)`: sets the entity on fire for `ticks` ticks
- `.when(condition)`: only while `condition` holds for the entity there (see [conditions](details/conditions.md))

The engine already suffocates anyone whose head is in an opaque full block, and a block tagged `hot` without hazards of its own burns like lava.

## Plants and growth

### Plants

```rust
r.block(&DANDELION)
    .texture("flower_dandelion")
    .plant()
    .replaceable()
    .hitbox([5.0, 0.0, 5.0], [11.0, 10.0, 11.0])
    .needs_below("#plant_soil !#sand"); // Any soil that's not sand
```

- `.plant()`: draws two crossed flat planes. Nothing stops a walker and it breaks instantly
- `.replaceable()`: makes the block count as empty space when someone tries to place a block at this position (grass behavior)
- `.tall_plant("lower", "upper")`: makes it two blocks tall (its two pictures, with a `tint_kind` after it on both)
- `.needs_below("#plant_soil")`: allows the block only on matching blocks and breaks it off when they are removed

### Growing

`.grows(mean_seconds, min_light)`: about every `mean_seconds`, while the light is at least `min_light`, the block grows once. By default that means moving to the next state, until the last one. `.on_grow(|e| ..)` replaces that (for example, to grow a tree or place a block on top).

Growth runs on world time, so unloaded plants catch up when their area loads. *base*'s wheat:

```rust
r.block(&WHEAT)
    .prop("age", 0..=7)
    .states(|p, s| s.texture(&format!("wheat_stage_{}", p.get("age"))))
    .plant()
    .needs_below("#crop_soil")
    .no_item()
    .grows(205.0, 9);
r.loot(&WHEAT)
    .pool(Pool::new().when(Cond::state("age=7")).item("wheat"))
    .pool(Pool::new().entry(Entry::item("wheat_seeds")));
```

### Spreading

Blocks that creep onto neighbors:

- `.spreads("#grass_spreads_to", seconds, min_light)`: turns matching blocks in a box around it into this one: by default one block to each side (diagonals too), three down and one up
- `.spread_reach(min, max)`: sets how far it spreads (moss spreads every way and up walls)
- `.spread_when(cond)`: replaces the default condition for a target
- `.smothered_into("dirt", seconds)`: turns it into another block, after about `seconds`, once something opaque covers it

**`spread_reach`:** `min` and `max` are `[x, y, z]` offsets from the block. `min` is the furthest west, down and north, `max` the furthest east, up and south. The default is `[-1, -3, -1]` to `[1, 1, 1]`, and each offset is at most 4 either way.

**`spread_when`:** the default condition is enough light above the target, and not covered. Because your condition replaces it, `min_light` no longer applies unless your condition checks light itself: `.spread_when(Cond::new("light(9.., up)"))`. See [conditions](details/conditions.md).

`.needs_below`, `.grows`, `.smothered_into` and similar ones are shortcuts for [rules](details/rules.md), and you can make one yourself.

## Stairs, slabs and other shapes

Tag a block `.tag("has_shapes")` and base gives it stairs, slabs, vertical slabs, beams and eighths, whichever mod the block is from, with its textures, hardness, sounds and recipes. A beam is a quarter of a block (1 × 1/2 × 1/2).

Base's woodcutter cuts any of these out of the block with nothing lost. For your own shapes, add recipes to `base_api::recipes::WOODCUTTING`: see [recipes and loot](details/recipes-loot.md).

To make some of the shapes yourself, use `r.stairs("#my_tag")`, `r.slabs(..)`, `r.vertical_slabs(..)`, `r.beams(..)` or `r.eighths(..)`. If a block named `<source>_stairs` already exists (for example, if you registered `mymod:ruby_stairs` yourself), that one shape isn't generated again for that source, so you can hand-make one and let the rest be generated.

### Your own shape

`r.shapes_of(name, sources, suffix)` makes one new block for every full cube that `sources` matches. Each new block is called `<source><suffix>`. It inherits everything (texture, sounds, etc.) from the source, so you only specify what's different:

```rust
r.shapes_of("wall", "#my_tag", "_wall")
    .model_box([4.0, 0.0, 4.0], [12.0, 16.0, 12.0]);
```

`sources` is matched like other block patterns, so it can exclude: `"#logs !#log_not_hollow"` leaves out what a mod tagged `log_not_hollow`. A list of only exclusions takes every eligible full cube except those.

Name them all with one line in your lang file, `shape.<name>`. `{0}` stands for the source's name:

```
shape.wall = {0} Wall
```

The new blocks are tagged `wall/<source>`, and each source `wall_of/<source>`, so one recipe covers all of them (see [recipes and loot](details/recipes-loot.md) to learn about `r.shaped`):

```rust
r.shaped(C, "#wall/{m}", 6, &["###", "###"], &[('#', "#wall_of/{m}")]);
```

#### Placeholders

To vary something per source, such as a texture, use a placeholder in the text you pass to the builder calls (texture names, model textures and so on). For the source `base:birch_log`:

| placeholder | becomes |
|---|---|
| `{source}` | `base:birch_log` |
| `{source.name}` | `birch_log` |
| `{source.mod}` | `base` |
| `{source.side}` | the source's texture on that side (`top`, `bottom`, `north`...) |

`{source.name}` and `{source.mod}` are what a file name can have: `"mymod:block/{source.name}_inner"`. Write `a | b` to take the first texture that exists. For example, this works for any log:

```rust
r.shapes_of("hollow_log", "#logs", "_hollow")
    .model_file("hollow_log")
    .model_texture("inside", "{source.side}_inner | {source.top}");
```

## Connect to neighbors

`.joins(Facings::ALL, "my_cable")` adds a `true`/`false` property per side, so `.states` can check `p.is("north", "true")`. The engine keeps the properties right as the blocks around change.

- Joining by the block's own name connects only its own kind.
- Joining by a shared tag (`.joins(Facings::HORIZONTAL, "#fences")`) also connects other mods' blocks.
- If you just want to change texture but not shape or hitbox, use [connected textures](ctm.md) instead.

`.derived("part", |n| ..)` keeps the property `part` computed from the neighbors. Your function runs whenever a neighbor changes, looks at the blocks around with `n.at(Dir::Up)`, and returns the new value for the property (stairs' corners work this way):

```rust
static PILLARS: Tag = Tag::named("pillars");

r.block(&PILLAR)
    .tag("pillars")
    .prop("part", ["middle", "top", "bottom", "single"])
    .states(|p, s| s.texture(&format!("pillar_{}", p.get("part"))))
    .derived("part", |n| {
        let above = PILLARS.on_block(n.at(Dir::Up));
        let below = PILLARS.on_block(n.at(Dir::Down));
        match (above, below) {
            (true, true) => "middle",
            (false, true) => "top",
            (true, false) => "bottom",
            (false, false) => "single",
        }
    });
```

## Blocks on the sides of a cell

```rust
r.block(&LADDER)
    .on_sides(Facings::HORIZONTAL)
    .climbable()
    .not_solid();
```

`.on_sides(allowed)` makes a one-pixel-thick layer that lies against the sides of its cell (vines, ladders).

`allowed` is `Facings::ALL` (vines grow everywhere), `Facings::HORIZONTAL` (walls only) or `Facings::only(&[Dir::Down])` (the floor only). The block gets one property per allowed side, from `north` to `down`, and the value is `"true"` for each side it lies against, so `.states` can check `p.is("north", "true")`.

## Blocks bigger than one cell

A block's hitboxes decide which cells it occupies, so a bigger block is one with bigger boxes. Boxes are in 1/16 units from the block's own cell, written as if it faces north (model boxes count too when there is no hitbox). A box from `[0, 0, 0]` to `[16, 8, 32]` reaches into the next cell:

```rust
.hitbox([0.0, 0.0, 0.0], [16.0, 8.0, 32.0])
```

- It turns with the block.
- Hitting either half selects the same block.
- Clicking an interior face places the next block against that surface, and other blocks can fill the rest of the space.

## Several blocks in one cell

Blocks share a cell when their hitboxes don't overlap (for example, two slabs of different blocks). `.shares_cell_with("#tag")` lets this block share a cell with blocks tagged `#tag` even if the hitboxes overlap (snow over a plant).

### Waterlogging

`.shares_cell_with("#water")` makes the block waterloggable. Other liquids, like lava, stay out. Slabs and stairs have this built in.

Water doesn't flow through the sides the hitbox covers whole (a stair's back holds it, a hollow log lets it out its ends). To choose the sides yourself, use `.liquid_walls(&[Dir::North, Dir::South])`, written facing north and turned with the block (`&[]` for none).

### Block entities and circuits

Each block in a cell works on its own: it can have a circuit, light, its own growth rules, its own block entity, neighbor updates, scheduled ticks, support and loot. The player breaks and uses the one they aim at.

### Changing a cell from code

- `w.parts(pos)`: lists the blocks in the cell
- `w.part_add(pos, state)`: adds one, whatever its shape. Returns false for a duplicate, an unavailable cell, or when its hitbox would overlap a block in a neighboring cell
- `w.part_remove(pos, state)`: removes one
- `w.part_replace(pos, old, new)`: changes one
- `w.block(pos)`: returns the block the cell stands for
- `w.set_block(pos, ..)`: replaces everything, unless it is the same block and only a property changes

`w.prop` and `w.prop_set` find the part that has the property. Inside a block's own callback, `w.block(pos)` and `w.set_block(pos, ..)` mean that block, so a callback works the same in a cell with others.

### Breaking in pieces

By default a hit breaks the whole block. `.piece(min, max, left, alone)` goes inside `.states(..)` and lets a player break off part of a state:

- `min`, `max`: a box in 1/16 units: a hit inside it breaks this piece
- `left`: the properties the state is left with (`"type=top"`)
- `alone`: the properties of the state whose drops the piece gives (`"type=bottom"` drops a bottom slab)

A double slab breaks one half at a time:

```rust
.prop("type", ["bottom", "top", "double"])
.states(|p, s| if p.is("type", "double") {
    s.piece([0.0, 0.0, 0.0], [16.0, 8.0, 16.0], "type=top", "type=bottom")
     .piece([0.0, 8.0, 0.0], [16.0, 16.0, 16.0], "type=bottom", "type=top")
} else { s })
```

Candles or snow layers use a piece with the whole box, with `left` one step lower (`"candles=2"`) and `alone` a single one (`"candles=1"`). Every side of an `on_sides` block is a piece already.

## Reading what a block set

Your code can read what any mod's blocks and items were set up with. This is how a mod like [Jade](https://modrinth.com/mod/jade) shows what you're looking at:

```rust
let info = block_info(state)?;
format!("{} ({}), hardness {}", block_name(state), info.owner, info.hardness)
```

`state` is a block state's id, and `block_info` returns `None` for a state that doesn't exist.

What each call gives:

- `block_info(state)`: the state's block, mod (`owner`), `solid()`, `opaque()`, light, hardness, friction, model, textures, liquid and item
- `item_info(item)`: the item's mod, stack size, durability, attack, the block it places and the slot it is worn in
- `components_of(abi::entry_kind::BLOCK, state)`: the name and schema of every component and tag on the state
- `entries_with(abi::entry_kind::BLOCK, "logs")`: every state with that tag or component, one per state
- `texture_exists("othermod:block/log_oak_inner")`: whether that mod ships the texture (resource packs don't count)
- `asset_exists(name, "models", "json")`: the same for any other file

`components_of` lists names only: read a value with the component's handle (`Component::on_block`). In `texture_exists` and `asset_exists`, a name without a namespace is your mod's.

The last two calls let you make something for every mod's blocks only where the files exist, such as hollow logs for every log that has an `_inner` texture. See [world content](details/world-content.md).

A state can override its block's components and tags, so a drill reading the block in front of it should read the state from the world. `block_info(state).name` is the block's registered name, and `name_of(abi::entry_kind::BLOCK, state)` also includes the state's properties.

```rust
static PICKAXE: Tag = Tag::named("mineable/pickaxe");

fn drill_target(world: &World, pos: BlockPos) -> Option<(&'static str, f32, Option<u32>, bool)> {
    let state = world.block(pos);
    if state == 0 {
        return None;
    }
    let info = block_info(state)?;
    Some((info.name, info.hardness, HARVEST_LEVEL.on_block(state), PICKAXE.on_block(state)))
}
```

Read a custom component with `Component::<T>::named("othermod:property").on_block(state)`. `HARDNESS.on_block(state)` gives the configured hardness. An empty schema from `components_of` is a tag.
