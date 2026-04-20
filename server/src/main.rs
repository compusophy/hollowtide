use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use futures_util::{stream::StreamExt, SinkExt};
use rand::{Rng, SeedableRng};
use redb::{Database, TableDefinition};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use tokio::sync::{mpsc, Mutex};
use tower_http::services::ServeDir;

use shared::*;

// ---- Persistence ----
const META_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");
const WORLD_KEY: &str = "world_snapshot_v1";

// ---- Tunables ----
const SNAPSHOT_INTERVAL_SECS: u64 = 30;
const MOB_TARGET_COUNT: usize = 24;
const MOB_AGGRO_RANGE: f32 = 6.0;
const MOB_ATTACK_RANGE: f32 = 1.4;
const MOB_ATTACK_DAMAGE: i32 = 8;
const MOB_ATTACK_COOLDOWN: u32 = 18;
const MOB_MOVE_SPEED: f32 = 2.4;
const ECHO_HP_MAX: i32 = 10;
const CHRONICLE_KEEP: usize = 32;
const FRAGMENTS: &[&str] = &[
    "Tarnished Buckle", "Shard of Blackglass", "Rusted Sigil",
    "Brittle Vertebra", "Knot of Smoke", "Wax Tooth",
    "Half-Coin", "Threadbare Pennon", "Salt-Carved Eye",
    "Hollow Reed", "Pale Knucklebone", "Ember Lens",
];
const MOB_NAMES: &[&str] = &[
    "Hollow Knight", "Ash Wretch", "Pale Stalker", "Cinder Imp",
    "Bone Mire", "Veil-Walker", "Smoke Hound",
];
const PLAYER_RESPAWN_HP_FRAC: f32 = 0.7;

// ===================== STATE =====================

#[derive(Clone, Debug)]
struct EchoFrameRing {
    buf: VecDeque<EchoFrame>,
}
impl EchoFrameRing {
    fn new() -> Self { Self { buf: VecDeque::with_capacity(ECHO_FRAME_COUNT) } }
    fn push(&mut self, f: EchoFrame) {
        if self.buf.len() == ECHO_FRAME_COUNT { self.buf.pop_front(); }
        self.buf.push_back(f);
    }
    fn snapshot(&self) -> Vec<EchoFrame> { self.buf.iter().copied().collect() }
}

struct Player {
    id: EntityId,
    name: String,
    class: Class,
    pos: Vec2,
    facing: f32,
    hp: i32,
    hp_max: i32,
    move_dir: Vec2,
    last_attack_tick: Tick,
    kills: u32,
    history: EchoFrameRing,
    witnessing: Option<(EntityId, u32)>,
    witnessed_already: HashSet<EntityId>,
    tx: mpsc::UnboundedSender<Vec<u8>>,
    alive: bool,
    flash: u8,
    last_action: u8,
}

struct Mob {
    id: EntityId,
    name: String,
    pos: Vec2,
    facing: f32,
    hp: i32,
    hp_max: i32,
    target: Option<EntityId>,
    last_attack_tick: Tick,
    wander_to: Vec2,
    flash: u8,
}

#[derive(Clone)]
struct Echo {
    id: EntityId,
    of_player: String,
    class: Class,
    anchor: Vec2,
    hue: u16,
    frames: Vec<EchoFrame>,
    frame_idx: usize,
    last_frame_tick: Tick,
    witnesses: u32,
    hp: i32,
    born_tick: Tick,
}

#[derive(Serialize, Deserialize)]
struct EchoSnap {
    id: EntityId,
    of_player: String,
    class: Class,
    anchor: Vec2,
    hue: u16,
    frames: Vec<EchoFrame>,
    witnesses: u32,
    hp: i32,
    born_tick: Tick,
}

#[derive(Serialize, Deserialize)]
struct WorldSnap {
    tick: Tick,
    epoch: u32,
    epoch_theme: String,
    next_id: EntityId,
    chronicle: Vec<String>,
    echoes: Vec<EchoSnap>,
}

struct Projectile {
    id: EntityId,
    kind: ProjectileKind,
    owner: EntityId,
    pos: Vec2,
    vel: Vec2,
    damage: i32,
    ttl: u32,
    hue: u16,
}

struct World {
    tick: Tick,
    epoch: u32,
    epoch_theme: String,
    next_id: EntityId,
    players: HashMap<EntityId, Player>,
    mobs: HashMap<EntityId, Mob>,
    echoes: HashMap<EntityId, Echo>,
    projectiles: HashMap<EntityId, Projectile>,
    chronicle: VecDeque<String>,
    pending_events: Vec<WorldEvent>,
    pending_personal: Vec<(EntityId, ServerMsg)>,
    rng: rand::rngs::StdRng,
}

