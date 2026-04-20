use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use js_sys::{ArrayBuffer, Uint8Array};
use shared::{Class, ProjectileKind, *};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{
    BinaryType, Document, Element, HtmlCanvasElement, HtmlElement, HtmlInputElement,
    KeyboardEvent, MessageEvent, MouseEvent, WebGl2RenderingContext as GL, WebGlBuffer,
    WebGlProgram, WebGlShader, WebGlUniformLocation, WebGlVertexArrayObject, WebSocket,
};

const TILE_W: f32 = 48.0; // half-width of a diamond tile in screen pixels
const TILE_H: f32 = 24.0; // half-height
const ENTITY_PIXEL_SIZE: f32 = 56.0;
const HP_BAR_W: f32 = 48.0;
const HP_BAR_H: f32 = 5.0;

// ===================== ENTRY =====================

thread_local! {
    static APP: RefCell<Option<App>> = RefCell::new(None);
}

#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    let win = web_sys::window().unwrap();
    let doc = win.document().unwrap();

    let canvas = doc.get_element_by_id("c").ok_or("no #c canvas")?
        .dyn_into::<HtmlCanvasElement>()?;
    let dpr = win.device_pixel_ratio();
    resize_canvas(&win, &canvas, dpr);
    let gl = canvas.get_context("webgl2")?
        .ok_or("WebGL2 not available")?
        .dyn_into::<GL>()?;
    gl.enable(GL::BLEND);
    gl.blend_func(GL::SRC_ALPHA, GL::ONE_MINUS_SRC_ALPHA);

    let program = compile_program(&gl, VS, FS)?;
    let vao = gl.create_vertex_array().ok_or("create vao")?;
    let vbo = gl.create_buffer().ok_or("create vbo")?;
    gl.bind_vertex_array(Some(&vao));
    gl.bind_buffer(GL::ARRAY_BUFFER, Some(&vbo));

    // Layout (11 floats / vertex):
    // a_screen vec2, a_corner vec2, a_color vec4, a_kind float, a_extra vec2
    let stride = (2 + 2 + 4 + 1 + 2) * 4; // bytes
    let mut offset = 0;
    setup_attr(&gl, &program, "a_screen", 2, stride, offset)?; offset += 2 * 4;
    setup_attr(&gl, &program, "a_corner", 2, stride, offset)?; offset += 2 * 4;
    setup_attr(&gl, &program, "a_color",  4, stride, offset)?; offset += 4 * 4;
    setup_attr(&gl, &program, "a_kind",   1, stride, offset)?; offset += 1 * 4;
    setup_attr(&gl, &program, "a_extra",  2, stride, offset)?; let _ = offset;

    let u_resolution = gl.get_uniform_location(&program, "u_resolution").unwrap();
    let u_time = gl.get_uniform_location(&program, "u_time").unwrap();

    // WebSocket
    let loc = win.location();
    let proto = if loc.protocol()? == "https:" { "wss" } else { "ws" };
    let host = loc.host()?;
    let url = format!("{proto}://{host}/ws");
    let ws = WebSocket::new(&url)?;
    ws.set_binary_type(BinaryType::Arraybuffer);

    // HUD panels
    let hud = ensure_hud(&doc)?;

    // Random pretty name
    let name = pick_name();

    let app = App {
        canvas, gl, program, vao, vbo, u_resolution, u_time,
        ws: ws.clone(),
        verts: Vec::with_capacity(4096),
        state: GameState::new(name.clone()),
        hud,
        chat_input_open: false,
    };
    APP.with(|a| *a.borrow_mut() = Some(app));

    // WebSocket open: send Hello
    {
        let name_for_open = name.clone();
        let ws_for_open = ws.clone();
        let onopen = Closure::<dyn FnMut(_)>::new(move |_e: web_sys::Event| {
            let bytes = encode(&ClientMsg::Hello { name: name_for_open.clone() });
            let arr = Uint8Array::from(bytes.as_slice());
            let _ = ws_for_open.send_with_array_buffer(&arr.buffer());
        });
        ws.set_onopen(Some(onopen.as_ref().unchecked_ref()));
        onopen.forget();
    }

    // WebSocket message
    {
        let onmsg = Closure::<dyn FnMut(_)>::new(move |e: MessageEvent| {
            if let Ok(buf) = e.data().dyn_into::<ArrayBuffer>() {
                let u = Uint8Array::new(&buf);
                let mut bytes = vec![0u8; u.length() as usize];
                u.copy_to(&mut bytes);
                APP.with(|a| {
                    if let Some(app) = a.borrow_mut().as_mut() {
                        app.on_msg(&bytes);
                    }
                });
            }
        });
        ws.set_onmessage(Some(onmsg.as_ref().unchecked_ref()));
        onmsg.forget();
    }

    // Keyboard
    {
        let onkey_down = Closure::<dyn FnMut(_)>::new(move |e: KeyboardEvent| {
            APP.with(|a| {
                if let Some(app) = a.borrow_mut().as_mut() {
                    app.on_key(&e, true);
                }
            });
        });
        win.add_event_listener_with_callback("keydown", onkey_down.as_ref().unchecked_ref())?;
        onkey_down.forget();

        let onkey_up = Closure::<dyn FnMut(_)>::new(move |e: KeyboardEvent| {
            APP.with(|a| {
                if let Some(app) = a.borrow_mut().as_mut() {
                    app.on_key(&e, false);
                }
            });
        });
        win.add_event_listener_with_callback("keyup", onkey_up.as_ref().unchecked_ref())?;
        onkey_up.forget();
    }

    // Mouse click
    {
        let canvas_ref = doc.get_element_by_id("c").unwrap()
            .dyn_into::<HtmlCanvasElement>()?;
        let onclick = Closure::<dyn FnMut(_)>::new(move |e: MouseEvent| {
            APP.with(|a| {
                if let Some(app) = a.borrow_mut().as_mut() {
                    app.on_click(&e);
                }
            });
        });
        canvas_ref.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    // Resize
    {
        let onresize = Closure::<dyn FnMut(_)>::new(move |_e: web_sys::Event| {
            APP.with(|a| {
                if let Some(app) = a.borrow_mut().as_mut() {
                    let win = web_sys::window().unwrap();
                    let dpr = win.device_pixel_ratio();
                    resize_canvas(&win, &app.canvas, dpr);
                }
            });
        });
        win.add_event_listener_with_callback("resize", onresize.as_ref().unchecked_ref())?;
        onresize.forget();
    }

    // RAF loop
    let f: Rc<RefCell<Option<Closure<dyn FnMut(f64)>>>> = Rc::new(RefCell::new(None));
    let g = f.clone();
    *g.borrow_mut() = Some(Closure::<dyn FnMut(_)>::new(move |t: f64| {
        APP.with(|a| {
            if let Some(app) = a.borrow_mut().as_mut() {
                app.frame(t);
            }
        });
        let _ = web_sys::window().unwrap()
            .request_animation_frame(f.borrow().as_ref().unwrap().as_ref().unchecked_ref());
    }));
    web_sys::window().unwrap()
        .request_animation_frame(g.borrow().as_ref().unwrap().as_ref().unchecked_ref())?;

    Ok(())
}

