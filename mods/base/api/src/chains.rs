use weft_sdk::Chain;

/// A furnace's cooking time: base is the recipe's time, `entity` the furnace, `item` the input.
pub static SMELT_TIME: Chain = Chain::named("base:smelt_time");
