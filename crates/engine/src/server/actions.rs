//! What players ask for, one list of what each message takes (`allowed`), then the actions.

use super::*;

use crate::world::placement::Part;

fn landed_hit(turn: abi::Turn, face: u8, hit: [f32; 3]) -> (u8, [f32; 3]) {
    if turn == abi::Turn::NONE || face >= 6 { (face, hit) } else { (turn.face(face as usize) as u8, turn.point(hit, 1.0)) }
}
use weft_abi::entry_kind as ek;

impl Server {
    pub(super) fn handle_messages(&mut self, c: &'static Content) {
        for pi in 0..self.players.len() {
            while let Some(msg) = self.players[pi].conn.recv() {
                self.handle(c, pi, msg);
                self.settle(c);
            }
        }
    }

    /// The hands to try, in order, for the player's right click.
    fn hands(&mut self, c: &'static Content, pi: usize) -> Vec<u32> {
        self.region(pi).map_or(Vec::new(), |(r, id)| {
            let Some(e) = r.entities.get(id) else { return Vec::new() };
            r.entities
                .inventories
                .get(&id)
                .map_or(Vec::new(), |i| crate::content::hands_in_order(c, e.kind, i, e.selected))
        })
    }

    /// A click on a block with the item in `slot`: places it if it is a block, uses it if not.
    #[allow(clippy::too_many_arguments)]
    fn place_or_use(
        &mut self,
        c: &'static Content,
        pi: usize,
        slot: u32,
        frame: u64,
        pos: IVec3,
        surface: Part,
        face: u8,
        look: [f32; 3],
        hit: [f32; 3],
        by_face: bool,
    ) -> bool {
        let Some((r, entity)) = self.region(pi) else { return false };
        let Some(e) = r.entities.get(entity) else { return false };
        let off_hand = c.game.entities[e.kind as usize].off == Some(slot);
        let creative = e.mode.has(crate::tag_bits::game_mode::NO_COSTS);
        let no_place = e.mode.has(crate::tag_bits::game_mode::NO_PLACE);
        let held = r.entities.inventory_slot(entity, slot);
        let Some(state) = c
            .items
            .get(held.item as usize)
            .and_then(|i| i.block)
            .filter(|_| !held.is_empty() && !no_place)
        else {
            return self.use_in_hand(c, pi, slot, frame, Some((pos, face)));
        };
        let placed = r.with_space(c, frame, |d, space_id, o| {
            let look = o
                .vector_to_local(glam::Vec3::from_array(look).as_dvec3())
                .as_vec3()
                .to_array();
            let (target, part) = d.place_clicked(space_id, c, pos, surface, face, state, look, hit, entity, held.item, by_face)?;
            // What the stack carries goes onto the block entity it becomes (a renamed chest).
            if let Some(id) = d.as_placed(space_id, target, part, |d| d.anchored_part(space_id, c, target, part.state)) {
                for &comp in &c.game.carried {
                    if let Some(v) = crate::stack::value(held.data, comp) {
                        d.entities.comps.insert((id, comp), v.into());
                    }
                }
            }
            Some(())
        });
        // Where it can't be placed, it is used like any item (a seed's own use, a bucket of it).
        if !matches!(placed, Some(Some(()))) {
            return self.use_in_hand(c, pi, slot, frame, Some((pos, face)));
        }
        if !creative {
            r.entities
                .set_slot(entity, slot, ItemStack { count: held.count - 1, ..held });
        }
        r.events
            .push(DimEvent::EntitySwing { id: entity, off: off_hand });
        self.players[pi]
            .conn
            .send(ToClient::Swing { used_item: true, off_hand });
        true
    }

    /// Player `pi` uses the item in `slot` on what was clicked: true if it did something, or
    /// started to.
    fn use_in_hand(&mut self, c: &'static Content, pi: usize, slot: u32, frame: u64, target: Option<(IVec3, u8)>) -> bool {
        let Some((region, entity)) = self.region(pi) else { return false };
        region
            .with_space(c, frame, |region, space_id, _| region.use_in_hand(space_id, c, entity, slot, target, false))
            .unwrap_or(false)
    }

    /// The region holding player `pi`'s entity, with the entity id.
    pub(super) fn region(&mut self, pi: usize) -> Option<(&mut Dim, u64)> {
        let p = &self.players[pi];
        let id = p.entity;
        self.dims[p.dim as usize].entity_region(id).map(|r| (r, id))
    }

    /// Whether player `pi` may send this at all. One list before the actions, so a new action
    /// can't forget that its sender may be dead, a spectator or not in the game yet.
    pub(super) fn allowed(&self, pi: usize, msg: &ToServer) -> bool {
        use ToServer as M;
        let p = &self.players[pi];
        if p.conn.is_closed() {
            return false;
        }
        if matches!(msg, M::Leave) {
            return true;
        }
        if matches!(msg, M::Join { .. }) {
            return !p.streams_world();
        }
        if p.arrival.is_some() {
            return matches!(msg, M::Ready | M::View(_) | M::Skin(_) | M::Avatar(_));
        }
        if matches!(msg, M::Ready) {
            return false;
        }
        let Some(e) = self.dims[p.dim as usize]
            .entity(p.entity)
            .filter(|_| p.joined)
        else {
            return false;
        };
        match msg {
            // Sent by a modified client only; they would index out of bounds.
            M::Use { face, .. } if *face >= 6 => false,
            M::Respawn => !e.alive(),
            M::Break { .. } | M::Use { .. } | M::UseItem | M::Attack(_) | M::Swing | M::Interact(_) | M::Drop { .. } => {
                e.alive() && !e.mode.has(crate::tag_bits::game_mode::NO_INTERACT)
            }
            M::ControllerSave { .. } | M::HelmHeld { .. } => e.alive() && !e.mode.has(crate::tag_bits::game_mode::NO_INTERACT),
            M::GrabBody | M::TurnGrab(_) => e.alive() && !e.mode.has(crate::tag_bits::game_mode::NO_INTERACT),
            M::Move { .. } | M::OpenInventory | M::Click { .. } | M::Drag { .. } | M::CreativeTake { .. } | M::Pick { .. } => e.alive(),
            _ => true,
        }
    }

