//! Stage 8: moving collisions with the test mod's lift, skiff and crate.

use std::time::Duration;

use weft_sdk::*;

use super::add;
use crate::runner::{R, T};

async fn spawn(t: &T, kind: &str, p: [i32; 3]) -> Result<u64, String> {
    let id = t
        .op(&format!("spawn {kind} {} {} {}", p[0], p[1], p[2]))
        .await?;
    id.trim().parse().map_err(|_| format!("spawn {kind}: {id}"))
}

/// Server position of an entity (`me`: the player as the server last heard).
pub(super) async fn pos_of(t: &T, id: &str) -> Result<[f64; 3], String> {
    let line = t.op(&format!("entity {id}")).await?;
    let v: Vec<f64> = line
        .split_whitespace()
        .skip(1)
        .take(3)
        .filter_map(|s| s.parse().ok())
        .collect();
    v.try_into().map_err(|_| format!("entity #{id}: {line}"))
}

pub async fn spring_wire(t: T) -> R {
    t.flat().await?;
    let base = add(t.feet(), [0, 4, -5]);
    let at = |p| add(base, p);
    t.set(base, "base:linear_guide[facing=up]").await?;
    for y in 1..=3 {
        t.set(at([0, y, 0]), "base:guide_rail").await?;
        t.set(at([-1, y, 0]), "base:redstone[east=true]").await?;
    }
    t.fill(at([0, 3, 1]), at([0, 3, 2]), "base:gold_block")
        .await?;
    t.set(at([-1, 3, 1]), "base:redstone[east=true]").await?;
    t.set(at([-1, 3, 2]), "base:redstone_lamp").await?;
    let (source, activation) = (at([-1, 0, 0]), at([1, 0, 0]));
    t.set(source, "base:redstone_block").await?;
    t.set(activation, "base:redstone_block").await?;
    t.secs(1.2).await?;

    let list = t
        .op(&format!("near {} {} {} 8", base[0], base[1], base[2]))
        .await?;
    let body = list
        .split_whitespace()
        .find_map(|entry| entry.strip_suffix(":core:platform"))
        .ok_or_else(|| format!("no guide carriage among {list}"))?;
    let [_, y, _] = pos_of(&t, body).await?;
    t.check(y >= f64::from(base[1] + 1) - 0.01 && y < f64::from(base[1] + 2), format!("the load compressed the guide: {y}"));
    let lamp = format!("sblock {body} -1 0 1");
    t.check(t.op(&lamp).await?.contains("power=on"), "dust powers the lamp across the compressed joint");

    t.set(activation, "core:air").await?;
    t.set(source, "core:air").await?;
    t.secs(1.0).await?;
    t.check(t.op(&lamp).await?.contains("power=off"), "without a source neither side keeps the other powered");
    Ok(())
}

/// A chest on a flying structure is a chest: it keeps what is in it, opens, goes with the structure,
/// is saved with it, and spills where it is when it breaks.
pub async fn body_chest(t: T) -> R {
    t.world(1).await?;
    t.op("mode creative").await?;
    let f = t.feet();
    let id = spawn(&t, "testmod:skiff", add(f, [0, 12, 0])).await?;
    t.op(&format!("structureset {id} 1 1 0 base:chest")).await?;
    t.frames(5).await?;
    let chest = t.op(&format!("block_instance 1 1 0 {id}")).await?;
    t.check(chest != "0", "the chest on the skiff owns block data");
    let block_pos = |t: T, body: u64, chest: String| async move {
        let text = t.op(&format!("block_pos {chest} {body}")).await?;
        let values: Vec<f64> = text
            .split_whitespace()
            .filter_map(|s| s.parse().ok())
            .collect();
        <[f64; 3]>::try_from(values).map_err(|_| format!("bad block position {text}"))
    };
    t.op(&format!("put {chest} 0 base:cobblestone 7")).await?;
    // It is where its block is, however the structure goes.
    let (a, b) = (pos_of(&t, &id.to_string()).await?, block_pos(t.clone(), id, chest.clone()).await?);
    // Asked a tick apart, the structure having gone on by 0.3 of a block.
    t.check(
        (b[0] - a[0] - 1.0).abs() < 0.7 && (b[1] - a[1] - 1.5).abs() < 0.1,
        format!("the chest is at its block: structure {a:?}, chest {b:?}"),
    );
    t.secs(1.0).await?;
    let (a, b2) = (pos_of(&t, &id.to_string()).await?, block_pos(t.clone(), id, chest.clone()).await?);
    t.check((b2[0] - b[0]).abs() > 2.0, "it goes with the structure");
    t.check((b2[0] - a[0] - 1.0).abs() < 0.7, format!("and stays at its block: structure {a:?}, chest {b2:?}"));

    // Open from on board.
    t.op(&format!("onto {id} 1.0")).await?;
    t.secs(0.4).await?;
    t.c().set_look(-90.0, -40.0);
    t.secs(0.2).await?;
    t.check_eq(t.state().target_body, id, "the chest on the structure is looked at");
    t.press("core:use");
    t.secs(0.4).await?;
    let open = t.c().screen();
    t.check(!open.is_empty(), "it opens from on board");
    // Still open after it flew on.
    t.secs(0.4).await?;
    t.check_eq(t.c().screen(), open, "and stays open as it flies");
    t.c().menu_close();
    t.no_screen().await?;

    // Saved with it.
    let name = format!("test-{}", t.name());
    t.c().world_leave();
    t.screen("core:title").await?;
    t.c().world_open(&name, 1);
    t.until("joined again", Duration::from_secs(60), |t| t.flag(client::client_flags::JOINED))
        .await?;
    t.loaded().await?;
    let list = t
        .op(&format!("near {} {} {} 96", f[0], f[1] + 12, f[2]))
        .await?;
    let id: u64 = list
        .split_whitespace()
        .find(|e| e.ends_with("testmod:skiff"))
        .and_then(|e| e.split(':').next())
        .and_then(|e| e.parse().ok())
        .ok_or(format!("no skiff among {list}"))?;
    let chest = t.op(&format!("block_instance 1 1 0 {id}")).await?;
    let (name, count, _) = t.slot(&chest, 0).await?;
    t.check(name.contains("cobblestone") && count == 7, format!("what it held came back: {name} {count}"));

    // Spills in the world, at the chest.
    let at = block_pos(t.clone(), id, chest.clone()).await?;
    t.op(&format!("structureset {id} 1 1 0 air")).await?;
    t.frames(5).await?;
    let list = t
        .op(&format!("near {} {} {} 6", at[0].floor(), at[1].floor(), at[2].floor()))
        .await?;
    t.check(list.contains("core:item"), format!("what it held fell out right there: {list}"));
    t.check(t.op(&format!("entity {chest}")).await.is_err(), "the chest is gone");
    Ok(())
}

