# HOLLOWTIDE

> A Diablo-shaped MMO where the world dies every seven days and your final ten seconds become a ghost.

Pure Rust. Pure WASM. WebGL2. WebSocket. `redb`. One HTML file.
No engine. No JS framework. No SQLite.

---

## The hook (the thing that makes this special)

Every other MMO treats death as a setback. Hollowtide treats death as **content creation**.

### Soul Echoes

When you die, the server saves the **last 10 seconds of your existence** — every position, every facing, every swing, every flinch — as a **Soul Echo** anchored to the spot where you fell.

Your echo loops there forever. A translucent ghost of your final moments, replaying.

Other players, walking through the world, see motes of light wherever death has happened. Stepping close, they can:

- **Witness** — stand in the echo for its full loop, gain a sliver of XP and unlock a one-line entry in the shared chronicle ("Here, on day 3 of the Embertide, _Mira_ fell to the Hollow Knight").
- **Exorcise** — destroy the echo for a fragment of the dead player's gear (random one item shard).
- **Commune** — pay a soul-coin to learn one of the dead player's final-moment skills for a single use.

Locations with many deaths become **haunted**. Boss rooms grow forests of looping ghosts. Famous deaths become pilgrimage sites. Newcomers are greeted not by NPCs explaining the world but by **the silent re-enactment of everyone who came before**.

The world remembers.

### The Reaping

Every Sunday at 00:00 UTC, the **Reaping** happens. The world is unmade and reborn with new geography, new themes (Embertide, Frostfall, Bloodmoon, Eldritch...), new bosses, new biomes — but **echoes from the previous epoch** persist as shadow-residue in the new world's terrain. You walk through a frost zone and faintly see the silhouettes of where players died last week in the volcano.

When the world dies, every character dies with it. What carries forward is your **Soulfragment** — 10% of your stats, one chosen item, and your accumulated chronicle entries. Weekly veterans are stronger than weekly newcomers, but only modestly. Every Sunday, every player starts roughly even.

This means:
- **No content treadmill exhaustion** — the world is fresh weekly.
- **No alt-character grind** — your one character is your soul lineage.
- **Real FOMO without subscription pressure** — miss a week, you miss an entire world.
- **Built-in social rituals** — last night before the Reaping is a party. The first night is a rush.

### The Underworld (between reapings)

For 6 hours after the Reaping (Sunday 00:00–06:00 UTC), the **Underworld** opens. All of the past week's echoes congregate in a shared shadow-realm. Living players (those who saved a Soulfragment with the "Hunter" trait) can enter and battle the echoes for their stored loot before they dissolve. PvE only, but high-stakes — a brutal six-hour scramble for the cream of last week's deaths.

---

## v0 status (this commit)

- ✅ Pure-Rust client (WASM), WebGL2 renderer, isometric tile world
- ✅ Single `index.html` host loads compiled wasm
- ✅ Axum server with WebSocket endpoint
- ✅ 20Hz tick loop, server-authoritative state
- ✅ Movement, basic combat, mob spawning + AI
- ✅ **Soul Echoes** — death recording, ghost looping, witness/exorcise interactions
- ✅ `redb` persistence with auto-snapshots
- ✅ Live chat
- ⏳ The Reaping (epoch reset) — scaffold exists, scheduled job pending
- ⏳ The Underworld — designed, not yet built
- ⏳ Soulfragment carry-over — designed, not yet built
- ⏳ World themes / biomes — designed, not yet built
- ⏳ Professions, AH, dungeons, raids — designed, not yet built

---

## Stack

| Layer | Tech |
|---|---|
| Client engine | None. Hand-written. |
| Client language | Rust → WASM via `wasm-bindgen` |
| Rendering | WebGL2 (one shader, batched sprites) |
| Networking | WebSocket, binary frames, `postcard` wire format |
| Server | `axum` + `tokio` |
| Persistence | `redb` (single-file embedded ACID KV) |
| Snapshots | `postcard` blob to `world.snapshot` every 30s |
| Total Cargo deps | ~12 across both binaries |

---

## Run it

Requirements: `rustc` (stable), `wasm-pack`.

```bash
# one-time
rustup target add wasm32-unknown-unknown
cargo install wasm-pack

# build + run
./build.sh
cd server && cargo run --release
```

Open http://localhost:8080 in two browser tabs. WASD to move. Click a mob to attack. `/die` in chat to test the echo system. Walk through a glowing mote to witness.

---

## Layout

```
hollowtide/
├── Cargo.toml          # workspace
├── shared/             # protocol types (used by client + server)
│   ├── Cargo.toml
│   └── src/lib.rs
├── server/             # axum + tokio + redb
│   ├── Cargo.toml
│   └── src/main.rs
├── client/             # wasm-bindgen + web-sys + WebGL2
│   ├── Cargo.toml
│   └── src/lib.rs
├── web/
│   └── index.html      # the entire frontend host
├── build.sh            # compile client → web/pkg, run server
└── README.md
```
