# Interactive blocks

## Your first inventory block

```rust
static CHEST: Block = Block::named("chest");
static CHEST_MENU: Menu = Menu::named("chest");

fn init(r: &mut Registry) {
    r.block(&CHEST)
        .hardness(2.5)
        .mined_by("axe", tool_tier::HAND)
        .inventory(27)
        .opens(&CHEST_MENU);

    r.menu(&CHEST_MENU)
        .group("chest", Slots::chest(3).holder())
        .default_screen();
}
```

This is *base*'s chest (simplified). The block needs a texture and a name like any other block (see [Blocks](blocks.md)).

- `.inventory(27)`: each placed chest has 27 slots.
- `.opens(&CHEST_MENU)`: using the block opens this menu.
- `Slots::chest(3)`: three rows of nine slots.
- `.holder()`: the group shows the inventory of what the menu was opened on. Leave it out for slots that should be empty (like a crafting grid)
- `.default_screen()`: the window is its title, your `.group` calls in order, then the player's inventory. See [screens](ui.md#menus) if you want a different layout.

A placed block can carry inventory slots and its own component values. Blocks without either cost no per-instance storage. A holder is anything that can carry inventory: a block, an entity, or a stack in a slot.

Storage and menus are independent. A chest uses both, a crafting table can open a menu with temporary slots, and a water wheel can attach state without a menu.

### Migrating an existing mod

Move the inventory size, attached components, storages, and shown slots onto the block builder. Keep the block and component names so older worlds can restore their data. Use `World::block_instance` and `Holder::block_id` where code needs to retain the identity of a block as it moves. Systems receive typed holders from `SystemBatch::owners` and `SystemBatch::iter_mut`.

Rebuild native mods with the current SDK. The engine converts old block records when it loads them and writes the new format when it saves.

## Quick navigation

You can jump to any section you're interested in, or just read the page top-to-bottom.