/// Sneaking, nobody steps off the edge of what they stand on, whichever way that is going.
/// Each walk has to get to the edge, or it says nothing.
pub async fn body_sneak_edge(t: T) -> R {
    t.flat().await?;
    t.cmd("/rule testkit:no_hurt 1").await?;
    let f = t.feet();
    let y = 8;
    let skiff = t
        .op(&format!("spawn testmod:skiff {} {} {}", f[0], f[1] + y, f[2] + 9))
        .await?;
    // With more on it, lower, the way it goes: past the deck's edge isn't past the box around it.
    let more = t
        .op(&format!("spawn testmod:skiff {} {} {}", f[0], f[1] + y + 8, f[2] + 9))
        .await?;
    for x in [-2, 2] {
        t.op(&format!("structureset {more} {x} -1 0 base:oak_planks"))
            .await?;
    }
    let still_at = add(f, [-8, y, 0]);
    t.fill(add(still_at, [-1, 0, -1]), add(still_at, [1, 0, 1]), "base:stone")
        .await?;
    let still = t
        .op(&format!("lift {} {} {} 1", still_at[0], still_at[1], still_at[2]))
        .await?;
    let cross_at = add(f, [8, y, 0]);
    for p in [[0, 0], [1, 0], [-1, 0], [0, 1], [0, -1]] {
        t.set(add(cross_at, [p[0], 0, p[1]]), "base:stone").await?;
    }
    let cross = t
        .op(&format!("lift {} {} {} 1", cross_at[0], cross_at[1], cross_at[2]))
        .await?;
    t.frames(5).await?;
    let along = [("core:left", 0.0f32), ("core:right", 0.0)];
    let corners = [("core:forward", 45.0f32), ("core:forward", 225.0)];
    for (name, id, walks) in [
        ("a structure going sideways", &skiff, &along),
        ("one with more on it", &more, &along),
        ("one standing still", &still, &along),
        ("the corners of a cross", &cross, &corners),
    ] {
        let Ok(body) = id.parse::<u64>() else { return Err(format!("no {name}")) };
        for (step, &(key, yaw)) in walks.iter().enumerate() {
            // Off what it was on first. The chat helper needs a distinct reply each time.
            t.cmd(&format!("/tp {} {} {}", f[0], f[1] + 20 + step as i32, f[2]))
                .await?;
            t.secs(0.2).await?;
            t.op(&format!("onto {id} 1.0")).await?;
            t.c().set_look(yaw, 0.0);
            t.secs(0.5).await?;
            t.hold("core:sneak", 2.4);
            t.frames(3).await?;
            t.hold(key, 2.2);
            let began = std::time::Instant::now();
            let (mut far, mut fell, mut log) = (0.0f64, false, Vec::new());
            while began.elapsed() < Duration::from_secs_f32(2.2) {
                t.frames(1).await?;
                let st = t.state();
                let Some(on) = t.c().entity(body).map(|e| e.pos) else { continue };
                let rel = [st.pos.x - on.x, st.pos.y - on.y, st.pos.z - on.z];
                far = far.max(rel[0].abs()).max(rel[2].abs());
                log.push(format!("{:.3} {:.3} {:.3} ground {}", rel[0], rel[1], rel[2], t.flag(client::client_flags::ON_GROUND)));
                if rel[1] < 0.7 {
                    fell = true;
                    break;
                }
            }
            let last = log[log.len().saturating_sub(6)..].join(" | ");
            t.check(!fell, format!("not off {name}, {key} at {yaw}: {last}"));
            t.check(fell || far > 0.7, format!("walking got to the edge of {name}, {key} at {yaw}: only {far:.2}"));
            t.secs(2.5 - began.elapsed().as_secs_f32().min(2.5)).await?;
        }
    }
    Ok(())
}

/// Blocks go on a structure wherever it has none yet, not only in the parts of it that happen to
/// be stored already.
pub async fn body_place(t: T) -> R {
    t.flat().await?;
    t.op("mode creative").await?;
    t.op("put me 0 oak_planks 64").await?;
    t.c().select_slot(0);
    let f = t.feet();
    // A plate lifted around a spot over it: all it has is a layer under its middle.
    let center = add(f, [0, 4, -5]);
    t.fill(add(center, [-1, -1, -1]), add(center, [1, -1, 1]), "base:slime_block")
        .await?;
    let id = t
        .op(&format!("lift {} {} {} 1", center[0], center[1], center[2]))
        .await?;
    t.cmd(&format!("/tp {} {} {}", center[0] as f64 + 0.5, center[1] + 2, center[2] as f64 + 3.5))
        .await?;
    t.c().set_flying(true);
    t.secs(0.5).await?;
    t.look_at(add(center, [0, -1, 0]));
    t.secs(0.3).await?;
    t.check(t.state().target_body.to_string() == id, "looking at the structure");
    t.press("core:use");
    t.secs(0.5).await?;
    t.check_eq(t.op(&format!("sblock {id} 0 0 0")).await?, "base:oak_planks".to_string(), "a block goes on top of the plate");
    // Beyond anything it has: past its edge, from the side.
    t.look_at(add(center, [1, -1, 1]));
    t.secs(0.3).await?;
    t.press("core:use");
    t.secs(0.5).await?;
    t.check_eq(t.op(&format!("sblock {id} 1 0 1")).await?, "base:oak_planks".to_string(), "and another");

    t.op(&format!("sblock {id} 0 0 0 base:stone_slab")).await?;
    t.op("put me 0 base:water_bucket 1").await?;
    t.look_at(center);
    t.frames(3).await?;
    t.press("core:use");
    t.secs(0.3).await?;
    let parts = format!("parts 0 0 0 {id}");
    let wet = t.op(&parts).await?;
    t.check(wet.contains("stone_slab") && wet.contains("water[level=0]"), format!("deck slab holds water: {wet}"));
    t.check_eq(t.slot("me", 0).await?.0, "base:water_bucket".into(), "creative keeps its filled bucket");
    t.op("put me 0 base:bucket 1").await?;
    t.cmd(&format!("/tp {} {} {}", f64::from(center[0]) + 0.5, center[1] + 1, f64::from(center[2]) + 1.2))
        .await?;
    t.secs(0.3).await?;
    t.look_at(center);
    t.until("aimed at the slab from above", Duration::from_secs(2), |t| t.state().target_body.to_string() == id)
        .await?;
    t.press("core:use");
    t.secs(0.3).await?;
    let dry = t.op(&parts).await?;
    t.check(dry.contains("stone_slab") && !dry.contains("water"), format!("deck slab drained: {dry}"));
    t.check_eq(t.slot("me", 0).await?.0, "base:bucket".into(), "creative keeps its empty bucket");
    Ok(())
}

