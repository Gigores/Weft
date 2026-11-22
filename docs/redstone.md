# Redstone guide

Redstone here isn't vanilla's. The numbers are familiar (power 0 to 15, a redstone tick is 2 game ticks), but the rules are simpler and they don't care which way you build:

- A build works the same in any direction, turned or mirrored. There's no update order to fight and no quasi-connectivity, so 0-tick tricks and the like don't exist.
- Wire goes on floors, walls and ceilings. Gates and switches go on the face you click, like torches.
- A circuit that isn't changing costs nothing, however big it is.

Vanilla circuits that rely on those quirks behave differently. That's on purpose.

## Quick links
[The rules](#the-rules) | [What's in the game](#whats-in-the-game) | [Making your own](#making-your-own) | [Reading it from code](#reading-it-from-code) | [Limits](#limits)

## The rules

**Wire** lies on any face of a solid block, several faces in one cell if you like (it is an `on_sides` block, `west`..`south` properties saying where it lies). A piece connects to the next over each of its edges: into the corner of a wall in the same cell, on along the same plane, or around the edge of the block it lies on. Going up a step means going floor, wall, top. Power drops by one with each piece, so it reaches fifteen pieces.

Only wire of the same block connects, so another wire block is an insulated wire.

**Power reaches a block two ways:**

- *Strongly*, from a gate facing it, a lever on it, or a torch next to it (not the block the torch is on). Wire touching that block takes the power, and so do devices.
- *Weakly*, from wire lying on it. Only devices next to it take that. Otherwise all the wires on one wall would be one wire.

Solid blocks carry power by default, lamps and other blocks that only take power included. `.signal_blocker()` turns that off, the way glass, ice, leaves and the golden lamp have it. Wire can still lie on them. Slabs and other partial solid blocks carry power too.

**Wire reaches** the block it lies on and any device touching one of its edges or its back. It doesn't reach across the cell, so wire on a floor doesn't touch what is on the ceiling above it. Wire running past a lamp lights it. To get past without lighting it, take another face.

**Timing:** gates act after their delay and wire is instant. A piston takes 2 ticks: what it moves is there for circuits then, and is drawn as a block 2 ticks later, when the moving picture has arrived. All of a tick is read as the last tick left it, then applied, which is why order doesn't show. Every loop goes through a gate, so nothing can hang a tick.

**Pistons** move chests and anything else with an entity, which stays the same one, with what is in it. Those that want the same block in the same tick both give way, since letting one win would favor a direction, and the one still wanting it goes as soon as the other stops. Two sticky ones bringing back one block both come back without it. A sticky piston that loses power within 2 ticks of starting comes back without its block. A piston being pushed by another doesn't count as wanting anything: it waits, and goes from where it lands if it has power there.

**On what moves:** circuits go on on a piston's blocks and on a body, pulses on their way included, and a piston moving a block takes what stands on it: dust, gates, torches, levers. Only dust lying directly in the way breaks. Being on a piston's blocks, a piston waits till it lands. Bodies and the world power each other where they are next to each other: a redstone block going past lamps lights them, and a lamp going past one is lit. Wire of a body and wire of the world don't join or feed each other, though each lights what it touches on the other side. An observer sees a body come and go, and one on a body sees the world change as it goes.

**Unloaded blocks:** what can't see all of its inputs stays as it is until they load, instead of flipping. A saved world starts the same way. A pulse in flight inside a repeater isn't saved, and doesn't survive parts of the world joining into one region or splitting. Blocks on the way from a piston get there at once when that happens or when the world saves.

## What's in the game

| Block | What it does |
|---|---|
| `redstone` | The dust, one piece per click |
| `lever` | Toggles, strongly powers what it's on |
| `stone_button` | On for a second |
| `redstone_block` | Always on, no strong power |
| `redstone_torch` | Off while what it's on has power. Strongly powers every other block around it, so one on a wall powers both the block in front and the one above. 8 flips in 3 seconds burn it out for 8 |
| `repeater` | Full power out the front, 1 to 4 redstone ticks after the back gets any (click to change). A gate pointing into its side locks it |
| `comparator` | Back against the sides; the other mode subtracts (click to change). Reads how full a container behind it is |
| `redstone_lamp` | Lit with any power around it. Carries power like any solid block |
| `piston`, `sticky_piston` | Pushes up to 12 blocks, powered from any side but its front. Breaks what isn't solid, stops at block entities and `.immovable()` blocks. The blocks move as a body, so whoever stands on them rides along smoothly |
| `slime_block` | What a piston moves takes what touches it along, for flying machines. `.sticky()` on any block does the same |
| `floater` | Moves in the direction of its front arrow. Power from any other side moves it and the connected blocks at its front by half a block per second per power level. Attached parts come along and drop if they hit a solid block. Sneak and right-click with an empty hand to switch between Disassemble and Hold. Hold keeps a floater's platform assembled after it stops. |
| `thruster` | Pushes an assembled body in the direction of its arrow. The force scales with input power. A thruster offset from the center of mass also turns the body. |
| `rotor` | Turns the assembly on its front relative to the surface it is mounted on. Its facing sets the axis and direction. A stronger signal turns it faster. Sneak and right-click with an empty hand to change its stop mode. The amber face marks Disassemble, which finishes the quarter turn or returns to the last clear quarter turn before placing the blocks on its parent's grid. Cycle power to restart after a blocked disassembly. The cyan pause mark indicates Hold. The rotor keeps its current angle and resumes when it has power and the path is clear. |
| `assembler` | Holds the assembly on its front stationary relative to the surface it is mounted on while powered. The blocks return to that grid when power cuts out. |
| Controller (`helm`) | Assembles a free contraption and provides a seat with configurable redstone outputs on each face. |
| `golden_lamp` | Each new pulse flips it. A comparator reads 15 while it is lit. Doesn't carry power |
| `anchor` | With power it can't be moved: in a row being pushed it stops the push, touching slime it stays behind. On a platform on its way, getting power with more of the platform pushing it stops the platform at the next whole block; with nothing pushing it, it rides on. Without power it moves like any block |
| `observer` | A 1 redstone tick pulse out of its back, 1 redstone tick after the block it looks at changes (wire's power too), and when a piston moves it |
| `dispenser` | One item out of the front for each new pulse |

## Connected assemblies

Right-click a Controller, floater, rotor or assembler to preview its connected blocks. Right-click it again to confirm. A Controller launches the contraption immediately. A floater, rotor or assembler saves the selection and uses it when powered.

The selection starts at the Controller or the block immediately in front of another assembly device. It follows movable solid blocks that share a face and takes supported parts within the selected bounds. A floater also carries itself. If the build touches a dock, set two opposite corners by right-clicking them with an Assembly Wand, then preview and confirm on the controller. The limit is 4,096 carried cells and parts.

After choosing two corners with the wand, right-click into the air to assemble the previewed volume as a standalone contraption that stays in place. Sneak while right-clicking into the air to assemble it as a free body affected by gravity and buoyancy. Both modes can include separated groups of blocks inside the box.

After launching a contraption, right-click the Controller with an empty hand to sit. Sneak and right-click a face with an empty hand to assign keys and signal levels to that face. Use the outputs with redstone devices such as thrusters. Dismount with Shift by default. Use the Assembly Wand on an assembled Controller to put the contraption back on the grid when there is room for its blocks.

Controllers can be placed on a structure after it was assembled. Using a new Controller on a free platform selects its seat and redstone outputs. A floater on a ship adds straight-line drive. Thrusters work on free ships and platforms, and force from several thrusters adds up at their individual mounting points.

Rotors and assemblers also work on moving structures. Each attachment can carry more controllers. To select a smaller part of a moving structure, mark both corners on that structure with the Assembly Wand and confirm the selection on its controller. A thruster on a rotor turns its thrust together with the rotor. Attachments return to their parent grids before the parent disassembles.

In Creative, take the Creative Grab Wand from the Tools tab. Left-click a contraption to grab it at the point you aimed at. Look or move to carry it. Hold right-click and move the mouse to rotate it around the grabbed point while the camera stays put. Left-click again to release. The wand can carry any mass and stops at obstacles. Grabbing a nested section carries the entire contraption. A section mounted directly in the world detaches from its mount.

Place Guide Rails from the Linear Guide's socket along its arrows. A rail's long stripes run perpendicular to the face you click when placing it. The marked side of the guide shows which side of the last rail to put the carriage against. Power the guide to assemble the carriage. Sneak and right-click with an empty hand to edit its travel limits, unloaded rest position, stiffness and damping. Closing the screen saves valid settings. The available travel is limited by the length of the installed rails and the 32-block maximum.

World Rules and `/rule` tune ship motion for the current world. `ship_thrust` sets the Debug drive's maximum acceleration and defaults to `7` blocks/s². `ship_water_drag` defaults to `1.8`. More thrust or less drag makes a ship faster.

`ship_water_side_drag` sets sideways drag as a fraction of the forward value. The default `0.4` lets the hull carry sideways momentum into a turn. Set it to `1` for equal drag along and across the hull.

Moving hulls leave a continuous foam wake that follows their path. The wake and bow wave span the submerged hull. A hard entry into the water throws an expanding splash along the waterline. Powered thrusters show exhaust at the nozzle, or bubbles when the nozzle is submerged in a liquid with bubbles.

In Creative, the Controller's optional Debug drive uses the forward and backward movement keys for thrust and the left and right movement keys to steer. Its facing sets forward. On land, the Debug drive runs at reduced power and ground drag slows the body.

Each thruster's force scales with its redstone input. The total force, the body's mass and its drag determine the resulting speed. Air resistance grows with speed, including during free fall.

Wood usually floats and stone and metal usually sink. A compartment with a floor and watertight sides displaces water even with an open deck. It stays dry below the waterline until water reaches an opening, such as a submerged breach or a low edge of the hull. Blocks placed away from the center of mass can change the ship's trim or roll. A hit away from the center of mass can turn the ship, and turning into an obstacle can push it sideways.

### Wheels and a starter car

Wheels come in 1, 1.5, and 2 block diameters. Place a wheel with its axle in line with a rotor to drive it. The tread grips the ground and other contraptions, including moving decks. Smooth blocks such as ice give less traction.

Use the rotor's **Freewheel** program to let a single wheel centered on its axle turn with the surface. Its speed setting limits the wheel's coasting speed. Mount another rotor across the axle to steer. The wheel can keep rolling as the steering rotor turns it.

Use `/spawncar [x y z]` on a clear flat area to assemble a car with a Controller, rear drive rotors and a steerable front axle with free-rolling wheels. Right-click the Controller with an empty hand. W and S drive the rear wheels, A and D turn the front axle, Space lifts the car, Q and E roll it with side thrusters, and left Ctrl fires the rear booster. Shift dismounts.

## Making your own

Say what a block is and the engine runs it. No ticking and no neighbor callbacks to write.

```rust
// Always on.
r.block(&CRYSTAL).signal_constant();

// Follows the power around it: a `power` property (`off`, `on`) changes by itself.
r.block(&LIGHT)
    .signal_sink()
    .prop("power", ["off", "on"])
    .states(|p, s| if p.is("power", "on") { s.light(15) } else { s });

// The same with code: called when the power around it changes.
r.block(&ALARM).signal_sink().on_signal(|e| {
    if e.rising() {
        e.world().spawn_item(/* ... */);
    }
});

// A gate with your own logic, on the clicked face. Runs only when something around it changes,
// and what it returns comes out of the front after the delay (in redstone ticks, like a repeater's).
// `Gate::new()` reads behind it and both sides; `.inputs(&[Side::Left, Side::Right, Side::Top])`
// names other faces, `.outputs(&[Side::Front, Side::Top])` gives at more than the front, `.delay(4)`
// waits longer. Sides are the gate's own, so they turn with it.
r.block(&AND_GATE)
    .signal_gate(Gate::new(), |s| if s.side(Side::Rear) > 0 && (s.side(Side::Left) > 0 || s.side(Side::Right) > 0) { 15 } else { 0 })
    .model_box([0.0, 0.0, 0.0], [16.0, 2.0, 16.0])
    .not_solid();

// Wire that doesn't join base's. Textures: the dot on top, the line on the sides, in gray; power
// colors it.
r.block(&BLUE_WIRE).wire().texture("blue_line").ends("blue_dot");

// Glass-like, and something a piston can't move.
r.block(&CASING).signal_blocker();
r.block(&BEDROCK_ISH).immovable();
```

The rest of the set, each with the properties it needs added for you:

- `.signal_switch()`: a lever or button. Goes on the clicked face; flip its `power` property from `on_use`.
- `.signal_torch()`, `.signal_repeater()`, `.signal_comparator()`: as in the game.
- `.signal_read(|w, p| level)`: what a comparator behind the block reads, instead of how full it is. It looks again when the block changes; `w.signal_read_changed(p)` for anything else it depends on.
- Use `.signal_floater(breaks)` after `.facing(..)` to register a floater. Setting `breaks` lets it break obstacles. The `power` property is 0 when idle, 1 while moving and 2 when blocked. The `mode` property defaults to `disassemble` and also accepts `hold`.
- `.signal_thruster(force)` after `.facing(..)` gives a block redstone-driven propulsion. `force` is the force at power 15, applied at the block's center. The body's mass and inertia determine how it responds.
- `.signal_body_mount(rotates)` after `.facing(..)` assembles a body attached to the controller's frame. Set `rotates` for a rotor with a `mode` of `disassemble` or `hold`. The `power` state is 0 idle, 1 assembled, or 2 blocked after an attempted disassembly or assembly.
- `.body_helm()` gives a horizontal block the role of a Controller. Use `w.assembly_select(player, Some(pos), tool)` from a controller or tool callback. A tool click on other blocks selects corners. `None` confirms the volume with `tool = true` or clears the selection with `tool = false`. A free wand assembly responds to gravity and buoyancy and stays assembled when it comes to rest. `.density(value)` gives a block a mass per unit of collision volume, in units where water is `1.0` and unspecified blocks are `1.45`.
- `w.set_ship_controls(id, Some(ShipControls { throttle, steering }))` supplies inputs at the next physics step. Both axes run from `-1.0` to `1.0`. Positive values mean forward and right. Pass `None` to return control to the pilot. The override is kept in memory until cleared or unloaded.
- `w.ship_flooded_cells(id)` returns the last physics step's flooded cells in the coordinates used by `w.in_space(id)`. A dry ship returns an empty list. The world-space `w.in_liquid(point)` accounts for dry hull volume.
- `r.on_event(&SHIP_COLLISION, |batch| { .. })` reads the strongest applied impulse for each obstacle a ship hits in its tick. The event has the ship and other body IDs (zero means the terrain), a world-space point and normal, the normal approach speed used by the solver and the normal impulse. Sustained contact can produce events on consecutive ticks.
- `w.body_from_blocks(center, &cells)` takes blocks into a scripted body with its chests, parts and circuits. Move it with `w.entity_modify(id).velocity(..)` or `w.body_move_to(id, pos, ticks)`. `w.body_dynamics(id)` enables gravity, buoyancy and rigid-body contacts. `w.body_force(id, at, force)` applies a force for one physics step at a point in the body's local coordinates. Use `w.in_space(id)` to read or edit its blocks.
- `w.body_to_blocks(id)` docks a body at grid coordinates. `w.body_fall(id)` puts its blocks down when it reaches support. `w.push_blocks(pos, dir)` moves blocks one cell by piston rules, including sticky neighbors.
- `.signal_observer()` and `.signal_piston(head, sticky)`, `.signal_piston_head()`: after `.facing(..)`.

**A model on any face:** those blocks have `attach` and `front` properties. Write the model once for a floor with the front to the north, and the block turns it onto its face, pictures included:

```rust
r.block(&GATE).signal_repeater().states(|p, s| {
    let top = if p.is("power", "on") { "gate_on" } else { "gate" };
    let mut faces = [("smooth_stone", UV_FROM_BOX); 6];
    faces[3].0 = top;
    s.model_box_faces([0.0, 0.0, 0.0], [16.0, 2.0, 16.0], faces)
});
```

A block with `facing` turns the same way, its front to it; `s.orientation(Orientation::up(p.facing()))` turns its top that way instead, like a piston.

`base` is written with exactly this: see `mods/base/src/redstone.rs`.

## Reading it from code

`w.signal(pos)` is the strongest power coming into a cell, 0 to 15.

## Limits

- Wire on moving bodies runs but isn't drawn.
- A gate changing is still a block change, so on the client it remeshes its cube. Only wire avoids that.