fn resize_canvas(win: &web_sys::Window, canvas: &HtmlCanvasElement, dpr: f64) {
    let w = win.inner_width().unwrap().as_f64().unwrap();
    let h = win.inner_height().unwrap().as_f64().unwrap();
    canvas.set_width((w * dpr) as u32);
    canvas.set_height((h * dpr) as u32);
    let style = canvas.style();
    let _ = style.set_property("width", &format!("{w}px"));
    let _ = style.set_property("height", &format!("{h}px"));
}

fn setup_attr(gl: &GL, prog: &WebGlProgram, name: &str, size: i32, stride: i32, offset: i32) -> Result<(), JsValue> {
    let loc = gl.get_attrib_location(prog, name);
    if loc < 0 { return Err(format!("attr {name}").into()); }
    let loc = loc as u32;
    gl.enable_vertex_attrib_array(loc);
    gl.vertex_attrib_pointer_with_i32(loc, size, GL::FLOAT, false, stride, offset);
    Ok(())
}

fn compile_program(gl: &GL, vs: &str, fs: &str) -> Result<WebGlProgram, JsValue> {
    let v = compile_shader(gl, GL::VERTEX_SHADER, vs)?;
    let f = compile_shader(gl, GL::FRAGMENT_SHADER, fs)?;
    let p = gl.create_program().ok_or("create_program")?;
    gl.attach_shader(&p, &v);
    gl.attach_shader(&p, &f);
    gl.link_program(&p);
    if gl.get_program_parameter(&p, GL::LINK_STATUS).as_bool() != Some(true) {
        return Err(gl.get_program_info_log(&p).unwrap_or_default().into());
    }
    gl.use_program(Some(&p));
    Ok(p)
}

fn compile_shader(gl: &GL, kind: u32, src: &str) -> Result<WebGlShader, JsValue> {
    let s = gl.create_shader(kind).ok_or("create_shader")?;
    gl.shader_source(&s, src);
    gl.compile_shader(&s);
    if gl.get_shader_parameter(&s, GL::COMPILE_STATUS).as_bool() != Some(true) {
        return Err(gl.get_shader_info_log(&s).unwrap_or_default().into());
    }
    Ok(s)
}

// ===================== APP =====================

struct App {
    canvas: HtmlCanvasElement,
    gl: GL,
    program: WebGlProgram,
    vao: WebGlVertexArrayObject,
    vbo: WebGlBuffer,
    u_resolution: WebGlUniformLocation,
    u_time: WebGlUniformLocation,
    ws: WebSocket,
    verts: Vec<f32>,
    state: GameState,
    hud: HudHandles,
    chat_input_open: bool,
}

struct HudHandles {
    chat_log: HtmlElement,
    chronicle: HtmlElement,
    status: HtmlElement,
    chat_input: HtmlInputElement,
    name_tag: HtmlElement,
}

struct GameState {
    name: String,
    me: Option<EntityId>,
    epoch: u32,
    epoch_theme: String,
    world_tiles: i32,
    last_tick: Tick,
    entities: HashMap<EntityId, EntityCache>,
    chronicle: Vec<String>,
    chat_log: VecDeque<String>,
    keys: KeysHeld,
    last_send_ms: f64,
    last_dir_sent: Vec2,
    cam: Vec2, // world-space camera target
    dead: bool,
    death_msg: Option<String>,
    flash_events: VecDeque<(String, f64)>,
    self_hp: i32,
    self_hp_max: i32,
}

#[derive(Default)]
struct KeysHeld {
    w: bool, a: bool, s: bool, d: bool,
}

struct EntityCache {
    view: EntityView,
    prev_pos: Vec2,
    target_pos: Vec2,
    last_update_ms: f64,
}

