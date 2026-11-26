//! Sound events: block groups (`block.<group>.break|place|step|hit`) blocks pick with
//! `.sound_group(group)`, and the background music; mobs' voices come with them (`mobs.rs`). Files are in `assets/sounds/`.

use crate::worldgen::statics::*;
use weft_sdk::*;
static CRICKETS: Music = Music::named("crickets");
static DAY_FOREST: Music = Music::named("day_forest");
static SHEEP: Music = Music::named("sheep");
static LIGHT_RAIN: Music = Music::named("light_rain");
static FOREST_RAIN: Music = Music::named("forest_rain");
static BEACH_WAVES: Music = Music::named("beach_waves");
static CAVE_DRONE: Sound = Sound::named("cave_drone");
static CAVE_DRONES: Music = Music::named("cave_drones");

pub fn register(r: &mut Registry) {
    for g in ["stone", "wood", "gravel", "grass", "sand", "glass", "cloth"] {
        let files = |kind: &str| {
            (1..=3)
                .map(|i| format!("{kind}/{g}{i}"))
                .collect::<Vec<_>>()
        };
        let (dig, step) = (files("dig"), files("step"));
        let dig: Vec<&str> = dig.iter().map(String::as_str).collect();
        let step: Vec<&str> = step.iter().map(String::as_str).collect();
        // Vanilla breaks glass with its own sound and knocks on it like on stone.
        let breaks: Vec<&str> = if g == "glass" { vec!["random/glass1", "random/glass2", "random/glass3"] } else { dig.clone() };
        r.sound(Sound::generated(format!("block.{g}.break")))
            .files(&breaks)
            .pitch(0.75, 0.85);
        r.sound(Sound::generated(format!("block.{g}.place")))
            .files(&dig)
            .pitch(0.75, 0.85);
        r.sound(Sound::generated(format!("block.{g}.step")))
            .files(&step)
            .pitch(0.9, 1.1);
        r.sound(Sound::generated(format!("block.{g}.hit")))
            .files(&step)
            .volume(0.6)
            .pitch(0.5, 0.55);
    }
    r.music(&SHEEP)
        .tracks(&["music/418DreamingSheep"])
        .when(&conditions::SURFACE);

    r.music(&CRICKETS)
        .tracks(&["ambient/crickets"])
        .ambient(0.1)
        .when(&conditions::NIGHT)
        .biome(&PLAINS)
        .biome(&LAVENDER_FIELDS)
        .biome(&TALLGRASS_MEADOW)
        .biome(&CLOVER_MEADOW)
        .biome(&FOREST)
        .biome(&BIRCH_FOREST)
        .rain(0.0, 0.05);

    r.music(&LIGHT_RAIN)
        .tracks(&["ambient/light_rain"])
        .ambient(0.1)
        .biome(&PLAINS)
        .biome(&LAVENDER_FIELDS)
        .biome(&TALLGRASS_MEADOW)
        .biome(&CLOVER_MEADOW)
        .rain(0.05, 1.0);

    r.music(&DAY_FOREST)
        .tracks(&["ambient/day_forest"])
        .ambient(0.1)
        .when(&conditions::DAY)
        .biome(&FOREST)
        .biome(&BIRCH_FOREST)
        .rain(0.0, 0.05);

    r.music(&FOREST_RAIN)
        .tracks(&["ambient/forest_rain"])
        .ambient(0.1)
        .biome(&FOREST)
        .biome(&BIRCH_FOREST)
        .rain(0.05, 1.0);

    r.music(&BEACH_WAVES)
        .tracks(&["ambient/beach_waves"])
        .ambient(0.1)
        .when(&conditions::SURFACE)
        .biome(&BEACH);

    r.sound(&CAVE_DRONE)
        .files(&["ambient/cave_drone"])
        .category(&sound_categories::AMBIENT)
        .range(40.0);

    r.music(&CAVE_DRONES)
        .ambient(5.0)
        .when(&conditions::UNDERGROUND)
        .spot(&CAVE_DRONE)
        .spot_every(10.0, 10.0)
        .spot_distance(8.0, 24.0);
}