/// A player's movement and hazards use the blocks of an assembled structure.
pub async fn assembly_contact(t: T) -> R {
    t.flat().await?;
    let origin = add(t.feet(), [0, 6, -6]);
    let [x, y, z] = origin;
    t.fill(add(origin, [0, 0, -1]), add(origin, [0, 2, -1]), "base:cobblestone")
        .await?;
    t.fill(origin, add(origin, [0, 2, 0]), "base:ladder[north=true]")
        .await?;
    t.set(add(origin, [0, -1, 0]), "base:cobblestone").await?;
    let id = t.op(&format!("lift {x} {y} {z} 3")).await?;
    let id: u64 = id
        .trim()
        .parse()
        .map_err(|_| format!("assembly id: {id}"))?;
    let ladder = t.op(&format!("sblock {id} 0 1 0")).await?;
    t.check(ladder.contains("ladder") && ladder.contains("north=true"), format!("ladder carried by the assembly: {ladder}"));

    t.op("mode survival").await?;
    t.cmd(&format!("/tp {} {y} {}", f64::from(x) + 0.5, f64::from(z) + 0.31))
        .await?;
    t.c().set_look(0.0, 0.0);
    t.secs(0.3).await?;
    let from = t.state().pos.y;
    t.hold("core:forward", 0.85);
    t.secs(0.9).await?;
    let climbed = t.state().pos.y - from;
    t.check(climbed > 1.5, format!("walking against the assembly ladder climbs it ({climbed:.2} blocks)"));

    t.op(&format!("sblock {id} 2 -1 0 base:cobblestone"))
        .await?;
    t.op(&format!("sblock {id} 2 0 0 base:water")).await?;
    t.op(&format!("sblock {id} 2 1 0 base:water")).await?;
    t.cmd(&format!("/tp {} {y} {}", f64::from(x) + 2.5, f64::from(z) + 0.5))
        .await?;
    t.c().set_look(0.0, 0.0);
    t.secs(0.3).await?;
    t.hold("core:jump", 0.35);
    t.secs(0.3).await?;
    t.check(t.state().pos.y > f64::from(y) + 0.5, "jump swims up through assembly water");

    t.op(&format!("sblock {id} -2 -1 0 base:cobblestone"))
        .await?;
    t.op(&format!("sblock {id} -2 0 0 base:lava")).await?;
    t.cmd(&format!("/tp {} {y} {}", f64::from(x) - 1.5, f64::from(z) + 0.5))
        .await?;
    for _ in 0..15 {
        t.secs(0.1).await?;
        if t.op("health").await? != "20" {
            return Ok(());
        }
    }
    t.check(false, "assembly lava damages the player");
    Ok(())
}

/// Standing on a lift carries the player up and back down with it, without fall damage.
pub async fn lift(t: T) -> R {
    t.flat().await?;
    let f = t.feet();
    let id = spawn(&t, "testmod:lift", add(f, [3, 0, 0])).await?;
    // Its box comes from its blocks (2 x 2 planks from block (0, 0, 0) on), no size of its own.
    t.check_eq(t.op(&format!("size {id}")).await?, "3.000 1.000 0.850".to_string(), "the lift's box from its blocks");
    let p = pos_of(&t, &format!("{id}")).await?;
    t.op(&format!("onto {id} 1.0")).await?;
    t.secs(2.0).await?;
    let (me, lift) = (t.state().pos, pos_of(&t, &format!("{id}")).await?);
    t.check(lift[1] > p[1] + 1.2, "the lift went up");
    t.check((me.y - (lift[1] + 1.0)).abs() < 0.15, format!("the player rides on top (feet {:.2}, deck {:.2})", me.y, lift[1] + 1.0));
    // Up to the top, then all the way down.
    t.secs(5.0).await?;
    let (me, lift) = (t.state().pos, pos_of(&t, &format!("{id}")).await?);
    t.check(lift[1] < p[1] + 1.5, "the lift came down");
    t.check((me.y - (lift[1] + 1.0)).abs() < 0.15, format!("the player rode it down (feet {:.2}, deck {:.2})", me.y, lift[1] + 1.0));
    t.check_eq(t.op("health").await?, "20".to_string(), "health after the ride");
    Ok(())
}

/// Motion in the frame of what one stands on: the camera glides with a lift (no 0.05 block
/// jump a tick), and an idol riding the skiff stands still on its deck, not walking in place.
pub async fn ride_frames(t: T) -> R {
    t.flat().await?;
    let f = t.feet();
    let id = spawn(&t, "testmod:lift", add(f, [3, 0, 0])).await?;
    t.op(&format!("onto {id} 1.0")).await?;
    t.secs(0.5).await?;
    let mut steps = Vec::new();
    let mut last = t.state().shown.y;
    for _ in 0..60 {
        t.frames(1).await?;
        let y = t.state().shown.y;
        steps.push(y - last);
        last = y;
    }
    let still = steps.iter().filter(|d| d.abs() < 1e-5).count();
    let big = steps.iter().fold(0.0f64, |a, d| a.max(d.abs()));
    t.note(format!("{still} still frames of 60 while climbing, largest step {big:.4}"));
    t.check(still < 6, "the camera glides with the lift");
    t.check(big < 0.04, "no tick-sized jumps");

    t.cmd(&format!("/tp {} {} {}", f[0], f[1], f[2] + 10))
        .await?;
    let skiff = spawn(&t, "testmod:skiff", add(f, [0, 3, -6])).await?;
    let idol = spawn(&t, "testmod:idol", add(f, [0, 0, -6])).await?;
    t.op(&format!("onto {skiff} 1.0 {idol}")).await?;
    t.secs(1.0).await?;
    let (s, i) = (pos_of(&t, &format!("{skiff}")).await?, pos_of(&t, &format!("{idol}")).await?);
    t.check((i[0] - s[0]).abs() < 1.6 && (i[1] - (s[1] + 1.0)).abs() < 0.1, format!("the idol rides the skiff (idol {i:?}, skiff {s:?})"));
    let swing = t.c().entity(idol).map_or(1.0, |e| e.moved);
    t.check(swing < 0.15, format!("the riding idol does not walk ({swing:.2})"));

    let at = add(f, [4, 5, 2]);
    let support = add(at, [0, -1, 0]);
    t.set(at, "base:oak_planks").await?;
    t.set(support, "base:stone").await?;
    let result = t
        .op(&format!("assemble_one {} {} {}", at[0], at[1], at[2]))
        .await?;
    t.check_eq(result, abi::assembly_result::CONFIRMED.to_string(), "plain wand assembly");
    let platform = |t: &T| {
        t.c().entities().iter().copied().find(|&id| {
            t.c()
                .entity(id)
                .is_some_and(|e| name_of(abi::entry_kind::ENTITY, e.kind) == "core:platform")
        })
    };
    t.until("plain body", Duration::from_secs(4), |t| platform(t).is_some())
        .await?;
    let body = platform(&t).unwrap().to_string();
    t.check((pos_of(&t, &body).await?[1] - f64::from(at[1])).abs() < 0.1, "the support holds the body");
    t.set(support, "air").await?;
    t.secs(2.0).await?;
    let y = pos_of(&t, &body).await?[1];
    t.check(y < f64::from(at[1] - 2), format!("body falls when the support goes away: y={y}"));
    t.check_eq(t.op(&format!("sblock {body} 0 0 0")).await?, "base:oak_planks".into(), "the settled body keeps its cells and ID");
    Ok(())
}