impl GameState {
    fn new(name: String) -> Self {
        Self {
            name, me: None, epoch: 0, epoch_theme: String::new(),
            world_tiles: WORLD_TILES, last_tick: 0,
            entities: HashMap::new(),
            chronicle: Vec::new(),
            chat_log: VecDeque::with_capacity(64),
            keys: KeysHeld::default(),
            last_send_ms: 0.0, last_dir_sent: Vec2::ZERO,
            cam: Vec2::ZERO, dead: false, death_msg: None,
            flash_events: VecDeque::with_capacity(8),
            self_hp: 0, self_hp_max: PLAYER_BASE_HP,
        }
    }
}

impl App {
    fn on_msg(&mut self, bytes: &[u8]) {
        let now = performance_now();
        let Ok(msg) = decode::<ServerMsg>(bytes) else { return };
        match msg {
            ServerMsg::Welcome { you, epoch, epoch_theme, world_tiles, tick } => {
                self.state.me = Some(you);
                self.state.epoch = epoch;
                self.state.epoch_theme = epoch_theme.clone();
                self.state.world_tiles = world_tiles;
                self.state.last_tick = tick;
                self.state.dead = false;
                self.state.death_msg = None;
                self.flash(format!("Welcome to {epoch_theme}, epoch {epoch}."));
            }
            ServerMsg::Snapshot { tick, entities, chronicle_recent } => {
                self.state.last_tick = tick;
                let mut seen: std::collections::HashSet<EntityId> = std::collections::HashSet::new();
                for v in entities {
                    seen.insert(v.id);
                    if let Some(c) = self.state.entities.get_mut(&v.id) {
                        c.prev_pos = c.target_pos;
                        c.target_pos = v.pos;
                        c.last_update_ms = now;
                        c.view = v;
                    } else {
                        self.state.entities.insert(v.id, EntityCache {
                            prev_pos: v.pos, target_pos: v.pos,
                            last_update_ms: now, view: v,
                        });
                    }
                }
                self.state.entities.retain(|id, _| seen.contains(id));
                self.state.chronicle = chronicle_recent;
                if let Some(me) = self.state.me {
                    if let Some(self_e) = self.state.entities.get(&me) {
                        self.state.self_hp = self_e.view.hp;
                        self.state.self_hp_max = self_e.view.hp_max;
                        self.state.cam = self.state.cam.lerp(self_e.view.pos, 0.18);
                        self.state.dead = self_e.view.hp <= 0;
                    }
                }
            }
            ServerMsg::Chat(line) => {
                self.state.chat_log.push_back(format!("{}: {}", line.from, line.text));
                if self.state.chat_log.len() > 64 { self.state.chat_log.pop_front(); }
            }
            ServerMsg::Event(ev) => match ev {
                WorldEvent::EchoBorn { who, .. } => self.flash(format!("✦ {who} fell. An echo remains.")),
                WorldEvent::EchoWitnessed { by, total_witnesses, .. } =>
                    self.flash(format!("◇ {by} witnessed an echo (now {total_witnesses}).")),
                WorldEvent::EchoExorcised { by, fragment, .. } =>
                    self.flash(format!("✕ {by} exorcised an echo: {fragment}.")),
                WorldEvent::PlayerDied { who, .. } => self.flash(format!("† {who} has died.")),
                WorldEvent::MobSlain { who, mob_name } =>
                    self.flash(format!("✦ {who} slew a {mob_name}.")),
                WorldEvent::Reaping { epoch, theme } =>
                    self.flash(format!("✶✶✶ Reaping. Epoch {epoch}: {theme} ✶✶✶")),
                WorldEvent::Damage { target, amount, pos } => {
                    self.spawn_damage_label(amount, target, pos);
                }
            },
            ServerMsg::Killed { by } => {
                self.state.dead = true;
                self.state.death_msg = Some(format!("You were slain by {by}. Press R to respawn."));
            }
            ServerMsg::Respawned { hp, .. } => {
                self.state.dead = false;
                self.state.death_msg = None;
                self.state.self_hp = hp;
                self.flash("You return to the world.".into());
            }
        }
        self.update_hud();
    }

    fn flash(&mut self, s: String) {
        let now = performance_now();
        self.state.flash_events.push_back((s, now + 4000.0));
        while self.state.flash_events.len() > 6 {
            self.state.flash_events.pop_front();
        }
    }

    fn on_key(&mut self, e: &KeyboardEvent, down: bool) {
        let key = e.key();
        if self.chat_input_open {
            if down && key == "Enter" {
                let val = self.hud.chat_input.value();
                if !val.is_empty() {
                    self.send(&ClientMsg::Chat { text: val });
                    self.hud.chat_input.set_value("");
                }
                self.toggle_chat(false);
                e.prevent_default();
            } else if down && key == "Escape" {
                self.toggle_chat(false);
                e.prevent_default();
            }
            return;
        }
        match key.as_str() {
            "w" | "W" | "ArrowUp" => { self.state.keys.w = down; e.prevent_default(); }
            "a" | "A" | "ArrowLeft" => { self.state.keys.a = down; e.prevent_default(); }
            "s" | "S" | "ArrowDown" => { self.state.keys.s = down; e.prevent_default(); }
            "d" | "D" | "ArrowRight" => { self.state.keys.d = down; e.prevent_default(); }
            "e" | "E" if down => { self.try_witness_nearest(); e.prevent_default(); }
            "r" | "R" if down && self.state.dead => {
                self.send(&ClientMsg::Respawn);
                e.prevent_default();
            }
            "Enter" | "t" | "T" | "/" if down => {
                self.toggle_chat(true);
                e.prevent_default();
            }
            _ => {}
        }
    }