    pub(super) fn handle(&mut self, c: &'static Content, pi: usize, msg: ToServer) {
        if !self.allowed(pi, &msg) {
            log::debug!(target: "net", "refused {} from {}", crate::net::Message::kind(&msg), self.players[pi].identity.name);
            return;
        }
        match msg {
            ToServer::Join { name, key } => self.prepare_join(c, pi, name, key),
            ToServer::Ready => {
                if let Some(arrival) = &mut self.players[pi].arrival {
                    arrival.ready = true;
                }
            }
            ToServer::StartBreak { pos, part, frame } => {
                let Some((r, entity)) = self.region(pi) else { return };
                let request = ToClient::BreakRefused { frame, pos };
                let Some((frame, pos, part, _)) = r.landed_as(c, frame, pos, part) else {
                    self.players[pi].conn.send(request);
                    return;
                };
                if !r.start_mining(c, entity, frame, pos, part) {
                    self.players[pi].conn.send(request);
                }
            }
            ToServer::StopBreak => {
                if let Some((r, id)) = self.region(pi) {
                    r.stop_mining(id);
                }
            }
            ToServer::ControllerSave { controller, data } => {
                if self.players[pi].controller_edit.take() == Some(controller)
                    && let Some((d, player)) = self.region(pi)
                {
                    d.save_controller(c, player, controller, &data);
                }
            }
            ToServer::HelmHeld { mount, controller, held } => {
                if let Some((d, player)) = self.region(pi) {
                    d.helm_held(player, mount, controller, held);
                }
            }
            ToServer::GrabBody => {
                let body = self
                    .region(pi)
                    .and_then(|(d, player)| d.toggle_grab(c, player));
                self.players[pi].conn.send(ToClient::BodyGrab(body));
            }
            ToServer::ReleaseBody => {
                if let Some((d, player)) = self.region(pi) {
                    d.release_grab(player);
                }
                self.players[pi].conn.send(ToClient::BodyGrab(None));
            }
            ToServer::TurnGrab(turn) => {
                if let Some((d, player)) = self.region(pi) {
                    d.turn_grab(c, player, turn);
                }
            }
            ToServer::Keys { down, up } => {
                let Some((r, entity)) = self.region(pi) else { return };
                let now = r.time;
                let mut told = Vec::new();
                if let Some(e) = r.entities.get_mut(entity) {
                    // Downs first: a tap comes down and up in one message.
                    for key in down
                        .into_iter()
                        .filter(|&k| (k as usize) < c.game.keys.len())
                    {
                        if !e.keys.iter().any(|k| k.0 == key) {
                            e.keys.push((key, now));
                            told.push(abi::KeyChange { player: entity, key, down: 1, held_ticks: 0, pad: 0 });
                        }
                    }
                    for key in up.into_iter().filter(|&k| (k as usize) < c.game.keys.len()) {
                        if let Some(i) = e.keys.iter().position(|k| k.0 == key) {
                            let (_, since) = e.keys.swap_remove(i);
                            told.push(abi::KeyChange {
                                player: entity,
                                key,
                                down: 0,
                                held_ticks: now.saturating_sub(since) as u32,
                                pad: 0,
                            });
                        }
                    }
                }
                // Letting go of the use button is the end of using what it held (a bow shoots).
                let use_key = c.game.keys.iter().position(|k| k.name == "core:use");
                if told
                    .iter()
                    .any(|k| k.down == 0 && Some(k.key as usize) == use_key)
                {
                    r.release_use(0, c, entity);
                }
                for k in told {
                    r.emit_engine(0, c, abi::event::KEY, &k);
                }
            }
            ToServer::SwapHands => {
                let Some(off) = c.game.player_layout.off else { return };
                let Some((r, entity)) = self.region(pi) else { return };
                let Some(e) = r
                    .entities
                    .get(entity)
                    .filter(|e| !e.mode.has(crate::tag_bits::game_mode::NO_INTERACT))
                else {
                    return;
                };
                let sel = e.selected;
                let (main, other) = (r.entities.inventory_slot(entity, sel), r.entities.inventory_slot(entity, off));
                if crate::content::fits_one_hand(c, main.item) {
                    r.entities.set_slot(entity, sel, other);
                    r.entities.set_slot(entity, off, main);
                }
            }
            ToServer::Move { pos, vel, yaw, pitch, on_ground, pose, sprint, flying, jumped, frame } => {
                if !pos.is_finite() || !vel.is_finite() {
                    return;
                }
                // On a body: where the client has the player on it, on the body as it is here.
                let dim = &self.dims[self.players[pi].dim as usize];
                let on = frame.filter(|f| f.1.is_finite()).and_then(|(b, rel)| {
                    dim.entity_region_ref(b)
                        .and_then(|r| r.entities.get(b))
                        .map(|e| (b, e.body_place().to_world(rel), rel))
                });
                let pos = on.map_or(pos, |o| o.1);
                let place = on.map_or((None, pos), |o| (Some(o.0), o.2));
                let p = &mut self.players[pi];
                if let Some(t) = p.teleported {
                    if (pos - t).length_squared() > 16.0 {
                        return;
                    }
                    p.teleported = None;
                    p.last_place = place;
                }
                // Measured on what it rides, like a mob's: a lift going down under a jump isn't a
                // fall. Getting on or off isn't a step.
                let walked = if p.last_place.0 == place.0 { place.1 - p.last_place.1 } else { DVec3::ZERO };
                p.last_place = place;
                let Some((r, entity)) = self.region(pi) else { return };
                let Some(e) = r.entities.get_mut(entity) else { return };
                // On a mount it is where its seat is; only where it looks is its own.
                if e.vehicle.is_some() {
                    (e.yaw, e.pitch) = (yaw, pitch);
                    return;
                }
                e.pose = pose;
                let dims = e.dims(c);
                let dry = r.dry_volumes();
                let seen = crate::physics::ObservedCells { cells: super::space::CubeCells(&r.space.cubes), dry: &dry };
                let here = crate::physics::around(c, &seen, pos, dims.height, dims.half_width);
                let Some(e) = r.entities.get_mut(entity) else { return };
                let splash = entity::splashes(e.in_liquid, here.in_liquid, -walked.y);
                let fell = entity::track_fall(&mut e.fall, walked.y, on_ground, here.in_liquid || here.climbing);
                e.sprinting = sprint;
                e.flying = flying;
                e.jumped |= jumped;
                e.moved = walked.with_y(0.0).length() as f32;
                e.pos = pos;
                e.vel = vel;
                e.frame = on.map(|o| o.0);
                e.yaw = yaw;
                e.pitch = pitch;
                e.on_ground = on_ground;
                e.in_liquid = here.in_liquid;
                r.fall_effects(0, c, entity, fell, splash);
                if let Some(amount) = fell.and_then(|f| r.fall_damage(0, c, entity, f)) {
                    r.damage(0, c, entity, &abi::Damage::new(amount, abi::damage_cause::FALL));
                }
            }

            ToServer::MountMove { pos, yaw, vel, on_ground } => {
                if !pos.is_finite() || !vel.is_finite() || !yaw.is_finite() {
                    return;
                }
                let Some((r, entity)) = self.region(pi) else { return };
                r.mount_moved(0, c, entity, pos, yaw, vel, on_ground);
            }
            ToServer::Dismount => {
                let Some((r, entity)) = self.region(pi) else { return };
                r.dismount(0, c, entity);
            }
            ToServer::Camera(mode) => {
                let Some(m) = c.game.cameras.get(mode as usize) else { return };
                let Some((r, entity)) = self.region(pi) else { return };
                let pos = r.entities.get(entity).map_or(DVec3::ZERO, |e| e.pos);
                let ask = crate::conditions::Ask { pos, entity, ..Default::default() };
                let mut q = weft_abi::Query { entity, amount: mode as f64, ..weft_abi::Query::DEFAULT };
                let holds = m
                    .when
                    .is_none_or(|w| crate::conditions::on_server(r, 0, c, w, &ask));
                let may = r.ask_or(0, c, weft_abi::action::CAMERA, &mut q, holds);
                if may {
                    self.players[pi]
                        .conn
                        .send(ToClient::Camera { mode, locked: false, transition: f32::NAN, easing: 0 });
                }
            }

            ToServer::Break { pos, part, face, hit, frame } => {
                let Some((r, entity)) = self.region(pi) else { return };
                let request = ToClient::BreakRefused { frame, pos };
                let hit = (glam::Vec3::from_array(hit) - part.shift().as_vec3()).to_array();
                let Some((frame, pos, part, turn)) = r.landed_as(c, frame, pos, part) else {
                    self.players[pi].conn.send(request);
                    return;
                };
                let (face, hit) = landed_hit(turn, face, hit);
                let mining = r.entities.get_mut(entity).and_then(|e| e.mining.take());
                let rate = r.mining_rate_of(c, entity, frame, pos, part);
                let progress = mining
                    .filter(|m| m.frame == frame && m.pos == pos && m.part == part)
                    .map_or(0.0, |m| m.progress);
                // Done by what the server added up, with room for lag: the client's word is only
                // that it is, and a modified one could say anything.
                let too_soon = rate < 1.0 && progress + 2.0 * rate < 0.7;
                if r.break_mined(c, entity, frame, pos, part, face, hit, too_soon) {
                    self.players[pi].conn.send(request);
                }
            }

            ToServer::Use { pos, part, face, look, hit, sneak, by_face, frame, placed } => {
                let Some((r, entity)) = self.region(pi) else { return };
                // What the client shows there is set right at the end of the tick, placed or not.
                if let Some(p) = placed.filter(|_| r.has_space(frame)) {
                    r.resend_cell(frame, p);
                }
                let Some(normal) = crate::world::DIRS.get(face as usize) else { return };
                if !hit
                    .iter()
                    .all(|v| v.is_finite() && (-0.01..=1.01).contains(v))
                {
                    return;
                }
                let local = (glam::Vec3::from_array(hit) - part.shift().as_vec3()).to_array();
                let Some((frame, owner, part, turn)) = r.landed_as(c, frame, pos, part) else { return };
                let (face, local) = landed_hit(turn, face, local);
                let point = DVec3::from_array(local.map(f64::from)) + part.shift();
                let normal = IVec3::from(turn.vector(normal.to_array()));
                let delta = (point - normal.as_dvec3() * 1e-5).floor().as_ivec3();
                let (pos, part) = (owner + delta, part.in_cell(delta));
                let hit = (point - delta.as_dvec3()).as_vec3().to_array();
                let Some(e) = r.entities.get(entity) else { return };
                let eye = e.pos + DVec3::Y * e.eye(c);
                let reach = r.block_reach(0, c, entity);
                // The block clicked goes first; only then what is in the hands.
                let used = r.with_space(c, frame, |d, space_id, o| {
                    if !within(o.to_local(eye), pos, reach) {
                        return None;
                    }
                    if !d.has_placed(space_id, pos, part) {
                        return None;
                    }
                    let (owner, part) = part.owner(pos);
                    Some(d.as_placed(space_id, owner, part, |d| d.player_use(space_id, c, owner, part.state, face, entity, sneak)))
                });
                match used {
                    Some(Some(true)) => {
                        r.events
                            .push(DimEvent::EntitySwing { id: entity, off: false });
                        self.players[pi]
                            .conn
                            .send(ToClient::Swing { used_item: false, off_hand: false });
                    }
                    Some(Some(false)) => {
                        for slot in self.hands(c, pi) {
                            if self.place_or_use(c, pi, slot, frame, pos, part, face, look, hit, by_face) {
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }

            ToServer::UseItem => {
                for slot in self.hands(c, pi) {
                    if self.use_in_hand(c, pi, slot, 0, None) {
                        break;
                    }
                }
            }
            ToServer::Pick { item } => {
                let Some((r, entity)) = self.region(pi) else { return };
                let Some(e) = r.entities.get(entity) else { return };
                let (sel, creative) = (e.selected, e.mode.has(crate::tag_bits::game_mode::NO_COSTS));
                let Some(inv) = r.entities.inventories.get(&entity) else { return };
                let current = inv.get(sel as usize).copied().unwrap_or_default();
                if let Some(j) = c
                    .game
                    .player_layout
                    .main()
                    .map(|j| j as usize)
                    .find(|&j| inv.get(j).is_some_and(|s| s.item == item && !s.is_empty()))
                {
                    let found = inv[j];
                    r.entities.set_slot(entity, j as u32, current);
                    r.entities.set_slot(entity, sel, found);
                } else if creative && c.items.get(item as usize).is_some() {
                    r.entities.set_slot(entity, sel, ItemStack::new(item, 1));
                }
            }
            ToServer::Select(slot) => {
                if slot < c.game.player_layout.hotbar {
                    if let Some((r, entity)) = self.region(pi) {
                        if let Some(e) = r.entities.get_mut(entity) {
                            e.selected = slot;
                        }
                        let held = r.entities.held(entity);
                        if let Some(e) = r.entities.get_mut(entity) {
                            e.notice_hand(held);
                        }
                    }
                }
            }

            ToServer::Attack(target) => {
                let Some((r, entity)) = self.region(pi) else { return };
                let held = r.entities.held(entity);
                if let Some((owner, callback)) = c
                    .items
                    .get(held.item as usize)
                    .and_then(|item| item.on_attack)
                {
                    if target == crate::net::ITEM_ATTACK && r.attack_with_item(0, entity, held, owner, callback) {
                        self.players[pi]
                            .conn
                            .send(ToClient::Swing { used_item: false, off_hand: false });
                    }
                    return;
                }
                if !in_reach(c, r, entity, target) {
                    return;
                }
                r.melee(0, c, entity, target);
            }

            ToServer::Swing => {
                if let Some(e) = self.region(pi).and_then(|(r, id)| r.entities.get_mut(id)) {
                    e.swing_ticks = 0;
                }
            }

            ToServer::Interact(target) => {
                let Some((r, entity)) = self.region(pi) else { return };
                let Some(kind) = r.entities.get(target).map(|e| e.kind) else { return };
                if !in_reach(c, r, entity, target) {
                    return;
                }
                let held = r.entities.held(entity);
                let mut q = abi::Query { entity: target, other: entity, item: held.item, ..abi::Query::DEFAULT };
                if !r.ask(0, c, abi::action::INTERACT_ENTITY, &mut q) {
                    return;
                }
                r.emit_engine(
                    0,
                    c,
                    abi::action::INTERACT_ENTITY,
                    &abi::Interaction { player: entity, entity: target, item: held.item, pad: 0 },
                );
                let mut handled = false;
                for &(owner, cb) in &c.game.entities[kind as usize].on_interact {
                    handled |= r
                        .call(0, owner, cb, |w| abi::EntityEventArgs { world: w, entity: target, other: entity, handled: 0 })
                        .handled
                        != 0;
                }
                if handled {
                    self.players[pi]
                        .conn
                        .send(ToClient::Swing { used_item: false, off_hand: false });
                } else {
                    for slot in self.hands(c, pi) {
                        if self.use_in_hand(c, pi, slot, 0, None) {
                            break;
                        }
                    }
                }
            }
            ToServer::OpenInventory => {
                if let Some(m) = c.game.player_menu {
                    self.open_menu(c, pi, m, None, 0);
                }
            }
            ToServer::Click { slot, button, shift } => {
                let Server { players, dims, .. } = self;
                let p = &mut players[pi];
                if let (Some(m), Some(r)) = (&mut p.menu, dims[p.dim as usize].entity_region(p.entity)) {
                    menu::click(m, r, c, p.entity, slot, button, shift);
                }
            }
            ToServer::CreativeTake { item, button } => {
                let Server { players, dims, .. } = self;
                let p = &mut players[pi];
                let creative = dims[p.dim as usize]
                    .entity(p.entity)
                    .is_some_and(|e| e.mode.has(crate::tag_bits::game_mode::CREATIVE_INVENTORY));
                let hot = button.wrapping_sub(menu::HOTBAR_SWAP);
                if let (true, Some(m), true, true) = (creative, &mut p.menu, item != 0 && (item as usize) < c.items.len(), hot < 9) {
                    if let Some(r) = dims[p.dim as usize].entity_region(p.entity) {
                        menu::creative_to_hotbar(m, r, c, p.entity, hot as u32, item);
                    }
                } else if let (true, Some(m), true) = (creative, &mut p.menu, item != 0 && (item as usize) < c.items.len()) {
                    // Like vanilla: the same item adds one, anything else held is thrown away
                    // first, an empty hand takes a stack (left) or one (right).
                    let max = c.max_stack(&ItemStack::new(item, 1));
                    m.cursor = if m.cursor.is_empty() {
                        ItemStack::new(item, if button == 1 { 1 } else { max })
                    } else if m.cursor.item == item && m.cursor.data == 0 {
                        ItemStack { count: (m.cursor.count + 1).min(max), ..m.cursor }
                    } else {
                        ItemStack::EMPTY
                    };
                }
            }
            ToServer::Drag { slots, button } => {
                let Server { players, dims, .. } = self;
                let p = &mut players[pi];
                if let (Some(m), Some(r)) = (&mut p.menu, dims[p.dim as usize].entity_region(p.entity)) {
                    menu::drag(m, r, c, p.entity, &slots, button);
                }
            }
            ToServer::CloseMenu => {
                let Server { players, dims, .. } = self;
                let p = &mut players[pi];
                if let (Some(m), Some(r)) = (p.menu.take(), dims[p.dim as usize].entity_region(p.entity)) {
                    menu::close(m, r, c, p.entity);
                }
            }

            ToServer::Drop { all } => {
                let Some((r, entity)) = self.region(pi) else { return };
                let Some(sel) = r.entities.get(entity).map(|e| e.selected) else { return };
                // A two-handed item that does nothing can still be thrown away.
                let held = r.entities.in_selected_slot(entity);
                if held.is_empty() {
                    return;
                }
                let n = if all { held.count } else { 1 };
                r.entities
                    .set_slot(entity, sel, ItemStack { count: held.count - n, ..held });
                r.throw(0, c, entity, ItemStack { count: n, ..held });
            }
            ToServer::Respawn => {
                if let Some((r, id)) = self.region(pi) {
                    if let Some(e) = r.entities.get_mut(id) {
                        e.fall = 0.0;
                    }
                }
                let spawn = self.dims[c.spawn_dim as usize].spawn_point(c);
                self.change_dim(c, pi, c.spawn_dim, spawn);
                if let Some((r, entity)) = self.region(pi) {
                    // A death ends every effect, as vanilla's.
                    let effect = c.game.fields.effect;
                    let had: Vec<_> = crate::world::effects_on(r.entities.comps.get(&(entity, effect)).map(|b| &b[..]), r.time).collect();
                    for f in had {
                        let end = abi::EffectChange { entity, effect: f.id, level: f.level, ticks: 0, pad: 0 };
                        r.emit_engine(0, c, abi::event::EFFECT_ENDED, &end);
                    }
                    r.entities.reset_components(c, entity);
                    r.entities.comps.insert((entity, effect), Box::default());
                    r.entities.dirty_comp.insert((entity, effect));
                    // The game mode's modifiers went with the rest.
                    r.keep_sources(c, entity);
                    // After the components: modifiers are gone with them. Sent as any change.
                    let max = r.max_health(0, c, entity);
                    if let Some(e) = r.entities.get_mut(entity) {
                        e.health = max;
                        e.fire = 0;
                        e.hurt_cooldown = 0;
                    }
                    r.emit_engine(0, c, abi::event::PLAYER_RESPAWNED, &abi::PlayerEvent { player: entity });
                }
                // Out of lives: on as a spectator, as in vanilla's hardcore.
                let lives = crate::config::get(c.game.lives) as u32;
                if lives > 0 && self.players[pi].deaths >= lives {
                    self.set_mode(pi, crate::world::GameMode::SPECTATOR);
                }
            }
            ToServer::Chat(text) => {
                let name = self.players[pi].identity.name.clone();
                if let Some(cmd) = text.strip_prefix('/') {
                    for line in self.command(c, pi, cmd) {
                        let from = crate::net::Shown::REPLY;
                        self.players[pi]
                            .conn
                            .send(ToClient::Show(crate::net::Shown { from, ..crate::net::Shown::chat(line) }));
                    }
                } else {
                    let text: String = text.chars().take(256).collect();
                    let mut line = None;
                    if let Some((r, player)) = self.region(pi) {
                        let mut q = abi::Query { entity: player, text: abi::Str::new(&text), ..abi::Query::DEFAULT };
                        if !r.ask(0, c, abi::action::CHAT, &mut q) {
                            return;
                        }
                        if let Some((owner, cb)) = c.game.chat_format {
                            let out = &raw mut line as *mut std::ffi::c_void;
                            r.call(0, owner, cb, |world| abi::ChatFormatArgs { world, player, text: abi::Str::new(&text), out });
                        }
                        r.emit_engine(0, c, abi::action::CHAT, &abi::ChatMessage::new(player, &text));
                    }
                    log::info!("<{name}> {text}");
                    let line = line.unwrap_or_else(|| Msg::new("chat.message", vec![Arg::Text(name), Arg::Text(text)]));
                    let shown = crate::net::Shown { from: crate::net::Shown::PLAYER, ..crate::net::Shown::chat(line) };
                    self.show(abi::Who::All, shown);
                }
            }
            ToServer::Command { id, line } => {
                let lines = self.command(c, pi, line.trim_start_matches('/'));
                self.players[pi]
                    .conn
                    .send(ToClient::CommandReply { id, lines });
            }
            ToServer::Packet { channel, data } => {
                let Some((owner, cb)) = c.game.packets.get(channel as usize).and_then(|d| d.server) else { return };
                let Some((r, player)) = self.region(pi) else { return };
                let slice = abi::Slice::new(&data);
                r.call(0, owner, cb, |w| abi::PacketArgs { world: w, player, data: slice });
            }
            ToServer::Skin(skin) => {
                // Only a skin's size of PNG: everyone else stores and uploads it.
                let size = crate::assets::png_size(&skin.png).filter(|_| skin.png.len() <= 65536);
                if !matches!(size, Some((64, 64) | (64, 32))) {
                    return;
                }
                let skin = Arc::new(skin);
                let entity = self.players[pi].entity;
                self.players[pi].skin = Some(skin.clone());
                if !self.players[pi].joined {
                    return;
                }
                for p in self.players.iter_mut().filter(|p| p.joined) {
                    p.conn
                        .send(ToClient::Skin { entity, skin: (*skin).clone() });
                }
            }
            ToServer::Avatar(avatar) => {
                // Once a stay, and only what fits: everyone else keeps it for as long as they play.
                let allowed = crate::config::by_name("core:allow_avatars").is_none_or(|id| crate::config::get(id) != 0.0);
                if self.players[pi].avatar.is_some() || avatar.size() > crate::net::AVATAR_BYTES || !allowed {
                    return;
                }
                let avatar = Arc::new(avatar);
                let entity = self.players[pi].entity;
                self.players[pi].avatar = Some(avatar.clone());
                if !self.players[pi].joined {
                    return;
                }
                for p in self.players.iter_mut().filter(|p| p.joined) {
                    p.conn
                        .send(ToClient::Avatar { entity, avatar: (*avatar).clone() });
                }
            }
            ToServer::View(r) => {
                let c = content();
                let def = crate::config::by_name("core:view_distance").map(|id| &c.game.configs[id as usize]);
                self.players[pi].view_radius = def.map_or(r as f64, |d| d.fit(r as f64)) as i32;
            }
            ToServer::Gizmos(mut names) => {
                names.retain(|n| n.len() <= 64);
                names.truncate(64);
                self.players[pi].gizmos = names;
            }
            ToServer::Leave => {
                if !self.players[pi].joined {
                    let p = &mut self.players[pi];
                    if p.arrival.take().is_some() {
                        p.conn.send(ToClient::Disconnect("bye".into()));
                        p.conn.close();
                    }
                    return;
                }
                log::info!("{} left", self.players[pi].identity.name);
                let line = Msg::new("multiplayer.left", vec![Arg::Text(self.players[pi].identity.name.clone())]);
                let entity = self.players[pi].entity;
                for q in self.players.iter_mut().filter(|q| q.joined) {
                    q.conn
                        .send(ToClient::Show(crate::net::Shown::chat(line.clone())));
                    q.conn.send(ToClient::PlayerGone { entity });
                }
                let Server { players, dims, storage, identities, .. } = self;
                let p = &mut players[pi];
                if let Some(r) = dims[p.dim as usize].entity_region(p.entity) {
                    if let Some(m) = p.menu.take() {
                        menu::close(m, r, c, p.entity);
                    }
                    if let Some(s) = storage {
                        save_player(&s.lock(), p, r);
                    }
                    // What it held it lets go of as it leaves.
                    let held = r
                        .entities
                        .get(p.entity)
                        .map_or(Vec::new(), |e| e.keys.clone());
                    for (key, since) in held {
                        let up = abi::KeyChange { player: p.entity, key, down: 0, held_ticks: r.time.saturating_sub(since) as u32, pad: 0 };
                        r.emit_engine(0, c, abi::event::KEY, &up);
                    }
                    r.entities.remove(p.entity, abi::removal::LEFT);
                }
                p.conn.send(ToClient::Disconnect("bye".into()));
                identities.write().remove(&p.entity);
                p.joined = false;
            }
        }
    }
}

/// Within its `ENTITY_REACH`, from the eyes to the nearest point of the target's box, like
/// modern's; a player's with room for what its client saw last.
pub(crate) fn in_reach(c: &'static Content, d: &mut Dim, me: u64, target: u64) -> bool {
    let reach = d.attribute(0, c, me, crate::registry::attr::ENTITY_REACH, false);
    let (Some(me), Some(target)) = (d.entities.get(me), d.entities.get(target)) else { return false };
    let eye = me.pos + DVec3::Y * me.eye(c);
    let b = target.aabb(c);
    let reach = reach + if me.player { ENTITY_SLACK } else { 0.0 };
    (eye.clamp(b.min, b.max) - eye).length_squared() <= reach * reach && (me.player || sees(c, d, eye, target))
}

/// Whether nothing solid stands between `eye` and the middle or the top of `target`'s box, as
/// vanilla's line of sight: a mob doesn't hit through a wall or a staircase's ceiling.
/// Players aim by what their crosshair hits, which a wall already stops.
fn sees(c: &Content, d: &Dim, eye: DVec3, target: &Entity) -> bool {
    let b = target.aabb(c);
    let middle = (b.min + b.max) / 2.0;
    [middle, DVec3::new(middle.x, b.max.y - 0.1, middle.z)]
        .into_iter()
        .any(|to| {
            let (mut from, mut left) = (eye, (to - eye).length());
            let dir = (to - eye) / left.max(1e-9);
            // Plants and the like are seen through: only what stops a walker blocks the view.
            for _ in 0..16 {
                let Some(hit) = d.ray(0, c, from, dir, left, abi::ray::BLOCKS, &[]) else { return true };
                if c.states[hit.state as usize].flags & weft_abi::block_flags::SOLID != 0 {
                    return false;
                }
                let gone = hit.dist + 1e-3;
                from += dir * gone;
                left -= gone;
                if left <= 0.0 {
                    return true;
                }
            }
            false
        })
}

pub(super) fn within(eye: DVec3, block: IVec3, reach: f64) -> bool {
    (block.as_dvec3() + DVec3::splat(0.5) - eye).length_squared() < reach * reach
}

/// Damages the held tool; breaks it when used up. Creative players' tools never wear.
pub(super) fn wear(c: &Content, d: &mut Dim, player: u64, slot: u32, amount: u32) {
    if d.entities
        .get(player)
        .is_some_and(|e| e.mode.has(crate::tag_bits::game_mode::NO_COSTS))
    {
        return;
    }
    let stack = d.entities.inventory_slot(player, slot);
    if stack.is_empty() {
        return;
    }
    let next = crate::stack::worn(c, stack, amount);
    if next != stack {
        d.entities.set_slot(player, slot, next);
    }
}

impl Dim {
    /// `entity` uses the item in `slot` on what it clicked (`target`), or finishes using one that
    /// takes time: true if it did something, or started to. A player's right click and a mob's
    /// `World::use_item` alike.
    pub fn use_in_hand(
        &mut self,
        space_id: u64,
        c: &'static Content,
        entity: u64,
        slot: u32,
        target: Option<(IVec3, u8)>,
        finishing: bool,
    ) -> bool {
        let Some(e) = self.entities.get(entity) else { return false };
        let off_hand = c.game.entities[e.kind as usize].off == Some(slot);
        let held = self.entities.inventory_slot(entity, slot);
        let Some(def) = c.items.get(held.item as usize) else { return false };
        if e.blocking_disabled_until > self.time && self.blocking_item(c, &held).is_some() {
            self.events.push(DimEvent::Using { entity, using: None });
            return false;
        }
        let (pos, face) = target.unwrap_or((IVec3::ZERO, 0));
        let state = target.and_then(|_| self.get(space_id, pos)).unwrap_or(0);
        let mut q = abi::Query { entity, item: held.item, pos: pos.to_array().into(), state, ..abi::Query::DEFAULT };
        if !self.ask(space_id, c, abi::action::USE_ITEM, &mut q) {
            return false;
        }
        // An item that takes time starts here, and comes back through this when it is done.
        let ticks = crate::content::use_ticks(c, &held);
        if ticks > 0 && !finishing {
            let Some(e) = self.entities.get_mut(entity) else { return false };
            // Asked again while at it (an AI asking every tick), it goes on.
            if e.use_done
                .is_some_and(|(item, _, s)| item == held.item && s == slot)
            {
                return true;
            }
            (e.using, e.use_done) = ((ticks, off_hand), Some((held.item, ticks, slot)));
            self.events
                .push(DimEvent::Using { entity, using: Some((ticks, off_hand)) });
            return true;
        }
        let mut used = false;
        let gifts = c
            .components
            .id(abi::comp::GIVES_EFFECTS)
            .and_then(|id| c.components.bytes(crate::components::ITEM, held.item, id))
            .and_then(abi::data::from_bytes::<Vec<abi::EffectGift>>);
        let give = |d: &mut Dim, gifts: &[abi::EffectGift]| {
            for g in gifts {
                if let Some(effect) = c.game.catalog.find(ek::ENTITY_EFFECT, &g.effect) {
                    d.give_effect(space_id, c, entity, effect, g.level, g.ticks);
                }
            }
        };
        if let Some(gifts) = gifts.as_ref().filter(|_| def.on_use.is_none()) {
            give(self, gifts);
            // What is left of it: its remainder (the potion's bottle), else one fewer.
            let creative = self
                .entities
                .get(entity)
                .is_some_and(|e| e.mode.has(crate::tag_bits::game_mode::NO_COSTS));
            if !creative {
                let rest = c
                    .components
                    .id(abi::comp::REMAINDER)
                    .and_then(|id| {
                        c.components
                            .get::<u32>(crate::components::ITEM, held.item, id)
                    })
                    .filter(|&r| r != 0);
                let after = match rest {
                    Some(r) if held.count <= 1 => ItemStack::new(r, 1),
                    Some(r) => {
                        let left = self.entities.insert_item(c, entity, ItemStack::new(r, 1));
                        if !left.is_empty() {
                            self.spawn_item(0, c, self.entities.get(entity).map_or(DVec3::ZERO, |e| e.pos), left);
                        }
                        ItemStack { count: held.count - 1, ..held }
                    }
                    None => ItemStack { count: held.count - 1, ..held },
                };
                self.entities
                    .set_slot(entity, slot, if after.count == 0 { ItemStack::EMPTY } else { after });
            }
            used = true;
        } else if def.on_use.is_none() {
            if let Some(after) = self.use_container(space_id, c, entity, held) {
                if after != held {
                    self.entities.set_slot(entity, slot, after);
                }
                used = true;
            }
        } else if let Some((by, cb)) = def.on_use {
            let a = self.call(space_id, by, cb, |w| abi::ItemUseArgs {
                world: w,
                entity,
                stack: held,
                slot,
                pos: pos.to_array().into(),
                face: face as u32,
                has_target: target.is_some() as u32,
                handled: 0,
                held_ticks: 0,
            });
            if a.handled != 0 && a.stack != held && crate::host::known_item("on_use", &a.stack) {
                self.entities.set_slot(entity, slot, a.stack);
            }
            used = a.handled != 0;
            // Its own use (eating) and what it gives besides: a golden apple.
            if let Some(gifts) = gifts.as_ref().filter(|_| used) {
                give(self, gifts);
            }
        }
        if used {
            self.emit_engine(
                space_id,
                c,
                abi::action::USE_ITEM,
                &abi::ItemUsed { entity, stack: held, pos: pos.to_array().into(), has_target: target.is_some() as u32 },
            );
            self.events
                .push(DimEvent::EntitySwing { id: entity, off: off_hand });
            self.events.push(DimEvent::Used { entity, off: off_hand });
        }
        used
    }

    fn attack_with_item(&mut self, space_id: u64, entity: u64, held: ItemStack, owner: abi::ModId, callback: abi::Callback) -> bool {
        let Some(e) = self.entities.get(entity) else { return false };
        let slot = e.selected;
        let a = self.call(space_id, owner, callback, |w| abi::ItemUseArgs {
            world: w,
            entity,
            stack: held,
            slot,
            pos: [0; 3].into(),
            face: 0,
            has_target: 0,
            handled: 0,
            held_ticks: 0,
        });
        if a.handled == 0 {
            return false;
        }
        if a.stack != held && crate::host::known_item("on_attack", &a.stack) {
            self.entities.set_slot(entity, slot, a.stack);
        }
        self.events
            .push(DimEvent::EntitySwing { id: entity, off: false });
        true
    }

    /// One tick of using items: held long enough they take effect; another item in hand ends
    /// their use. `players_only` while the world is frozen: players still act.
    pub fn finish_uses(&mut self, space_id: u64, c: &'static Content, players_only: bool) {
        let using: Vec<(u64, u32, u32, u32, u32, u32)> = self
            .entities
            .list
            .iter()
            .filter(|e| !e.removed && (e.player || !players_only))
            .filter_map(|e| {
                e.use_done
                    .map(|(item, left, slot)| (e.id, item, left, slot, e.selected, e.kind))
            })
            .collect();
        for (id, item, left, slot, selected, kind) in using {
            let held = self.entities.inventory_slot(id, slot);
            let off = c.game.entities[kind as usize].off == Some(slot);
            if held.item != item || held.is_empty() || (!off && selected != slot) {
                self.stop_using(id);
            } else if left <= 1 {
                self.stop_using(id);
                self.use_in_hand(space_id, c, id, slot, None, true);
            } else if let Some(e) = self.entities.get_mut(id) {
                e.use_done = Some((item, left - 1, slot));
            }
        }
    }

    /// `id` let go of the use button: what it was using hears after how long (its
    /// `comp::item::ON_RELEASE`: a bow shoots), then it stops.
    pub fn release_use(&mut self, space_id: u64, c: &'static Content, id: u64) {
        let Some(e) = self.entities.get(id) else { return };
        let (Some((item, left, slot)), total) = (e.use_done, e.using.0) else { return self.stop_using(id) };
        self.stop_using(id);
        let held = self.entities.inventory_slot(id, slot);
        let Some((by, cb)) = c
            .items
            .get(item as usize)
            .and_then(|d| d.on_release)
            .filter(|_| held.item == item)
        else {
            return;
        };
        let a = self.call(space_id, by, cb, |w| abi::ItemUseArgs {
            world: w,
            entity: id,
            stack: held,
            slot,
            pos: abi::BlockPos::ZERO,
            face: 0,
            has_target: 0,
            handled: 0,
            held_ticks: total.saturating_sub(left),
        });
        if a.handled != 0 && a.stack != held && crate::host::known_item("on_release", &a.stack) {
            self.entities.set_slot(id, slot, a.stack);
        }
    }

    /// `id` lets go of what it was using.
    pub fn stop_using(&mut self, id: u64) {
        let Some(e) = self.entities.get_mut(id) else { return };
        let was = e.use_done.take().is_some();
        e.using = (0, false);
        if was {
            self.events
                .push(DimEvent::Using { entity: id, using: None });
        }
    }
}

impl Dim {
    /// `entity` starts breaking `part` of the cell at `pos` in `frame` (`u32::MAX`: what it
    /// stands for), a player holding the button or a mob (`World::start_mining`). False when
    /// it may not (`actions::BREAK_BLOCK`), said now so it doesn't mine all that time for nothing.
    pub fn start_mining(&mut self, c: &'static Content, entity: u64, frame: u64, pos: IVec3, part: Part) -> bool {
        let held = self.entities.held(entity).item;
        if c.items
            .get(held as usize)
            .is_some_and(|item| item.on_attack.is_some())
        {
            self.stop_mining(entity);
            return false;
        }
        let aimed = self.with_space(c, frame, |d, space_id, _| {
            d.has_placed(space_id, pos, part)
                && d.as_placed(space_id, pos, part, |d| d.may_break(space_id, c, pos, part.state, held, entity))
        });
        let Some(e) = self.entities.get_mut(entity) else { return false };
        e.mining = (aimed == Some(true)).then_some(super::entity::Mining { frame, pos, part, progress: 0.0 });
        e.mining.is_some()
    }

    pub fn stop_mining(&mut self, id: u64) {
        if let Some(e) = self.entities.get_mut(id) {
            e.mining = None;
        }
    }

    /// One tick of mining, by what each one holds and where it is now. A player's client says
    /// when it is done (`ToServer::Break`, checked against this); anything else breaks the block
    /// when it gets there. `players_only` while the world is frozen: players still act.
    pub fn mine(&mut self, c: &'static Content, players_only: bool) {
        let mining: Vec<(u64, super::entity::Mining, bool)> = self
            .entities
            .list
            .iter()
            .filter(|e| !e.removed && e.melee_recovery == 0 && (e.player || !players_only))
            .filter_map(|e| e.mining.map(|m| (e.id, m, e.player)))
            .collect();
        for (id, m, player) in mining {
            let progress = (m.progress + self.mining_rate_of(c, id, m.frame, m.pos, m.part)).min(1.0);
            let Some(e) = self.entities.get_mut(id) else { continue };
            e.mining = Some(super::entity::Mining { progress, ..m });
            if player || progress < 1.0 {
                continue;
            }
            e.mining = None;
            // The face toward it, its middle hit.
            let eye = e.pos + DVec3::Y * e.eye(c);
            let to = self.space_pose(m.frame).to_local(eye) - (m.pos.as_dvec3() + DVec3::splat(0.5));
            let axis = to.abs().max_position();
            let face = (axis * 2 + (to[axis] > 0.0) as usize) as u8;
            self.break_mined(c, id, m.frame, m.pos, m.part, face, [0.5; 3], false);
        }
    }

    /// `entity` breaks what it mined: within its reach, unless its mode keeps blocks, what it
    /// holds wearing for a block that took work. `too_soon`: refused anyway (a client saying it is
    /// done before the server's count does). True when a block stays that it didn't break, which
    /// is sent back to who already shows it gone.
    #[allow(clippy::too_many_arguments)]
    pub fn break_mined(
        &mut self,
        c: &'static Content,
        entity: u64,
        frame: u64,
        pos: IVec3,
        part: Part,
        face: u8,
        hit: [f32; 3],
        too_soon: bool,
    ) -> bool {
        let Some(e) = self.entities.get(entity) else { return false };
        let (eye, sel, creative) = (e.pos + DVec3::Y * e.eye(c), e.selected, e.mode.has(crate::tag_bits::game_mode::INSTANT_BREAK));
        let recovering = e.melee_recovery > 0;
        let no_break = e.mode.has(crate::tag_bits::game_mode::NO_BREAK);
        let (reach, held) = (self.block_reach(0, c, entity), self.entities.held(entity));
        let mut refused = false;
        let worn = self.with_space(c, frame, |d, space_id, o| {
            let valid =
                d.has_placed(space_id, pos, part) && c.states[part.state as usize].flags & weft_abi::block_flags::NOT_TARGETABLE == 0;
            let keeps = (creative && c.no_creative_break(held.item))
                || no_break
                || c.items
                    .get(held.item as usize)
                    .is_some_and(|item| item.on_attack.is_some());
            let free = c
                .states
                .get(part.state as usize)
                .is_some_and(|s| s.hardness <= 0.0);
            let local_eye = o.to_local(eye) - pos.as_dvec3();
            let done = valid
                && part
                    .bounds(c)
                    .any(|b| local_eye.distance_squared(local_eye.clamp(b.min, b.max)) <= (reach + 0.5).powi(2))
                && !too_soon
                && !recovering
                && !keeps
                && d.as_placed(space_id, pos, part, |d| d.break_piece(space_id, c, pos, part.state, face, hit, held, entity, !creative));
            if !done && d.get(space_id, pos).is_some() {
                d.resend_cell(space_id, pos);
                // A held click can retry once the server's swing has recovered.
                refused = !recovering;
            }
            done && !free
        });
        if worn == Some(true) && !held.is_empty() {
            wear(c, self, entity, sel, 1);
        }
        refused
    }
}
