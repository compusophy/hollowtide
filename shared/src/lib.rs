#![no_std]
extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

pub const TICK_HZ: u32 = 20;
pub const ECHO_FRAME_HZ: u32 = 10;
pub const ECHO_DURATION_SECS: u32 = 10;
pub const ECHO_FRAME_COUNT: usize = (ECHO_FRAME_HZ * ECHO_DURATION_SECS) as usize;
pub const WORLD_TILES: i32 = WORLD_TILES_MAJOR;
pub const PLAYER_BASE_HP: i32 = 100;
pub const MOB_BASE_HP: i32 = 40;
pub const PLAYER_MOVE_SPEED: f32 = 4.0; // tiles per second
pub const ATTACK_RANGE: f32 = 1.5;
pub const ATTACK_COOLDOWN_TICKS: u32 = 8; // ~0.4s
pub const ATTACK_DAMAGE: i32 = 12;
pub const WITNESS_RANGE: f32 = 0.8;
pub const WORLD_TILES_MAJOR: i32 = 200; // open-world size (linear)
pub const WITNESS_TICKS_REQUIRED: u32 = 200; // 10s
pub const EXORCISE_DAMAGE: i32 = 1;

pub const BIOME_RADIUS: f32 = 42.0; // distance from origin where biomes start
pub const LANDMARK_COUNT: usize = 12;

// Combat tuning — class-differentiated
pub const WARRIOR_CLEAVE_RANGE: f32 = 1.8;
pub const WARRIOR_CLEAVE_ARC_RAD: f32 = 2.2;     // ~126° cone
pub const WARRIOR_CLEAVE_DAMAGE: i32 = 10;
pub const ARCHER_ARROW_SPEED: f32 = 14.0;         // tiles / sec
pub const ARCHER_ARROW_DAMAGE: i32 = 14;
pub const ARCHER_ARROW_TTL_TICKS: u32 = 40;       // 2s
pub const MAGICIAN_BOLT_SPEED: f32 = 9.0;
pub const MAGICIAN_BOLT_DAMAGE: i32 = 18;
pub const MAGICIAN_BOLT_TTL_TICKS: u32 = 60;      // 3s
pub const PROJECTILE_HIT_RADIUS: f32 = 0.7;