    fn toggle_chat(&mut self, on: bool) {
        self.chat_input_open = on;
        let style = self.hud.chat_input.style();
        let _ = style.set_property("display", if on { "block" } else { "none" });
        if on { let _ = self.hud.chat_input.focus(); }
    }

    fn spawn_damage_label(&self, amount: i32, target: EntityId, world_pos: Vec2) {
        let Some(win) = web_sys::window() else { return };
        let Some(doc) = win.document() else { return };
        let Some(body) = doc.body() else { return };

        let dpr = win.device_pixel_ratio() as f32;
        let w = self.canvas.width() as f32;
        let h = self.canvas.height() as f32;

        // Prefer the target's current interpolated position (may have moved since event emitted)
        let pos = self.state.entities.get(&target).map(|c| c.view.pos).unwrap_or(world_pos);
        let (sx, sy) = world_to_screen(pos, w, h, self.state.cam, dpr);
        let css_x = sx / dpr;
        let css_y = (sy / dpr) - 40.0;

        let Ok(el) = doc.create_element("div") else { return };
        el.set_class_name("dmg-label");
        el.set_text_content(Some(&amount.to_string()));
        if let Ok(html_el) = el.clone().dyn_into::<HtmlElement>() {
            let style = html_el.style();
            let _ = style.set_property("left", &format!("{css_x}px"));
            let _ = style.set_property("top", &format!("{css_y}px"));
        }
        let _ = el.set_attribute("onanimationend", "this.remove()");
        let _ = body.append_child(&el);
    }

    fn on_click(&mut self, e: &MouseEvent) {
        if self.state.dead { return; }
        let win = web_sys::window().unwrap();
        let dpr = win.device_pixel_ratio() as f32;
        let rect = self.canvas.get_bounding_client_rect();
        let cx = (e.client_x() as f64 - rect.left()) as f32;
        let cy = (e.client_y() as f64 - rect.top()) as f32;
        let ww = self.canvas.width() as f32 / dpr;
        let hh = self.canvas.height() as f32 / dpr;
        let world = screen_to_world(cx, cy, ww, hh, self.state.cam);

        // Find nearest entity under cursor (within an interactive radius)
        let mut best: Option<(EntityId, EntityKind, f32)> = None;
        for c in self.state.entities.values() {
            if Some(c.view.id) == self.state.me { continue; }
            let d = c.view.pos.dist(world);
            if d < 1.2 {
                if best.map_or(true, |(_, _, bd)| d < bd) {
                    best = Some((c.view.id, c.view.kind, d));
                }
            }
        }
        let Some((id, kind, _)) = best else { return };
        // Let the server enforce range/cooldown — ranged classes need it that way.
        match kind {
            EntityKind::Mob | EntityKind::Player => self.send(&ClientMsg::Attack { target: id }),
            EntityKind::Echo => self.send(&ClientMsg::Exorcise { target: id }),
            EntityKind::Projectile => {}
        }
    }

    fn try_witness_nearest(&mut self) {
        let me_pos = self.state.me
            .and_then(|m| self.state.entities.get(&m))
            .map(|c| c.view.pos);
        let Some(me_pos) = me_pos else { return };
        let mut best: Option<(EntityId, f32)> = None;
        for c in self.state.entities.values() {
            if c.view.kind != EntityKind::Echo { continue; }
            let d = c.view.pos.dist(me_pos);
            if d < WITNESS_RANGE * 2.0 && best.map_or(true, |(_, bd)| d < bd) {
                best = Some((c.view.id, d));
            }
        }
        if let Some((id, _)) = best {
            self.send(&ClientMsg::Witness { target: id });
            self.flash("Witnessing… stay still.".into());
        }
    }

    fn send(&self, msg: &ClientMsg) {
        if self.ws.ready_state() != WebSocket::OPEN { return; }
        let bytes = encode(msg);
        let arr = Uint8Array::from(bytes.as_slice());
        let _ = self.ws.send_with_array_buffer(&arr.buffer());
    }

    fn frame(&mut self, time_ms: f64) {
        // Send movement at most ~10Hz
        let dx = (self.state.keys.d as i32 - self.state.keys.a as i32) as f32;
        let dy = (self.state.keys.s as i32 - self.state.keys.w as i32) as f32;
        let dir = if !self.state.dead { Vec2::new(dx, dy) } else { Vec2::ZERO };
        let changed = (dir.x - self.state.last_dir_sent.x).abs() > 0.01
            || (dir.y - self.state.last_dir_sent.y).abs() > 0.01;
        if changed && time_ms - self.state.last_send_ms > 80.0 {
            self.send(&ClientMsg::Move { dir });
            self.state.last_dir_sent = dir;
            self.state.last_send_ms = time_ms;
        }

        // Drop expired flash events
        while let Some((_, exp)) = self.state.flash_events.front() {
            if *exp < time_ms { self.state.flash_events.pop_front(); } else { break; }
        }
        self.update_hud();
        self.render(time_ms);
    }

