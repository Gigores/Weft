use weft_sdk::HudElement;

/// Information about the crosshair's target; `screen_part(&TARGET_INFO, "details", ..)` adds
/// bound text, icons or bars inside its panel. `ui.view().target()` identifies what is shown.
pub static TARGET_INFO: HudElement = HudElement::named("base:target_info");
