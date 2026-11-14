//! The island window: state, message loop, input, docking, timers.
use std::sync::atomic::{AtomicIsize, AtomicPtr, Ordering};
use std::time::Instant;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::{self, Config, Pos};
use crate::gfx::Gfx;
use crate::model::{AgentEvent, Alert, Store, Usage};
use crate::ui::{self, Action, Edge, Hit, Setting, Theme, CANVAS_H, CANVAS_W, PAD, PANEL_W, PILL_H, PILL_W};
use crate::{logln, tray, winutil};

pub const WM_TRAY: u32 = WM_APP + 1;
pub const WM_UIMSG: u32 = WM_APP + 2;
pub const WM_FOREGROUND: u32 = WM_APP + 3;
const WM_FIRSTRUN: u32 = WM_APP + 4;
const WM_MOUSELEAVE: u32 = 0x02A3;

const T_PULSE: usize = 1;
const T_SECOND: usize = 2;
const T_HOVER_OPEN: usize = 3;
const T_HOVER_CLOSE: usize = 4;
const T_AUTOCLOSE: usize = 5;
const T_BENCH: usize = 6;
const T_USAGE: usize = 7;
const T_RC: usize = 8;
const T_EXPIRE: usize = 9;
const T_FSCHECK: usize = 10;
const T_TRIM: usize = 11;
const T_COALESCE: usize = 12;
const T_HOVERWATCH: usize = 13;

/// How close (DIPs) the notch's centre must come to a screen edge to attach to it.
const SNAP_DIST: f32 = 56.0;
const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
const WM_HOTKEY: u32 = 0x0312;

static APP: AtomicPtr<App> = AtomicPtr::new(std::ptr::null_mut());
static HWND_MAIN: AtomicIsize = AtomicIsize::new(0);

/// Messages posted to the UI thread from worker threads.
pub enum UiMsg {
    Event(AgentEvent),
    Events(Vec<AgentEvent>),
    Usage(Usage),
}

/// Thread-safe: hand a message to the UI thread.
pub fn post(msg: UiMsg) {
    let h = HWND_MAIN.load(Ordering::Relaxed);
    if h == 0 {
        return;
    }
    let p = Box::into_raw(Box::new(msg));
    unsafe {
        if PostMessageW(Some(HWND(h as _)), WM_UIMSG, WPARAM(0), LPARAM(p as isize)).is_err() {
            drop(Box::from_raw(p));
        }
    }
}

fn app() -> Option<&'static mut App> {
    let p = APP.load(Ordering::Relaxed);
    if p.is_null() { None } else { Some(unsafe { &mut *p }) }
}