    fn render(&mut self, time_ms: f64) {
        let gl = &self.gl;
        let w = self.canvas.width() as f32;
        let h = self.canvas.height() as f32;
        gl.viewport(0, 0, w as i32, h as i32);

        // Background tinted by epoch theme (just a color for now)
        let (r, g, b) = epoch_bg(&self.state.epoch_theme);
        gl.clear_color(r, g, b, 1.0);
        gl.clear(GL::COLOR_BUFFER_BIT);

        gl.use_program(Some(&self.program));
        gl.uniform2f(Some(&self.u_resolution), w, h);
        gl.uniform1f(Some(&self.u_time), (time_ms / 1000.0) as f32);

        let dpr = web_sys::window().unwrap().device_pixel_ratio() as f32;

        // Build vertex buffer
        self.verts.clear();

        // Tile diamond outlines drawn as faint filled quads per tile, sparse grid
        let half_tiles = self.state.world_tiles / 2;
        for ty in (-half_tiles..half_tiles).step_by(2) {
            for tx in (-half_tiles..half_tiles).step_by(2) {
                let world = Vec2::new(tx as f32, ty as f32);
                let (sx, sy) = world_to_screen(world, w, h, self.state.cam, dpr);
                push_quad(&mut self.verts, sx, sy,
                    [0.10, 0.09, 0.13, 1.0],
                    TILE_W * dpr * 1.95, 6.0, [0.0, 0.0]);
            }
        }

        // Sort entities by world Y for depth
        let mut ents: Vec<&EntityCache> = self.state.entities.values().collect();
        ents.sort_by(|a, b| a.view.pos.y.partial_cmp(&b.view.pos.y).unwrap_or(std::cmp::Ordering::Equal));

        for c in &ents {
            let (sx, sy) = world_to_screen(c.view.pos, w, h, self.state.cam, dpr);
            let color = hue_to_rgb(c.view.hue, c.view.kind, c.view.flash);
            let kind_id = match (c.view.kind, c.view.class, c.view.proj_kind) {
                (EntityKind::Player, Some(Class::Warrior), _)  => 10.0,
                (EntityKind::Player, Some(Class::Archer), _)   => 11.0,
                (EntityKind::Player, Some(Class::Magician), _) => 12.0,
                (EntityKind::Player, None, _) => 0.0,
                (EntityKind::Mob, _, _) => 1.0,
                (EntityKind::Echo, Some(Class::Warrior), _)  => 22.0,
                (EntityKind::Echo, Some(Class::Archer), _)   => 23.0,
                (EntityKind::Echo, Some(Class::Magician), _) => 24.0,
                (EntityKind::Echo, None, _) => 2.0,
                (EntityKind::Projectile, _, Some(ProjectileKind::Arrow)) => 30.0,
                (EntityKind::Projectile, _, Some(ProjectileKind::Bolt))  => 31.0,
                (EntityKind::Projectile, _, None) => 30.0,
            };
            let extra = match c.view.kind {
                EntityKind::Echo => [(c.view.hue as f32) * 0.1, 0.0],
                EntityKind::Player | EntityKind::Projectile => [c.view.facing, 0.0],
                _ => [0.0, 0.0],
            };
            let size = match c.view.kind {
                EntityKind::Projectile => ENTITY_PIXEL_SIZE * dpr * 0.55,
                _ => ENTITY_PIXEL_SIZE * dpr,
            };
            push_quad(&mut self.verts, sx, sy, color, size, kind_id, extra);

            // HP bar above (only for players + mobs)
            if matches!(c.view.kind, EntityKind::Player | EntityKind::Mob) && c.view.hp > 0 {
                let ratio = (c.view.hp as f32 / c.view.hp_max.max(1) as f32).clamp(0.0, 1.0);
                let bar_color = match c.view.kind {
                    EntityKind::Player => [0.5, 1.0, 0.6, 1.0],
                    EntityKind::Mob => [1.0, 0.4, 0.4, 1.0],
                    _ => [0.8, 0.8, 0.8, 1.0],
                };
                push_quad_bar(&mut self.verts, sx, sy - size * 0.55, bar_color,
                    HP_BAR_W * dpr, HP_BAR_H * dpr, ratio);
            }
        }

        // Witness ring around current witness target — if we are witnessing
        // (We don't get explicit witness state; instead show a hint ring on every nearby echo.)
        if let Some(me_id) = self.state.me {
            if let Some(me) = self.state.entities.get(&me_id) {
                for c in self.state.entities.values() {
                    if c.view.kind == EntityKind::Echo
                        && me.view.pos.dist(c.view.pos) <= WITNESS_RANGE * 2.0
                    {
                        let (sx, sy) = world_to_screen(c.view.pos, w, h, self.state.cam, dpr);
                        let color = hue_to_rgb(c.view.hue, EntityKind::Echo, 0);
                        push_quad(&mut self.verts, sx, sy, color,
                            ENTITY_PIXEL_SIZE * dpr * 1.6, 5.0, [0.5, 0.0]);
                    }
                }
            }
        }

        // Upload + draw
        gl.bind_vertex_array(Some(&self.vao));
        gl.bind_buffer(GL::ARRAY_BUFFER, Some(&self.vbo));
        unsafe {
            let view = js_sys::Float32Array::view(&self.verts);
            gl.buffer_data_with_array_buffer_view(GL::ARRAY_BUFFER, &view, GL::DYNAMIC_DRAW);
        }
        let stride_floats = 2 + 2 + 4 + 1 + 2;
        let count = (self.verts.len() / stride_floats) as i32;
        gl.draw_arrays(GL::TRIANGLES, 0, count);
    }

