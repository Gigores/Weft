# Raycasts and hitboxes



## Raycasts

`w.in_liquid(pos)` knows a liquid's surface, which the block alone doesn't tell. `w.raycast(from, dir, max, Ray::blocks().entities().skip(me))` (`.solid()` for blocks by what stops a body, `.liquids()`) finds what any line hits first, a block or an entity (a turret, whether a mob sees a player), with the face it went in by and where on it: `hit.local` is from the block's or the entity's box's low corner, a block's 0..1, times 16 the pixel (a panel of buttons on one face); `w.looked_at(id, max, ray)` is that from an entity's eyes.

## Hitboxes

`Hitboxes::new().bone(name, bone, min, max)` defines a box in rest-pose model pixels. It follows the named bone of the registered model during walking, riding and other movement. Resource-pack and avatar changes are cosmetic. `Hitboxes::humanoid()` and `Hitboxes::quadruped()` bind their parts to the standard bones. Use `.part(..)`, `.head(..)` or `.limb(..)` to drive a box without a bone binding.

`hit.part` says which part was hit (`hitbox_part::HEAD`, `hitbox_part_id("tail")`). `hit.face` and `hit.local` are in that part's coordinates. The part goes into `Damage::part` and the query of a `DAMAGE`, `MELEE` or `PROJECTILE_DAMAGE` step. Base's arrows hurt half as much again in the head. A kind without parts is hit by its plain box. `/gizmo hitboxes` (F3+B) draws the movement box in white and the damage zones in pink.
