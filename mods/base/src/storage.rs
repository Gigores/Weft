//! Tools in the corner of every menu: sorting what it is opened on (or the player's own inventory), and
//! putting what the player carries into the storage around it that already holds the same
//! (blocks tagged `storage`, any mod's: chests, not a furnace's slots).

use weft_sdk::*;

static DEPOSIT: Channel<()> = Channel::named("deposit_nearby");
/// Storage that takes what is put away near it: what is tagged so.
static STORAGE: Tag = Tag::named("storage");
/// Blocks around the player storage is looked for in.
const REACH: i32 = 5;

pub fn register(r: &mut Registry) {
    r.tag("storage", &["chest"]);
    // Small, in the corner of every menu's panel; what they do in their tooltips.
    r.screen_part(&screens::MENUS, "corner", 0, |ui| {
        ui.icon_button("base:gui/icon_sort", 8.0, 8.0, S::default().tooltip("storage.base.sort"), |c| {
            c.client().menu_sort(None);
        });
        ui.icon_button("base:gui/icon_put_away", 8.0, 8.0, S::default().tooltip("storage.base.deposit"), |c| {
            c.client().send(&DEPOSIT, &())
        });
    });
    r.packet(&DEPOSIT, |p, ()| deposit(&p.world(), p.player()));
}

/// What `player` carries in its main inventory and hotbar into the storage around it
/// already holding some of the same, as much as fits.
fn deposit(w: &World, player: u64) {
    let Some(e) = w.entity(player) else { return };
    let at = e.pos.block();
    let mut storage = Vec::new();
    for x in -REACH..=REACH {
        for y in -REACH..=REACH {
            for z in -REACH..=REACH {
                let p = at.offset(x, y, z);
                if STORAGE.on_block(w.block(p)) && w.inventory(p).len() > 0 {
                    storage.push(w.inventory(p));
                }
            }
        }
    }
    if storage.is_empty() {
        w.hint("storage.base.deposit_no_storage", &[]).to(player);
        return;
    }
    let player_inventory = w.inventory(player);
    let mut moved = false;
    for group in ["main", "hotbar"]
        .into_iter()
        .filter_map(|name| player_inventory.find_group(name))
    {
        for i in 0..group.len() {
            let stack = group.get(i);
            if stack.is_empty() {
                continue;
            }
            let Some(from) = group.inventory_slot(i) else { continue };
            let mut left = stack.count;
            for inv in &storage {
                if !(0..inv.len()).any(|k| inv.get(k).stacks_with(&stack)) {
                    continue;
                }
                let count = inv.move_in_from(from);
                if count != 0 {
                    left -= count;
                    moved = true;
                    if left == 0 {
                        break;
                    }
                }
            }
        }
    }
    if !moved {
        w.hint("storage.base.deposit_no_match", &[]).to(player);
    }
}