    fn update_hud(&self) {
        // Status
        let hp_pct = if self.state.self_hp_max > 0 {
            (self.state.self_hp.max(0) as f32 / self.state.self_hp_max as f32 * 100.0) as i32
        } else { 0 };
        let class_label = Class::from_name(&self.state.name).label();
        let st = if self.state.dead {
            self.state.death_msg.clone().unwrap_or_else(|| "You are dead.".into())
        } else {
            format!("{class_label} · HP {}/{} ({hp_pct}%) · tick {} · epoch {} ({})",
                self.state.self_hp.max(0), self.state.self_hp_max,
                self.state.last_tick, self.state.epoch, self.state.epoch_theme)
        };
        self.hud.status.set_inner_text(&st);

        // Chronicle (recent world events + flashes)
        let mut chron = String::new();
        for (s, _) in self.state.flash_events.iter() {
            chron.push_str(s); chron.push('\n');
        }
        chron.push_str("— chronicle —\n");
        for s in &self.state.chronicle {
            chron.push_str(s); chron.push('\n');
        }
        self.hud.chronicle.set_inner_text(&chron);

        // Chat
        let mut chat = String::new();
        for s in self.state.chat_log.iter() {
            chat.push_str(s); chat.push('\n');
        }
        self.hud.chat_log.set_inner_text(&chat);

        // Name tag
        self.hud.name_tag.set_inner_text(&format!("you: {}", self.state.name));
    }
}

fn epoch_bg(theme: &str) -> (f32, f32, f32) {
    match theme {
        "Embertide" => (0.10, 0.04, 0.05),
        "Frostfall" => (0.04, 0.05, 0.10),
        "Bloodmoon" => (0.10, 0.03, 0.03),
        "Eldritch"  => (0.04, 0.06, 0.05),
        _ => (0.06, 0.06, 0.08),
    }
}

fn hue_to_rgb(hue: u16, kind: EntityKind, flash: u8) -> [f32; 4] {
    let h = (hue as f32) / 360.0;
    let (r, g, b) = hsv_to_rgb(h, match kind {
        EntityKind::Player => 0.7,
        EntityKind::Mob => 0.7,
        EntityKind::Echo => 0.55,
        EntityKind::Projectile => 0.8,
    }, match kind {
        EntityKind::Mob => 0.85,
        _ => 1.0,
    });
    let (r, g, b) = match kind {
        EntityKind::Mob => (0.85, 0.25 + r * 0.1, 0.25 + g * 0.1),
        _ => (r, g, b),
    };
    let boost = if flash > 0 { 0.6 } else { 0.0 };
    [
        (r + boost).min(1.0),
        (g + boost).min(1.0),
        (b + boost).min(1.0),
        1.0,
    ]
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    let i = (h * 6.0).floor() as i32;
    let f = h * 6.0 - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    match i.rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    }
}

fn world_to_screen(p: Vec2, w: f32, h: f32, cam: Vec2, dpr: f32) -> (f32, f32) {
    let dx = p.x - cam.x;
    let dy = p.y - cam.y;
    let sx = (dx - dy) * TILE_W * dpr + w * 0.5;
    let sy = (dx + dy) * TILE_H * dpr + h * 0.5;
    (sx, sy)
}

fn screen_to_world(sx: f32, sy: f32, w: f32, h: f32, cam: Vec2) -> Vec2 {
    // inverse of world_to_screen, ignoring dpr (canvas client-coords already match css px)
    let cx = sx - w * 0.5;
    let cy = sy - h * 0.5;
    let dx_minus_dy = cx / TILE_W;
    let dx_plus_dy = cy / TILE_H;
    let dx = (dx_minus_dy + dx_plus_dy) * 0.5;
    let dy = (dx_plus_dy - dx_minus_dy) * 0.5;
    Vec2::new(cam.x + dx, cam.y + dy)
}

fn push_quad(out: &mut Vec<f32>, sx: f32, sy: f32, col: [f32; 4], size: f32, kind: f32, extra: [f32; 2]) {
    let half = size * 0.5;
    let corners = [(-half, -half, -0.5, -0.5),
                   ( half, -half,  0.5, -0.5),
                   (-half,  half, -0.5,  0.5),
                   ( half, -half,  0.5, -0.5),
                   ( half,  half,  0.5,  0.5),
                   (-half,  half, -0.5,  0.5)];
    for (ox, oy, cu, cv) in corners {
        out.extend_from_slice(&[sx + ox, sy + oy, cu, cv,
            col[0], col[1], col[2], col[3], kind, extra[0], extra[1]]);
    }
}

fn push_quad_bar(out: &mut Vec<f32>, sx: f32, sy: f32, col: [f32; 4], w: f32, h: f32, ratio: f32) {
    let hw = w * 0.5;
    let hh = h * 0.5;
    let corners = [(-hw, -hh, -0.5, -0.5),
                   ( hw, -hh,  0.5, -0.5),
                   (-hw,  hh, -0.5,  0.5),
                   ( hw, -hh,  0.5, -0.5),
                   ( hw,  hh,  0.5,  0.5),
                   (-hw,  hh, -0.5,  0.5)];
    for (ox, oy, cu, cv) in corners {
        out.extend_from_slice(&[sx + ox, sy + oy, cu, cv,
            col[0], col[1], col[2], col[3], 4.0, ratio, 0.0]);
    }
}

fn pick_name() -> String {
    let names = [
        "Mira", "Kestrel", "Ashen", "Vesper", "Lior", "Briar",
        "Onyx", "Wren", "Rune", "Sable", "Cinder", "Ivo",
        "Marrow", "Pale", "Eira", "Hollow", "Tess", "Quill",
    ];
    let n = (performance_now() as u64).wrapping_mul(2654435761) as usize;
    names[n % names.len()].to_string()
}

fn performance_now() -> f64 {
    web_sys::window().unwrap().performance().map(|p| p.now()).unwrap_or(0.0)
}

// ===================== HUD (DOM overlay) =====================