| I want a block like... | Section |
|---|---|
| furnace, machine | [Slots](#slots), [State](#state), [The tick](#the-tick) |
| water wheel | [State](#state) |
| crafting table | [Crafting grids](#crafting-grids) |
| tank | [Liquid slots](#liquid-slots) |
| generator, battery, cable | [Energy](#energy) |
| backpack, ender chest | [Holders and storages](#holders-and-storages) |
| quarry (running while nobody is near) | [Keeping cubes loaded](#keeping-cubes-loaded) |

| I want to customize... | Section |
|---|---|
| what items a slot accepts | [.accepts](#slots) |
| which groups the sort button touches | [.allow_sorting](#slots) |
| which group a shift-click fills first | [.priority](#slots) |
| progress indicator (like burning), window layout | [UI](ui.md) |
| what happens every tick | [The tick](#the-tick) |
| catching up after being unloaded | [Catching up](#catching-up) |
| keep state when broken | [Keeping state when broken](#keeping-state-when-broken) |
| smelting or crafting | [Recipes](#recipes) |
| redstone behavior | [Redstone](redstone.md) |

Examples to copy from:

- `mods/base/src/block_entities.rs`: the furnace and the chest
- `mods/testmod/src/lib.rs`: a generator, an electric furnace and a battery on the energy contract, plus a `cable` block

## Slots

A **slot group** is a named set of slots with rules of its own. The block's inventory holds your `.group(..)` calls that end in `.holder()`, in order, so `.inventory(n)` is their total.

*base*'s furnace, its slots:

```rust
r.block(&FURNACE)
    .inventory(3)      // 3 slots: input, fuel, output
    .opens(&FURNACE_MENU);
r.menu(&FURNACE_MENU)
    .group("input", Slots::single().holder())
    .group("fuel", Slots::single().accepts("#fuel").priority(1, "").holder())
    .group("output", Slots::single().output().holder())
    .default_screen();
```

The first argument of `.group` is the name code and screens use.

`Slots` shapes: `single()`, `row(n)`, `column(n)`, `grid(cols, rows)`, `chest(rows)` (`rows` of 9 slots)

### How the slots behave

- `.output()`: items only come out (the output slot like in the crafting table).
- `.input()`: items only go in and the player can't take them back out (a machine uses them up).
- `.accepts("#fuel")`: only these items.
- `.limit(n)`: stack size.
- `.priority(n, "#fuel")`: shift-click prefers this group for the matching items. `""` matches only items in `.accepts`.
- `.allow_sorting(bool)`: Call it after the shape and the other calls to change it.
- `.liquid(capacity)`: see [Liquid slots](#liquid-slots).
- `.crafting()`: see [Crafting grids](#crafting-grids).

Sorting is off by default for `Slots::single()`, `.input()`, `.output()`, `.liquid(..)`, `.crafting()` and the player's trash slot.

### How the slots look

- `.large()`: the big slot vanilla uses for results.
- `.background("slot_fuel")`: a texture from `gui/` shown while the slot is empty, like the armor silhouettes.
- `.tooltip("mymod.fuel_slot")`: a translation shown over the empty slot.

## State

Slots hold items and liquids. Anything else a block keeps (heat, progress, a mode) is a component.

```rust
value! {
    pub struct Smelting { pub burn: u32, pub burn_max: u32, pub cook: u32, pub cook_max: u32 }
}
static SMELTING: Component<Smelting> = Component::named("smelting");

r.instance_component(
    &SMELTING,
    Smelting { burn: 0, burn_max: 0, cook: 0, cook_max: 0 },  // default
    component_flags::SAVE | component_flags::SYNC_MENU,
);
r.block(&FURNACE)
    .attach(&SMELTING)
    .inventory(3)
    .opens(&FURNACE_MENU);
```

- `w.component(pos, &C)`, `w.set_component(pos, &C, value)`: read and change.
- `.attach_with(&C, value)`: attach with another starting value.

The value lives on the server. A player's game only knows it if the server **syncs** it, which means sending a copy, and sending it again whenever the value changes. The flags choose what is kept and who gets a copy:

- `SAVE`: the value is written to the world, so it survives a restart. Without it the value starts over from its default.
- `SYNC_MENU`: players who have the block's menu open get a copy, so its screen can read it (a progress indicator needs this).
- `SYNC_WATCHERS`: every player who can see the block gets a copy.
- `SYNC_OWNER`: only the player the entity belongs to gets a copy (a player's hunger on the HUD). Use SYNC_MENU or SYNC_WATCHERS for blocks.

How another mod reads it: [components and tags](details/components-and-tags.md).

### Keeping state when broken

A charged battery or a half-done job should survive being broken. Declare its component with `instance_component` and the `CARRIED` flag:

```rust
r.instance_component(&CHARGE, 0, component_flags::SAVE | component_flags::CARRIED);
```

The broken block's item takes the value along, and placing the item puts it back on the new block. `w.component(h, &C)` and `w.set_component(h, &C, v)` read and write such a value on any holder, the stack in a player's slot included.

## The tick

`r.system(&C, f)` calls `f` in batches per region and space, once per tick. The batch includes each holder with `C`, its value, and the number of ticks to simulate.

```rust
fn tick_generators(b: &mut SystemBatch<Burner>) {
    let w = b.world();
    for (owner, burner, _ticks) in b.iter_mut() {
        if burner.burn == 0 {
            let slot = w.inventory(owner).group("fuel");
            let fuel = slot.get(0);
            // FUEL is base's component, so any mod's fuel works
            if let Some(ticks) = FUEL.on_item(fuel.item).filter(|_| !fuel.is_empty()) {
                burner.burn = ticks;
                slot.set(0, consume_one(fuel));
            }
        }
        if burner.burn > 0 {
            burner.burn -= 1;
        }
    }
}
```

Register it with `r.system(&BURNER, tick_generators)`. A chest needs no system. A furnace uses one for its ongoing work, and a block can schedule an individual action with a timer.

Give each kind of block its own state component (the generator's `Burner`, the furnace's `Smelting`), so a system only receives its own kind.

To read and change items, `w.inventory(owner)` takes the holder from the system. Outside a system, `w.inventory(pos)` addresses a block by position:

- `.group(name)`: one group of its slots, numbered from 0, with `.get(i)`, `.set(i, stack)` and `.len()`.
- `.insert(stack)`: puts the stack wherever it fits in the whole inventory, and returns what didn't fit.

Helpers for stacks:

- `consume_one(stack)`: takes one item away, a lava bucket leaves its bucket.
- `stacks_with(other)`: whether two stacks can share a slot.

Blocks don't pass items to each other by themselves. A pipe or a hopper is a tick that moves items with `insert`.

### Showing that it works

Give the block a property and flip it from the tick. The furnace has `.prop("lit", ["false", "true"])`, and every tick it does:

```rust
if let Some(state) = w.block_instance_state(owner.entity)
    && let Some(next) = state_with(state, "lit", if lit { "true" } else { "false" })
{
    w.set_block_instance_state(owner.entity, next);
}
```

The holder refers to the same part when more than one block shares the cell. See [States](blocks.md#states-blocks-that-change). `tick_furnaces` in `mods/base/src/block_entities.rs` is the whole furnace in one place.

### Catching up

The third value of `iter_mut()` (see [tick](#the-tick)) is how many ticks the call stands for. It is 1 normally. When the block's area was unloaded, or the world's time jumped, the engine doesn't tick it thousands of times. It hands over the missed ticks in up to 20 calls.

So a block that should keep working while nobody is near does `ticks` ticks of work in one go. For a furnace that is arithmetic: as many items as the fuel, the input and the output room allow. See `catch_up` in base's `block_entities.rs` and testmod's generator.

## Recipes

Anything where inputs turn into outputs over time is a **recipe kind**. A block uses the base game's kinds or one of its own.

```rust
r.menu(&FURNACE_MENU)
    .recipes(&recipes::SMELTING)
    ..;

// in the tick:
if let Some((output, time)) = recipe_process(&recipes::SMELTING, input.item) { .. }
```

- `.recipes(&KIND)` on the menu: `ui.recipe_arrow(..)` opens the kind in the recipe viewer, and the block is listed there as where those recipes are made.
- `recipe_process(&KIND, item)`: the output and the time in ticks for one input item.

Recipes are added the usual way:

```rust
r.shapeless(&recipes::SMELTING, "ruby", 1, &["ruby_ore"], 200);
```

### Crafting grids

The crafting table is a menu with a crafting grid. This is base's:

```rust
r.menu(&CRAFTING)
    .recipes(&recipes::CRAFTING)
    .group("grid", Slots::grid(3, 3).crafting())
    .group("result", Slots::single().output().large())
    .player_inventory();
```

A group made `.crafting()` is the crafting grid for the kind of your first `.recipes(..)` call. The engine does the rest:

- It shows what the grid makes in the menu's `output()` group.
- It uses the ingredients up when the result is taken, as each recipe says (`.keep`, `.wear`, `.leaves`).
- A shift-click crafts every run whose results fit in the inventory.
- `on_take` is still called, and `m.times()` says how many runs it was.

### Several inputs, outputs and a time

```rust
r.shapeless(&ALLOYING, "tin_ingot", 2, &["base:iron_ingot", "base:coal"], 100)
    .input_amounts(&[3, 1])                    // 3 iron and 1 coal
    .output_chance("base:diamond", 1, 0.05)    // and a diamond now and then
    .set(&ENERGY_PER_TICK, 32);                // data its tick reads
```

- `.input_amounts(&[..])`: how many of each input it takes, in order.
- `.output_chance(item, count, chance)`: an extra result with that chance each time.
- `.set(&C, value)`: a value of its own for your tick to read.
- `.hidden()`: keeps the recipe out of the recipe viewer.
- `.keep(key)`: an ingredient stays as it is (a mold, a catalyst).
- `.wear(key, amount)`: an ingredient stays and wears by `amount` a run, breaking when it is worn out (a hammer).
- `.leaves(key, item)`: an ingredient is used up and `item` takes its place. Without it an ingredient leaves its item's `remainder`, a lava bucket its bucket.

`key` is an ingredient's pattern character, or its place among the inputs for a recipe without a pattern.

A machine with several inputs finds its recipe from its slots:

- `recipe_find(&KIND, &stacks)`: the first recipe whose inputs, with their counts, are all among the stacks.
- `recipe.time()`: the time in ticks.
- `recipe.outputs()`: its results as `(stack, chance)` pairs. A liquid's stack is its item, counted in millibuckets.
- `recipe.value(&C)`: a value set with `.set`.
- `recipe.consume(&mut stacks)`: takes its inputs out of the stacks `recipe_find` was given. It returns false, and takes nothing, if they aren't all there.

### Your own recipe kinds

`r.recipe_kind(&KIND)` registers a kind and says how the recipe viewer shows it.

```rust
r.recipe_kind(&ALLOYING)
    .inputs(2, 1).outputs(2, 1)     // always these grids, like a crafting table's 3 x 3
    .info_line("recipe.mymod.alloying.info", &["mymod:energy_per_tick", "time"]);
```

- `.inputs(cols, rows)` and `.outputs(cols, rows)`: the grids the viewer shows, whatever a recipe uses.
- `.info_line(key, values)`: a line under every recipe. `key` is a translation like `Uses {0} FE/t for {1} s`, and `{0}`, `{1}` come from `values`: `time` in seconds, or components the recipe carries.
- `.layout(|ui, recipe| ..)`: builds each recipe yourself, with the usual UI parts plus `ui.recipe_inputs()`, `ui.recipe_outputs()` and `ui.recipe_input(i)` to place its slots. Testmod's `infusing` is one, with a liquid.

More about recipes: [Recipes and loot](details/recipes-loot.md).

## Liquid slots

```rust
.group("water", Slots::single().liquid(16_000))
```

A liquid is an item too: every liquid has one named like it, counted in millibuckets. A liquid slot holds up to its capacity of one liquid (16,000 is sixteen buckets).

In the tick it reads and sets like any slot: `w.inventory(owner).group("water").get(0)` is the water item with its millibuckets as the count.

A recipe takes a liquid as an input like any item, named like the liquid, with its millibuckets in `.input_amounts(&[1000])`. For flowing liquids and their rules see [Liquids](details/liquids.md).

## Energy

Generators, cables and machines from different mods work together through one contract in the engine.

- A block with the `ENERGY` component (`core:energy`) takes part. The component is built in, so nothing needs declaring.
- A block tagged `conductor` connects to other blocks tagged `conductor` (a cable is one).
- A **network** is the conductors that touch each other, plus the blocks with `ENERGY` that sit in one of them or next to one. A block joins through its own block when that is a conductor, or through a neighbor that is one.
- Once per tick the engine shares what the generators offer among the machines that want it.

```rust
r.block(&GENERATOR)
    .attach(&BURNER)
    .attach_with(&ENERGY, Energy { stored: 0, capacity: 20_000, max_in: 0, max_out: 200 })
    .inventory(1)
    .opens(&GENERATOR_MENU);
r.block(&CABLE).model_box([5.0, 5.0, 5.0], [11.0, 11.0, 11.0]).tag("conductor");
```

| `Energy` field | meaning |
|---|---|
| `stored` | what it holds now |
| `capacity` | the most it can hold |
| `max_in` | the most it takes from the network per tick |
| `max_out` | the most it gives to the network per tick |

A generator has `max_in: 0`, a machine `max_out: 0`, a battery both, without feeding itself. In their systems, generators add to `stored` and machines spend it:

```rust
let Some(mut e) = w.component(id, &ENERGY) else { continue };
e.stored = (e.stored + 40).min(e.capacity);      // a generator, per tick
w.set_component(id, &ENERGY, e);
// a machine: if e.stored >= COST { e.stored -= COST; work }
```

- `w.energy_move(holder, amount, simulate)`: moves energy in (positive) or out (negative), respecting the limits, and returns how much moved. With `simulate = true` it only reports it.

`w.component` and `w.energy_move` take any holder, not only a block's id: a block (`w.component(pos, &ENERGY)`) or a stack in someone's inventory, `Holder::held(player, slot)`.

An item whose stacks carry `energy` is a battery: `.set(&ENERGY, Energy { .. })` on the item gives each stack a charge of its own. A machine that charges the tool in its slot does `w.energy_move(Holder::held(id, slot), amount, false)`.

Networks are rebuilt when a conductor, or a block next to one, is placed or broken (and when its cube loads or unloads), not every tick.

There are no units, conversion or losses. Testmod uses 40 per tick and capacities in the tens of thousands, so if your mod should play along with others, stay in that range.

Cables that connect to their neighbors are covered in [Connect to neighbors](blocks.md#connect-to-neighbors).

### A machine that is also a conductor

Two machines side by side form no network on their own: a conductor has to touch them. A block can be both, with the `conductor` tag and a block with `ENERGY`. This is *testmod*'s electric furnace:

```rust
r.block(&ELECTRIC_FURNACE)
    .tag("conductor")
    .attach(&WORK)
    .attach_with(&ENERGY, Energy { stored: 0, capacity: 40_000, max_in: 400, max_out: 0 })
    .inventory(2)
    .opens(&FURNACE_MENU);
```

The network runs through this block, so cables on its two sides stay one network, and the furnace draws power by its `Energy` limits. A machine next to it joins too. A generator that is a conductor works the same way and supplies power instead.

## Holders and storages

`.inventory(n)` works the same on every holder:

- A block: `r.block(&CRATE).inventory(27)`. `w.inventory(pos)` reaches the slots, and `w.open_menu(player, &CRATE_MENU, pos)` shows them.
- An entity kind: `.inventory(n)` on the kind.
- An item: `.inventory(n)` makes it a backpack.

A backpack opens with `w.open_menu(player, &BAG_MENU, Holder::held(player, slot))`.

### Storages

A **storage** is a named inventory a holder carries besides its own.

```rust
static ENDER: Storage = Storage::named("ender");

r.storage(&ENDER, 27);
r.edit(&PLAYER).attach_storage(&ENDER);
```

Every player now has 27 more slots.

- `w.inventory(ENDER.of(player))`: reaches them.
- `w.open_menu(player, &ENDER_MENU, ENDER.of(player))`: shows them. An ender chest is a block whose use does this, the menu's `.holder()` groups being the storage.
- `.attach_storage(&S)` on your own entity kind gives its entities the storage.
- `S.of(Holder::held(player, slot))`: any stack can carry one. A tool made of parts keeps them this way, see [Tools made of parts](items.md#tools-made-of-parts).

Worn equipment is covered in [Armor and wearables](items.md#armor-and-wearables).

## Rarer features

### Keeping cubes loaded

Catching up is enough for one block. A network that has to run while nobody is there (a generator in one cube, its consumers in others, a quarry digging far below) keeps its cubes loaded:

```rust
w.keep_loaded(id, from, to, Loading::BlockEntities, 200);
```

This keeps the cubes from block `from` to block `to` for 200 ticks under the key `id`.

- `w.keep_loaded(key, from, to, loading, ticks)`: call it again before it runs out, or with other blocks to move it.
- `w.release_loaded(key)`: lets go.

| `Loading::` | runs there |
|---|---|
| `BlockEntities` | blocks, systems, scheduled ticks and redstone. Mobs stand still and nothing grows |
| `All` | everything, as if a player stood there |

All tickets together keep at most `core:max_forced_cubes` cubes, and `question::FORCE_LOAD` lets a mod refuse one (someone else's claim).

### Another mod's block

Block inventories and attachments belong to their block definitions. Use `r.edit(&BLOCK).attach(..)` to add a component to an existing block, or set up its inventory and attachments on the replacement when using `r.replace(&BLOCK)`. See [Changing something of another mod](details/editing-others.md).
