# Components and tags

A **component** is a named value (`fuel = 800`), a **tag** says what group a thing belongs to (`#sand`, `#logs`). Hardness, light, fuel, food and durability are all components. You them to work nicely with other mods.

## The rule

Don't check for content of a specific mod, check for a component or a tag.

| instead of | check |
|---|---|
| `base:coal` | has the component `fuel` |
| `base:sand` | has the tag `#sand` |

Do the same for your own content: give it tags and components, so other mods can work with you.

## Setting and reading

```rust
static FUEL: Component<u32> = Component::named("fuel");
static HOT: Tag = Tag::named("hot");

r.item(&COAL_DUST).set(&FUEL, 800).tag("hot");   // your own content
r.set("base:stick", &FUEL, 100);                 // someone else's
r.set("#planks/*", &FUEL, 300);                  // matches `#planks/spruce`, `#planks/oak`, etc.

if let Some(ticks) = FUEL.on_item(stack.item) { /* burns */ }
if HOT.on_block(state) { /* harm player */ }
```

- Reads go by kind: `on_block`, `on_item`, `on_entity`, `on_biome`, `on_dimension`, etc.
- `.unset(&C)` and `.untag("t")` take them away.

### Don't keep ids in a saved value

Ids of blocks/items/etc shift after restart, so save to disk stable keys instead (e.g. `dimension_key(id)` for a dimension).

## Sharing types with other mods

To agree on layouts, publish a small crate with just your types and handles, as *base* does with `base_api`. `r.has_mod("othermod")` tells whether a mod is loaded.

## Names

Component and tag names are a shared vocabulary, written without a namespace: `fuel`, `sand`, `ingots/iron`. A term only your mod uses carries a prefix, `mymod:heat`. A `/` separates a group from a material or class (`ores/coal`), and `#ingots/*` matches any of them. See [Names, references and handles](names-and-references.md).

In `r.set`, `r.tag` and `r.edit_pattern`, a `#tag` that no block or item has stops the game at load. In rules and matchers it just matches nothing.

## How to find out what tags already exist

- The built-in recipe viewer has a tags button that lists all existing tags.
- Search all files in `mods/base/src/` for `.tag(` or `.set(`. Do the same for other mods.
- **In runtime:**

```rust
components_of(entry_kind::ITEM, item)         // every (name, schema) on it; empty schema = tag
entries_with(entry_kind::BLOCK, "logs")       // every block tagged `#logs` or with a component `logs`
```

Internally, a tag is a component without a value, that's why `components_of` and `entries_with` work for tags too.

A tag you make up costs nothing, but check first that nobody already has one.

