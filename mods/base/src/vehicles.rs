use weft_sdk::{
    abi::{RotorRule, assembly_result, rotor_motion},
    *,
};

static WHEEL_SMALL: Block = Block::named("wheel_small");
static WHEEL: Block = Block::named("wheel");
static WHEEL_LARGE: Block = Block::named("wheel_large");
static PLANKS: Block = Block::named("oak_planks");
static STRIPE: Block = Block::named("blue_wool");
static GLASS: Block = Block::named("glass");
static IRON: Block = Block::named("iron_block");
static HELM: Block = Block::named("helm");
static AIR: Block = Block::named("core:air");

pub fn register(r: &mut Registry) {
    for (block, radius, half_length, recipe) in [
        (&WHEEL_SMALL, 8.0f32, 4.0, &["wiw"][..]),
        (&WHEEL, 12.0, 6.0, &["w w", " i ", "w w"][..]),
        (&WHEEL_LARGE, 16.0, 7.0, &["www", "wiw", "www"][..]),
    ] {
        r.block(block)
            .facing(Facings::ALL, Toward::Away)
            .texture("wheel_tread")
            .signal_blocker()
            .density(0.9)
            .hardness(1.0)
            .sound_group("cloth")
            .tab(&crate::tabs::REDSTONE)
            .states(move |_, s| {
                let mut s = s.cylinder(radius / 16.0, half_length / 16.0, 1.15);
                let tread = "wheel_tread";
                let side = "wheel_side";
                let faces = [tread, tread, tread, tread, side, side].map(|texture| (texture, UV_FROM_BOX));
                let strips = 16;
                for j in 0..strips {
                    let y0 = 2.0 * radius * (j as f32 / strips as f32 - 0.5);
                    let y1 = y0 + 2.0 * radius / strips as f32;
                    let mid = (y0 + y1) * 0.5;
                    let x = (radius * radius - mid * mid).sqrt();
                    s = s.model_box_faces([8.0 - x, 8.0 + y0, 9.0 - half_length], [8.0 + x, 8.0 + y1, 7.0 + half_length], faces);
                }
                let hub = radius * 0.33;
                for z in [8.0 - half_length, 7.0 + half_length] {
                    s = s.model_box_faces([8.0 - hub, 8.0 - hub, z], [8.0 + hub, 8.0 + hub, z + 1.0], [("wheel_hub", UV_FROM_BOX); 6]);
                }
                s
            });
        r.shaped(&base_api::CRAFTING, block.name(), 1, recipe, &[('w', "#wool/*"), ('i', "#ingots/iron")]);
    }
    r.command("spawncar", "[at:pos]", spawn_car);
}

fn rule(min: u8, max: u8, motion: u8, degrees: f32, speed: f32) -> RotorRule {
    RotorRule { min, max, motion, degrees, speed }
}

