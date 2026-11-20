use weft_sdk::Attribute;

/// Protection: about 4% less damage per point, less against big hits unless toughness holds them
/// back, the way modern's armor works. Attributes, so a piece gives them worn
/// (`Modifier::add(&ARMOR, 3.0).worn()`), and so may anything else (an effect, a pose).
pub static ARMOR: Attribute = Attribute::named("base:armor");
pub static ARMOR_TOUGHNESS: Attribute = Attribute::named("base:armor_toughness");