impl World {
    fn new() -> Self {
        let mut w = Self {
            tick: 0,
            epoch: 1,
            epoch_theme: "Embertide".to_string(),
            next_id: 1,
            players: HashMap::new(),
            mobs: HashMap::new(),
            echoes: HashMap::new(),
            projectiles: HashMap::new(),
            chronicle: VecDeque::with_capacity(CHRONICLE_KEEP),
            pending_events: Vec::new(),
            pending_personal: Vec::new(),
            rng: rand::rngs::StdRng::seed_from_u64(0xC0FFEE_DEAD_BEEF),
        };
        for _ in 0..MOB_TARGET_COUNT { w.spawn_mob(); }
        w
    }

    fn alloc_id(&mut self) -> EntityId { let i = self.next_id; self.next_id += 1; i }

    fn spawn_mob(&mut self) {
        let id = self.alloc_id();
        let pos = self.random_open_tile();
        let name = MOB_NAMES[self.rng.gen_range(0..MOB_NAMES.len())].to_string();
        self.mobs.insert(id, Mob {
            id, name, pos, facing: 0.0,
            hp: MOB_BASE_HP, hp_max: MOB_BASE_HP,
            target: None, last_attack_tick: 0,
            wander_to: pos, flash: 0,
        });
    }

    fn random_open_tile(&mut self) -> Vec2 {
        let half = (WORLD_TILES as f32) / 2.0 - 2.0;
        Vec2::new(
            self.rng.gen_range(-half..half),
            self.rng.gen_range(-half..half),
        )
    }

    fn add_player(&mut self, name: String, tx: mpsc::UnboundedSender<Vec<u8>>) -> EntityId {
        let id = self.alloc_id();
        let pos = self.random_open_tile();
        let class = Class::from_name(&name);
        self.players.insert(id, Player {
            id, name, class, pos, facing: 0.0,
            hp: PLAYER_BASE_HP, hp_max: PLAYER_BASE_HP,
            move_dir: Vec2::ZERO, last_attack_tick: 0,
            kills: 0, history: EchoFrameRing::new(),
            witnessing: None, witnessed_already: HashSet::new(),
            tx, alive: true, flash: 0, last_action: 0,
        });
        id
    }

    fn remove_player(&mut self, id: EntityId) { self.players.remove(&id); }

    fn handle_command(&mut self, id: EntityId, cmd: ClientMsg) {
        let alive = match self.players.get(&id) {
            Some(p) => p.alive,
            None => return,
        };

        if !alive {
            match cmd {
                ClientMsg::Respawn => {
                    let pos = self.random_open_tile();
                    let hp_after = match self.players.get_mut(&id) {
                        Some(p) => {
                            p.pos = pos;
                            p.hp = (p.hp_max as f32 * PLAYER_RESPAWN_HP_FRAC) as i32;
                            p.alive = true;
                            p.move_dir = Vec2::ZERO;
                            p.history = EchoFrameRing::new();
                            p.witnessing = None;
                            p.hp
                        }
                        None => return,
                    };
                    self.pending_personal.push((id, ServerMsg::Respawned { pos, hp: hp_after }));
                }
                ClientMsg::Chat { text } => {
                    let from = self.players.get(&id).map(|p| p.name.clone()).unwrap_or_default();
                    let line = ChatLine { from, text, tick: self.tick };
                    self.broadcast_chat(line);
                }
                _ => {}
            }
            return;
        }

        match cmd {
            ClientMsg::Hello { .. } | ClientMsg::Respawn => {}
            ClientMsg::Move { dir } => {
                if let Some(p) = self.players.get_mut(&id) {
                    let len = (dir.x * dir.x + dir.y * dir.y).sqrt();
                    p.move_dir = if len > 0.01 {
                        Vec2::new(dir.x / len, dir.y / len)
                    } else { Vec2::ZERO };
                    if len > 0.01 { p.facing = dir.y.atan2(dir.x); }
                    p.witnessing = None;
                }
            }
            ClientMsg::Attack { target } => self.try_attack(id, target),
            ClientMsg::Witness { target } => self.try_witness(id, target),
            ClientMsg::Exorcise { target } => self.try_exorcise(id, target),
            ClientMsg::Chat { text } => {
                if text.starts_with("/die") {
                    if let Some(p) = self.players.get_mut(&id) {
                        p.hp = 0;
                    }
                } else {
                    let from = self.players.get(&id).map(|p| p.name.clone()).unwrap_or_default();
                    let line = ChatLine { from, text, tick: self.tick };
                    self.broadcast_chat(line);
                }
            }
        }
    }