/// A skiff and its rider glide on the client: drawn every frame at the speed the server moves
/// them (6 blocks a second), with no frames standing still and none jumping a tick ahead.
pub async fn skiff_smooth(t: T) -> R {
    t.flat().await?;
    let f = t.feet();
    let skiff = spawn(&t, "testmod:skiff", add(f, [-8, 3, -6])).await?;
    let idol = spawn(&t, "testmod:idol", add(f, [-8, 0, -6])).await?;
    t.op(&format!("onto {skiff} 1.0 {idol}")).await?;
    // Into a leg, away from its turn (it turns every 3 seconds of its age).
    t.secs(0.5).await?;
    let x = |t: &T, id| t.c().entity(id).map_or(0.0, |e| e.pos.x);
    // For 3.5 seconds (a turn is in it): it reverses once (overshooting and coming back would
    // reverse three times, jitter many times), never jumps, and its rider stays on its spot.
    // A jump is what a step has beyond its 6 blocks a second: a long frame of the machine's makes
    // a long step of a steady glide too, seen a frame later.
    let (mut turns, mut dir, mut jump, mut slip, mut longest, mut before) = (0, 0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64);
    let (mut last, mut rel) = (x(&t, skiff), x(&t, idol) - x(&t, skiff));
    let start = std::time::Instant::now();
    while start.elapsed().as_secs_f64() < 3.5 {
        t.frames(1).await?;
        let dt = t.state().dt as f64;
        longest = longest.max(dt);
        let (now, now_rel) = (x(&t, skiff), x(&t, idol) - x(&t, skiff));
        let d = now - last;
        if d.abs() > 1e-6 {
            if dir != 0.0 && d.signum() != dir {
                turns += 1;
            }
            dir = d.signum();
        }
        (jump, slip) = (jump.max(d.abs() - 6.0 * (dt + before)), slip.max((now_rel - rel).abs()));
        (last, rel, before) = (now, now_rel, dt);
    }
    t.note(format!(
        "{turns} reversals in 3.5 s, jumps up to {jump:.3}, longest frame {:.0} ms, rider moves on the deck up to {slip:.4} a frame",
        longest * 1000.0
    ));
    t.check_eq(turns, 1, "the skiff turns once, without overshooting or jitter");
    t.check(jump < 0.15, "the skiff glides (no jumps)");
    t.check(slip < 0.02, "its rider stays on its spot of the deck");
    // Riding it too: the deck keeps its place in front of the camera.
    t.op(&format!("onto {skiff} 1.0")).await?;
    t.secs(0.5).await?;
    // For 3.5 seconds, so a turn is in it.
    let mut steps = Vec::new();
    let mut last = x(&t, skiff) - t.state().shown.x;
    let start = std::time::Instant::now();
    while start.elapsed().as_secs_f64() < 3.5 {
        t.frames(1).await?;
        let now = x(&t, skiff) - t.state().shown.x;
        steps.push((now - last).abs());
        last = now;
    }
    steps.sort_by(|a, b| a.total_cmp(b));
    let big = steps.last().copied().unwrap_or(0.0);
    t.note(format!("skiff seen from its rider: steps up to {big:.4}"));
    t.check(big < 0.01, "the skiff stays put in front of a rider, turns included");
    Ok(())
}

/// A neutral mob: a spider by day minds its own business until hit, then fights back for a while;
/// in the dark it comes by itself. And a condition by distance, like a warden's.
pub async fn neutral_mobs(t: T) -> R {
    t.flat().await?;
    t.op("time 6000").await?;
    t.cmd("/weather clear").await?;
    let f = t.feet();
    let health = |t: T| async move { Ok::<f32, String>(t.op("health").await?.parse().unwrap_or(0.0)) };
    let spider = spawn(&t, "spider", add(f, [3, 0, 0])).await?;
    t.secs(3.0).await?;
    t.check_eq(health(t.clone()).await?, 20.0, "by day a spider leaves you alone");
    t.op(&format!("hurt {spider} 1")).await?;
    t.until("the spider fights back", std::time::Duration::from_secs(6), |t| t.c().entity(0).is_some_and(|e| e.health < 20.0))
        .await?;
    // Away for longer than it holds a grudge, and back.
    t.cmd(&format!("/tp {} {} {}", f[0], f[1], f[2] + 60))
        .await?;
    t.cmd("/tick sprint 450").await?;
    t.secs(1.0).await?;
    let at = pos_of(&t, &format!("{spider}")).await?;
    t.cmd(&format!("/tp {} {} {}", at[0] + 2.0, at[1], at[2]))
        .await?;
    t.op("heal").await?;
    t.secs(3.0).await?;
    t.check_eq(health(t.clone()).await?, 20.0, "it calms down");
    t.op(&format!("remove {spider}")).await?;

    // In the dark it needs no reason (this dimension's time stands still, so under a roof).
    t.fill(add(f, [-16, 3, -16]), add(f, [16, 3, 16]), "stone")
        .await?;
    let dark = spawn(&t, "spider", add(f, [3, 0, 0])).await?;
    t.cmd(&format!("/tp {} {} {}", f[0], f[1], f[2])).await?;
    t.until("a spider in the dark comes by itself", std::time::Duration::from_secs(8), |t| {
        t.c().entity(0).is_some_and(|e| e.health < 20.0)
    })
    .await?;
    t.op(&format!("remove {dark}")).await?;
    t.op("heal").await?;

    let sentry = spawn(&t, "testmod:sentry", add(f, [10, 0, 0])).await?;
    t.secs(2.5).await?;
    t.check_eq(health(t.clone()).await?, 20.0, "out of its distance it does nothing");
    let at = pos_of(&t, &format!("{sentry}")).await?;
    t.check((at[0] - (f[0] as f64 + 10.5)).abs() < 1.0, format!("nor does it come: {at:?}"));
    t.cmd(&format!("/tp {} {} {}", at[0] + 3.0, at[1], at[2]))
        .await?;
    t.until("within it, it attacks", std::time::Duration::from_secs(5), |t| t.c().entity(0).is_some_and(|e| e.health < 20.0))
        .await?;
    Ok(())
}