fn spawn_car(cmd: &mut ServerCommand) {
    let world = cmd.world();
    let player = cmd.player();
    let Some(position) = cmd.pos("at").or_else(|| {
        world.entity(player).map(|e| {
            let [x, z] = yaw_dir(e.yaw);
            [e.pos.x + 8.0 * f64::from(x), e.pos.y, e.pos.z + 8.0 * f64::from(z)].into()
        })
    }) else {
        return;
    };
    if position
        .to_array()
        .iter()
        .any(|&n| !n.is_finite() || n.abs() > f64::from(i32::MAX - 16))
    {
        cmd.reply("vehicle.base.no_space", &[]);
        return;
    }
    let origin = block_pos_of(position).offset(0, 3, 0);
    let at = |[x, y, z]: [i32; 3]| origin.offset(x, y, z);
    let air = AIR.id();
    let clear = (-3..=3).all(|x| (-3..=2).all(|y| (-4..=5).all(|z| world.block_loaded(at([x, y, z])) == Some(air))));
    if !clear {
        cmd.reply("vehicle.base.no_space", &[]);
        return;
    }

    let plank = PLANKS.id();
    let stripe = STRIPE.id();
    let rotor = |facing: &str| crate::redstone::ROTOR.with(&[("facing", facing), ("mode", "hold")]);
    let steer = at([0, 0, -2]);
    let lift = crate::redstone::THRUSTER.with(&[("facing", "up")]);
    let boost = crate::redstone::THRUSTER.with(&[("facing", "north")]);
    let comparator = crate::redstone::COMPARATOR.with(&[("front", "south")]);
    let wire = crate::redstone::REDSTONE.with(&[("up", "true")]);
    let mut cells = vec![
        (origin, HELM.with(&[("facing", "north")])),
        (steer, rotor("down")),
        (at([0, 0, -1]), plank),
        (at([0, 0, -3]), plank),
        (at([0, -1, -2]), plank),
        (at([0, -1, 0]), plank),
        (at([0, -1, 1]), boost),
        (at([0, -1, 2]), GLASS.id()),
        (at([0, 0, 1]), comparator),
        (at([0, 0, 2]), comparator),
        (at([0, 0, 3]), plank),
        (at([0, -1, 3]), wire),
    ];
    for x in -1..=1 {
        cells.push((at([x, 1, -3]), plank));
    }
    for (x, face) in [(-1, "west"), (1, "east")] {
        cells.extend([
            (at([x, 0, 0]), stripe),
            (at([2 * x, 0, 0]), lift),
            (at([x, 0, 1]), lift),
            (at([x, 0, 2]), IRON.id()),
            (at([x, 0, 3]), plank),
            (at([x, -1, 3]), rotor(face)),
            (at([x, -1, -2]), rotor(face)),
            (at([2 * x, -1, -2]), WHEEL.with(&[("facing", face)])),
            (at([2 * x, -1, 3]), WHEEL.with(&[("facing", face)])),
        ]);
    }

    let mut placed = 0;
    for &(p, state) in &cells {
        if !world.set_block(p, state) {
            break;
        }
        placed += 1;
    }
    let steer_rules = [
        rule(0, 2, rotor_motion::ABSOLUTE, 0.0, 96.0),
        rule(3, 9, rotor_motion::ABSOLUTE, -28.0, 96.0),
        rule(10, 15, rotor_motion::ABSOLUTE, 28.0, 96.0),
    ];
    let controls = [
        (Dir::South, "W", 15),
        (Dir::South, "S", 8),
        (Dir::North, "A", 7),
        (Dir::North, "D", 15),
        (Dir::East, "Q", 7),
        (Dir::West, "E", 7),
        (Dir::East, "Space", 15),
        (Dir::West, "Space", 15),
        (Dir::Down, "ControlLeft", 15),
    ];
    let programmed = placed == cells.len()
        && world.set_controller_bindings(origin, &controls)
        && world.set_rotor_program(steer, &steer_rules, true)
        && world.set_assembly_bounds(steer, at([-2, -2, -3]), at([2, 0, -1]))
        && [-1, 1].into_iter().all(|x| {
            let motor = at([x, -1, 3]);
            let (forward, reverse) = if x > 0 {
                (rotor_motion::COUNTERCLOCKWISE, rotor_motion::CLOCKWISE)
            } else {
                (rotor_motion::CLOCKWISE, rotor_motion::COUNTERCLOCKWISE)
            };
            world.set_rotor_program(
                motor,
                &[rule(0, 0, rotor_motion::FREEWHEEL, 0.0, 720.0), rule(1, 9, reverse, 0.0, 360.0), rule(10, 15, forward, 0.0, 550.0)],
                true,
            ) && world.set_assembly_bounds(motor, at([2 * x, -2, 2]), at([2 * x, 0, 4]))
                && world.set_rotor_program(at([x, -1, -2]), &[rule(0, 15, rotor_motion::FREEWHEEL, 0.0, 720.0)], true)
                && world.set_assembly_bounds(at([x, -1, -2]), at([2 * x, -2, -3]), at([2 * x, 0, -1]))
        });
    if programmed && world.assemble_controller(player, origin) == assembly_result::CONFIRMED {
        cmd.reply("vehicle.base.spawned", &[]);
        return;
    }
    for &(p, _) in cells[..placed].iter().rev() {
        world.set_block(p, air);
    }
    cmd.reply("vehicle.base.spawn_failed", &[]);
}