    fn broadcast_chat(&mut self, line: ChatLine) {
        let bytes = encode(&ServerMsg::Chat(line));
        for p in self.players.values() {
            let _ = p.tx.send(bytes.clone());
        }
    }

    fn try_attack(&mut self, attacker: EntityId, target: EntityId) {
        let (pp, pname, last_atk, class, hue) = match self.players.get(&attacker) {
            Some(p) => (p.pos, p.name.clone(), p.last_attack_tick, p.class, hue_from_name(&p.name)),
            None => return,
        };
        if self.tick.saturating_sub(last_atk) < ATTACK_COOLDOWN_TICKS as u64 { return; }

        // Target direction — where the player clicked. We'll use either the target entity's
        // position (if found) or the player's facing as a fallback.
        let target_pos = self.mobs.get(&target).map(|m| m.pos)
            .or_else(|| self.players.get(&target).map(|p| p.pos))
            .or_else(|| self.echoes.get(&target).map(|e| e.anchor));
        let Some(target_pos) = target_pos else { return };

        // Commit the cooldown/action for the attacker regardless of class.
        if let Some(p) = self.players.get_mut(&attacker) {
            p.last_attack_tick = self.tick;
            p.last_action = 2;
        }

        match class {
            Class::Warrior => self.warrior_cleave(attacker, pp, target_pos, &pname),
            Class::Archer => {
                let dir = target_pos.sub(pp).normalize();
                self.spawn_projectile(attacker, ProjectileKind::Arrow, pp, dir, hue);
            }
            Class::Magician => {
                let dir = target_pos.sub(pp).normalize();
                self.spawn_projectile(attacker, ProjectileKind::Bolt, pp, dir, hue);
            }
        }
    }

    fn warrior_cleave(&mut self, attacker: EntityId, pp: Vec2, target_pos: Vec2, pname: &str) {
        let forward = target_pos.sub(pp).normalize();
        if forward.x == 0.0 && forward.y == 0.0 { return; }

        // Collect targets in arc first to avoid borrow chaos.
        let cos_half = (WARRIOR_CLEAVE_ARC_RAD * 0.5).cos();
        let mut mob_hits: Vec<EntityId> = Vec::new();
        for m in self.mobs.values() {
            let to = m.pos.sub(pp);
            let d = to.length();
            if d > WARRIOR_CLEAVE_RANGE || d < 0.01 { continue; }
            let n = Vec2::new(to.x / d, to.y / d);
            if n.x * forward.x + n.y * forward.y >= cos_half {
                mob_hits.push(m.id);
            }
        }
        let mut pvp_hits: Vec<EntityId> = Vec::new();
        for (id, p) in &self.players {
            if *id == attacker || !p.alive { continue; }
            let to = p.pos.sub(pp);
            let d = to.length();
            if d > WARRIOR_CLEAVE_RANGE || d < 0.01 { continue; }
            let n = Vec2::new(to.x / d, to.y / d);
            if n.x * forward.x + n.y * forward.y >= cos_half {
                pvp_hits.push(*id);
            }
        }

        let _ = attacker;
        for id in &mob_hits {
            let (dead, pos, mob_name) = if let Some(m) = self.mobs.get_mut(id) {
                m.hp -= WARRIOR_CLEAVE_DAMAGE;
                m.flash = 3;
                (m.hp <= 0, m.pos, m.name.clone())
            } else { continue };
            self.pending_events.push(WorldEvent::Damage {
                target: *id, amount: WARRIOR_CLEAVE_DAMAGE, pos,
            });
            if dead {
                self.mobs.remove(id);
                if let Some(a) = self.players.get_mut(&attacker) { a.kills += 1; }
                self.pending_events.push(WorldEvent::MobSlain {
                    who: pname.to_string(), mob_name,
                });
            }
        }
        for id in &pvp_hits {
            let pos = if let Some(p) = self.players.get_mut(id) {
                p.hp -= WARRIOR_CLEAVE_DAMAGE;
                p.flash = 3;
                p.pos
            } else { continue };
            self.pending_events.push(WorldEvent::Damage {
                target: *id, amount: WARRIOR_CLEAVE_DAMAGE, pos,
            });
        }
    }