fn ensure_hud(doc: &Document) -> Result<HudHandles, JsValue> {
    let body_el: Element = doc.body().unwrap().dyn_into::<Element>()?;
    let make = |id: &str, parent: &Element| -> Result<HtmlElement, JsValue> {
        let el = match doc.get_element_by_id(id) {
            Some(e) => e,
            None => {
                let e = doc.create_element("div")?;
                e.set_id(id);
                parent.append_child(&e)?;
                e
            }
        };
        Ok(el.dyn_into::<HtmlElement>()?)
    };
    let overlay = make("hud", &body_el)?;
    let overlay_el: Element = overlay.clone().dyn_into::<Element>()?;
    let status  = make("hud-status", &overlay_el)?;
    let chronicle = make("hud-chronicle", &overlay_el)?;
    let chat_log  = make("hud-chat", &overlay_el)?;
    let name_tag  = make("hud-name", &overlay_el)?;

    // Chat input
    let chat_input = match doc.get_element_by_id("hud-chat-input") {
        Some(e) => e.dyn_into::<HtmlInputElement>()?,
        None => {
            let e = doc.create_element("input")?;
            e.set_id("hud-chat-input");
            e.set_attribute("type", "text")?;
            e.set_attribute("autocomplete", "off")?;
            overlay_el.append_child(&e)?;
            e.dyn_into::<HtmlInputElement>()?
        }
    };
    let _ = chat_input.style().set_property("display", "none");
    let _ = overlay;
    Ok(HudHandles { chat_log, chronicle, status, chat_input, name_tag })
}

// ===================== SHADERS =====================

const VS: &str = r#"#version 300 es
in vec2 a_screen;
in vec2 a_corner;
in vec4 a_color;
in float a_size;
in float a_kind;
in vec2 a_extra;

uniform vec2 u_resolution;

out vec2 v_uv;
out vec4 v_color;
flat out float v_kind;
out vec2 v_extra;

void main() {
    vec2 ndc = (a_screen / u_resolution) * 2.0 - 1.0;
    ndc.y = -ndc.y;
    gl_Position = vec4(ndc, 0.0, 1.0);
    v_uv = a_corner;
    v_color = a_color;
    v_kind = a_kind;
    v_extra = a_extra;
}
"#;

const FS: &str = r#"#version 300 es
precision highp float;

in vec2 v_uv;
in vec4 v_color;
flat in float v_kind;
in vec2 v_extra;

uniform float u_time;

out vec4 outColor;

// Rotate a 2D vector by angle (radians).
vec2 rot2(vec2 p, float a) {
    float c = cos(a); float s = sin(a);
    return vec2(c * p.x - s * p.y, s * p.x + c * p.y);
}