/// A held torch displaces both the chase and the attack, and putting it away restores pursuit.
pub async fn torch_shy_brain(t: T) -> R {
    t.flat().await?;
    let f = t.feet();
    t.op("put me 2 base:torch 1").await?;
    t.c().select_slot(0);
    let id = spawn(&t, "testmod:torch_shy", add(f, [6, 0, 0])).await?;
    let state = |rule: &str| format!("brain {id} {rule}");
    t.until("the mob attacks without a torch", std::time::Duration::from_secs(8), |t| t.c().entity(0).is_some_and(|e| e.health < 20.0))
        .await?;
    let before = pos_of(&t, &id.to_string()).await?;
    t.c().select_slot(2);
    t.secs(0.35).await?;
    let fleeing = t.op(&state("testmod:run_from_torch")).await?;
    t.check(fleeing != "idle", format!("the torch triggers flight: {fleeing}"));
    t.check_eq(t.op(&state("core:chase")).await?, "idle".to_owned(), "flight owns movement");
    t.check_eq(t.op(&state("core:melee")).await?, "idle".to_owned(), "flight owns attacks");
    t.secs(0.6).await?;
    let after = pos_of(&t, &id.to_string()).await?;
    t.check(after[0] > before[0] + 0.6, format!("it moved away: {before:?} -> {after:?}"));
    t.c().select_slot(0);
    t.secs(0.35).await?;
    t.check_eq(t.op(&state("testmod:run_from_torch")).await?, "idle".to_owned(), "putting the torch away ends flight");
    let chasing = t.op(&state("core:chase")).await?;
    t.check(chasing != "idle", format!("pursuit resumes: {chasing}"));
    t.check_eq(t.op(&format!("brain_edit {id} core:chase disable")).await?, "true".to_owned(), "disable the default movement rule");
    t.check_eq(t.op(&format!("brain_edit {id} core:melee disable")).await?, "true".to_owned(), "disable the default attack rule");
    t.check_eq(
        t.op(&format!("brain_edit {id} core:watch_player base:skeleton"))
            .await?,
        "true".to_owned(),
        "install a rule from another kind",
    );
    t.secs(0.35).await?;
    t.check(t.op(&state("core:watch_player")).await? != "idle", "installed rule runs before reload");
    super::audit::reopen(&t, 1).await?;
    t.op("mode survival").await?;
    t.secs(1.0).await?;
    t.check_eq(t.op(&state("core:chase")).await?, "idle".to_owned(), "disabled rule stays disabled after reload");
    let watching = t.op(&state("core:watch_player")).await?;
    t.check(watching != "idle", format!("installed rule survives reload: {watching}"));
    Ok(())
}

