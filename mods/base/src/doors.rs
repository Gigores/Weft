//! Doors: one block two cells tall, opened by a click on either or by power.

use weft_sdk::*;

pub use base_api::blocks::OAK_DOOR;

pub fn register(r: &mut Registry) {
    r.block(&OAK_DOOR)
        .sound_group("wood")
        .facing(Facings::HORIZONTAL, Toward::Player)
        .prop("open", ["false", "true"])
        .prop("hinge", ["right", "left"])
        .placement(|place, props| {
            // The front points toward the player, so its clockwise side is the player's left.
            let left = props.facing().clockwise().offset();
            let hit = place.hit();
            let side = (hit.x - 0.5) * f64::from(left.x) + (hit.z - 0.5) * f64::from(left.z);
            props.set("hinge", if side > 0.0 { "left" } else { "right" });
        })
        .states(|p, s| {
            let (open, left) = (p.is("open", "true"), p.is("hinge", "left"));
            let (min, max) = match (open, left) {
                (false, _) => ([0.0; 3], [16.0, 32.0, 3.0]),
                (true, false) => ([0.0; 3], [3.0, 32.0, 16.0]),
                (true, true) => ([13.0, 0.0, 0.0], [16.0, 32.0, 16.0]),
            };
            let size = [max[0] - min[0], max[1], max[2] - min[2]];
            // Keep the handle on the free edge on both faces of the leaf.
            let reverse = if open { Dir::East } else { Dir::North };
            let face = |f: usize| {
                let (w, h) = [(size[2], size[1]), (size[0], size[2]), (size[0], size[1])][f / 2];
                let [u0, u1] = if left ^ (f == reverse.face() as usize) { [w / 2.0, 0.0] } else { [0.0, w / 2.0] };
                ("door_oak", [u0, 0.0, u1, h / 2.0])
            };
            s.cutout()
                .model_box_faces(min, max, std::array::from_fn(face))
        })
        .needs_below("")
        .hardness(3.0)
        .mined_by("axe", tool_tier::HAND)
        .on_use(|u| {
            u.world().prop_cycle(u.pos(), "open");
            true
        })
        .signal_sink()
        .on_signal(|e| {
            let open = if e.level() > 0 { "true" } else { "false" };
            e.world().prop_set(e.pos(), "open", open);
        })
        .tab(&crate::tabs::REDSTONE);
    r.shaped(&base_api::recipes::CRAFTING, "oak_door", 3, &["pp", "pp", "pp"], &[('p', "oak_planks")]);
}
