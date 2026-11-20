use weft_sdk::*;

value! {
    /// A furnace's state, synced to whoever has it open.
    pub struct Smelting { pub burn: u32, pub burn_max: u32, pub cook: u32, pub cook_max: u32, pub cooking_item: u32 }
}
pub static SMELTING_STATE: Component<Smelting> = Component::named("base:smelting");

value! {
    /// A player's hunger and air.
    pub struct Hunger { pub food: f32, pub saturation: f32, pub exhaustion: f32, pub air: f32, pub food_timer: u32 }
}
pub static HUNGER: Component<Hunger> = Component::named("base:hunger");

value! {
    /// A player's advancements, a bit each in the order of the base game's list.
    pub struct Advancements { pub done: u64 }
}
pub static ADVANCEMENTS: Component<Advancements> = Component::named("base:advancements");

value! {
    /// Where a player respawns: the foot of the bed they last used and its dimension
    /// (`dimension_key`), when `set` is 1. Kept through death.
    pub struct BedSpawn { pub bed: [i32; 3], pub set: u32, pub dim_key: u64 }
}
pub static BED_SPAWN: Component<BedSpawn> = Component::named("base:bed_spawn");

value! {
    /// Read by the hunger system; other mods set it on their own food.
    pub struct Food { pub hunger: u32, pub saturation: f32 }
}

/// The common vocabulary the base game reads; any mod may set these on its own content.
pub static FOOD: Component<Food> = Component::named("food");

/// Burn time in ticks.
pub static FUEL: Component<u32> = Component::named("fuel");

/// How readily fire takes it.
pub static FLAMMABLE: Component<u32> = Component::named("flammable");

/// Eating behavior for item builders, with the nutrition kept in the shared food component.
pub trait FoodItem: Sized {
    fn food(self, nutrition: Food) -> Self;
}

impl FoodItem for ItemBuilder<'_> {
    fn food(self, nutrition: Food) -> Self {
        self.set(&FOOD, nutrition).use_ticks(32).on_use(eat)
    }
}

/// Makes the same food builder available through the editor's parameter form.
pub fn editor_methods(r: &mut Registry) {
    r.editor_method(editor::Method {
        on: editor::ITEM,
        name: "food".into(),
        doc: "Eating behavior with hunger restoration and saturation.".into(),
        import: "base_api::components::FoodItem".into(),
        params: vec![editor::parameter("nutrition", "base_api::components::Food", "food", Food { hunger: 4, saturation: 2.4 })],
    });
}

/// Eating for `.on_use`: hunger and saturation from its `FOOD`, one item less. With `.use_ticks(32)`
/// it takes as long as the base game's food.
pub fn eat(u: &mut ItemUse) -> bool {
    let stack = u.stack();
    let Some(food) = FOOD.on_stack(&stack) else { return false };
    let w = u.world();
    let Some(h) = w.component_mut(u.entity(), &HUNGER) else { return false };
    if h.food >= 20.0 {
        return false;
    }
    h.food = (h.food + food.hunger as f32).min(20.0);
    h.saturation = (h.saturation + food.saturation).min(h.food);
    u.set_stack(consume_one(stack));
    true
}