    fn spawn_projectile(&mut self, owner: EntityId, kind: ProjectileKind, pos: Vec2, dir: Vec2, hue: u16) {
        if dir.x == 0.0 && dir.y == 0.0 { return; }
        let (speed, damage, ttl) = match kind {
            ProjectileKind::Arrow => (ARCHER_ARROW_SPEED, ARCHER_ARROW_DAMAGE, ARCHER_ARROW_TTL_TICKS),
            ProjectileKind::Bolt  => (MAGICIAN_BOLT_SPEED, MAGICIAN_BOLT_DAMAGE, MAGICIAN_BOLT_TTL_TICKS),
        };
        let id = self.alloc_id();
        self.projectiles.insert(id, Projectile {
            id, kind, owner,
            pos,
            vel: Vec2::new(dir.x * speed, dir.y * speed),
            damage, ttl, hue,
        });
    }

    fn step_projectiles(&mut self, dt: f32) {
        let mut remove: Vec<EntityId> = Vec::new();
        struct Hit { target: EntityId, target_kind: EntityKind, damage: i32, pos: Vec2, owner: EntityId }
        let mut hits: Vec<Hit> = Vec::new();
        let half = (WORLD_TILES as f32) / 2.0;

        for (pid, proj) in self.projectiles.iter_mut() {
            proj.pos.x += proj.vel.x * dt;
            proj.pos.y += proj.vel.y * dt;
            if proj.ttl == 0 { remove.push(*pid); continue; }
            proj.ttl -= 1;
            if proj.pos.x.abs() > half || proj.pos.y.abs() > half {
                remove.push(*pid); continue;
            }
            // mob collision
            let mut hit: Option<(EntityId, EntityKind, Vec2)> = None;
            for m in self.mobs.values() {
                if m.pos.dist(proj.pos) <= PROJECTILE_HIT_RADIUS {
                    hit = Some((m.id, EntityKind::Mob, m.pos));
                    break;
                }
            }
            if hit.is_none() {
                for (pl_id, pl) in &self.players {
                    if *pl_id == proj.owner || !pl.alive { continue; }
                    if pl.pos.dist(proj.pos) <= PROJECTILE_HIT_RADIUS {
                        hit = Some((*pl_id, EntityKind::Player, pl.pos));
                        break;
                    }
                }
            }
            if let Some((tid, kind, tpos)) = hit {
                hits.push(Hit { target: tid, target_kind: kind, damage: proj.damage, pos: tpos, owner: proj.owner });
                remove.push(*pid);
            }
        }

        for pid in remove { self.projectiles.remove(&pid); }

        for h in hits {
            let owner_name = self.players.get(&h.owner).map(|p| p.name.clone()).unwrap_or_default();
            match h.target_kind {
                EntityKind::Mob => {
                    let (dead, mob_name) = if let Some(m) = self.mobs.get_mut(&h.target) {
                        m.hp -= h.damage;
                        m.flash = 3;
                        (m.hp <= 0, m.name.clone())
                    } else { continue };
                    self.pending_events.push(WorldEvent::Damage {
                        target: h.target, amount: h.damage, pos: h.pos,
                    });
                    if dead {
                        self.mobs.remove(&h.target);
                        if let Some(a) = self.players.get_mut(&h.owner) { a.kills += 1; }
                        self.pending_events.push(WorldEvent::MobSlain {
                            who: owner_name, mob_name,
                        });
                    }
                }
                EntityKind::Player => {
                    if let Some(p) = self.players.get_mut(&h.target) {
                        p.hp -= h.damage;
                        p.flash = 3;
                    }
                    self.pending_events.push(WorldEvent::Damage {
                        target: h.target, amount: h.damage, pos: h.pos,
                    });
                }
                _ => {}
            }
        }
    }

    fn try_witness(&mut self, id: EntityId, target: EntityId) {
        let (pp, already) = match self.players.get(&id) {
            Some(p) => (p.pos, p.witnessed_already.contains(&target)),
            None => return,
        };
        if already { return; }
        let anchor = match self.echoes.get(&target) {
            Some(e) => e.anchor,
            None => return,
        };
        if pp.dist(anchor) <= WITNESS_RANGE * 2.0 {
            if let Some(p) = self.players.get_mut(&id) {
                p.witnessing = Some((target, 0));
            }
        }
    }

