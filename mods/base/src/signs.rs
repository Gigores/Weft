use base_api::{blocks::OAK_SIGN, menus::SIGN};
use weft_sdk::*;

pub fn register(r: &mut Registry) {
    r.menu(&SIGN);
    r.block(&OAK_SIGN)
        .texture("planks_oak")
        .sound_group("wood")
        .hardness(1.0)
        .liquid_walls(&[])
        .shares_cell_with("#water")
        .mined_by("axe", tool_tier::HAND)
        .tab(&crate::tabs::FUNCTIONAL)
        .facing(Facings::HORIZONTAL, Toward::Player)
        .prop("mount", ["floor", "wall"])
        .placement(|place, p| {
            if place.face().horizontal() {
                p.set("mount", "wall");
                p.set("facing", place.face().name());
            }
        })
        .states(|p, s| {
            if p.is("mount", "wall") {
                s.needs(Dir::South, "")
                    .sign_surface([8.0, 10.0, 14.0], [13.6, 6.72], Dir::North)
                    .model_box([0.5, 6.0, 14.0], [15.5, 14.0, 16.0])
            } else {
                s.needs(Dir::Down, "")
                    .sign_surface([8.0, 10.0, 7.0], [13.6, 6.72], Dir::North)
                    .model_box([7.0, 0.0, 7.0], [9.0, 6.0, 9.0])
                    .model_box([0.5, 6.0, 7.0], [15.5, 14.0, 9.0])
            }
        })
        .writable_sign()
        .opens(&SIGN);
    r.shaped(&base_api::recipes::CRAFTING, "oak_sign", 3, &["ppp", "ppp", " s "], &[('p', "oak_planks"), ('s', "stick")]);
}
