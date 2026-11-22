# Weft modding quick start

This page gets you from nothing to a working mod and points you to other pages.\
If you get stuck, see [Getting help and community](#getting-help-and-community).

## Introduction
The base game is a mod itself, and it has no private hooks and no special priority over other mods, so anything the base game can do, your mod can do too.\
When in doubt, look in `mods/base` and `mods/testmod` and copy from there.

## Preparation
1. You need an editor before you start. I recommend JetBrains RustRover: it is free and has everything you need.
2. Clone the repo with the built-in Clone Repository feature, use `https://github.com/Weft-Rust/Weft` as the link. This creates a folder with the code on your computer.
3. Configure a Run Configuration. Find the Run Configuration tab in the top-right corner, set the Cargo command to `run` and name it whatever you like. The game will then launch from your local copy of the repo.

## Creating a mod in two minutes

To get started, copy `examples/examplemod/` to some location on your PC.

It is a ready-made mod with one item, a setting and an event. Every kind of registration is written out there as a commented example, with a pointer to where the base game uses it.

Rename the three things listed at the top of `lib.rs` and you are ready to start modding.

To run the mod:
1. Build the mod with `cargo build --release`
2. Create a folder in `run/mods/`, copy the built library from `target/release/` into it.
3. Symlink `assets/` into it. (e.g. on Linux: `ln -s my-mods/mymod/assets/ weft/run/mods/mymod/assets`).

You should end up with something like that:

```
run/mods/mymod/
    mymod.dll          (libmymod.so / libmymod.dylib might be also there)
    assets/
        textures/block/ruby_ore.png
        textures/item/ruby.png
        lang/en_us.lang
```

If you can't see the mod in-game, double check that the folder name matches the one in `Cargo.toml`.\
If you're stuck, you can always join [our Discord server](https://discord.com/invite/zTEekbrc7U) to get some help.

To send the mod to someone, or to get it into the launcher, see [Packaging and publishing](publishing.md).

## Three ideas to know first

- **Names.** Everything a mod adds is called `namespace:path`. This goes for blocks, items, dimensions, particles or any other stuff. In your own mod you write `ruby` and it becomes `mymod:ruby`, and you reference stuff from other mods by their full name (`base:furnace`). You can also use more complex selectors (`#logs`, `!`, `base:furnace[lit=true]`). See [Names, references and handles](names-and-references.md).
- **Registering.** Registering is how your mod adds content to the game. In your mod's `init` function you get `r`, the registry: call `r.block(..)`, `r.item(..)` and so on to add content. The same `r` also changes things that already exist, no matter what mod added them, the engine included: `r.edit(..)` changes some properties, `r.replace(..)` starts over and `r.remove(..)` deletes the thing entirely. See [Changing something of another mod](details/editing-others.md).
- **Components and tags.** Every property of a block or item, like fuel, food or hardness, is a *component*: a named value such as `fuel = 800`. A *tag* is a label that groups things, like `#sand` or `#logs`. Any mod can read and write both. So instead of checking whether something is `base:coal`, check whether it has `fuel`, and instead of checking for `base:sand`, check for `#sand`. Your code then works with other mods' content too. See [Components and tags](details/components-and-tags.md).

## Where to go next

New to Weft? Read [blocks](blocks.md), then [items](items.md), then [registration](registration.md).

Each guide starts with the common case and goes on to the rare ones. The reference lists every method you can call.

**Content you register**

| I want to make | Guide | Reference |
|---|---|---|
| Blocks | [blocks](blocks.md) | [reference](reference/blocks.md) |
| Blocks with data, inventories, menus, energy | [block entities](block_entities.md) | |
| Items | [items](items.md) | [reference](reference/items.md) |
| Mobs and other entities | [entities](entities.md), [poses and game modes](details/poses-and-modes.md) | [reference](reference/entities.md) |
| Status effects and potions | [effects](details/effects.md) | [reference](reference/entities.md) |
| Liquids | [liquids](details/liquids.md) | |
| Recipes and loot | [recipes and loot](details/recipes-loot.md) | [reference](reference/recipes.md) |
| Biomes, dimensions, ores, structures, spawns | [world content](details/world-content.md), [worldgen](worldgen.md) | [reference](reference/worldgen.md) |
| Redstone-style logic | [redstone](redstone.md) | |
| Every kind of content, with its components | [registration](registration.md) | [reference](reference/registration.md) |

**Making things happen**

| I want to | Guide | Reference |
|---|---|---|
| Hook into game events,<br>change damage or an attribute | [events](events.md), [attributes](details/attributes.md) | [reference](reference/events.md) |
| Test conditions | [conditions](details/conditions.md) | |
| Add commands | [commands](details/commands.md) | [reference](reference/commands.md) |
| Work with the world from code | [world API](world-api.md), [hitboxes](details/hitboxes.md), [world blocks](details/world-blocks.md), [world entities](details/world-entities.md) | [reference](reference/world.md) |
| Read or post-process generation | [reading generation](worldgen.md#reading-generation) | |
| Change another mod's content | [editing others](details/editing-others.md) | |

**Client side and looks**

| I want to | Guide | Reference |
|---|---|---|
| Keybinds, packets, settings,<br>code that runs every frame | [client](client.md) | [reference](reference/client.md) |
| Screens and the HUD | [UI](ui.md) | [reference](reference/ui.md) |
| Camera modes | [camera](details/camera.md) | |
| Models and animations | [models](models.md) | |
| Particles and sounds | [particles and sounds](details/particles-sounds.md) | [reference](reference/effects.md) |
| Fog, sky lighting, post effects, acoustics | [environment](environment.md), [sky](sky.md) | |
| Shaders | [shaders](shaders.md) | |
| Textures, connected textures | [textures](textures.md), [connected textures](ctm.md) | |
| Translations and rich text | [localization](localization.md), [text on screen](details/text.md) | |
| A resource pack | [resource packs](resourcepacks.md) | |

**Shipping and tooling**

- [Packaging and publishing](publishing.md): zips, all platforms, the launcher, versions
- [Debugging](debugging.md): logs, gizmos, reloads, crash reports
- [Content editor methods](editor.md)

## Getting help and community

The [Discord server](https://discord.com/invite/zTEekbrc7U) is the best place to:

- ask questions and get help with your mod
- see what other people are working on and share your own
- see sneak peeks for upcoming features and suggest ideas

If you had to work around the engine to do something reasonable, that is an engine flaw. Tell me about it instead of building a hack, so I can adjust the engine for those cases.\
You can open an issue, tell us about it on the Discord server, or contact me directly on Discord (`@vortex_so`).
