use weft_sdk::{abi::assembly_result as result, *};

static TOOL: Item = Item::named("assembly_tool");
static GRAB_TOOL: Item = Item::named("grab_tool");
static HELM: Block = Block::named("helm");
static GUIDE: Block = Block::named("linear_guide");
static GUIDE_RAIL: Block = Block::named("guide_rail");

pub fn use_controller(w: World, player: u64, p: BlockPos, powered: bool) -> bool {
    let stack = w.inventory(player).get(w.selected_slot(player));
    if w.frame().is_some_and(|frame| frame != 0) && !stack.is_empty() && item_block(stack.item).is_some() {
        return false;
    }
    let result = w.assembly_select(player, Some(p), !powered && !stack.is_empty() && stack.item == TOOL.id());
    if powered && result == result::CONFIRMED {
        w.hint("assembly.base.configured", &[]).to(player);
        return true;
    }
    feedback(w, player, result)
}

fn feedback(w: World, player: u64, result: u32) -> bool {
    let key = match result {
        result::FIRST_CORNER => "assembly.base.first_corner",
        result::BOUNDS => "assembly.base.bounds",
        result::PREVIEW => "assembly.base.preview",
        result::CONFIRMED => "assembly.base.confirmed",
        result::TOO_LARGE => "assembly.base.too_large",
        result::UNAVAILABLE => "assembly.base.unavailable",
        result::CLEARED => "assembly.base.cleared",
        result::DOCKING => "assembly.base.docking",
        result::ALREADY_ASSEMBLED => "assembly.base.already_assembled",
        result::SEATED => return true,
        _ => return false,
    };
    w.hint(key, &[]).to(player);
    true
}

pub fn register(r: &mut Registry) {
    r.item(&GRAB_TOOL)
        .texture("grab_tool")
        .handheld()
        .max_stack(1)
        .tag("body_grab")
        .tab(&crate::tabs::TOOLS);
    r.item(&TOOL)
        .texture("assembly_tool")
        .handheld()
        .max_stack(1)
        .tab(&crate::tabs::TOOLS)
        .on_use(|u| {
            let w = u.world();
            let player = u.entity();
            let cell = u.target_block().map(|(p, _)| p);
            feedback(w, player, w.assembly_select(player, cell, true))
        });
    r.block(&HELM)
        .facing(Facings::HORIZONTAL, Toward::Away)
        .body_helm()
        .density(0.65)
        .texture("helm_side")
        .states(|p, s| {
            let mut faces = [("helm_side", UV_FROM_BOX); 6];
            faces[Dir::North as usize].0 = "helm_front";
            faces[Dir::Up as usize].0 = "helm_top";
            faces[Dir::Down as usize].0 = "planks_oak";
            s.orientation(Orientation::front(p.facing()))
                .model_box_faces([0.0; 3], [16.0, 8.0, 16.0], faces)
        })
        .sound_group("wood")
        .hardness(2.0)
        .mined_by("axe", tool_tier::HAND)
        .tab(&crate::tabs::REDSTONE)
        .on_use(|u| use_controller(u.world(), u.player(), u.pos(), false));

    r.block(&GUIDE)
        .facing(Facings::ALL, Toward::Away)
        .signal_guide()
        .texture("guide_case_arrow")
        .top("guide_case_grip")
        .front("guide_case_front")
        .back("guide_case_back")
        .states(|_, s| s.face_turns([1, 3, 2, 0, 0, 0]))
        .sound_group("stone")
        .hardness(3.5)
        .mined_by("pickaxe", tool_tier::HAND)
        .tab(&crate::tabs::REDSTONE)
        .on_use(|u| use_controller(u.world(), u.player(), u.pos(), false));

    r.block(&GUIDE_RAIL)
        .axis()
        .guide_rail()
        .texture("guide_rail_side")
        .ends("guide_rail_end")
        .sound_group("stone")
        .hardness(3.5)
        .mined_by("pickaxe", tool_tier::HAND)
        .tab(&crate::tabs::REDSTONE);
    r.shaped(&base_api::CRAFTING, "linear_guide", 1, &["ipi", "iri"], &[('i', "#ingots/iron"), ('p', "piston"), ('r', "redstone")]);
    r.shaped(&base_api::CRAFTING, "guide_rail", 6, &["iii", "sss", "iii"], &[('i', "#ingots/iron"), ('s', "#stick")]);

    r.set("#materials/wood", &DENSITY, 0.55);
    r.set("#logs", &DENSITY, 0.62);
    r.set("#planks", &DENSITY, 0.55);
    r.set("base:iron_block", &DENSITY, 2.4);
    r.set("base:gold_block", &DENSITY, 3.5);
}