pub async fn mob_senses(t: T) -> R {
    t.flat().await?;
    let f = t.feet();
    let at = |offset| add(f, offset);
    t.fill(at([6, 0, -4]), at([16, 1, 4]), "glass").await?;
    t.fill(at([7, 0, -3]), at([15, 1, 3]), "water").await?;
    let [x, y, z] = at([5, 0, 0]);
    let sentry = t
        .op(&format!("spawn testmod:touch_sentry {x} {y} {z} 0"))
        .await?;
    let sentry = sentry
        .trim()
        .parse::<u64>()
        .map_err(|_| format!("spawn sentry: {sentry}"))?;
    // Enough bodies outside the sensing area to exercise the shared candidate path.
    for i in 0..56 {
        spawn(&t, "base:chicken", at([-10 - i % 8, 0, 7 + i / 8])).await?;
    }
    let mut group = Vec::new();
    for _ in 0..10 {
        group.push(spawn(&t, "testmod:shoal_fish", at([9, 0, 0])).await?);
    }
    let fish = spawn(&t, "testmod:shoal_fish", at([14, 0, 0])).await?;
    let brain = |id, rule| format!("brain {id} testmod:{rule}");
    let start = pos_of(&t, &fish.to_string()).await?;
    t.secs(1.7).await?;
    let joined = pos_of(&t, &fish.to_string()).await?;
    t.check(joined[0] < start[0] - 0.6, format!("the outlier swims toward the group: {start:?} -> {joined:?}"));
    let first = pos_of(&t, &group[0].to_string()).await?;
    let last = pos_of(&t, &group[9].to_string()).await?;
    t.check((first[0] - last[0]).hypot(first[2] - last[2]) > 0.2, "fish spawned together spread out");
    t.check_eq(t.op(&brain(fish, "flee_aggro")).await?, "idle".to_owned(), "calm sentry");
    t.cmd("/tick freeze").await?;
    t.op(&format!("brain_edit {fish} testmod:flee_aggro disable"))
        .await?;
    t.op(&format!("brain_edit {fish} testmod:flee_aggro enable"))
        .await?;
    let advance = |t: T| async move {
        let tick = t.game_time().await?;
        t.cmd("/tick step 1").await?;
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while t.game_time().await? == tick {
            if std::time::Instant::now() >= deadline {
                return Err("the frozen world did not step".to_owned());
            }
        }
        Ok(())
    };
    advance(t.clone()).await?;
    t.cmd(&format!("/senseaggro {sentry} 50")).await?;
    advance(t.clone()).await?;
    let flight = t.op(&brain(fish, "flee_aggro")).await?;
    t.check(flight != "idle", format!("the sentry's aggression causes flight through glass: {flight}"));
    t.check_eq(t.op(&brain(fish, "school")).await?, "idle".to_owned(), "flight overrides the shoal");
    t.cmd(&format!("/senseaggro {sentry} 40")).await?;
    advance(t.clone()).await?;
    t.check(t.op(&brain(fish, "flee_aggro")).await? != "idle", "flight holds between the two thresholds");
    t.cmd(&format!("/senseaggro {sentry} 30")).await?;
    advance(t.clone()).await?;
    t.check_eq(t.op(&brain(fish, "flee_aggro")).await?, "idle".to_owned(), "lower aggression ends flight");
    t.cmd("/tick unfreeze").await?;

    let probe = |offset| format!("sense_probe {sentry} {offset}");
    t.check_eq(t.op(&probe(-3.0)).await?, "true true".to_owned(), "clear view and body path ahead");
    t.check_eq(t.op(&probe(-0.2)).await?, "false true".to_owned(), "near blind spot");
    t.check_eq(t.op(&probe(2.0)).await?, "false true".to_owned(), "blind behind the observer");
    t.fill(at([5, 0, -2]), at([5, 1, -2]), "stone").await?;
    t.check_eq(t.op(&probe(-3.0)).await?, "false false".to_owned(), "stone stops sight and the body");
    t.fill(at([5, 0, -2]), at([5, 1, -2]), "air").await?;

    let [x, y, z] = pos_of(&t, &sentry.to_string()).await?;
    t.cmd(&format!("/tp {x} {y} {}", z + 1.0)).await?;
    t.secs(0.3).await?;
    t.check_eq(t.c().entity(0).map(|e| e.health), Some(20.0), "nearby behind the sentry is safe");
    t.check_eq(t.op(&brain(sentry, "probe_contact")).await?, "idle".to_owned(), "no probe overlap behind");
    t.cmd(&format!("/tp {x} {y} {}", z - 1.0)).await?;
    t.until("the probe outside the body box detects contact", Duration::from_secs(3), |t| t.c().entity(0).is_some_and(|e| e.health < 20.0))
        .await?;
    t.cmd(&format!("/tp {x} {y} {}", z - 5.0)).await?;
    t.secs(0.2).await?;
    let aim = |height: f64| {
        let eye = t.c().entity(0).expect("player in the scene").eye();
        let delta = Vec3::new(x, y + height, z) - eye;
        t.c()
            .set_look((-delta.x).atan2(-delta.z).to_degrees() as f32, delta.y.atan2(delta.x.hypot(delta.z)).to_degrees() as f32);
    };
    let head = brain(sentry, "head_gaze");
    let held = brain(sentry, "held_head_gaze");
    aim(0.35);
    t.secs(1.2).await?;
    t.check_eq(t.op(&head).await?, "idle".to_owned(), "the probe is not its head");
    aim(1.28);
    t.secs(0.3).await?;
    t.check(t.op(&head).await? != "idle", "eyes on the head wake the immediate rule");
    t.check_eq(t.op(&held).await?, "idle".to_owned(), "a glance does not reach the hold threshold");
    aim(0.35);
    t.secs(0.25).await?;
    t.check_eq(t.op(&head).await?, "idle".to_owned(), "looking off the head ends the gaze");
    t.cmd("/tick rate 2").await?;
    aim(1.28);
    t.secs(0.35).await?;
    t.check_eq(t.op(&held).await?, "idle".to_owned(), "the second look starts its own duration");
    let mut reached = false;
    for _ in 0..10 {
        t.secs(0.2).await?;
        if t.op(&held).await? != "idle" {
            reached = true;
            break;
        }
    }
    t.check(reached, "a continuous look crosses the real-time threshold even at 2 TPS");
    t.cmd("/tick rate 20").await?;
    t.fill(at([5, 0, -3]), at([5, 1, -3]), "stone").await?;
    t.secs(0.25).await?;
    t.check_eq(t.op(&held).await?, "idle".to_owned(), "an occluder interrupts an already held gaze");
    t.fill(at([5, 0, -3]), at([5, 1, -3]), "air").await?;
    t.op("mode spectator").await?;
    t.cmd(&format!("/tp {} {} {}", x - 2.7, y + 3.2, z - 3.0))
        .await?;
    t.secs(0.45).await?;
    t.look_at(at([8, 0, 0]));
    t.secs(0.2).await?;
    t.shot("senses").await?;
    Ok(())
}

