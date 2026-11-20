//! Typed handles and shared value types for the base game. Add `base_api` to your mod's
//! `Cargo.toml` to use them. The engine checks shared value layouts when mods load.
//!
//! Content handles use explicit `base:` names so they resolve to the base game's entries
//! in every mod. Shared vocabulary such as the `fuel` component keeps its unqualified name.

/// Shared components, their value types, and food-item helpers.
pub mod components;

/// Events emitted by the base game.
pub mod events;

/// Item and block tags exposed by the base game.
pub mod tags;

/// Attributes exposed by the base game.
pub mod attributes;

/// Value chains exposed by the base game.
pub mod chains;

/// HUD elements exposed by the base game.
pub mod hud;

/// Dimensions registered by the base game.
pub mod dimensions;

/// Vanilla's entity kinds.
pub mod entities;

/// What is in the sky of the game's dimensions. Any of them can be changed or taken out by a mod:
/// `r.edit(OVERWORLD.sky(&MOON)).size(40.0)`, `r.remove(OVERWORLD.sky(&STARS))`.
pub mod sky_objects;

/// Vanilla's blocks.
pub mod blocks;

/// Vanilla's menus.
pub mod menus;

/// Vanilla's items.
pub mod items;

/// Vanilla's equipment slots.
pub mod slots;

/// Vanilla's recipe types.
pub mod recipes;

/// Vanilla's creative tabs.
pub mod tabs;