    fn try_exorcise(&mut self, attacker: EntityId, target: EntityId) {
        let (pp, pname, last_atk) = match self.players.get(&attacker) {
            Some(p) => (p.pos, p.name.clone(), p.last_attack_tick),
            None => return,
        };
        if self.tick.saturating_sub(last_atk) < ATTACK_COOLDOWN_TICKS as u64 { return; }

        let echo_dead = if let Some(echo) = self.echoes.get_mut(&target) {
            if pp.dist(echo.anchor) > ATTACK_RANGE * 1.4 { return; }
            echo.hp -= EXORCISE_DAMAGE;
            if echo.hp <= 0 { Some(echo.of_player.clone()) } else { None }
        } else { return };

        if let Some(p) = self.players.get_mut(&attacker) {
            p.last_attack_tick = self.tick;
            p.last_action = 2;
        }

        if let Some(echo_owner) = echo_dead {
            self.echoes.remove(&target);
            let frag_idx = (self.tick as usize ^ target as usize) % FRAGMENTS.len();
            let frag = FRAGMENTS[frag_idx].to_string();
            let cur_tick = self.tick;
            self.pending_events.push(WorldEvent::EchoExorcised {
                id: target, by: pname.clone(), fragment: frag.clone(),
            });
            self.push_chronicle(format!(
                "Tick {}: {} exorcised {}'s echo and gained a {}.",
                cur_tick, pname, echo_owner, frag
            ));
        }
    }

    fn step(&mut self) {
        self.tick += 1;
        let dt = 1.0 / TICK_HZ as f32;

        // -- mob AI --
        let player_positions: Vec<(EntityId, Vec2, bool)> = self.players.iter()
            .map(|(id, p)| (*id, p.pos, p.alive)).collect();

        let half = (WORLD_TILES as f32) / 2.0 - 2.0;
        let mut wander_rolls: Vec<(EntityId, Vec2)> = Vec::new();
        // collect wander targets first to avoid borrowing rng inside iteration
        for m in self.mobs.values() {
            if m.target.is_none() && m.pos.dist(m.wander_to) < 0.5 {
                wander_rolls.push((m.id, Vec2::new(
                    self.rng.gen_range(-6.0..6.0_f32),
                    self.rng.gen_range(-6.0..6.0_f32),
                )));
            }
        }

        for m in self.mobs.values_mut() {
            if m.flash > 0 { m.flash -= 1; }
            // pick or refresh target
            let mut nearest: Option<(EntityId, Vec2, f32)> = None;
            for (pid, pp, alive) in &player_positions {
                if !*alive { continue; }
                let d = m.pos.dist(*pp);
                if d <= MOB_AGGRO_RANGE && nearest.map_or(true, |(_,_,nd)| d < nd) {
                    nearest = Some((*pid, *pp, d));
                }
            }
            m.target = nearest.map(|t| t.0);
            if let Some((_, target_pos, dist)) = nearest {
                if dist > MOB_ATTACK_RANGE {
                    let dx = target_pos.x - m.pos.x;
                    let dy = target_pos.y - m.pos.y;
                    let l = (dx*dx + dy*dy).sqrt().max(1e-4);
                    m.pos.x += dx / l * MOB_MOVE_SPEED * dt;
                    m.pos.y += dy / l * MOB_MOVE_SPEED * dt;
                    m.facing = dy.atan2(dx);
                } else if self.tick.saturating_sub(m.last_attack_tick) >= MOB_ATTACK_COOLDOWN as u64 {
                    m.last_attack_tick = self.tick;
                }
            } else {
                if let Some((_, off)) = wander_rolls.iter().find(|(id, _)| *id == m.id) {
                    let nx = (m.pos.x + off.x).clamp(-half, half);
                    let ny = (m.pos.y + off.y).clamp(-half, half);
                    m.wander_to = Vec2::new(nx, ny);
                }
                let dx = m.wander_to.x - m.pos.x;
                let dy = m.wander_to.y - m.pos.y;
                let l = (dx*dx + dy*dy).sqrt().max(1e-4);
                m.pos.x += dx / l * (MOB_MOVE_SPEED * 0.4) * dt;
                m.pos.y += dy / l * (MOB_MOVE_SPEED * 0.4) * dt;
            }
        }

        // -- mob attacks resolve --
        let mob_attacks: Vec<(EntityId, EntityId)> = self.mobs.values()
            .filter(|m| m.target.is_some() && m.last_attack_tick == self.tick)
            .filter_map(|m| m.target.map(|t| (m.id, t))).collect();
        for (mob_id, target_id) in mob_attacks {
            let in_range = match (self.mobs.get(&mob_id), self.players.get(&target_id)) {
                (Some(m), Some(p)) => m.pos.dist(p.pos) <= MOB_ATTACK_RANGE && p.alive,
                _ => false,
            };
            if in_range {
                let pos = if let Some(p) = self.players.get_mut(&target_id) {
                    p.hp -= MOB_ATTACK_DAMAGE;
                    p.flash = 3;
                    p.pos
                } else { continue };
                self.pending_events.push(WorldEvent::Damage {
                    target: target_id, amount: MOB_ATTACK_DAMAGE, pos,
                });
            }
        }

        // -- projectile simulation --
        self.step_projectiles(dt);

        // -- player movement + echo recording --
        let half_p = (WORLD_TILES as f32) / 2.0 - 1.0;
        let frame_step = TICK_HZ as u64 / ECHO_FRAME_HZ as u64;
        let record_now = self.tick % frame_step == 0;
        for p in self.players.values_mut() {
            if p.flash > 0 { p.flash -= 1; }
            if !p.alive { continue; }
            p.pos.x = (p.pos.x + p.move_dir.x * PLAYER_MOVE_SPEED * dt).clamp(-half_p, half_p);
            p.pos.y = (p.pos.y + p.move_dir.y * PLAYER_MOVE_SPEED * dt).clamp(-half_p, half_p);
            if record_now {
                let action = if p.last_action != 0 { p.last_action }
                             else if p.move_dir.x.abs() + p.move_dir.y.abs() > 0.01 { 1 } else { 0 };
                p.history.push(EchoFrame { pos: p.pos, facing: p.facing, action });
                p.last_action = 0;
            }
        }

        // -- handle deaths --
        let dead_players: Vec<EntityId> = self.players.iter()
            .filter(|(_, p)| p.alive && p.hp <= 0)
            .map(|(id, _)| *id).collect();
        for pid in dead_players {
            self.kill_player(pid);
        }

        // -- echo playback animation --
        for echo in self.echoes.values_mut() {
            if !echo.frames.is_empty()
                && self.tick.saturating_sub(echo.last_frame_tick) >= frame_step
            {
                echo.frame_idx = (echo.frame_idx + 1) % echo.frames.len();
                echo.last_frame_tick = self.tick;
            }
        }

        // -- witness progress --
        let echo_anchors: HashMap<EntityId, Vec2> = self.echoes.iter()
            .map(|(id, e)| (*id, e.anchor)).collect();
        let mut completions: Vec<(EntityId, EntityId)> = Vec::new();
        for p in self.players.values_mut() {
            if let Some((eid, prog)) = p.witnessing {
                let still = echo_anchors.get(&eid)
                    .map_or(false, |a| p.pos.dist(*a) <= WITNESS_RANGE * 2.0);
                if !still {
                    p.witnessing = None;
                } else {
                    let new_prog = prog + 1;
                    if new_prog >= WITNESS_TICKS_REQUIRED {
                        p.witnessed_already.insert(eid);
                        p.witnessing = None;
                        completions.push((p.id, eid));
                    } else {
                        p.witnessing = Some((eid, new_prog));
                    }
                }
            }
        }
        for (pid, eid) in completions {
            let pname = self.players.get(&pid).map(|p| p.name.clone()).unwrap_or_default();
            let (echo_owner, total_after) = if let Some(e) = self.echoes.get_mut(&eid) {
                e.witnesses += 1;
                (e.of_player.clone(), e.witnesses)
            } else { continue };
            self.pending_events.push(WorldEvent::EchoWitnessed {
                id: eid, by: pname.clone(), total_witnesses: total_after,
            });
            let cur = self.tick;
            self.push_chronicle(format!(
                "Tick {}: {} witnessed {}'s end. (witnesses: {})",
                cur, pname, echo_owner, total_after
            ));
        }

        // -- mob respawning --
        while self.mobs.len() < MOB_TARGET_COUNT {
            self.spawn_mob();
        }
    }

