use weft_sdk::EquipmentSlot;

pub static HEAD: EquipmentSlot = EquipmentSlot::named("base:head");
pub static CHEST: EquipmentSlot = EquipmentSlot::named("base:chest");
pub static LEGS: EquipmentSlot = EquipmentSlot::named("base:legs");
pub static FEET: EquipmentSlot = EquipmentSlot::named("base:feet");

/// The player's equipment slots, head to feet.
pub const EQUIPMENT: [&EquipmentSlot; 4] = [&HEAD, &CHEST, &LEGS, &FEET];
