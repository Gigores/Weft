# Items

## Your first item

```rust
use weft_sdk::*;

declare_mod!(deps: ["base"], init: init);

static RUBY: Item = Item::named("ruby");

fn init(r: &mut Registry) {
    r.item(&RUBY);
}
```

`RUBY` is a handle, and you use it to reference your item throughout the code. You only write a namespace for items of other mods (`Item::named("base:coal")`).

The item then needs two files:

- `assets/textures/item/ruby.png`
- a name in `assets/lang/en_us.lang` (replace `mymod` with your mod id):

```
item.mymod.ruby = Ruby
```

Optionally, you can also add a tooltip:

```
item.mymod.ruby.tooltip = A gray line shown under the name.
```

Without more calls it gets:

- the texture `item/<n>`
- a stack size of 64 (`.max_stack(n)` changes it)
- a place in your mod's `misc` creative tab (`.tab(&base_api::tabs::INGREDIENTS)` picks another)

## Quick navigation

Before going forward, you should know the difference between an item and a stack.\
An **item** is a kind of thing (an iron pickaxe). A **stack** is what sits in a slot: an item, a count, and optionally its own values (this pickaxe has taken 10 damage).

Jump to a section you're interested in, or just read the page top-to-bottom.

| I want an item like... | Section |
|---|---|
| ingot, fuel, dye | [Tabs, tags and components](#tabs-tags-and-components) |
| pickaxe, sword | [Tools and weapons](#tools-and-weapons) |
| food, potion | [Food and potions](#food-and-potions) |
| helmet, boots, a fox tail | [Armor and wearables](#armor-and-wearables) |
| seeds, a block-placing item | [Items that place or spawn](#items-that-place-or-spawn) |
| bow, wand, shield | [Using an item](#using-an-item) |
| a whole set (all armor of a metal) | [Items in a loop](#items-in-a-loop) |
| enchanted or custom-built tool | [Stacks with their own data](#stacks-with-their-own-data) |

| I want to customize... | Section |
|---|---|
| creative tab | [Tabs, tags and components](#tabs-tags-and-components) |
| stack size, durability, what is left after use | [Stack size, durability and leftovers](#stack-size-durability-and-leftovers) |
| texture, tint, layers | [Textures and looks](#textures-and-looks) |
| light while held | [Light](#light) |
| right click, left click | [Using an item](#using-an-item) |
| how it sits in the hands | [Hands](#hands) |
| read what other mods' items are | [Reading what an item set](#reading-what-an-item-set) |
| an item from another mod | [Changing something of another mod](details/editing-others.md) |

Every method is also listed in the [reference](reference/items.md).

## Tabs, tags and components

A **tag** groups things (`#ingots/iron`), and a **component** is a named value on a thing (`fuel = 300`). Tools, food, fuel and armor are all components or tags. *base*'s stick:

```rust
r.item(&STICK)
    .tag("stick")
    .handheld()
    .set(&FUEL, 100)  // base_api::components::FUEL, the burn time in ticks
    .tab(&base_api::tabs::INGREDIENTS);
```

- `tab(&TAB)`: the creative tab. `Registry::creative_tab` makes your own.
- `no_tab()`: keeps it out of every tab and the search, but you can still give it out (a variant, a spoiler). Another mod brings it back with `.untag(abi::comp::NO_TAB)`.
- `tag("name")`: adds a tag. A tag you make up costs nothing.

Some tags that *base* reads: (files mentioned here are located in `mods/base/src/`)

- `pickaxes/ruby`, `axes/iron`: automatically gets a recipe (see `recipes.rs`)
- `logs`, `torches`, `furnaces`: used in advancements (see `advancements.rs`)
- `cooked_meat/beef`, `raw_meat/beef`: gets a furnace recipe
- `raw_ores/copper`: smelts to `ingots/copper`
- `tools/hoe`: can till soil (see `blocks.rs`)
- `ingots/iron`, `gems/diamond`: used to generate recipes for tools, buckets, etc.

A recipe like `pickaxes/copper` expects that `ingots/copper` exists. If there's none, it fails to get registered. See `recipes.rs`.

For more: [How to find out what tags already exist?](details/components-and-tags.md)

## Stack size, durability and leftovers

```rust
r.item(&BUCKET).max_stack(16);
r.item(&MACE).durability(500);
```

- `durability(n)`: an item that can wear down never stacks, so `max_stack` doesn't apply to it.
- `remainder(&ITEM)`: what is left when the last one is used up (a potion's bottle, a lava bucket's bucket). `consume_one(stack)` automatically handles this rule; it's used by recipes and fuel, and your own code can call it too.

**Wear** (used up durability) belongs to the stack, not to the item: two pickaxes of one kind wear separately. `stack.wear()` reads it, and `stack.add_wear(n)` adds to it.

## Textures and looks

`texture(t)` sets the picture. Names are relative to your mod's `textures/item/`, and `"base:diamond"` borrows *base*'s.

```rust
r.item(&RUBY).texture("base:diamond").tint(0xE0204A);
```

- `texture(t)`: the item's only picture. It drops any layers added before it.
- `layer(t)`: draws another picture over the previous one. The first layer replaces the item's own picture.
- `tint(0xRRGGBB)`: multiplies all pixels of a just added layer by a color. (small trick: a white texture can serve every color of an item)
- `model_file("mymod:item/model")`: a model asset instead of a flat picture. See [models](models.md#an-item-as-a-model).
- `animations(name)`: the clips from `animations/<name>.animation.json`, played on whoever holds it. A file named after the item is used without this call.
- `handheld()`: held by its end and pointing ahead in third person, like a stick. `tool(..)` and the tag `handheld` do it too.

A wand with a stick and a tinted gem on top (testmod):

```rust
r.item(&WAND)
    .layer("base:stick")
    .layer("base:diamond")
    .tint(0x40E0FF)
    .max_stack(1)
    .handheld();
```

An item that places a block (`.places(..)`) has no texture of its own to make. The block's model is used. To give a *block's* item a flat picture instead, see [blocks](blocks.md#flat-item-texture).

### Light

- `light(14)`: lights the area around whoever holds or wears it, or around the item lying on the ground.
- `light_rgb(r, g, b)`: the same, in color (*base*'s gold armor uses `light_rgb(5, 5, 0)`).

The light is client-only, so it doesn't affect mob spawn or crop growth.

## Tools and weapons

A tool is an item with a few components.

```rust
r.item(&RUBY_PICKAXE)
    .tool("pickaxe", tool_tier::DIAMOND)
    .mining_speed(8.0)    // base: 2 for wood, 12 for gold
    .durability(1200)
    .attack_damage(4.0)   // of a full swing
    .attack_speed(1.2);
```

- `tool(class, level)`: tags it `tools/<class>`. It mines blocks that are `mined_by` that class, and it drops them only if `level` reaches the block's tier (see [mining and drops](blocks.md#mining-and-drops)). It is held like a tool already.
- `attack_speed(per_second)`: full swings per second. Hitting sooner does less. A sword is 1.6, and an empty hand is 4.
- `sweeps()`: a full swing on the ground also hits what stands next to the target (like a sword).
- `no_creative_break()`: in creative it doesn't break blocks (like a sword). Both sides know it, so nothing flickers. A rule that depends on more is `actions::BREAK_BLOCK`: it is asked when mining starts and tells the client no right away.
- `set(&WEAR_PER_HIT, n)`: how much wear one hit does. *base* uses 1 for weapons and 2 for tools.
- `modifier(Modifier::add(&attribute, value))`: an **attribute modifier** its holder gets while it is in the main hand. See [attributes](details/attributes.md).

Damage is an attribute, so other mods can also change it from outside the item: see [changing something of another mod](details/editing-others.md).

## Food and potions

```rust
use base_api::components::*;

r.item(&SNACK).food(Food { hunger: 3, saturation: 1.8 });
```

`food(..)` is a shortcut for `.set(&FOOD, ..).use_ticks(32).on_use(eat)`.

- `use_ticks(n)`: it takes effect after the use button is held this long (food is 32), and slows the player down meanwhile.
- `gives_effect(&EFFECT, level, secs)`: `f32::INFINITY` seconds makes it permanent. Call it again for more effects.

*base*'s potions (see [effects](details/effects.md)):

```rust
r.item(&SWIFTNESS)
    .use_ticks(32)
    .remainder(&GLASS_BOTTLE)
    .gives_effect(&SPEED, 1, 180.0);
```

## Armor and wearables

An item is worn when it sits in an equipment slot. *base*'s iron helmet:

```rust
r.item(&IRON_HELMET)
    .durability(165)
    .modifier(Modifier::add(&ARMOR, 2.0).worn())
    .wearable(&HEAD, 0.75, "armor/iron_1")
    .tag(ARMOR_PIECE.name());
```

- `wearable(&SLOT, inflate, texture)`: the slot (any mod's, `EquipmentSlot::named("base:head")`), how many pixels the armor sticks out from the body, and the texture under `textures/entity/`.
- `modifier(..).worn()`: the modifier applies while the item is worn instead of held. Armor points is a modifier, see: [modifiers](modifiers.md).
- `wearable_model(&SLOT, "model", "bone")`: a model on a bone of the wearer. Testmod's fox tail is worn on `base:legs` and only whoever wears it has a tail.

Skin layers `inflate` by 0.5 on the head and 0.25 on the rest of the body. Please use bigger values for armor so it doesn't look awkward.

Slots, textures and bone layout are in [models](models.md#armor-and-other-worn-items).

## Items that place or spawn

```rust
r.item(&WHEAT_SEEDS).places(&WHEAT);
```

`places(&BLOCK)`, `spawns(&ENTITY)`.

Every entity automatically gets its own egg, so you don't need to make one.

Filled buckets come with their liquid (see [liquids](details/liquids.md)).

## Using an item

```rust
r.item(&RUNE).max_stack(1).on_use(|u| {
    let w = u.world();
    w.action_play(u.entity(), "wave", false);
    true
});
```

`on_use(|u| ..)`: a right click while holding it. Return `true` if you handled it, `false` to pass it on.\
Before it runs, anyone can prevent it using `actions::USE_ITEM`, and after it the same action's event tells every mod (testmod heals with base's food through it). See [events](events.md).

`u` is an `ItemUse`:

- `u.world()`: the world
- `u.entity()`: who uses it: a player, or a mob
- `u.stack()`: the stack being used
- `u.set_stack(s)`: replaces it (count 0 consumes it)
- `u.slot()`: the slot of the user's inventory it is in
- `u.holder()`: the stack as a holder, what a backpack opens its menu on
- `u.target_block()`: the block clicked, if any: its position and face
- `u.held_ticks()`: how long the button was held (for `on_release`)

### Hold to use

- `use_ticks(n)`: takes effect once held this long (eating).
- `use_until_released()`: it is used as long as the button is held, with nothing at the end.
- `on_release(|u| ..)`: the button was let go. `u.held_ticks()` says after how long. It also runs for an item with `use_ticks` let go too early. Return `true` if it did something.

*base*'s bow, the draw's strength coming from the ticks:

```rust
r.item(&BOW)
    .durability(384)
    .handheld()
    .use_until_released()
    .on_release(shoot_bow);
```

*base*'s shield is `use_until_released()` plus a `BLOCKS_ATTACKS` component.

### Left click

`on_attack(|u| ..)` takes over melee and mining while it is in the main hand. It runs once per press. Return `true` to apply the stack changes and play the swing. Testmod's blaster fires a fast arrow this way.

### Hands

```rust
r.item(&GREATSWORD).two_handed();
r.item(&SCROLL).one_handed_action("hold_one");
r.item(&WAND).use_priority(10);
```

- `two_handed()`: needs both hands, so it does nothing while the other hand holds something, and it can't go in the other hand.
- `one_handed_action("name")`: lets a `two_handed` item be held in a single hand too, playing this action. It can then go in the other hand as well.
- `use_priority(n)`: right click runs for an item with the highest priority (0 by default, then the main hand). The other hand gets its turn when this item's `on_use` returns `false`: a bow with no arrows.

The clips, poses and where the item sits in the hand are in [models](models.md#items-in-the-hand).

## Stacks with their own data

```rust
value! { pub struct Quality { pub grade: u32, pub maker: u32 } }
static QUALITY: Component<Quality> = Component::named("quality");

// While registering, once:
r.stack_component(&QUALITY, 0);

// Making one, a custom workbench's output for example:
let out = PICKAXE.stack(1)
    .with(&QUALITY, Quality { grade: 3, maker: 7 })
    .with_wear(10);
inventory.set(slot, out);
```

- `QUALITY.on_stack(&stack)`: the stack's own value, else what the item got
- `QUALITY.on_stack_own(&stack)`: only the stack's own value
- `stack.with(&C, v)`, `stack.without(&C)`: sets or removes its value
- `stack.with_wear(n)`, `stack.wear()`: writes and reads the wear

`.with(..)` on a component nobody declared with `r.stack_component` does nothing and says so in the log. Stacks with different data don't merge.

The second argument of `r.stack_component` is its flags (`abi::stack_flags`), or 0 for none:

| flag | the component... |
|---|---|
| `SERVER_ONLY` | isn't sent to clients, including nested stacks |
| `NO_SAVE` | isn't saved |
| `VISUAL` | changes how the stack looks (see below) |


A stack's own value wins over its item's for everything the engine reads too. Give one stack its own `DURABILITY`, `MINING_SPEED`, `TOOL_LEVEL`, `ATTACK_DAMAGE`, `ATTACK_SPEED`, `USE_TICKS` or a `tools/<class>` tag, and it digs, wears and hits by those. `stack.with_modifiers(&[Modifier::add(&attribute::ATTACK_DAMAGE, 2.0)])` gives it modifiers of its own instead of its item's (an enchanted sword).

Ids inside a value shift when a mod comes or goes. The same warning applies to any saved component (see [components](details/components-and-tags.md)).

### A look that depends on the stack

`.condition(|stack, looks| ..)` decides how a stack looks, wherever it is shown. It reads only components marked `VISUAL` (wear is by default), and the result is cached until they change. Testmod's `DISPLAY_TOKEN` picks its picture from a `DISPLAY_PHASE` component this way.

### Tools made of parts

A tool made of parts keeps them in a storage (`r.storage(&PARTS, 3)`, see [block entities](block_entities.md)):

```rust
let pick = PICKAXE.stack(1)
    .with_stacks(&PARTS, &[handle, head, binding])
    .with_layers(Layers::new().layer("rod").layer("pick_head").tint(0xB0B0B0));
```

- `stack.stacks(&PARTS)`: reads the parts. Each is a stack with its own values and parts, as deep as you go.
- `stack.with_stacks(&PARTS, &[..])`: writes them.
- `stack.with_layers(..)`: the stack shows its own pictures one over the other (`tint` colors the layer just added), in slots, in the hand and on the ground.

Add the parts' values up when you make the tool, and write the result onto the stack.

A stack can also override `MODEL`, `WORN_MODEL` (`model@bone`), `WORN_TEXTURE` and `WORN` the same way, with `Text::new("full:asset/path")` for text. To read what a hand, an equipment slot or a dropped item shows, see [client](client.md).

## Reading what an item set

Your code can read what any mod's items were set up with, the same way as for [blocks](blocks.md#reading-what-a-block-set):

```rust
let info = item_info(item)?;
format!("{} ({}), stack {}", info.name, info.owner, info.max_stack)
```

- `item_info(item)` gives the item's name, mod (`owner`), stack size, durability, attack, the block it places, the slot it is worn in and whether it is `handheld`.
- `components_of(entry_kind::ITEM, item)` lists every component and tag on it.
- `entries_with(entry_kind::ITEM, "ingots/iron")` finds every item with a tag or component.

## Items in a loop

`Item::generated(name)` makes a handle for a name built at runtime, so a loop can register a whole family. It lives for the rest of the game, so use it while registering, not every tick. *base* generates armor like this:

```rust
for (piece, slot, mult, points, inflate, layer) in [
    ("helmet", "base:head", 11, 3.0, 0.75, 1),
    ("chestplate", "base:chest", 16, 8.0, 0.75, 1),
    ("leggings", "base:legs", 15, 6.0, 0.5, 2),
    ("boots", "base:feet", 13, 3.0, 0.75, 1),
] {
    let tag = format!("armor_{}", slot.trim_start_matches("base:")); // `base:head` -> `armor_head`, etc.
    r.item(Item::generated(format!("ruby_{piece}")))
        .durability(33 * mult)
        .modifier(Modifier::add(&ARMOR, points).worn())
        .wearable(&EquipmentSlot::named(slot), inflate, &format!("armor/ruby_{layer}"))
        .tag("armor")
        .tag(&tag)
        .tag(&format!("{tag}/ruby"))
        .tab(&base_api::tabs::COMBAT);
}
```

The calls are explained in [Tabs, tags and components](#tabs-tags-and-components) and [Armor and wearables](#armor-and-wearables).