    fn kill_player(&mut self, pid: EntityId) {
        let (name, class, pos, hue, frames) = match self.players.get(&pid) {
            Some(p) => {
                let frames = p.history.snapshot();
                let frames = if frames.is_empty() {
                    vec![EchoFrame { pos: p.pos, facing: p.facing, action: 3 }]
                } else { frames };
                (p.name.clone(), p.class, p.pos, hue_from_name(&p.name), frames)
            }
            None => return,
        };
        if let Some(p) = self.players.get_mut(&pid) {
            p.alive = false;
            p.hp = 0;
            p.move_dir = Vec2::ZERO;
        }
        let eid = self.alloc_id();
        let cur = self.tick;
        self.echoes.insert(eid, Echo {
            id: eid, of_player: name.clone(), class, anchor: pos, hue,
            frames, frame_idx: 0, last_frame_tick: cur,
            witnesses: 0, hp: ECHO_HP_MAX, born_tick: cur,
        });
        let killer = "the world".to_string();
        self.pending_events.push(WorldEvent::EchoBorn { id: eid, who: name.clone(), pos, hue });
        self.pending_events.push(WorldEvent::PlayerDied { who: name.clone(), killer: killer.clone(), pos });
        self.push_chronicle(format!("Tick {}: {} fell.", cur, name));
        self.pending_personal.push((pid, ServerMsg::Killed { by: killer }));
    }