pub async fn brain_showcase(t: T) -> R {
    t.flat().await?;
    let center = t.c().entity(0).expect("player in the scene").pos;
    t.cmd("/brainshowcase hunt").await?;
    let near = t.op("near ~ ~ ~ 12").await?;
    let find = |near: &str, kind: &str| {
        near.split_whitespace()
            .find_map(|s| {
                let (id, name) = s.split_once(':')?;
                (name == kind).then(|| id.parse::<u64>().ok()).flatten()
            })
            .ok_or_else(|| format!("{kind} missing from the hunt: {near}"))
    };
    let amber = find(&near, "testmod:amber_hunter")?;
    let cyan = find(&near, "testmod:cyan_hunter")?;
    let initial = pos_of(&t, &amber.to_string()).await?;
    let mut last = initial;
    for _ in 0..8 {
        t.secs(0.3).await?;
        let next = pos_of(&t, &amber.to_string()).await?;
        t.check((next[0] - last[0]).hypot(next[2] - last[2]) > 0.12, "the hunter keeps moving between waypoints");
        last = next;
    }
    let [ax, _, az] = pos_of(&t, &amber.to_string()).await?;
    let [bx, _, bz] = pos_of(&t, &cyan.to_string()).await?;
    let (a, b) = ((ax - center.x, az - center.z), (bx - center.x, bz - center.z));
    let (ra, rb) = (a.0.hypot(a.1), b.0.hypot(b.1));
    t.check((ax - initial[0]).hypot(az - initial[2]) > 1.8, "hunters move around the player");
    t.check((4.5..7.5).contains(&ra) && (4.5..7.5).contains(&rb), format!("both keep their distance: {ra}, {rb}"));
    t.check((a.0 * b.0 + a.1 * b.1) / (ra * rb) < -0.75, "hunters stay on opposite sides");
    for id in [amber, cyan] {
        t.op(&format!("remove {id}")).await?;
    }

    t.cmd("/brainshowcase shoal").await?;
    let f = t.feet();
    let near = t.op("near ~ ~ ~ 17").await?;
    let fish: Vec<u64> = near
        .split_whitespace()
        .filter_map(|line| line.strip_suffix(":testmod:shoal_fish")?.parse().ok())
        .collect();
    t.check_eq(fish.len(), 6, "the demo creates a group on both sides of the wall");
    let far = *fish.last().ok_or("no fish in the demonstration")?;
    t.until("the isolated fish swims up and over the wall toward the shoal", Duration::from_secs(8), |t| {
        t.c()
            .entity(far)
            .is_some_and(|e| e.pos.x < f64::from(f[0]) + 8.0)
    })
    .await?;
    let mut alarmed = false;
    for _ in 0..15 {
        t.secs(0.35).await?;
        if t.op(&format!("brain {far} testmod:flee_aggro")).await? != "idle" {
            alarmed = true;
            break;
        }
    }
    t.check(alarmed, "the alarm changes nearby fish behavior through glass");
    t.op("mode spectator").await?;
    t.cmd(&format!("/tp {} {} {}", f[0], f[1] + 3, f[2] - 4))
        .await?;
    t.secs(0.35).await?;
    t.look_at(add(f, [7, 2, 0]));
    t.secs(0.2).await?;
    t.shot("shoal").await?;

    t.cmd(&format!("/tp {} {} {}", f[0], f[1], f[2] - 14))
        .await?;
    t.op("mode survival").await?;
    t.secs(0.35).await?;
    let f = t.feet();
    t.cmd("/brainshowcase routes").await?;
    let near = t.op("near ~ ~ ~ 12").await?;
    let amber = find(&near, "testmod:amber_hunter")?;
    let cyan = find(&near, "testmod:cyan_hunter")?;
    let (mut avoided, mut entered) = (false, false);
    t.until("the route showcase visibly separates avoidance and attraction", Duration::from_secs(8), |t| {
        let in_strip_x = |x: f64| (f64::from(f[0] + 6)..f64::from(f[0] + 10)).contains(&x);
        avoided |= t
            .c()
            .entity(amber)
            .is_some_and(|e| in_strip_x(e.pos.x) && !(f64::from(f[2] - 4)..f64::from(f[2] - 1)).contains(&e.pos.z));
        entered |= t
            .c()
            .entity(cyan)
            .is_some_and(|e| in_strip_x(e.pos.x) && (f64::from(f[2] + 2)..f64::from(f[2] + 5)).contains(&e.pos.z));
        avoided && entered
    })
    .await?;

    t.cmd(&format!("/tp {} {} {}", f[0], f[1], f[2] + 28))
        .await?;
    t.secs(0.35).await?;
    let f = t.feet();
    t.cmd("/brainshowcase gap").await?;
    let near = t.op("near ~ ~ ~ 14").await?;
    let small = find(&near, "testmod:shoal_fish")?;
    let wide = find(&near, "testmod:wide_shoal_fish")?;
    t.until("the small demo fish clears the gap", Duration::from_secs(8), |t| {
        t.c()
            .entity(small)
            .is_some_and(|e| e.pos.x < f64::from(f[0] + 7))
    })
    .await?;
    let [x, _, _] = pos_of(&t, &wide.to_string()).await?;
    t.check(x >= f64::from(f[0] + 9), "the large demo fish cannot fit");
    t.op("mode spectator").await?;
    t.cmd(&format!("/tp {} {} {}", f[0] + 8, f[1] + 1, f[2] - 8))
        .await?;
    t.secs(0.25).await?;
    t.look_at(add(f, [8, 1, 0]));
    t.secs(0.2).await?;
    t.shot("gap").await?;
    Ok(())
}

pub async fn fish_navigation(t: T) -> R {
    t.flat().await?;
    let f = t.feet();
    let at = |offset| add(f, offset);
    t.fill(at([3, 0, -3]), at([14, 4, 3]), "glass").await?;
    t.fill(at([4, 0, -2]), at([13, 4, 2]), "water").await?;
    t.fill(at([8, 0, -2]), at([8, 1, 2]), "stone").await?;
    let fish = spawn(&t, "testmod:shoal_fish", at([5, 0, 0])).await?;
    let goal = |pos: [i32; 3]| format!("route {fish} {} {} {} swim", pos[0], pos[1], pos[2]);
    t.op(&goal(at([5, 3, 0]))).await?;
    t.until("a straight swim changes depth", Duration::from_secs(5), |t| {
        t.c()
            .entity(fish)
            .is_some_and(|e| e.pos.y > f64::from(f[1]) + 3.0)
    })
    .await?;
    t.op(&goal(at([5, 0, 0]))).await?;
    t.until("the fish swims back down", Duration::from_secs(5), |t| {
        t.c()
            .entity(fish)
            .is_some_and(|e| e.pos.y < f64::from(f[1]) + 0.5)
    })
    .await?;
    t.op(&goal(at([11, 0, 0]))).await?;
    t.until("the fish goes over the submerged wall", Duration::from_secs(8), |t| {
        t.c()
            .entity(fish)
            .is_some_and(|e| e.pos.x > f64::from(f[0]) + 9.3)
    })
    .await?;
    t.until("the fish arrives on the far side at the original depth", Duration::from_secs(6), |t| {
        t.c()
            .entity(fish)
            .is_some_and(|e| e.pos.x > f64::from(f[0]) + 10.9 && e.pos.y < f64::from(f[1]) + 0.65)
    })
    .await?;

    t.op(&format!("brain_edit {fish} testmod:school disable"))
        .await?;
    t.fill(at([8, 0, -2]), at([8, 4, 2]), "stone").await?;
    t.fill(at([8, 0, 0]), at([8, 4, 0]), "water").await?;
    let wide = spawn(&t, "testmod:shoal_fish", at([11, 0, 0])).await?;
    t.op(&format!("brain_edit {wide} testmod:school disable"))
        .await?;
    t.op(&format!("size {wide} 1.4 0.3")).await?;
    let [x, y, z] = at([5, 0, 0]);
    t.op(&format!("route {wide} {x} {y} {z} swim")).await?;
    t.op(&goal(at([5, 0, 0]))).await?;
    t.until("a small fish fits through a single-block opening", Duration::from_secs(8), |t| {
        t.c()
            .entity(fish)
            .is_some_and(|e| e.pos.x < f64::from(f[0]) + 6.9)
    })
    .await?;
    let [x, _, _] = pos_of(&t, &wide.to_string()).await?;
    t.check(x >= f64::from(f[0]) + 9.0, "a wide fish stays outside the undersized opening");

    t.fill(at([-8, 0, -1]), at([-5, 0, 1]), "short_grass")
        .await?;
    let cautious = spawn(&t, "testmod:touch_sentry", at([-10, 0, 0])).await?;
    let attracted = spawn(&t, "testmod:touch_sentry", at([-10, 0, 0])).await?;
    let [x, y, z] = at([-2, 0, 0]);
    t.op(&format!("route {cautious} {x} {y} {z} walk 10"))
        .await?;
    t.op(&format!("route {attracted} {x} {y} {z} walk 0.25"))
        .await?;
    let in_strip_x = |x: f64| (f64::from(f[0] - 8)..f64::from(f[0] - 4)).contains(&x);
    let entered = std::cell::Cell::new(false);
    let avoided = std::cell::Cell::new(false);
    t.until("low cost crosses the strip and high cost goes around it", Duration::from_secs(5), |t| {
        let lateral = |e: EntityState| (e.pos.z - f64::from(f[2]) - 0.5).abs();
        entered.set(
            entered.get()
                || t.c()
                    .entity(attracted)
                    .is_some_and(|e| in_strip_x(e.pos.x) && lateral(e) < 0.8),
        );
        avoided.set(
            avoided.get()
                || t.c()
                    .entity(cautious)
                    .is_some_and(|e| in_strip_x(e.pos.x) && lateral(e) > 1.6),
        );
        entered.get() && avoided.get()
    })
    .await?;
    Ok(())
}