#[derive(Clone, Copy, Default)]
pub struct Spring {
    pub x: f32,
    pub v: f32,
    pub target: f32,
}
impl Spring {
    pub fn at(v: f32) -> Spring {
        Spring { x: v, v: 0.0, target: v }
    }
    pub fn jump(&mut self, v: f32) {
        *self = Spring::at(v);
    }
    /// Advance; returns true while still moving.
    fn step(&mut self, dt: f32, eps: f32) -> bool {
        const K: f32 = 340.0;
        const C: f32 = 30.0;
        let n = 4;
        let h = dt / n as f32;
        for _ in 0..n {
            let acc = -K * (self.x - self.target) - C * self.v;
            self.v += acc * h;
            self.x += self.v * h;
        }
        if (self.x - self.target).abs() < eps && self.v.abs() < eps * 8.0 {
            self.x = self.target;
            self.v = 0.0;
            false
        } else {
            true
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum View {
    Main,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum RcState {
    Off,
    Starting,
    On,
}

struct Drag {
    start: POINT,
    /// Cursor position relative to the notch's centre when the drag began.
    grab: (f32, f32),
    moved: bool,
}

#[derive(Default, Clone)]
pub struct Opts {
    pub bench: Option<u32>,
    pub expanded: bool,
    pub demo: bool,
    pub snapshot: Option<String>,
    pub settings: bool,
    /// Skip the first-run question and all toasts (tests, bench).
    pub no_prompt: bool,
    /// Snapshot / demo only: dock to this edge ("top", "bottom", "left", "right", "free") at `at` (0..1).
    pub edge: Option<String>,
    pub at: Option<f32>,
}

pub struct App {
    pub hwnd: HWND,
    pub gfx: Gfx,
    pub cfg: Config,
    pub theme: Theme,
    pub scale: f32,
    mons: Vec<winutil::Mon>,
    /// Screen edge the notch is attached to (Free = floating pill).
    pub edge: Edge,
    /// Position along that edge, 0..1, and the monitor it is on.
    dock_t: f32,
    dock_mon: String,
    /// Collapsed notch top-left, screen pixels.
    pill: [Spring; 2],
    win: (i32, i32),
    /// Panel rectangle inside the canvas, DIPs: x, y, w, h.
    pub rect: [Spring; 4],
    pub open: Spring,
    pub expanded: bool,
    pinned: bool,
    pub view: View,
    pub hits: Vec<Hit>,
    pub hover: Option<Action>,
    pressed: Option<Action>,
    drag: Option<Drag>,
    tracking: bool,
    suppress_hover: bool,
    /// The short delay before a hover opens the panel is running.
    open_pending: bool,
    /// Consecutive hover checks that found the cursor away from the open panel.
    away_ticks: u8,
    /// An event opened the panel: keep it open until then even though nobody hovers it.
    hold_until: Option<Instant>,
    /// Opened from the tray menu: stays open until the cursor has visited it once.
    release_pin_on_enter: bool,
    user_hidden: bool,
    fs_hidden: bool,
    shown: bool,
    animating: bool,
    dirty: bool,
    /// Only the pulsing dots need a new frame.
    pulse_dirty: bool,
    /// Event-driven redraws are limited to about 8 a second: `coalescing` is set while
    /// that window is open, `redraw_pending` when something arrived during it.
    coalescing: bool,
    redraw_pending: bool,
    last_height: f32,
    /// The pulsing dots of the last full frame.
    pub pulses: std::cell::RefCell<Vec<ui::PulseDot>>,
    trimmed: bool,
    last_frame: Instant,
    t0: Instant,
    pub phase: f32,
    pub store: Store,
    pub selected: Option<String>,
    pub scroll: usize,
    pub usage: Usage,
    pub rc: RcState,
    /// Remote Control was started by Notch (so Notch can stop it).
    rc_ours: bool,
    rc_deadline: u64,
    pub awake: bool,
    /// Notch is holding the system awake (as opposed to the lid setting just being "do nothing").
    awake_hold: bool,
    usage_next: u64,
    /// Settings is waiting for the user to press the new hotkey.
    capturing_hotkey: bool,
    hotkey_ok: bool,
    locked: bool,
    demo: bool,
    pub hooks_installed: bool,
    quiet: bool,
    glass: bool,
    region: (i32, i32, i32, i32, i32),
    bench: Option<crate::bench::Bench>,
    taskbar_created: u32,
}

impl App {
    // ---------------------------------------------------------------- geometry

    fn refresh_monitors(&mut self) {
        self.mons = winutil::monitors();
    }

    fn mon_at(&self, x: f32, y: f32) -> winutil::Mon {
        winutil::nearest(&self.mons, x, y)
    }

    fn setup_key(&self) -> String {
        let mut parts: Vec<String> = self
            .mons
            .iter()
            .map(|m| format!("{},{},{}x{}@{}", m.rc.left, m.rc.top, m.rc.right - m.rc.left, m.rc.bottom - m.rc.top, (m.scale * 96.0).round() as i32))
            .collect();
        parts.sort();
        parts.join("|")
    }

    /// The monitor the notch is docked to (falls back to the primary one).
    fn dock_monitor(&self) -> winutil::Mon {
        self.mons.iter().find(|m| m.name == self.dock_mon).or(self.mons.iter().find(|m| m.primary)).or(self.mons.first()).cloned().unwrap_or_default()
    }

    /// Centre of the collapsed notch, screen pixels.
    fn pill_center(&self) -> (f32, f32) {
        let (w, h) = ui::collapsed_size(self.edge);
        (self.pill[0].x + w * self.scale / 2.0, self.pill[1].x + h * self.scale / 2.0)
    }

    /// Top-left pixel of the collapsed notch when attached to `edge` of monitor `m` at
    /// fraction `t` along it. The notch is flush with the work area, so on the bottom edge
    /// it sits on top of the taskbar.
    fn docked_pos(m: &winutil::Mon, edge: Edge, t: f32) -> (f32, f32) {
        let s = m.scale;
        let (cw, ch) = ui::collapsed_size(edge);
        let (pw, ph) = (cw * s, ch * s);
        let margin = (ui::EAR + 6.0) * s;
        let (l, tp, r, b) = (m.work.left as f32, m.work.top as f32, m.work.right as f32, m.work.bottom as f32);
        match edge {
            Edge::Left | Edge::Right => {
                let y = (tp + t * (b - tp) - ph / 2.0).clamp(tp + margin, (b - ph - margin).max(tp + margin));
                (if edge == Edge::Left { l } else { r - pw }, y.round())
            }
            _ => {
                let x = (l + t * (r - l) - pw / 2.0).clamp(l + margin, (r - pw - margin).max(l + margin));
                (x.round(), if edge == Edge::Bottom { b - ph } else { tp })
            }
        }
    }

    /// Put the notch where the config says for the current monitor setup.
    fn place_from_config(&mut self, animate: bool) {
        let p = self.cfg.positions.get(&self.setup_key()).cloned().unwrap_or_default();
        self.edge = Edge::from_name(&p.snap);
        // names from earlier builds
        self.dock_t = match p.snap.as_str() {
            "top-right" => 0.92,
            "top-left" => 0.08,
            "top-center" => 0.5,
            _ => p.t.clamp(0.0, 1.0),
        };
        self.dock_mon = p.monitor.clone();
        let (x, y) = if self.edge == Edge::Free {
            let m = self.mon_at(p.x as f32, p.y as f32);
            let (pw, ph) = (PILL_W * m.scale, PILL_H * m.scale);
            ((p.x as f32).clamp(m.work.left as f32, (m.work.right as f32 - pw).max(m.work.left as f32)), (p.y as f32).clamp(m.work.top as f32, (m.work.bottom as f32 - ph).max(m.work.top as f32)))
        } else {
            let m = self.dock_monitor();
            self.dock_mon = m.name.clone();
            Self::docked_pos(&m, self.edge, self.dock_t)
        };
        if animate {
            self.pill[0].target = x;
            self.pill[1].target = y;
            self.kick();
        } else {
            self.pill[0].jump(x);
            self.pill[1].jump(y);
        }
    }

    fn save_position(&mut self) {
        let key = self.setup_key();
        let pos = Pos { x: self.pill[0].target as i32, y: self.pill[1].target as i32, snap: self.edge.name().to_string(), monitor: self.dock_mon.clone(), t: self.dock_t };
        self.cfg.positions.insert(key, pos);
        config::save(&self.cfg);
    }

    /// Move the notch so its centre is at `c` (screen pixels). Within reach of a screen
    /// edge it attaches to that edge, anywhere along it; otherwise it floats.
    fn drag_to(&mut self, c: (f32, f32)) {
        let m = self.mon_at(c.0, c.1);
        let (l, t, r, b) = (m.work.left as f32, m.work.top as f32, m.work.right as f32, m.work.bottom as f32);
        let cands = [(c.1 - t, Edge::Top), (b - c.1, Edge::Bottom), (c.0 - l, Edge::Left), (r - c.0, Edge::Right)];
        let (dist, near) = cands.iter().copied().fold((f32::MAX, Edge::Top), |best, cand| if cand.0 < best.0 { cand } else { best });
        let (x, y) = if dist <= SNAP_DIST * m.scale {
            self.edge = near;
            self.dock_t = if near.vertical() { (c.1 - t) / (b - t).max(1.0) } else { (c.0 - l) / (r - l).max(1.0) }.clamp(0.0, 1.0);
            self.dock_mon = m.name.clone();
            Self::docked_pos(&m, near, self.dock_t)
        } else {
            self.edge = Edge::Free;
            self.dock_mon = m.name.clone();
            (c.0 - PILL_W * m.scale / 2.0, c.1 - PILL_H * m.scale / 2.0)
        };
        self.pill[0].jump(x);
        self.pill[1].jump(y);
    }

    /// Compute the canvas position and the panel's target rectangle from the notch position.
    fn layout(&mut self) {
        let docked = self.edge != Edge::Free;
        let (cwd, chd) = ui::collapsed_size(self.edge);
        let c0 = self.pill_center();
        let m = if docked { self.dock_monitor() } else { self.mon_at(c0.0, c0.1) };
        if (m.scale - self.scale).abs() > 0.001 {
            // moved onto a monitor with a different DPI
            self.scale = m.scale;
            if docked {
                let (x, y) = Self::docked_pos(&m, self.edge, self.dock_t);
                self.pill[0].jump(x);
                self.pill[1].jump(y);
            } else {
                let (dx, dy) = (c0.0 - cwd * m.scale / 2.0 - self.pill[0].x, c0.1 - chd * m.scale / 2.0 - self.pill[1].x);
                self.pill[0].x += dx;
                self.pill[0].target += dx;
                self.pill[1].x += dy;
                self.pill[1].target += dy;
            }
        }
        let s = self.scale;
        let (cw, ch) = ((CANVAS_W * s).ceil() as i32, (CANVAS_H * s).ceil() as i32);
        self.gfx.resize(cw, ch, s);
        let (cwf, chf) = (cw as f32, ch as f32);
        let (px, py) = (self.pill[0].x, self.pill[1].x);
        let (pw, ph, pad) = (cwd * s, chd * s, PAD * s);
        let (wl, wt, wr, wb) = (m.work.left as f32, m.work.top as f32, m.work.right as f32, m.work.bottom as f32);
        let need = ui::content_height(self).max(chd);
        let clamp_x = |v: f32| v.clamp(wl - pad, (wr - cwf + pad).max(wl - pad));
        let clamp_y = |v: f32| v.clamp(wt - pad, (wb - chf + pad).max(wt - pad));
        // canvas origin, and whether the panel grows upward from the notch
        let (wx, wy, up) = match self.edge {
            Edge::Top => (clamp_x(px + pw / 2.0 - cwf / 2.0), py - pad, false),
            Edge::Bottom => (clamp_x(px + pw / 2.0 - cwf / 2.0), py + ph + pad - chf, true),
            Edge::Left => (px - pad, clamp_y(py + ph / 2.0 - chf / 2.0), false),
            Edge::Right => (px + pw + pad - cwf, clamp_y(py + ph / 2.0 - chf / 2.0), false),
            Edge::Free => {
                let up = py + need * s > wb && py - (need * s - ph) >= wt;
                (clamp_x(px + pw / 2.0 - cwf / 2.0), if up { py + ph + pad - chf } else { py - pad }, up)
            }
        };
        self.win = (wx.round() as i32, wy.round() as i32);
        let off = ((px - self.win.0 as f32) / s, (py - self.win.1 as f32) / s);

        let (tx, ty, tw, th) = if self.expanded {
            let (w, h) = (PANEL_W, need);
            let mid_y = (off.1 + chd / 2.0 - h / 2.0).clamp(PAD, (CANVAS_H - PAD - h).max(PAD));
            match self.edge {
                Edge::Left => (off.0, mid_y, w, h),
                Edge::Right => (off.0 + cwd - w, mid_y, w, h),
                _ => ((off.0 + cwd / 2.0 - w / 2.0).clamp(PAD, CANVAS_W - PAD - w), if up { off.1 + chd - h } else { off.1 }, w, h),
            }
        } else {
            (off.0, off.1, cwd, chd)
        };
        self.rect[0].target = tx;
        self.rect[1].target = ty;
        self.rect[2].target = tw;
        self.rect[3].target = th;
        let moving = self.drag.as_ref().map(|d| d.moved).unwrap_or(false) || self.pill[0].x != self.pill[0].target || self.pill[1].x != self.pill[1].target;
        if moving && !self.expanded {
            // While the notch itself moves, only its size springs: keep the side that
            // touches the edge (or the centre, when floating) exactly in place.
            let (w, h) = (self.rect[2].x, self.rect[3].x);
            let (x, y) = match self.edge {
                Edge::Left => (tx, ty + (th - h) / 2.0),
                Edge::Right => (tx + tw - w, ty + (th - h) / 2.0),
                Edge::Bottom => (tx + (tw - w) / 2.0, ty + th - h),
                _ => (tx + (tw - w) / 2.0, ty),
            };
            self.rect[0].jump(x);
            self.rect[1].jump(y);
        }
    }

    // ---------------------------------------------------------------- rendering

    fn kick(&mut self) {
        if !self.animating {
            self.animating = true;
            self.last_frame = Instant::now();
        }
    }

    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// Repaint just the pulsing dots over the frame already on screen: a few hundred
    /// pixels, no text layout, and only that area goes to the compositor.
    fn render_pulse(&mut self) {
        self.pulse_dirty = false;
        if !self.shown || self.animating {
            return;
        }
        if self.theme.bg.3 < 1.0 {
            // translucent panel (glass on): the dots cannot be repainted in place
            self.render();
            return;
        }
        let dots = self.pulses.borrow().clone();
        if dots.is_empty() {
            return;
        }
        let t_start = Instant::now();
        self.phase = (self.t0.elapsed().as_secs_f32() / 1.6).fract();
        let mut dirty = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for d in &dots {
            let b = d.bounds();
            dirty = [dirty[0].min(b[0]), dirty[1].min(b[1]), dirty[2].max(b[2]), dirty[3].max(b[3])];
        }
        self.gfx.begin_over(dirty);
        ui::draw_pulses(self, &dots);
        crate::bench::draw(t_start.elapsed());
        self.gfx.present(self.hwnd, self.win.0, self.win.1);
        crate::bench::frame(t_start.elapsed());
    }

    fn step(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32().clamp(0.001, 0.034);
        self.last_frame = now;
        let mut moving = false;
        for sp in self.pill.iter_mut() {
            moving |= sp.step(dt, 0.4);
        }
        self.layout();
        for sp in self.rect.iter_mut() {
            moving |= sp.step(dt, 0.15);
        }
        moving |= self.open.step(dt, 0.004);
        self.animating = moving;
        self.phase = (self.t0.elapsed().as_secs_f32() / 1.6).fract();
        self.draw_and_present();
    }

    fn render(&mut self) {
        self.dirty = false;
        self.pulse_dirty = false;
        if !self.shown {
            return;
        }
        self.layout();
        if !self.animating {
            for sp in self.rect.iter_mut() {
                sp.x = sp.target;
            }
        }
        self.phase = (self.t0.elapsed().as_secs_f32() / 1.6).fract();
        self.draw_and_present();
    }

    fn draw_and_present(&mut self) {
        let t_start = Instant::now();
        let mut hits = std::mem::take(&mut self.hits);
        self.gfx.begin(None);
        ui::draw(self, &mut hits);
        self.hits = hits;
        if self.hwnd.is_invalid() {
            self.gfx.end();
        } else {
            if self.glass {
                // clip the blur to the panel, one pixel outside its antialiased edge
                let s = self.scale;
                let (x, y, w, h) = (self.rect[0].x, self.rect[1].x, self.rect[2].x.max(PILL_H), self.rect[3].x.max(PILL_H));
                let r = ui::panel_radius(self.open.x, h);
                let reg = (((x - 1.0) * s).floor() as i32, ((y - 1.0) * s).floor() as i32, ((w + 2.0) * s).ceil() as i32, ((h + 2.0) * s).ceil() as i32, ((r + 1.0) * s).round() as i32);
                if reg != self.region {
                    self.region = reg;
                    crate::glass::set_region(self.hwnd, reg.0, reg.1, reg.2, reg.3, reg.4);
                }
            }
            self.gfx.present(self.hwnd, self.win.0, self.win.1);
        }
        crate::bench::frame(t_start.elapsed());
    }

    // ---------------------------------------------------------------- visibility, timers

    fn update_visibility(&mut self) {
        let want = !self.user_hidden && !self.fs_hidden && !(self.cfg.hide_when_idle && self.store.sessions.is_empty() && !self.expanded);
        if want != self.shown {
            self.shown = want;
            unsafe {
                if want {
                    self.render();
                    let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                    let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                } else {
                    let _ = ShowWindow(self.hwnd, SW_HIDE);
                }
            }
            self.update_timers();
        }
    }

    fn timer(&self, id: usize, ms: Option<u32>) {
        unsafe {
            match ms {
                Some(ms) => {
                    SetTimer(Some(self.hwnd), id, ms, None);
                }
                None => {
                    let _ = KillTimer(Some(self.hwnd), id);
                }
            }
        }
    }

    /// Timers only run while there is something to animate or count.
    fn update_timers(&mut self) {
        // only sessions that want a human (needs input, finished, error) pulse
        let attention = self.store.sessions.iter().any(|s| s.state != crate::model::State::Working);
        self.timer(T_PULSE, (self.shown && attention).then_some(125));
        self.timer(T_SECOND, (self.shown && self.expanded && self.view == View::Main).then_some(1000));
        self.timer(T_HOVERWATCH, (self.shown && self.expanded).then_some(150));
        self.timer(T_EXPIRE, (!self.store.sessions.is_empty()).then_some(15_000));
    }

    fn set_expanded(&mut self, v: bool) {
        if self.expanded == v {
            return;
        }
        self.expanded = v;
        self.open.target = if v { 1.0 } else { 0.0 };
        if !v {
            self.pinned = false;
            self.hover = None;
            self.view = View::Main;
            self.hold_until = None;
            self.release_pin_on_enter = false;
        } else {
            self.on_expand();
        }
        self.away_ticks = 0;
        self.open_pending = false;
        self.timer(T_HOVER_OPEN, None);
        self.update_visibility();
        self.update_timers();
        self.kick();
    }

    fn on_expand(&mut self) {
        self.refresh_usage(true);
        self.refresh_rc(true);
        self.refresh_awake();
    }

    // ---------------------------------------------------------------- input

    fn cursor() -> POINT {
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
        }
        p
    }

    fn on_mouse_move(&mut self, x: i32, y: i32) {
        if let Some(d) = self.drag.as_mut() {
            let c = Self::cursor();
            if !d.moved && (c.x - d.start.x).abs().max((c.y - d.start.y).abs()) as f32 > 4.0 * self.scale {
                d.moved = true;
            }
            if d.moved {
                let center = (c.x as f32 - d.grab.0, c.y as f32 - d.grab.1);
                if self.expanded {
                    self.set_expanded(false);
                }
                self.drag_to(center);
                self.kick();
            }
            return;
        }
        if !self.tracking {
            self.tracking = true;
            let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: self.hwnd, dwHoverTime: 0 };
            unsafe {
                let _ = TrackMouseEvent(&mut tme);
            }
        }
        let h = ui::hit_test(&self.hits, x as f32 / self.scale, y as f32 / self.scale);
        if h != self.hover {
            self.hover = h;
            self.invalidate();
        }
        // Open after a very short pause (so a cursor just passing by does not open it).
        // The pause starts once and is not restarted by further movement.
        if !self.expanded && self.cfg.hover_expand && !self.suppress_hover && self.hover.is_some() && !self.open_pending {
            self.open_pending = true;
            self.timer(T_HOVER_OPEN, Some(45));
        }
    }

    /// Is the cursor over the panel right now (current or target shape, plus `margin` DIPs)?
    /// Asked directly instead of trusting enter / leave messages, which can go missing when
    /// the panel changes shape under a still cursor.
    fn cursor_over(&self, margin: f32) -> bool {
        let c = Self::cursor();
        let (cx, cy) = ((c.x - self.win.0) as f32 / self.scale, (c.y - self.win.1) as f32 / self.scale);
        let inside = |x: f32, y: f32, w: f32, h: f32| cx >= x - margin && cx < x + w + margin && cy >= y - margin && cy < y + h + margin;
        let r = &self.rect;
        inside(r[0].x, r[1].x, r[2].x, r[3].x) || inside(r[0].target, r[1].target, r[2].target, r[3].target)
    }

    /// Runs a few times a second while the panel is open and closes it once the cursor has
    /// been away for a moment.
    fn hover_watch(&mut self) {
        if !self.expanded || self.drag.is_some() {
            return;
        }
        if self.cursor_over(8.0) {
            self.away_ticks = 0;
            self.hold_until = None; // you are looking at it: normal hover rules from here
            if self.release_pin_on_enter {
                self.release_pin_on_enter = false;
                self.pinned = false;
            }
            return;
        }
        let held = self.hold_until.map(|t| Instant::now() < t).unwrap_or(false);
        if self.pinned || self.capturing_hotkey || held {
            return;
        }
        self.away_ticks += 1;
        if self.away_ticks >= 2 {
            self.set_expanded(false);
        }
    }

    fn on_mouse_leave(&mut self) {
        self.tracking = false;
        self.suppress_hover = false;
        self.open_pending = false;
        self.timer(T_HOVER_OPEN, None);
        if self.hover.take().is_some() {
            self.invalidate();
        }
        // closing is decided by hover_watch, from the real cursor position
    }

    fn on_down(&mut self, x: i32, y: i32) {
        let h = ui::hit_test(&self.hits, x as f32 / self.scale, y as f32 / self.scale);
        match h {
            Some(Action::Header) => {
                unsafe {
                    SetCapture(self.hwnd);
                }
                let c = Self::cursor();
                let center = self.pill_center();
                self.drag = Some(Drag { start: c, grab: (c.x as f32 - center.0, c.y as f32 - center.1), moved: false });
            }
            other => self.pressed = other,
        }
    }

    fn on_up(&mut self, x: i32, y: i32) {
        if let Some(d) = self.drag.take() {
            unsafe {
                let _ = ReleaseCapture();
            }
            if d.moved {
                self.finish_drag();
            } else if self.expanded {
                // a click on the header closes it; it will not reopen until the cursor leaves
                self.suppress_hover = true;
                self.set_expanded(false);
            } else {
                self.set_expanded(true);
                // with hover-to-open switched off, a click opens it until the next click
                self.pinned = !self.cfg.hover_expand;
            }
            return;
        }
        let h = ui::hit_test(&self.hits, x as f32 / self.scale, y as f32 / self.scale);
        if h.is_some() && h == self.pressed {
            self.act(h.unwrap());
        }
        self.pressed = None;
    }

    fn finish_drag(&mut self) {
        if self.edge == Edge::Free {
            // keep a floating pill on screen
            let c = self.pill_center();
            let m = self.mon_at(c.0, c.1);
            let x = self.pill[0].x.clamp(m.work.left as f32, (m.work.right as f32 - PILL_W * m.scale).max(m.work.left as f32));
            let y = self.pill[1].x.clamp(m.work.top as f32, (m.work.bottom as f32 - PILL_H * m.scale).max(m.work.top as f32));
            self.pill[0].target = x;
            self.pill[1].target = y;
        }
        self.save_position();
        self.kick();
    }

    /// A key arrived while Settings is waiting for a new hotkey.
    fn on_key(&mut self, vk: u32) {
        if !self.capturing_hotkey {
            return;
        }
        match vk {
            0x1B => {} // Esc: keep the old one
            0x08 | 0x2E => self.cfg.hotkey.clear(), // Backspace / Delete: no hotkey
            0x10 | 0x11 | 0x12 | 0x5B | 0x5C | 0xA0..=0xA5 => return, // a modifier on its own: keep waiting
            _ => match crate::hotkey::from_keypress(vk) {
                Some(combo) => self.cfg.hotkey = combo,
                None => return, // needs Ctrl, Alt or Win (or a function key)
            },
        }
        self.capturing_hotkey = false;
        self.hotkey_ok = self.cfg.hotkey.is_empty() || crate::hotkey::register(self.hwnd, &self.cfg.hotkey);
        config::save(&self.cfg);
        self.invalidate();
    }

    fn toggle_hidden(&mut self) {
        self.user_hidden = self.shown;
        self.fs_hidden = false;
        self.update_visibility();
    }

    fn on_wheel(&mut self, delta: i32) {
        let n = self.store.sessions.len();
        if !self.expanded || self.view != View::Main || n <= ui::MAX_ROWS {
            return;
        }
        let max = n - ui::MAX_ROWS;
        let ns = if delta < 0 { (self.scroll + 1).min(max) } else { self.scroll.saturating_sub(1) };
        if ns != self.scroll {
            self.scroll = ns;
            self.invalidate();
        }
    }

    fn act(&mut self, a: Action) {
        match a {
            Action::Header => {}
            Action::Gear => {
                self.view = View::Settings;
                self.hooks_installed = self.query_hooks_installed();
                self.kick();
                self.update_timers();
            }
            Action::Back => {
                if self.capturing_hotkey {
                    self.capturing_hotkey = false;
                    self.hotkey_ok = self.cfg.hotkey.is_empty() || crate::hotkey::register(self.hwnd, &self.cfg.hotkey);
                }
                self.view = View::Main;
                self.kick();
                self.update_timers();
            }
            Action::Session(i) => self.click_session(i),
            Action::Dismiss(i) => {
                self.store.dismiss(i);
                self.hover = None;
                self.update_visibility();
                self.update_timers();
                self.kick();
            }
            Action::Remote => self.toggle_remote(),
            Action::Awake => self.toggle_awake(),
            Action::Usage => self.refresh_usage(true),
            Action::Set(s) => self.change_setting(s),
        }
        self.invalidate();
    }

    // ---------------------------------------------------------------- settings

    pub fn setting_value(&self, s: Setting) -> (String, bool) {
        let c = &self.cfg;
        let onoff = |b: bool| (if b { "On" } else { "Off" }.to_string(), b);
        match s {
            Setting::Theme => (
                match c.theme.as_str() {
                    "system" => "Follow Windows",
                    "light" => "Light",
                    _ => "Dark",
                }
                .to_string(),
                false,
            ),
            Setting::Dock => (
                match self.edge {
                    Edge::Top => "Top edge",
                    Edge::Bottom => "Bottom edge",
                    Edge::Left => "Left edge",
                    Edge::Right => "Right edge",
                    Edge::Free => "Floating",
                }
                .to_string(),
                false,
            ),
            Setting::Hotkey => {
                if self.capturing_hotkey {
                    ("Press the new keys… (Esc cancels)".to_string(), true)
                } else if c.hotkey.is_empty() {
                    ("None · click to set".to_string(), false)
                } else if !self.hotkey_ok {
                    (format!("{} · in use elsewhere", c.hotkey), false)
                } else {
                    (c.hotkey.clone(), false)
                }
            }
            Setting::HoverExpand => onoff(c.hover_expand),
            Setting::HideIdle => onoff(c.hide_when_idle),
            Setting::HideFullscreen => onoff(c.hide_on_fullscreen),
            Setting::KeepFinished => (format!("{} min", c.keep_finished_minutes), false),
            Setting::ToastInput => onoff(c.notify.toast_needs_input),
            Setting::ToastDone => onoff(c.notify.toast_done),
            Setting::Sound => onoff(c.notify.sound),
            Setting::AutoExpand => onoff(c.notify.auto_expand),
            Setting::AdapterClaude => onoff(c.adapters.claude),
            Setting::AdapterCodex => onoff(c.adapters.codex),
            Setting::AdapterT3 => onoff(c.adapters.t3code),
            Setting::Autostart => onoff(winutil::autostart_enabled()),
            Setting::RcTerminal => (
                match c.remote_control.terminal.as_str() {
                    "powershell" => "PowerShell",
                    "wsl" => "WSL",
                    _ => "Windows Terminal",
                }
                .to_string(),
                false,
            ),
            Setting::RcFolder => {
                if c.remote_control.default_dir.is_empty() {
                    ("Last used project".to_string(), false)
                } else {
                    (crate::util::clip(crate::util::base_name(&c.remote_control.default_dir), 22), false)
                }
            }
            Setting::Hooks => {
                if self.hooks_installed {
                    ("Installed · click to remove".to_string(), true)
                } else {
                    ("Not installed · click to install".to_string(), false)
                }
            }
        }
    }

    fn change_setting(&mut self, s: Setting) {
        fn cycle(cur: &str, opts: &[&str]) -> String {
            let i = opts.iter().position(|o| *o == cur).unwrap_or(0);
            opts[(i + 1) % opts.len()].to_string()
        }
        match s {
            Setting::Theme => {
                self.cfg.theme = cycle(&self.cfg.theme, &["dark", "light", "system"]);
                self.apply_theme();
            }
            Setting::Dock => {
                // cycle the edge; drag the notch to fine-tune where along the edge it sits
                let c = self.pill_center();
                let m = self.mon_at(c.0, c.1);
                self.edge = match self.edge {
                    Edge::Top => Edge::Right,
                    Edge::Right => Edge::Bottom,
                    Edge::Bottom => Edge::Left,
                    _ => Edge::Top,
                };
                self.dock_mon = m.name.clone();
                self.dock_t = 0.5;
                let (x, y) = Self::docked_pos(&m, self.edge, self.dock_t);
                self.pill[0].jump(x);
                self.pill[1].jump(y);
                self.save_position();
                self.kick();
            }
            Setting::Hotkey => {
                // take keyboard focus so the next key press reaches this window
                self.capturing_hotkey = !self.capturing_hotkey;
                if self.capturing_hotkey {
                    crate::hotkey::unregister(self.hwnd);
                    unsafe {
                        let _ = SetForegroundWindow(self.hwnd);
                        let _ = SetFocus(Some(self.hwnd));
                    }
                } else {
                    self.hotkey_ok = self.cfg.hotkey.is_empty() || crate::hotkey::register(self.hwnd, &self.cfg.hotkey);
                }
            }
            Setting::HoverExpand => self.cfg.hover_expand ^= true,
            Setting::HideIdle => self.cfg.hide_when_idle ^= true,
            Setting::HideFullscreen => {
                self.cfg.hide_on_fullscreen ^= true;
                self.check_fullscreen();
            }
            Setting::KeepFinished => {
                self.cfg.keep_finished_minutes = match self.cfg.keep_finished_minutes {
                    0..=4 => 5,
                    5..=9 => 10,
                    10..=29 => 30,
                    30..=59 => 60,
                    _ => 2,
                }
            }
            Setting::ToastInput => self.cfg.notify.toast_needs_input ^= true,
            Setting::ToastDone => self.cfg.notify.toast_done ^= true,
            Setting::Sound => self.cfg.notify.sound ^= true,
            Setting::AutoExpand => self.cfg.notify.auto_expand ^= true,
            Setting::AdapterClaude => self.cfg.adapters.claude ^= true,
            Setting::AdapterCodex => self.cfg.adapters.codex ^= true,
            Setting::AdapterT3 => self.cfg.adapters.t3code ^= true,
            Setting::Autostart => winutil::set_autostart(!winutil::autostart_enabled()),
            Setting::RcTerminal => self.cfg.remote_control.terminal = cycle(&self.cfg.remote_control.terminal, &["wt", "powershell", "wsl"]),
            Setting::RcFolder => self.pick_rc_folder(),
            Setting::Hooks => self.toggle_hooks(),
        }
        config::save(&self.cfg);
        self.update_visibility();
    }

    fn apply_theme(&mut self) {
        let dark = match self.cfg.theme.as_str() {
            "light" => false,
            "system" => winutil::system_dark(),
            _ => true,
        };
        self.glass = !self.hwnd.is_invalid() && self.cfg.glass && crate::glass::set_blur(self.hwnd, true, if dark { 0x01000000 } else { 0x01FFFFFF });
        if !self.glass && !self.hwnd.is_invalid() {
            crate::glass::set_blur(self.hwnd, false, 0);
            crate::glass::clear_region(self.hwnd);
            self.region = (0, 0, 0, 0, 0);
        }
        self.theme = ui::theme(dark, self.glass || (self.hwnd.is_invalid() && self.cfg.glass));
        self.invalidate();
    }

    // ---------------------------------------------------------------- features wired in later milestones

    fn query_hooks_installed(&self) -> bool {
        crate::adapters::claude_hooks::installed(&crate::adapters::claude_hooks::default_settings())
    }

    fn toggle_hooks(&mut self) {
        use crate::adapters::claude_hooks as h;
        let res = if self.hooks_installed { h::uninstall(&h::default_settings()) } else { h::install(&h::windows_target(None)) };
        match res {
            Ok(m) => logln!("hooks: {m}"),
            Err(e) => {
                logln!("hooks: failed: {e}");
                winutil::message_box(self.hwnd, "Notch", &format!("Could not change the Claude Code hooks:\n\n{e}"), false);
            }
        }
        self.hooks_installed = self.query_hooks_installed();
    }

    /// First run: offer to install the hooks. Asked once.
    fn offer_hooks(&mut self) {
        if self.cfg.hooks_prompted || !self.cfg.adapters.claude {
            return;
        }
        self.cfg.hooks_prompted = true;
        config::save(&self.cfg);
        if self.query_hooks_installed() {
            return;
        }
        let text = "Install Claude Code hooks so sessions show up live?\n\nThis adds entries to ~/.claude/settings.json next to your existing hooks. \
                    A backup is saved first, and you can remove them again in Notch settings.";
        if winutil::message_box(self.hwnd, "Notch", text, true) {
            self.hooks_installed = false;
            self.toggle_hooks();
        }
    }

    fn click_session(&mut self, i: usize) {
        let Some(s) = self.store.sessions.get(i) else { return };
        self.selected = Some(s.id.clone());
        let how = crate::proc::focus_session(&s.pids, &s.dir);
        logln!("focus: {} -> {how}", s.agent.label());
    }
    // ---- Remote Control

    /// Work out the Remote Control state. `scan` also looks for a Remote Control that was
    /// started outside Notch (a process scan, so only done now and then).
    fn refresh_rc(&mut self, scan: bool) {
        if self.demo {
            return;
        }
        let ours = crate::rc::our_pid().is_some();
        let external = if ours {
            false
        } else if scan {
            crate::rc::external_running() || self.store.sessions.iter().any(|s| s.remote && s.active())
        } else {
            self.rc == RcState::On && !self.rc_ours
        };
        let now = crate::util::now_unix();
        let new = if ours || external {
            RcState::On
        } else if self.rc == RcState::Starting && now < self.rc_deadline {
            RcState::Starting
        } else {
            RcState::Off
        };
        self.rc_ours = ours;
        self.timer(T_RC, (new == RcState::Starting || ours).then_some(1000));
        if new != self.rc {
            self.rc = new;
            self.invalidate();
        }
    }

    pub fn rc_label(&self) -> (&'static str, bool) {
        match (self.rc, self.rc_ours) {
            (RcState::Off, _) => ("Off", false),
            (RcState::Starting, _) => ("Starting…", true),
            (RcState::On, true) => ("On · click to stop", true),
            (RcState::On, false) => ("On · already running", true),
        }
    }

    /// Folder for a new Remote Control: the selected session, else the configured default,
    /// else the last project an agent worked in, else the home folder.
    fn rc_dir(&self) -> String {
        if let Some(s) = self.selected.as_ref().and_then(|id| self.store.sessions.iter().find(|s| &s.id == id)) {
            if !s.dir.is_empty() {
                return s.dir.clone();
            }
        }
        if !self.cfg.remote_control.default_dir.is_empty() {
            return self.cfg.remote_control.default_dir.clone();
        }
        if !self.store.last_dir.is_empty() {
            return self.store.last_dir.clone();
        }
        self.store.sessions.first().map(|s| s.dir.clone()).filter(|d| !d.is_empty()).unwrap_or_else(|| crate::util::home().to_string_lossy().to_string())
    }

    fn toggle_remote(&mut self) {
        match (self.rc, self.rc_ours) {
            (RcState::On, true) => crate::rc::stop(),
            (RcState::Off, _) => {
                let dir = self.rc_dir();
                let rc = &self.cfg.remote_control;
                match crate::rc::start(&dir, &rc.terminal, &rc.wsl_distro) {
                    Ok(_) => {
                        logln!("remote control: starting");
                        self.rc = RcState::Starting;
                        self.rc_deadline = crate::util::now_unix() + 25;
                    }
                    Err(e) => {
                        winutil::message_box(self.hwnd, "Notch", &format!("Could not start Remote Control:\n\n{e}"), false);
                    }
                }
            }
            // started somewhere else, or still starting: nothing for Notch to stop
            _ => {}
        }
        self.refresh_rc(false);
    }

    // ---- Keep Awake

    fn refresh_awake(&mut self) {
        if self.demo {
            return;
        }
        let on = self.awake_hold || crate::power::lid_is_do_nothing() || crate::power::system_held_awake();
        if on != self.awake {
            self.awake = on;
            self.invalidate();
            if self.expanded {
                self.kick();
            }
        }
    }

    pub fn awake_label(&self) -> &'static str {
        if !self.awake {
            "Off"
        } else if self.awake_hold {
            "On · lid closed OK"
        } else if crate::power::lid_is_do_nothing() {
            "On · set in Windows"
        } else {
            "On · held by another app"
        }
    }

    fn toggle_awake(&mut self) {
        // Three cases: Notch is holding it (switch off and restore), Windows' lid setting is
        // already "do nothing" (switch that off), or it is off / only another app is holding
        // the PC awake (switch Notch's own hold on).
        let res = if self.awake_hold || crate::power::lid_is_do_nothing() {
            self.awake_hold = false;
            crate::power::disable(true)
        } else {
            crate::power::enable().map(|_| self.awake_hold = true)
        };
        match res {
            Ok(_) => logln!("keep awake: {}", if self.awake_hold { "on" } else { "off" }),
            Err(e) => {
                winutil::message_box(self.hwnd, "Notch", &format!("Could not change the power settings:\n\n{e}"), false);
            }
        }
        self.refresh_awake();
    }

    // ---- Usage

    /// Ask for fresh usage numbers. `force` = the user opened the panel or clicked the
    /// section; otherwise this is the 5-minute poll, which is skipped while the PC is
    /// locked or idle.
    fn refresh_usage(&mut self, force: bool) {
        if !self.cfg.usage.enabled || self.usage.loading || self.demo {
            return;
        }
        let now = crate::util::now_unix();
        let gap = if force { 60 } else { 280 };
        if self.usage.fetched_at != 0 && now < (self.usage.fetched_at + gap).max(self.usage_next) {
            return;
        }
        if !force && (self.locked || winutil::idle_secs() > 600) {
            return;
        }
        self.usage.loading = true;
        crate::usage::refresh(self.cfg.usage.clone());
        self.invalidate();
    }

    fn on_usage(&mut self, new: Usage) {
        let now = crate::util::now_unix();
        if new.note.contains("rate limited") {
            self.usage_next = now + 900; // leave the endpoint alone for a while
        }
        // keep recent real numbers rather than replace them with an estimate or an error
        let downgrade = self.usage.source == "api" && new.source != "api" && now.saturating_sub(self.usage.fetched_at) < 1800;
        if downgrade {
            self.usage.loading = false;
        } else {
            self.usage = new;
        }
    }

    fn pick_rc_folder(&mut self) {
        if !self.cfg.remote_control.default_dir.is_empty() {
            self.cfg.remote_control.default_dir.clear(); // back to "last used project"
        } else if let Some(dir) = winutil::pick_folder(self.hwnd) {
            self.cfg.remote_control.default_dir = dir;
        }
    }
    fn on_alert(&mut self, a: Alert) {
        let n = &self.cfg.notify;
        let (title, body, toast) = match &a {
            Alert::NeedsInput { name, agent, reason, .. } => (format!("{name} needs input"), format!("{} · {reason}", agent.label()), n.toast_needs_input),
            Alert::Done { name, agent, .. } => (format!("{name} finished"), agent.label().to_string(), n.toast_done),
            Alert::Error { name, agent, message, .. } => (format!("{name} hit an error"), format!("{} · {message}", agent.label()), n.toast_error),
        };
        if self.quiet {
            // bench / demo runs: no toasts or sounds
        } else if toast {
            crate::toast::show(title, body, !n.sound);
        } else if n.sound {
            winutil::beep();
        }
        self.peek();
    }

    // ---------------------------------------------------------------- events from workers

    fn on_uimsg(&mut self, msg: UiMsg) {
        let before = self.collapsed_signature();
        let usage_changed = matches!(msg, UiMsg::Usage(_));
        match msg {
            UiMsg::Event(ev) => self.apply_event(ev),
            UiMsg::Events(evs) => {
                for ev in evs {
                    self.apply_event(ev);
                }
            }
            UiMsg::Usage(u) => self.on_usage(u),
        }
        self.update_visibility();
        self.update_timers();
        if self.expanded {
            // animate only when the panel has to change size
            let h = ui::content_height(self);
            if (h - self.last_height).abs() > 0.5 {
                self.last_height = h;
                self.kick();
            }
            self.request_redraw();
        } else if usage_changed || before != self.collapsed_signature() {
            // collapsed: a new step text is not visible, so only state changes repaint
            self.request_redraw();
        }
    }

    /// What the collapsed notch shows: one state per session.
    fn collapsed_signature(&self) -> u64 {
        self.store.sessions.iter().fold(self.store.sessions.len() as u64, |acc, s| acc.wrapping_mul(31).wrapping_add(s.state as u64 + 1))
    }

    /// Redraw soon, but not more than about 8 times a second however fast events arrive.
    fn request_redraw(&mut self) {
        if self.coalescing {
            self.redraw_pending = true;
        } else {
            self.coalescing = true;
            self.invalidate();
            self.timer(T_COALESCE, Some(125));
        }
    }

    fn apply_event(&mut self, ev: AgentEvent) {
        if !crate::adapters::agent_enabled(&self.cfg, ev.agent) {
            return;
        }
        if ev.live {
            crate::adapters::transcript::mark_live(&ev.session_id);
        }
        let remote = ev.remote;
        if let Some(alert) = self.store.apply(ev) {
            self.on_alert(alert);
        }
        if remote && self.rc == RcState::Off {
            self.refresh_rc(true);
        }
    }

    /// Expand for a few seconds to show an event, unless the user is already interacting.
    pub fn peek(&mut self) {
        if !self.cfg.notify.auto_expand || self.pinned || self.drag.is_some() {
            return;
        }
        if !self.expanded {
            self.set_expanded(true);
        }
        if !self.cursor_over(8.0) {
            self.hold_until = Some(Instant::now() + std::time::Duration::from_millis(4500));
        }
    }

    // ---------------------------------------------------------------- fullscreen auto-hide

    fn check_fullscreen(&mut self) {
        let fs = self.cfg.hide_on_fullscreen && {
            let c = self.pill_center();
            let m = self.mon_at(c.0, c.1);
            winutil::foreground_is_fullscreen(self.hwnd, &m.rc)
        };
        if fs != self.fs_hidden {
            self.fs_hidden = fs;
            self.update_visibility();
        }
    }

    // ---------------------------------------------------------------- timers

    fn on_timer(&mut self, id: usize) {
        match id {
            T_PULSE => self.pulse_dirty = true,
            T_COALESCE => {
                if self.redraw_pending {
                    self.redraw_pending = false;
                    self.invalidate();
                } else {
                    self.coalescing = false;
                    self.timer(T_COALESCE, None);
                }
            }
            T_SECOND => self.invalidate(),
            T_HOVER_OPEN => {
                self.timer(T_HOVER_OPEN, None);
                self.open_pending = false;
                if !self.expanded && self.drag.is_none() && self.cursor_over(2.0) {
                    self.set_expanded(true);
                }
            }
            T_HOVERWATCH => self.hover_watch(),
            T_USAGE => self.refresh_usage(false),
            T_RC => self.refresh_rc(false),
            T_EXPIRE => {
                self.refresh_rc(true);
                let keep = self.cfg.keep_finished_minutes as u64 * 60;
                if self.store.expire(keep, 600, &winutil::pid_alive) {
                    self.update_visibility();
                    self.update_timers();
                    self.invalidate();
                    if self.expanded {
                        self.kick();
                    }
                }
            }
            T_FSCHECK => {
                self.timer(T_FSCHECK, None);
                self.check_fullscreen();
            }
            T_TRIM => {
                // Startup touches a lot of memory once (fonts, COM, the first usage scan).
                // Hand those pages back; what is really needed faults back in.
                self.timer(T_TRIM, None);
                if !self.trimmed {
                    self.trimmed = true;
                    winutil::trim_working_set();
                }
            }
            T_BENCH => {
                if let Some(b) = self.bench.as_mut() {
                    if b.tick() {
                        unsafe {
                            let _ = DestroyWindow(self.hwnd);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn on_display_change(&mut self) {
        self.refresh_monitors();
        self.place_from_config(false);
        self.check_fullscreen();
        self.invalidate();
    }

    fn menu(&mut self) {
        match tray::popup(self.hwnd, self.shown) {
            tray::Cmd::ShowHide => {
                self.user_hidden = self.shown;
                self.fs_hidden = false;
                self.update_visibility();
            }
            tray::Cmd::Settings => {
                self.user_hidden = false;
                self.update_visibility();
                self.view = View::Settings;
                self.hooks_installed = self.query_hooks_installed();
                self.set_expanded(true);
                // opened from the tray, far from the cursor: stay open until visited
                self.pinned = true;
                self.release_pin_on_enter = true;
                self.kick();
            }
            tray::Cmd::Autostart => winutil::set_autostart(!winutil::autostart_enabled()),
            tray::Cmd::ClearFinished => {
                self.store.clear_finished();
                self.update_visibility();
                self.update_timers();
                self.invalidate();
                self.kick();
            }
            tray::Cmd::Quit => unsafe {
                let _ = DestroyWindow(self.hwnd);
            },
            tray::Cmd::None => {}
        }
    }

    fn shutdown(&mut self) {
        // put the power settings back exactly as they were before Keep Awake
        if crate::power::ours() {
            if let Err(e) = crate::power::disable(false) {
                logln!("power: restore on exit failed: {e}");
            }
        }
        tray::remove(self.hwnd);
        config::save(&self.cfg);
    }
}

fn xy(l: LPARAM) -> (i32, i32) {
    ((l.0 & 0xFFFF) as i16 as i32, ((l.0 >> 16) & 0xFFFF) as i16 as i32)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(a) = app() else {
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    };
    if a.hwnd.is_invalid() {
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    }
    match msg {
        WM_MOUSEMOVE => {
            let (x, y) = xy(lp);
            a.on_mouse_move(x, y);
        }
        WM_MOUSELEAVE => a.on_mouse_leave(),
        WM_LBUTTONDOWN => {
            let (x, y) = xy(lp);
            a.on_down(x, y);
        }
        WM_LBUTTONUP => {
            let (x, y) = xy(lp);
            a.on_up(x, y);
        }
        WM_CAPTURECHANGED => {
            if a.drag.take().map(|d| d.moved).unwrap_or(false) {
                a.finish_drag();
            }
        }
        WM_RBUTTONUP => a.menu(),
        WM_MOUSEWHEEL => a.on_wheel(((wp.0 >> 16) & 0xFFFF) as i16 as i32),
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_TIMER => a.on_timer(wp.0),
        WM_TRAY => match (lp.0 & 0xFFFF) as u32 {
            WM_RBUTTONUP | WM_CONTEXTMENU => a.menu(),
            WM_LBUTTONUP => {
                a.user_hidden = a.shown;
                a.fs_hidden = false;
                a.update_visibility();
            }
            _ => {}
        },
        WM_UIMSG => {
            let m = unsafe { Box::from_raw(lp.0 as *mut UiMsg) };
            a.on_uimsg(*m);
        }
        WM_FIRSTRUN => a.offer_hooks(),
        WM_HOTKEY => a.toggle_hidden(),
        WM_KEYDOWN | WM_SYSKEYDOWN => a.on_key(wp.0 as u32),
        WM_WTSSESSION_CHANGE => {
            // 7 = locked, 8 = unlocked: no usage polling while locked
            match wp.0 {
                7 => {
                    a.locked = true;
                    a.timer(T_USAGE, None);
                }
                8 => {
                    a.locked = false;
                    a.timer(T_USAGE, a.cfg.usage.enabled.then_some(300_000));
                    a.refresh_usage(false);
                }
                _ => {}
            }
        }
        WM_FOREGROUND => {
            a.check_fullscreen();
            // apps often go fullscreen a moment after they take the foreground
            a.timer(T_FSCHECK, Some(1500));
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED => a.on_display_change(),
        WM_SETTINGCHANGE => {
            a.refresh_monitors();
            a.apply_theme();
            a.place_from_config(false);
        }
        WM_QUERYENDSESSION => return LRESULT(1),
        WM_ENDSESSION => {
            if wp.0 != 0 {
                a.shutdown();
            }
        }
        WM_DESTROY => {
            a.shutdown();
            unsafe { PostQuitMessage(0) };
        }
        m if m == a.taskbar_created && m != 0 => tray::add(hwnd),
        _ => return unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
    LRESULT(0)
}

unsafe extern "system" fn fg_hook(_h: HWINEVENTHOOK, _ev: u32, _hwnd: HWND, _obj: i32, _child: i32, _thread: u32, _time: u32) {
    let h = HWND_MAIN.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as _)), WM_FOREGROUND, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn run(opts: Opts) -> i32 {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let snapshot = opts.snapshot.is_some();
        if !snapshot {
            let name = crate::util::wide(&format!("Local\\NotchIslandSingleton{}", crate::util::instance_suffix()));
            let _mutex = CreateMutexW(None, true, PCWSTR(name.as_ptr()));
            if GetLastError() == ERROR_ALREADY_EXISTS {
                return 0;
            }
        }
        let cfg = config::load();
        let mons = winutil::monitors();
        let scale = mons.iter().find(|m| m.primary).or(mons.first()).map(|m| m.scale).unwrap_or(1.0);
        let gfx = match Gfx::new((CANVAS_W * scale).ceil() as i32, (CANVAS_H * scale).ceil() as i32, scale) {
            Ok(g) => g,
            Err(e) => {
                logln!("graphics init failed: {e}");
                return 1;
            }
        };
        let app = Box::new(App {
            hwnd: HWND::default(),
            gfx,
            theme: ui::theme(true, false),
            cfg,
            scale,
            mons,
            edge: Edge::Top,
            dock_t: 0.5,
            dock_mon: String::new(),
            pill: [Spring::default(); 2],
            win: (0, 0),
            rect: [Spring::at(PAD), Spring::at(PAD), Spring::at(PILL_W), Spring::at(PILL_H)],
            open: Spring::default(),
            expanded: false,
            pinned: false,
            view: View::Main,
            hits: Vec::new(),
            hover: None,
            pressed: None,
            drag: None,
            tracking: false,
            suppress_hover: false,
            open_pending: false,
            away_ticks: 0,
            hold_until: None,
            release_pin_on_enter: false,
            user_hidden: false,
            fs_hidden: false,
            shown: false,
            animating: false,
            dirty: false,
            pulse_dirty: false,
            coalescing: false,
            redraw_pending: false,
            last_height: 0.0,
            pulses: std::cell::RefCell::new(Vec::new()),
            trimmed: false,
            last_frame: Instant::now(),
            t0: Instant::now(),
            phase: 0.0,
            store: Store::default(),
            selected: None,
            scroll: 0,
            usage: Usage::default(),
            rc: RcState::Off,
            rc_ours: false,
            rc_deadline: 0,
            awake: false,
            awake_hold: false,
            usage_next: 0,
            capturing_hotkey: false,
            hotkey_ok: true,
            locked: false,
            // demo, snapshot and isolated test instances never call the usage endpoint
            demo: opts.demo || snapshot || !crate::util::instance_suffix().is_empty(),
            hooks_installed: false,
            quiet: opts.bench.is_some() || opts.demo,
            glass: false,
            region: (0, 0, 0, 0, 0),
            bench: None,
            taskbar_created: 0,
        });
        let a: &'static mut App = Box::leak(app);
        APP.store(a, Ordering::Relaxed);
        a.apply_theme();
        a.place_from_config(false);
        if let Some(e) = &opts.edge {
            // preview only: nothing is saved
            a.edge = Edge::from_name(if e == "free" { "" } else { e });
            a.dock_t = opts.at.unwrap_or(0.5).clamp(0.0, 1.0);
            let m = a.dock_monitor();
            let (x, y) = if a.edge == Edge::Free { (m.work.left as f32 + 300.0, m.work.top as f32 + 200.0) } else { App::docked_pos(&m, a.edge, a.dock_t) };
            a.pill[0].jump(x);
            a.pill[1].jump(y);
            let (w, h) = ui::collapsed_size(a.edge);
            a.rect[2].jump(w);
            a.rect[3].jump(h);
        }
        if opts.demo || snapshot {
            crate::demo::fill(a);
        }

        if let Some(path) = &opts.snapshot {
            a.shown = true;
            a.expanded = opts.expanded || opts.settings;
            if opts.settings {
                a.view = View::Settings;
            }
            a.open.jump(if a.expanded { 1.0 } else { 0.0 });
            a.render();
            return crate::demo::save_bmp(&a.gfx, path);
        }

        let hinst: HINSTANCE = GetModuleHandleW(None).map(|h| h.into()).unwrap_or_default();
        let class = w!("NotchIsland");
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst, lpszClassName: class, hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(), ..Default::default() };
        RegisterClassW(&wc);
        let hwnd = match CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            class,
            PCWSTR(crate::util::wide(&winutil::window_title()).as_ptr()),
            WS_POPUP,
            0,
            0,
            a.gfx.w,
            a.gfx.h,
            None,
            None,
            Some(hinst),
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                logln!("window creation failed: {e}");
                return 1;
            }
        };
        a.hwnd = hwnd;
        HWND_MAIN.store(hwnd.0 as isize, Ordering::Relaxed);
        a.taskbar_created = RegisterWindowMessageW(w!("TaskbarCreated"));
        a.apply_theme(); // now that the window exists the backdrop blur can be switched on
        tray::add(hwnd);
        let _ = SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, None, Some(fg_hook), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);

        if let Some(secs) = opts.bench {
            a.bench = Some(crate::bench::Bench::new(secs));
            a.timer(T_BENCH, Some(1000));
        }
        if opts.expanded {
            a.expanded = true;
            a.pinned = true;
            a.open.target = 1.0;
            a.kick();
        }
        a.update_visibility();
        a.update_timers();
        crate::toast::register();
        crate::ipc::start(a.cfg.wsl.tcp_fallback);
        if !opts.demo {
            a.hotkey_ok = a.cfg.hotkey.is_empty() || crate::hotkey::register(hwnd, &a.cfg.hotkey);
            crate::power::recover_after_crash();
            crate::adapters::start_all(&a.cfg);
            let _ = windows::Win32::System::RemoteDesktop::WTSRegisterSessionNotification(hwnd, 0);
            a.timer(T_USAGE, a.cfg.usage.enabled.then_some(300_000));
            a.refresh_usage(true);
            a.refresh_rc(true);
            a.refresh_awake();
        }
        if !a.quiet && !opts.no_prompt {
            let _ = PostMessageW(Some(hwnd), WM_FIRSTRUN, WPARAM(0), LPARAM(0));
        }
        a.timer(T_TRIM, Some(12_000));
        logln!("notch started (scale {:.2}, {} monitor(s))", a.scale, a.mons.len());

        let mut msg = MSG::default();
        loop {
            if a.animating && a.shown {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        return msg.wParam.0 as i32;
                    }
                    DispatchMessageW(&msg);
                }
                let t = Instant::now();
                a.step();
                // wait for the next vertical blank; never spin if DWM returns at once
                let _ = DwmFlush();
                if t.elapsed().as_millis() < 3 {
                    Sleep(6);
                }
            } else {
                a.animating = false;
                if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    return msg.wParam.0 as i32;
                }
                DispatchMessageW(&msg);
                // drain whatever else is queued, then draw once
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        return msg.wParam.0 as i32;
                    }
                    DispatchMessageW(&msg);
                }
                if a.dirty && !a.animating {
                    a.render();
                } else if a.pulse_dirty && !a.animating {
                    a.render_pulse();
                }
            }
        }
    }
}