pub type EntityId = u64;
pub type Tick = u64;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub const ZERO: Vec2 = Vec2 { x: 0.0, y: 0.0 };
    pub fn new(x: f32, y: f32) -> Self { Self { x, y } }
    pub fn dist(self, o: Vec2) -> f32 {
        let dx = self.x - o.x;
        let dy = self.y - o.y;
        (dx * dx + dy * dy).sqrt()
    }
    pub fn lerp(self, o: Vec2, t: f32) -> Vec2 {
        Vec2::new(self.x + (o.x - self.x) * t, self.y + (o.y - self.y) * t)
    }
    pub fn sub(self, o: Vec2) -> Vec2 { Vec2::new(self.x - o.x, self.y - o.y) }
    pub fn add(self, o: Vec2) -> Vec2 { Vec2::new(self.x + o.x, self.y + o.y) }
    pub fn scale(self, k: f32) -> Vec2 { Vec2::new(self.x * k, self.y * k) }
    pub fn length(self) -> f32 { (self.x * self.x + self.y * self.y).sqrt() }
    pub fn normalize(self) -> Vec2 {
        let l = self.length();
        if l < 1e-4 { Vec2::ZERO } else { Vec2::new(self.x / l, self.y / l) }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub enum EntityKind {
    Player,
    Mob,
    Echo,
    Projectile,
    Landmark,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Disposition {
    /// Doesn't aggro first. Becomes Hostile if attacked.
    Neutral,
    /// Aggros anything in range (current behavior). Roguelike.
    Hostile,
    /// Flees when player nearby.
    Skittish,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Biome {
    /// Central warm plains — starting area, mostly neutral wildlife.
    EmbertidePlains,
    /// Cold northern ridges.
    FrostfallRidge,
    /// Dark wooded west.
    HollowForest,
    /// Sickly southern marshes.
    BoneMarshes,
    /// Ruined eastern frontier — hostile mobs concentrate here.
    AshfallRuins,
}

impl Biome {
    /// Deterministic biome selection by world position. Radial+sector layout:
    /// small central Embertide Plains, then four quadrant biomes beyond.
    pub fn at(pos: Vec2) -> Biome {
        let r = (pos.x * pos.x + pos.y * pos.y).sqrt();
        if r < BIOME_RADIUS { return Biome::EmbertidePlains; }
        // Four cardinal quadrants based on atan2. 0 east, π/2 south (y grows down).
        let a = pos.y.atan2(pos.x); // -π..π
        // Bands (each π/2 wide), rotated by π/4 so axis-aligned splits look clean.
        let sector = ((a + core::f32::consts::PI) / (core::f32::consts::PI * 0.5)) as i32;
        match sector.rem_euclid(4) {
            0 => Biome::HollowForest,    // west-ish
            1 => Biome::FrostfallRidge,  // north-ish
            2 => Biome::AshfallRuins,    // east-ish
            _ => Biome::BoneMarshes,     // south-ish
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Biome::EmbertidePlains => "Embertide Plains",
            Biome::FrostfallRidge  => "Frostfall Ridge",
            Biome::HollowForest    => "Hollow Forest",
            Biome::BoneMarshes     => "Bone Marshes",
            Biome::AshfallRuins    => "Ashfall Ruins",
        }
    }
    /// Base tile color (linear RGB). Client shader adds noise variation on top.
    pub fn base_color(&self) -> [f32; 3] {
        match self {
            Biome::EmbertidePlains => [0.30, 0.16, 0.11],
            Biome::FrostfallRidge  => [0.14, 0.20, 0.30],
            Biome::HollowForest    => [0.10, 0.18, 0.12],
            Biome::BoneMarshes     => [0.18, 0.20, 0.10],
            Biome::AshfallRuins    => [0.22, 0.18, 0.22],
        }
    }
    pub fn idx(&self) -> u8 {
        match self {
            Biome::EmbertidePlains => 0,
            Biome::FrostfallRidge  => 1,
            Biome::HollowForest    => 2,
            Biome::BoneMarshes     => 3,
            Biome::AshfallRuins    => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProjectileKind {
    Arrow,
    Bolt,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Class {
    Warrior,
    Archer,
    Magician,
}

impl Class {
    pub fn from_name(name: &str) -> Self {
        let mut h: u32 = 2166136261;
        for b in name.as_bytes() {
            h ^= *b as u32;
            h = h.wrapping_mul(16777619);
        }
        match h % 3 {
            0 => Class::Warrior,
            1 => Class::Archer,
            _ => Class::Magician,
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Class::Warrior => "Warrior",
            Class::Archer => "Archer",
            Class::Magician => "Magician",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EntityView {
    pub id: EntityId,
    pub kind: EntityKind,
    pub pos: Vec2,
    pub facing: f32,
    pub hp: i32,
    pub hp_max: i32,
    pub name: String,
    /// for echoes: how many witnesses, for players: kills, for mobs: ignored
    pub badge: u32,
    /// hue (derived from owner name for players/echoes); 0..360
    pub hue: u16,
    /// 0 normal, >0 attacking-this-tick (visual flash ticks remaining)
    pub flash: u8,
    /// class archetype — only meaningful for Player + Echo
    pub class: Option<Class>,
    /// only meaningful for Projectile
    pub proj_kind: Option<ProjectileKind>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct EchoFrame {
    pub pos: Vec2,
    pub facing: f32,
    pub action: u8, // 0=idle 1=move 2=attack 3=hit
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatLine {
    pub from: String,
    pub text: String,
    pub tick: Tick,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ClientMsg {
    Hello { name: String },
    Move { dir: Vec2 },        // unit vector or zero
    Attack { target: EntityId },
    Witness { target: EntityId },   // begin witnessing an echo
    Exorcise { target: EntityId },  // attack an echo
    Chat { text: String },
    Respawn,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum WorldEvent {
    EchoBorn { id: EntityId, who: String, pos: Vec2, hue: u16 },
    EchoWitnessed { id: EntityId, by: String, total_witnesses: u32 },
    EchoExorcised { id: EntityId, by: String, fragment: String },
    PlayerDied { who: String, killer: String, pos: Vec2 },
    MobSlain { who: String, mob_name: String },
    Reaping { epoch: u32, theme: String },
    /// Visual-only: a target just took `amount` at `pos`. Client renders floating number.
    Damage { target: EntityId, amount: i32, pos: Vec2 },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ServerMsg {
    Welcome {
        you: EntityId,
        epoch: u32,
        epoch_theme: String,
        world_tiles: i32,
        tick: Tick,
    },
    Snapshot {
        tick: Tick,
        entities: Vec<EntityView>,
        chronicle_recent: Vec<String>,
    },
    Chat(ChatLine),
    Event(WorldEvent),
    Killed { by: String },
    Respawned { pos: Vec2, hp: i32 },
}

pub fn encode<T: Serialize>(msg: &T) -> Vec<u8> {
    postcard::to_allocvec(msg).expect("postcard encode")
}

pub fn decode<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, postcard::Error> {
    postcard::from_bytes(bytes)
}

/// Stable hue from a name — used to color a player's echo consistently.
pub fn hue_from_name(name: &str) -> u16 {
    let mut h: u32 = 5381;
    for b in name.as_bytes() {
        h = h.wrapping_mul(33).wrapping_add(*b as u32);
    }
    (h % 360) as u16
}