/// A zombie finds its way around a wall to the player and hits it.
pub async fn zombie_chases(t: T) -> R {
    t.flat().await?;
    let f = t.feet();
    t.fill(add(f, [4, 0, -6]), add(f, [4, 2, 6]), "cobblestone")
        .await?;
    t.fill(add(f, [4, 0, 5]), add(f, [4, 1, 5]), "air").await?;
    let id = spawn(&t, "zombie", add(f, [8, 0, 0])).await?;
    t.secs(10.0).await?;
    let z = pos_of(&t, &format!("{id}")).await?;
    let health: f32 = t.op("health").await?.parse().unwrap_or(20.0);
    t.check(z[0] < f[0] as f64 + 4.0, format!("the zombie came round the wall (x {:.1})", z[0]));
    t.check(health < 20.0, format!("and hit the player (health {health})"));
    // Mobs hit through the same question as players: a mod that forbids attacks stops them.
    t.op("heal").await?;
    t.cmd("/rule testkit:peace 1").await?;
    t.secs(2.5).await?;
    let calm: f32 = t.op("health").await?.parse().unwrap_or(0.0);
    t.cmd("/rule testkit:peace 0").await?;
    t.check(calm >= 20.0, format!("a zombie hit while attacks were forbidden (health {calm})"));
    Ok(())
}

/// The skiff is a structure of blocks: a player riding it places a block on its deck and
/// breaks it again while it flies; the block flies with it, collides, and mods read and write
/// its blocks.
pub async fn skiff_blocks(t: T) -> R {
    t.flat().await?;
    t.op("mode creative").await?;
    t.op("put me 0 stone 64").await?;
    t.c().select_slot(0);
    let f = t.feet();
    let id = spawn(&t, "testmod:skiff", add(f, [0, 3, 0])).await?;
    let sb = |p: [i32; 3]| format!("sblock {id} {} {} {}", p[0], p[1], p[2]);
    t.check_eq(t.op(&sb([1, 0, 1])).await?, "base:oak_planks".to_string(), "it is built of planks");
    t.op(&format!("onto {id} 1.0")).await?;
    t.secs(0.4).await?;
    // Down at the deck ahead: the block goes on top of it.
    t.c().set_look(0.0, -60.0);
    t.secs(0.2).await?;
    t.press("core:use");
    t.secs(0.5).await?;
    t.check_eq(t.op(&sb([0, 1, -1])).await?, "base:stone".to_string(), "a stone placed on the flying deck");
    // The server's pick (what mods' `look_at` gets) sees the structure's blocks too.
    let look = t.op("look").await?;
    t.check(look.starts_with(&format!("{id} ")), format!("look_at hits the flying structure: {look}"));
    // Looking at a spot of the deck 0.02 from the edge between two planks (along the way it
    // flies) while riding, the same block stays targeted.
    let st = t.state();
    let o = t.c().entity(id).map_or([0.0; 3], |e| e.pos.to_array());
    let spot = [o[0] - 0.5 + 0.98, o[1] + 1.0, o[2] - 0.5 + 0.5 - 1.0];
    let eye = [st.shown.x, st.shown.y + t.c().entity(0).map_or(1.62, |e| e.size.eye as f64), st.shown.z];
    let (dx, dy, dz) = (spot[0] - eye[0], spot[1] - eye[1], spot[2] - eye[2]);
    t.c()
        .set_look((-dx).atan2(-dz).to_degrees() as f32, dy.atan2((dx * dx + dz * dz).sqrt()).to_degrees() as f32);
    t.frames(2).await?;
    let mut targets = std::collections::BTreeSet::new();
    for _ in 0..60 {
        t.frames(1).await?;
        let s = t.state();
        targets.insert((s.target, s.target_body));
    }
    t.check(targets.len() == 1, format!("one block looked at while riding ({targets:?})"));
    let (a, sa) = (pos_of(&t, &format!("{id}")).await?, t.op(&sb([0, 1, -1])).await?);
    t.until("the skiff flies on", std::time::Duration::from_secs(5), |t| {
        t.c()
            .entity(id)
            .is_some_and(|e| (e.pos.x - a[0]).abs() > 2.0)
    })
    .await?;
    t.check_eq(t.op(&sb([0, 1, -1])).await?, sa, "the stone still rides on the flying deck");
    // A mod's write, then the player breaks the stone.
    t.check_eq(t.op(&format!("{} base:cobblestone", sb([1, 1, 1]))).await?, "1".to_string(), "a mod sets a block of it");
    t.check_eq(t.op(&sb([1, 1, 1])).await?, "base:cobblestone".to_string(), "and reads it back");
    t.c().set_look(0.0, -60.0);
    t.secs(0.2).await?;
    t.hold("core:attack", 0.1);
    t.secs(0.5).await?;
    t.check_eq(t.op(&sb([0, 1, -1])).await?, "core:air".to_string(), "the stone broken off the flying deck");
    Ok(())
}