    fn push_chronicle(&mut self, s: String) {
        if self.chronicle.len() >= CHRONICLE_KEEP { self.chronicle.pop_front(); }
        self.chronicle.push_back(s);
    }

    fn snapshot_msg(&self) -> ServerMsg {
        let mut entities: Vec<EntityView> = Vec::with_capacity(
            self.players.len() + self.mobs.len() + self.echoes.len()
        );
        for p in self.players.values() {
            entities.push(EntityView {
                id: p.id, kind: EntityKind::Player,
                pos: p.pos, facing: p.facing,
                hp: p.hp, hp_max: p.hp_max,
                name: p.name.clone(),
                badge: p.kills,
                hue: hue_from_name(&p.name),
                flash: p.flash,
                class: Some(p.class),
                proj_kind: None,
            });
        }
        for m in self.mobs.values() {
            entities.push(EntityView {
                id: m.id, kind: EntityKind::Mob,
                pos: m.pos, facing: m.facing,
                hp: m.hp, hp_max: m.hp_max,
                name: m.name.clone(),
                badge: 0, hue: 0, flash: m.flash,
                class: None,
                proj_kind: None,
            });
        }
        for e in self.echoes.values() {
            let (pos, facing, action) = if e.frames.is_empty() {
                (e.anchor, 0.0, 0)
            } else {
                let f = e.frames[e.frame_idx];
                (f.pos, f.facing, f.action)
            };
            entities.push(EntityView {
                id: e.id, kind: EntityKind::Echo,
                pos, facing,
                hp: e.hp, hp_max: ECHO_HP_MAX,
                name: e.of_player.clone(),
                badge: e.witnesses,
                hue: e.hue,
                flash: action,
                class: Some(e.class),
                proj_kind: None,
            });
        }
        for p in self.projectiles.values() {
            // Represent projectile heading as facing for the shader's trail direction.
            let facing = p.vel.y.atan2(p.vel.x);
            entities.push(EntityView {
                id: p.id, kind: EntityKind::Projectile,
                pos: p.pos, facing,
                hp: 1, hp_max: 1,
                name: String::new(),
                badge: 0,
                hue: p.hue,
                flash: 0,
                class: None,
                proj_kind: Some(p.kind),
            });
        }
        let chronicle_recent: Vec<String> = self.chronicle.iter().rev().take(6).cloned().collect();
        ServerMsg::Snapshot { tick: self.tick, entities, chronicle_recent }
    }

    fn to_snap(&self) -> WorldSnap {
        WorldSnap {
            tick: self.tick,
            epoch: self.epoch,
            epoch_theme: self.epoch_theme.clone(),
            next_id: self.next_id,
            chronicle: self.chronicle.iter().cloned().collect(),
            echoes: self.echoes.values().map(|e| EchoSnap {
                id: e.id, of_player: e.of_player.clone(), class: e.class,
                anchor: e.anchor, hue: e.hue,
                frames: e.frames.clone(),
                witnesses: e.witnesses, hp: e.hp, born_tick: e.born_tick,
            }).collect(),
        }
    }

    fn from_snap(&mut self, s: WorldSnap) {
        self.tick = s.tick;
        self.epoch = s.epoch;
        self.epoch_theme = s.epoch_theme;
        self.next_id = s.next_id.max(self.next_id);
        self.chronicle = s.chronicle.into_iter().collect();
        self.echoes.clear();
        for es in s.echoes {
            self.echoes.insert(es.id, Echo {
                id: es.id, of_player: es.of_player, class: es.class,
                anchor: es.anchor, hue: es.hue,
                frames: es.frames, frame_idx: 0, last_frame_tick: self.tick,
                witnesses: es.witnesses, hp: es.hp, born_tick: es.born_tick,
            });
        }
    }
}

// ===================== APP =====================

#[derive(Clone)]
struct AppState {
    world: Arc<Mutex<World>>,
    db: Arc<Database>,
}

