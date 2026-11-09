//! Tools beside every menu: sorting what it is opened on (or the player's own inventory), and
//! putting what the player carries into the storage around it that already holds the same
//! (blocks tagged `storage`, any mod's: chests, not a furnace's slots). A trash slot sits in the
//! player's own inventory.

use weft_sdk::*;

static DEPOSIT: Channel<()> = Channel::named("deposit_nearby");
/// Storage that takes what is put away near it: what is tagged so.
static STORAGE: Tag = Tag::named("storage");
/// Blocks around the player storage is looked for in.
const REACH: i32 = 5;

pub fn register(r: &mut Registry) {
    r.tag("storage", &["chest"]);
    r.screen_part(&screens::MENUS, "side", 0, |ui| {
        ui.button("storage.base.sort", S::column().min_w(80.0), |c| {
            c.client().menu_sort(None);
        });
        ui.button("storage.base.deposit", S::column().min_w(80.0), |c| c.client().send(&DEPOSIT, &()));
    });
    r.packet(&DEPOSIT, |p, ()| deposit(&p.world(), p.player()));
}

/// What `player` carries in its main inventory, the hotbar aside, into the storage around it
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
        return;
    }
    let main = w.inventory(player).group("main");
    for i in 0..main.len() {
        let mut s = main.get(i);
        if s.is_empty() {
            continue;
        }
        for inv in &storage {
            let has = (0..inv.len()).any(|k| {
                let t = inv.get(k);
                t.item == s.item && t.data == s.data
            });
            if has {
                s = inv.insert(s);
                if s.is_empty() {
                    break;
                }
            }
        }
        main.set(i, s);
    }
}

/// The trash slot of the player's inventory takes what is put in for good.
pub fn empty_trash(m: &MenuCtx) {
    if let Some(t) = m.find_group("trash") {
        if !t.get(0).is_empty() {
            t.set(0, ItemStack::EMPTY);
        }
    }
}
