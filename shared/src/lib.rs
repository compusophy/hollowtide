#![no_std]
extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

pub const TICK_HZ: u32 = 20;
pub const ECHO_FRAME_HZ: u32 = 10;
pub const ECHO_DURATION_SECS: u32 = 10;
pub const ECHO_FRAME_COUNT: usize = (ECHO_FRAME_HZ * ECHO_DURATION_SECS) as usize;
pub const WORLD_TILES: i32 = 64;
pub const PLAYER_BASE_HP: i32 = 100;
pub const MOB_BASE_HP: i32 = 40;
pub const PLAYER_MOVE_SPEED: f32 = 4.0; // tiles per second
pub const ATTACK_RANGE: f32 = 1.5;
pub const ATTACK_COOLDOWN_TICKS: u32 = 8; // ~0.4s
pub const ATTACK_DAMAGE: i32 = 12;
pub const WITNESS_RANGE: f32 = 0.8;
pub const WITNESS_TICKS_REQUIRED: u32 = 200; // 10s
pub const EXORCISE_DAMAGE: i32 = 1;

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
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub enum EntityKind {
    Player,
    Mob,
    Echo,
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
    /// for echoes: hue (derived from owner name); 0..360
    pub hue: u16,
    /// 0 normal, 1 attacking-this-tick (visual flash)
    pub flash: u8,
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