void main() {
    float d = length(v_uv);
    int kind = int(v_kind);

    if (kind == 0) {
        // Player (fallback, no class): filled circle with darker rim, glow
        if (d > 0.5) discard;
        float rim = smoothstep(0.42, 0.5, d);
        vec3 c = mix(v_color.rgb, v_color.rgb * 0.35, rim);
        c += 0.07 * (1.0 - d * 2.0);
        outColor = vec4(c, 1.0);
    } else if (kind == 10) {
        // WARRIOR: hexagonal body, thick dark rim, shoulder highlight
        // hex is max of |x|/cos(30) and (|x|*sin(30) + |y|)
        vec2 q = abs(v_uv);
        float hex = max(q.x * 1.1547, q.x * 0.5774 + q.y);
        if (hex > 0.44) discard;
        float rim = smoothstep(0.36, 0.44, hex);
        vec3 base = v_color.rgb * 0.85 + vec3(0.12, 0.06, 0.04);
        vec3 c = mix(base, base * 0.28, rim);
        // top "helm" band
        float helm = smoothstep(0.18, 0.30, -v_uv.y) * (1.0 - rim);
        c = mix(c, c * 1.35 + vec3(0.08), helm * 0.55);
        // belt line
        float belt = 1.0 - smoothstep(0.0, 0.035, abs(v_uv.y - 0.08));
        c = mix(c, c * 0.5, belt * 0.8 * (1.0 - rim));
        outColor = vec4(c, 1.0);
    } else if (kind == 11) {
        // ARCHER: lean diamond body with a chevron on top
        float dd = abs(v_uv.x) * 1.55 + abs(v_uv.y);
        if (dd > 0.56) discard;
        float rim = smoothstep(0.48, 0.56, dd);
        vec3 base = v_color.rgb * 0.70 + vec3(0.02, 0.10, 0.04);
        vec3 c = mix(base, base * 0.30, rim);
        // chevron
        float chev = smoothstep(0.10, 0.03, abs(v_uv.x) + v_uv.y + 0.22) * step(v_uv.y, -0.05);
        c = mix(c, c * 1.45 + vec3(0.05, 0.10, 0.05), chev * 0.8);
        // quiver dot
        float quiver = 1.0 - smoothstep(0.0, 0.05, distance(v_uv, vec2(0.22, 0.08)));
        c = mix(c, vec3(0.92, 0.78, 0.42), quiver * (1.0 - rim) * 0.9);
        outColor = vec4(c, 1.0);
    } else if (kind == 12) {
        // MAGICIAN: circle core + three orbiting runes
        float body = step(d, 0.30);
        float body_rim = smoothstep(0.22, 0.30, d) * body;
        float mote = 0.0;
        for (int i = 0; i < 3; i++) {
            float a = u_time * 1.6 + float(i) * 2.0944;
            vec2 p = vec2(cos(a), sin(a)) * 0.44;
            mote = max(mote, 1.0 - smoothstep(0.0, 0.055, distance(v_uv, p)));
        }
        float alpha = body + mote;
        if (alpha < 0.02) discard;
        vec3 base = v_color.rgb * 0.55 + vec3(0.08, 0.05, 0.18);
        vec3 c = mix(base, base * 0.30, body_rim);
        // inner sigil
        float sigil = 1.0 - smoothstep(0.0, 0.04, abs(d - 0.14));
        c = mix(c, c * 1.6 + vec3(0.2, 0.15, 0.35), sigil * body * 0.7);
        // motes glow
        c = mix(c, vec3(0.95, 0.82, 1.0), mote * 0.95);
        outColor = vec4(c, 1.0);
    } else if (kind == 1) {
        // Mob: diamond
        float dd = abs(v_uv.x) + abs(v_uv.y);
        if (dd > 0.45) discard;
        float edge = smoothstep(0.40, 0.45, dd);
        vec3 c = mix(v_color.rgb, v_color.rgb * 0.4, edge);
        outColor = vec4(c, 1.0);
    } else if (kind == 2) {
        // Echo (unclassed fallback): pulsing translucent ghost
        if (d > 0.5) discard;
        float pulse = 0.6 + 0.4 * sin(u_time * 1.6 + v_extra.x);
        float wisp = sin(v_uv.x * 18.0 + u_time * 2.7) * sin(v_uv.y * 18.0 - u_time * 2.0);
        float core = 1.0 - smoothstep(0.0, 0.5, d);
        float a = core * pulse * (0.45 + 0.45 * (0.5 + 0.5 * wisp));
        vec3 c = v_color.rgb + vec3(0.18, 0.20, 0.30);
        outColor = vec4(c, a);
    } else if (kind == 22) {
        // Warrior echo: warrior hex silhouette, translucent, pulse
        vec2 q = abs(v_uv);
        float hex = max(q.x * 1.1547, q.x * 0.5774 + q.y);
        if (hex > 0.44) discard;
        float pulse = 0.55 + 0.45 * sin(u_time * 1.4 + v_extra.x);
        float core = 1.0 - smoothstep(0.20, 0.44, hex);
        vec3 c = v_color.rgb + vec3(0.16, 0.12, 0.22);
        outColor = vec4(c, core * pulse * 0.65);
    } else if (kind == 23) {
        // Archer echo
        float dd = abs(v_uv.x) * 1.55 + abs(v_uv.y);
        if (dd > 0.56) discard;
        float pulse = 0.55 + 0.45 * sin(u_time * 1.4 + v_extra.x);
        float core = 1.0 - smoothstep(0.20, 0.56, dd);
        vec3 c = v_color.rgb + vec3(0.10, 0.18, 0.18);
        outColor = vec4(c, core * pulse * 0.65);
    } else if (kind == 24) {
        // Magician echo: ghostly circle + slow orbit motes
        float d2 = length(v_uv);
        float body = step(d2, 0.30);
        float mote = 0.0;
        for (int i = 0; i < 3; i++) {
            float a = u_time * 1.2 + float(i) * 2.0944;
            vec2 p = vec2(cos(a), sin(a)) * 0.44;
            mote = max(mote, 1.0 - smoothstep(0.0, 0.06, distance(v_uv, p)));
        }
        float alpha = body + mote;
        if (alpha < 0.02) discard;
        float pulse = 0.60 + 0.40 * sin(u_time * 1.4 + v_extra.x);
        vec3 c = v_color.rgb + vec3(0.14, 0.08, 0.24);
        c = mix(c, vec3(0.95, 0.85, 1.0), mote * 0.85);
        outColor = vec4(c, alpha * pulse * 0.7);
    } else if (kind == 30) {
        // Arrow: thin oval along facing, warm color
        float angle = v_extra.x;
        vec2 rp = rot2(v_uv, -angle);
        float oval = (rp.x * rp.x) / 0.22 + (rp.y * rp.y) / 0.020;
        if (oval > 1.0) discard;
        float tip = smoothstep(0.25, 0.45, rp.x);
        vec3 shaft = vec3(0.82, 0.68, 0.38);
        vec3 tipc  = vec3(1.0, 0.92, 0.70);
        outColor = vec4(mix(shaft, tipc, tip), 1.0);
    } else if (kind == 31) {
        // Bolt: glowing orb with aura
        float d2 = length(v_uv);
        float core = smoothstep(0.25, 0.0, d2);
        float glow = smoothstep(0.48, 0.0, d2) * 0.6;
        float alpha = core + glow;
        if (alpha < 0.02) discard;
        float pulse = 0.85 + 0.15 * sin(u_time * 8.0);
        vec3 c = mix(vec3(0.55, 0.35, 1.0), vec3(1.0, 0.9, 1.0), core);
        outColor = vec4(c * pulse, alpha);
    } else if (kind == 3) {
        // Tile faint quad
        if (d > 0.5) discard;
        outColor = vec4(v_color.rgb, 0.55);
    } else if (kind == 4) {
        // HP bar background and fill
        float ratio = v_extra.x;
        float u = v_uv.x + 0.5;
        if (u < ratio) outColor = vec4(v_color.rgb, 1.0);
        else outColor = vec4(0.05, 0.05, 0.06, 1.0);
    } else if (kind == 5) {
        // Witness hint ring around an echo
        float ring = abs(d - 0.45);
        if (ring > 0.04) discard;
        float pulse = 0.5 + 0.5 * sin(u_time * 4.0);
        outColor = vec4(v_color.rgb + vec3(0.2), 0.35 * pulse);
    } else {
        outColor = v_color;
    }
}
"#;
