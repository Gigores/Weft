use weft_sdk::resource::Storage;
use weft_sdk::{Component, Record};

pub static WATER: Record = Record::named("base:water_network");
pub static LAVA: Record = Record::named("base:lava_network");
pub static WATER_TANK: Component<Storage> = Component::named("base:water_tank");
pub static LAVA_TANK: Component<Storage> = Component::named("base:lava_tank");
