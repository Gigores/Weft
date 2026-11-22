use weft_sdk::*;

value! {
    /// Fired when a furnace finishes one item.
    pub struct Smelted { pub furnace: u64, pub input: u32, pub output: ItemStack }
}
pub static SMELTED: Event<Smelted> = Event::named("base:smelted");