#[tokio::main]
async fn main() {
    let db_path = PathBuf::from(
        std::env::var("HOLLOW_DB_PATH").unwrap_or_else(|_| "hollowtide.redb".to_string())
    );
    if let Some(parent) = db_path.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    eprintln!("[hollowtide] db: {}", db_path.display());
    let db = Database::create(&db_path).expect("open redb");
    let db = Arc::new(db);

    let mut world = World::new();
    if let Ok(read) = db.begin_read() {
        if let Ok(table) = read.open_table(META_TABLE) {
            if let Ok(Some(value)) = table.get(WORLD_KEY) {
                if let Ok(snap) = postcard::from_bytes::<WorldSnap>(value.value()) {
                    eprintln!("[hollowtide] restoring world: tick={} epoch={} echoes={}",
                        snap.tick, snap.epoch, snap.echoes.len());
                    world.from_snap(snap);
                }
            }
        }
    }

    let state = AppState { world: Arc::new(Mutex::new(world)), db: db.clone() };

    {
        let s = state.clone();
        tokio::spawn(game_loop(s));
    }
    {
        let s = state.clone();
        tokio::spawn(snapshot_loop(s));
    }

    let static_dir = std::env::var("HOLLOW_WEB_DIR").unwrap_or_else(|_| "../web".to_string());
    let app = Router::new()
        .route("/ws", get(ws_handler))
        .fallback_service(ServeDir::new(static_dir).append_index_html_on_directories(true))
        .with_state(state);

    let port: u16 = std::env::var("PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(8080);
    let addr: SocketAddr = ([0, 0, 0, 0], port).into();
    eprintln!("[hollowtide] listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn game_loop(state: AppState) {
    let mut interval = tokio::time::interval(Duration::from_millis(1000 / TICK_HZ as u64));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        let (snapshot_bytes, events, personal, txs) = {
            let mut w = state.world.lock().await;
            w.step();
            let snap = w.snapshot_msg();
            let bytes = encode(&snap);
            let events: Vec<WorldEvent> = std::mem::take(&mut w.pending_events);
            let personal: Vec<(EntityId, ServerMsg)> = std::mem::take(&mut w.pending_personal);
            let txs: Vec<(EntityId, mpsc::UnboundedSender<Vec<u8>>)> =
                w.players.iter().map(|(id, p)| (*id, p.tx.clone())).collect();
            (bytes, events, personal, txs)
        };
        for (_, tx) in &txs {
            let _ = tx.send(snapshot_bytes.clone());
        }
        for ev in events {
            let bytes = encode(&ServerMsg::Event(ev));
            for (_, tx) in &txs {
                let _ = tx.send(bytes.clone());
            }
        }
        for (id, msg) in personal {
            let bytes = encode(&msg);
            if let Some((_, tx)) = txs.iter().find(|(i, _)| *i == id) {
                let _ = tx.send(bytes);
            }
        }
    }
}

async fn snapshot_loop(state: AppState) {
    let mut interval = tokio::time::interval(Duration::from_secs(SNAPSHOT_INTERVAL_SECS));
    interval.tick().await;
    loop {
        interval.tick().await;
        let snap = {
            let w = state.world.lock().await;
            w.to_snap()
        };
        let bytes = postcard::to_allocvec(&snap).unwrap();
        let db = state.db.clone();
        tokio::task::spawn_blocking(move || {
            let txn = db.begin_write().expect("begin_write");
            {
                let mut t = txn.open_table(META_TABLE).expect("open_table");
                t.insert(WORLD_KEY, bytes.as_slice()).expect("insert");
            }
            txn.commit().expect("commit");
        });
    }
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();

    let send_task = tokio::spawn(async move {
        while let Some(bytes) = rx.recv().await {
            if sender.send(Message::Binary(bytes)).await.is_err() { break; }
        }
    });

    let name = loop {
        match receiver.next().await {
            Some(Ok(Message::Binary(data))) => {
                if let Ok(ClientMsg::Hello { name }) = decode::<ClientMsg>(&data) {
                    break name;
                }
            }
            Some(Ok(_)) => continue,
            _ => { send_task.abort(); return; }
        }
    };

    let (player_id, welcome) = {
        let mut w = state.world.lock().await;
        let pid = w.add_player(name.clone(), tx.clone());
        let wmsg = ServerMsg::Welcome {
            you: pid,
            epoch: w.epoch,
            epoch_theme: w.epoch_theme.clone(),
            world_tiles: WORLD_TILES,
            tick: w.tick,
        };
        (pid, wmsg)
    };
    let _ = tx.send(encode(&welcome));

    while let Some(msg) = receiver.next().await {
        match msg {
            Ok(Message::Binary(data)) => {
                if let Ok(cmd) = decode::<ClientMsg>(&data) {
                    let mut w = state.world.lock().await;
                    w.handle_command(player_id, cmd);
                }
            }
            Ok(Message::Close(_)) | Err(_) => break,
            _ => continue,
        }
    }

    {
        let mut w = state.world.lock().await;
        w.remove_player(player_id);
    }
    send_task.abort();
}
