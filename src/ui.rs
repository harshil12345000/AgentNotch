//! Layout and drawing of the pill, the expanded panel and the settings view.
//! Everything is in DIPs relative to the canvas; `draw` also produces the hit regions.
use crate::app::{App, RcState, View};
use crate::gfx::{hex, Align, Color, Font, Gfx};
use crate::model::{State, UsageWindow};
use crate::util::{fmt_dur, fmt_tokens, now_unix};

pub const PILL_W: f32 = 200.0;
pub const PILL_H: f32 = 32.0;
pub const PANEL_W: f32 = 380.0;
/// Transparent margin around the panel inside the canvas (room for spring overshoot).
pub const PAD: f32 = 10.0;
pub const CANVAS_W: f32 = PANEL_W + PAD * 2.0;
pub const CANVAS_H: f32 = 484.0 + PAD * 2.0;
/// Collapsed size when attached to the left or right edge (a vertical tab).
pub const VPILL_W: f32 = 34.0;
pub const VPILL_H: f32 = 142.0;
/// Radius of the concave fillets where a docked notch meets the screen edge.
pub const EAR: f32 = 8.0;
pub const ROW_H: f32 = 40.0;

/// Which screen edge the notch is attached to.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Edge {
    Free,
    Top,
    Bottom,
    Left,
    Right,
}

impl Edge {
    pub fn name(self) -> &'static str {
        match self {
            Edge::Free => "",
            Edge::Top => "top",
            Edge::Bottom => "bottom",
            Edge::Left => "left",
            Edge::Right => "right",
        }
    }
    pub fn from_name(s: &str) -> Edge {
        match s {
            "bottom" => Edge::Bottom,
            "left" => Edge::Left,
            "right" => Edge::Right,
            "" => Edge::Free,
            _ => Edge::Top, // includes the old "top-center" style names
        }
    }
    pub fn vertical(self) -> bool {
        matches!(self, Edge::Left | Edge::Right)
    }
    fn side(self) -> u8 {
        match self {
            Edge::Bottom => 1,
            Edge::Left => 2,
            Edge::Right => 3,
            _ => 0,
        }
    }
}

/// Size of the collapsed notch for an edge, in DIPs.
pub fn collapsed_size(e: Edge) -> (f32, f32) {
    if e.vertical() { (VPILL_W, VPILL_H) } else { (PILL_W, PILL_H) }
}
pub const MAX_ROWS: usize = 4;
/// On the left / right edge the panel is narrower and taller.
pub const SIDE_PANEL_W: f32 = 300.0;

/// Dots shown on the collapsed side tab before the "more" arrow.
pub const SIDE_DOTS: usize = 3;

/// Width of the open panel: narrower on a side edge (settings always use the full width).
pub fn panel_width(a: &App) -> f32 {
    if a.edge.vertical() && a.view == View::Main { SIDE_PANEL_W } else { PANEL_W }
}



/// Usage numbers older than this get an "as of" line.
const STALE_SECS: u64 = 420;

fn usage_stale(a: &App) -> Option<u64> {
    let age = now_unix().saturating_sub(a.usage.fetched_at);
    (a.usage.source == "api" && a.usage.fetched_at > 0 && age > STALE_SECS).then_some(age)
}
const HEADER_H: f32 = 34.0;
const SET_ROW_H: f32 = 23.0;

#[derive(Clone, PartialEq, Debug)]
pub enum Action {
    /// The pill / header strip: drag handle, click toggles the panel.
    Header,
    Session(usize),
    /// The close button on a session row.
    Dismiss(usize),
    /// Answer the permission request a session is waiting on.
    Approve(usize),
    Deny(usize),
    Gear,
    Back,
    Remote,
    Awake,
    /// Travel mode: Remote Control + Keep Awake + a hotspot reminder, in one click.
    Travel,
    Usage,
    Set(Setting),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Setting {
    Theme,
    Dock,
    Hotkey,
    HoverExpand,
    HideIdle,
    HideFullscreen,
    KeepFinished,
    ToastInput,
    ToastDone,
    Sound,
    AutoExpand,
    AdapterClaude,
    AdapterCodex,
    AdapterT3,
    Autostart,
    RcTerminal,
    RcFolder,
    Hooks,
    Approve,
}

pub const SETTINGS: [(Setting, &str); 19] = [
    (Setting::Theme, "Theme"),
    (Setting::Dock, "Dock edge (drag to fine-tune)"),
    (Setting::Hotkey, "Hide / show hotkey"),
    (Setting::HoverExpand, "Expand on hover"),
    (Setting::HideIdle, "Hide when no sessions"),
    (Setting::HideFullscreen, "Hide behind fullscreen apps"),
    (Setting::KeepFinished, "Keep finished sessions"),
    (Setting::ToastInput, "Toast: needs input"),
    (Setting::ToastDone, "Toast: finished"),
    (Setting::Sound, "Notification sound"),
    (Setting::AutoExpand, "Expand briefly on events"),
    (Setting::AdapterClaude, "Claude Code adapter"),
    (Setting::AdapterCodex, "Codex adapter"),
    (Setting::AdapterT3, "T3 Code adapter"),
    (Setting::Autostart, "Start with Windows"),
    (Setting::RcTerminal, "Remote Control terminal"),
    (Setting::RcFolder, "Remote Control folder"),
    (Setting::Hooks, "Claude Code hooks"),
    (Setting::Approve, "Approve / deny from the notch"),
];

pub struct Hit {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub action: Action,
}

pub struct Theme {
    pub dark: bool,
    pub bg: Color,
    /// Sheen: a soft highlight that fades out down the panel.
    pub sheen: Color,
    pub border: Color,
    pub text: Color,
    pub dim: Color,
    pub hover: Color,
    pub track: Color,
    pub chip: Color,
    pub line: Color,
}

/// `glass` = a real backdrop blur is active, so the panel can be translucent.
pub fn theme(dark: bool, glass: bool) -> Theme {
    if dark {
        Theme {
            dark,
            bg: hex(0x000000).a(if glass { 0.78 } else { 1.0 }),
            sheen: Color(1.0, 1.0, 1.0, if glass { 0.07 } else { 0.05 }),
            border: Color(1.0, 1.0, 1.0, 0.16),
            text: hex(0xF5F5F7),
            dim: hex(0xB4B6C2),
            hover: Color(1.0, 1.0, 1.0, 0.07),
            track: Color(1.0, 1.0, 1.0, 0.14),
            chip: Color(1.0, 1.0, 1.0, 0.11),
            line: Color(1.0, 1.0, 1.0, 0.08),
        }
    } else {
        Theme {
            dark,
            bg: hex(0xFBFBFD).a(if glass { 0.74 } else { 1.0 }),
            sheen: Color(1.0, 1.0, 1.0, if glass { 0.45 } else { 0.6 }),
            border: Color(1.0, 1.0, 1.0, 0.75),
            text: hex(0x16171B),
            dim: hex(0x5F6170),
            hover: Color(0.0, 0.0, 0.0, 0.05),
            track: Color(0.0, 0.0, 0.0, 0.11),
            chip: Color(0.0, 0.0, 0.0, 0.07),
            line: Color(0.0, 0.0, 0.0, 0.08),
        }
    }
}

pub const BLUE: u32 = 0x3B9BFF;
pub const AMBER: u32 = 0xFFB020;
pub const GREEN: u32 = 0x34C759;
pub const RED: u32 = 0xFF453A;
pub const VIOLET: u32 = 0xA78BFA;

fn state_color(s: State) -> Color {
    hex(match s {
        State::Working => BLUE,
        State::NeedsInput => AMBER,
        State::Done => GREEN,
        State::Error => RED,
    })
}

fn usage_color(pct: f32, t: &Theme) -> Color {
    if pct > 90.0 {
        hex(RED)
    } else if pct >= 60.0 {
        hex(AMBER)
    } else {
        t.dim
    }
}

/// Height of one running-subagent line under a session row. Every running subagent is
/// listed (up to a sanity cap).
const SUB_H: f32 = 16.0;
const MAX_SUBS: usize = 12;
/// Height of the Approve / Deny strip on a row that is waiting for permission.
const DECIDE_H: f32 = 30.0;

/// Height of a session row including its subagent lines and decision buttons.
fn row_height(s: &crate::model::Session) -> f32 {
    let subs = s.subs.len().min(MAX_SUBS);
    ROW_H + if subs > 0 { subs as f32 * SUB_H + 4.0 } else { 0.0 } + if s.pending.is_some() { DECIDE_H } else { 0.0 }
}

/// How much height the session list may take before it scrolls.
fn list_budget(a: &App) -> f32 {
    if a.edge.vertical() { 264.0 } else { 232.0 }
}

/// Which rows are on screen: (index of the first, how many). Rows have different heights,
/// so as many as fit the budget are shown; the first one always is.
pub fn visible_rows(a: &App) -> (usize, usize) {
    let n = a.store.sessions.len();
    if n == 0 {
        return (0, 0);
    }
    let first = a.scroll.min(n - 1);
    let (mut used, mut count) = (0.0, 0);
    for s in a.store.sessions.iter().skip(first) {
        let h = row_height(s);
        if count > 0 && used + h > list_budget(a) {
            break;
        }
        used += h;
        count += 1;
    }
    (first, count)
}

/// Height of the visible part of the session list.
fn list_height(a: &App) -> f32 {
    let (first, count) = visible_rows(a);
    if count == 0 {
        return ROW_H;
    }
    a.store.sessions.iter().skip(first).take(count).map(row_height).sum()
}

/// Height of the expanded panel for the current view.
pub fn content_height(a: &App) -> f32 {
    match a.view {
        View::Main => {
            let stale = if usage_stale(a).is_some() { 14.0 } else { 0.0 };
            // side layout stacks the two toggles
            // two toggles (stacked on the side layout) and the travel mode button under them
            let toggles = if panel_width(a) < 340.0 { 36.0 * 3.0 + 12.0 } else { 36.0 * 2.0 + 6.0 };
            HEADER_H + list_height(a) + 8.0 + 48.0 + stale + 8.0 + toggles + 12.0
        }
        View::Settings => HEADER_H + SETTINGS.len() as f32 * SET_ROW_H + 10.0,
    }
}

/// Corner radius of a free-floating panel: a full pill when collapsed, 20 when open.
pub fn panel_radius(open: f32, h: f32) -> f32 {
    ((PILL_H / 2.0) + (20.0 - PILL_H / 2.0) * open.clamp(0.0, 1.0)).min(h / 2.0)
}

pub fn draw(a: &App, hits: &mut Vec<Hit>) {
    hits.clear();
    a.pulses.borrow_mut().clear();
    let g = &a.gfx;
    let t = &a.theme;
    let (min_w, min_h) = collapsed_size(a.edge);
    let (x, y, w, h) = (a.rect[0].x, a.rect[1].x, a.rect[2].x.max(min_w.min(PILL_H)), a.rect[3].x.max(min_h.min(PILL_H)));
    let open = a.open.x.clamp(0.0, 1.0);

    if a.edge == Edge::Free {
        // floating: a pill with a hairline edge
        let radius = panel_radius(open, h);
        g.fill_rr(x, y, w, h, radius, t.bg);
        g.fill_rr_grad(x, y, w, h.min(150.0), radius, t.sheen, t.sheen.a(0.0));
        g.stroke_rr(x + 0.5, y + 0.5, w - 1.0, h - 1.0, radius - 0.5, t.border, 1.0);
    } else {
        // docked: grows out of the screen edge like a notch
        let radius = 11.0 + 9.0 * open;
        g.notch(x, y, w, h, radius, EAR, a.edge.side(), t.bg);
    }
    g.clip(x, y, w, h);
    if open < 0.98 {
        g.alpha.set((1.0 - open * 1.6).max(0.0));
        if a.edge.vertical() {
            collapsed_vertical(a, g, t, x, y, w, h);
        } else {
            // the strip stays against the docked edge while the panel grows away from it
            let sy = if a.edge == Edge::Bottom { y + h - PILL_H } else { y };
            collapsed(a, g, t, x, sy, w);
        }
    }
    if open > 0.02 {
        g.alpha.set(((open - 0.25) / 0.75).clamp(0.0, 1.0));
        match a.view {
            View::Main => expanded(a, g, t, x, y, w, hits),
            View::Settings => settings(a, g, t, x, y, w, hits),
        }
    }
    g.alpha.set(1.0);
    g.unclip();

    if !a.expanded {
        hits.clear();
        hits.push(Hit { x, y, w, h, action: Action::Header });
    }
}

/// Collapsed content for the left / right edge: count on top, one dot per session below,
/// The window to show on the small collapsed bar: the 5-hour one while a session block is
/// running, otherwise the weekly one (so the bar is not blank between sessions).
fn pill_window(a: &App) -> Option<f32> {
    a.usage
        .five_hour
        .as_ref()
        .filter(|u| u.has_bar())
        .or_else(|| a.usage.week.as_ref().filter(|u| u.has_bar()))
        .and_then(|u| u.pct)
}

/// usage as a small vertical bar at the bottom.
fn collapsed_vertical(a: &App, g: &Gfx, t: &Theme, x: f32, y: f32, w: f32, h: f32) {
    let sx = if a.edge == Edge::Right { x + w - VPILL_W } else { x };
    let top = y + h / 2.0 - VPILL_H / 2.0;
    let cx = sx + VPILL_W / 2.0;
    let mut bottom = top + VPILL_H - 12.0;

    if let Some(p) = pill_window(a) {
        let bh = 26.0;
        bottom -= bh;
        g.fill_rr(cx - 2.0, bottom, 4.0, bh, 2.0, t.track);
        let c = if p < 60.0 { t.text.a(0.75) } else { usage_color(p, t) };
        let fill = (bh * p / 100.0).clamp(2.0, bh);
        g.fill_rr(cx - 2.0, bottom + bh - fill, 4.0, fill, 2.0, c);
        bottom -= 6.0;
    }
    if a.awake {
        bottom -= 14.0;
        g.text("\u{E706}", Font::Icon, sx, bottom, VPILL_W, 14.0, Align::Center, hex(AMBER));
        bottom -= 3.0;
    }
    if a.rc != RcState::Off {
        bottom -= 14.0;
        g.text("\u{E8EA}", Font::Icon, sx, bottom, VPILL_W, 14.0, Align::Center, hex(BLUE));
        bottom -= 3.0;
    }
    let n = a.store.sessions.len();
    if n == 0 {
        g.dot(cx, top + 18.0, 3.5, t.dim.a(0.6));
        return;
    }
    g.text(&a.store.active_count().to_string(), Font::Bold, sx, top + 8.0, VPILL_W, 18.0, Align::Center, t.text);
    let mut dy = top + 38.0;
    for s in a.store.sessions.iter().take(SIDE_DOTS) {
        if dy + 6.0 > bottom {
            break;
        }
        status_dot(a, g, cx, dy, 4.0, s, None);
        dy += 13.0;
    }
    if n > SIDE_DOTS && dy + 8.0 <= bottom {
        // more sessions than dots: an arrow says the list continues (hover to open and scroll)
        g.text("\u{E70D}", Font::Icon, sx, dy - 7.0, VPILL_W, 14.0, Align::Center, t.dim);
    }
}

fn collapsed(a: &App, g: &Gfx, t: &Theme, x: f32, y: f32, w: f32) {
    let left = x + w / 2.0 - PILL_W / 2.0;
    let right = left + PILL_W;
    let cy = y + PILL_H / 2.0;
    let n = a.store.sessions.len();
    let mut rx = right - 14.0;

    // tiny usage bar (5-hour window)
    if let Some(p) = pill_window(a) {
        let bw = 34.0;
        rx -= bw;
        g.fill_rr(rx, cy - 2.0, bw, 4.0, 2.0, t.track);
        let c = if p < 60.0 { t.text.a(0.75) } else { usage_color(p, t) };
        g.fill_rr(rx, cy - 2.0, (bw * p / 100.0).clamp(2.0, bw), 4.0, 2.0, c);
        rx -= 8.0;
    }
    if a.awake {
        rx -= 14.0;
        g.text("\u{E706}", Font::Icon, rx, y, 14.0, PILL_H, Align::Center, hex(AMBER));
        rx -= 4.0;
    }
    if a.rc != RcState::Off {
        rx -= 14.0;
        g.text("\u{E8EA}", Font::Icon, rx, y, 14.0, PILL_H, Align::Center, hex(BLUE));
        rx -= 4.0;
    }

    if n == 0 {
        g.dot(left + 17.0, cy, 3.5, t.dim.a(0.6));
        g.text("AgentNotch", Font::Body, left + 28.0, y, 110.0, PILL_H, Align::Left, t.dim);
        return;
    }
    let active = a.store.active_count();
    let label = active.to_string();
    g.text(&label, Font::Bold, left + 13.0, y - 0.5, 22.0, PILL_H, Align::Left, t.text);
    let mut dx = left + 13.0 + g.measure(&label, Font::Bold) + 11.0;
    for s in a.store.sessions.iter() {
        if dx + 6.0 > rx {
            break;
        }
        status_dot(a, g, dx, cy, 4.0, s, None);
        dx += 13.0;
    }
}

/// A dot that pulses, remembered so later frames can repaint it alone.
#[derive(Clone, Copy)]
pub struct PulseDot {
    pub cx: f32,
    pub cy: f32,
    pub r: f32,
    pub state: State,
    /// Translucent fill that lies between the panel background and the dot (a hovered or
    /// selected row), if any.
    pub under: Option<Color>,
}

impl PulseDot {
    /// The area a pulse can touch.
    pub fn bounds(&self) -> [f32; 4] {
        let m = self.r + 4.5;
        [self.cx - m, self.cy - m, self.cx + m, self.cy + m]
    }
}

/// Draw one status dot at animation phase `phase` (0..1 loop).
pub fn paint_dot(g: &Gfx, phase: f32, cx: f32, cy: f32, r: f32, s: State, pulsing: bool) {
    let c = state_color(s);
    if !pulsing {
        // working, or finished a while ago: a steady dot (and no redraw timer)
        g.dot(cx, cy, r, c);
        return;
    }
    // needs input (amber), just finished (green), just failed (red): pulse
    let wave = 0.5 - 0.5 * (phase * std::f32::consts::TAU).cos(); // 0..1..0
    g.ring(cx, cy, r + 0.5 + 2.5 * wave, c.a(0.6 * (1.0 - wave)), 1.0);
    g.dot(cx, cy, r, c.a(0.75 + 0.25 * wave));
}

/// One status dot in a full frame; pulsing ones are recorded for the cheap pulse frames.
fn status_dot(a: &App, g: &Gfx, cx: f32, cy: f32, r: f32, sess: &crate::model::Session, under: Option<Color>) {
    let pulsing = sess.pulsing(now_unix());
    let s = sess.state;
    paint_dot(g, a.phase, cx, cy, r, s, pulsing);
    if pulsing && g.alpha.get() > 0.99 {
        a.pulses.borrow_mut().push(PulseDot { cx, cy, r, state: s, under });
    }
}

/// Repaint only the pulsing dots over the frame that is already on screen.
pub fn draw_pulses(a: &App, dots: &[PulseDot]) {
    let g = &a.gfx;
    for d in dots {
        let [x0, y0, x1, y1] = d.bounds();
        g.clip(x0, y0, x1 - x0, y1 - y0);
        g.fill_rr(x0 - 1.0, y0 - 1.0, x1 - x0 + 2.0, y1 - y0 + 2.0, 0.0, a.theme.bg);
        if let Some(c) = d.under {
            g.fill_rr(x0 - 1.0, y0 - 1.0, x1 - x0 + 2.0, y1 - y0 + 2.0, 0.0, c);
        }
        paint_dot(g, a.phase, d.cx, d.cy, d.r, d.state, true);
        g.unclip();
    }
}

fn expanded(a: &App, g: &Gfx, t: &Theme, x: f32, y: f32, w: f32, hits: &mut Vec<Hit>) {
    let hov = |act: &Action| a.hover.as_ref() == Some(act);
    hits.push(Hit { x, y, w, h: HEADER_H, action: Action::Header });
    let n = a.store.sessions.len();
    let active = a.store.active_count();
    let title = match (n, active) {
        (0, _) => "AgentNotch".to_string(),
        (1, 0) => "1 session, idle".to_string(),
        (_, 0) => format!("{n} sessions, idle"),
        (_, 1) => "1 active session".to_string(),
        _ => format!("{active} active sessions"),
    };
    g.text(&title, Font::Bold, x + 16.0, y, 220.0, HEADER_H, Align::Left, t.text);
    // gear
    let gx = x + w - 38.0;
    if hov(&Action::Gear) {
        g.fill_rr(gx, y + 5.0, 26.0, 24.0, 7.0, t.hover);
    }
    g.text("\u{E713}", Font::Icon, gx, y + 5.0, 26.0, 24.0, Align::Center, t.dim);
    hits.push(Hit { x: gx, y: y + 5.0, w: 26.0, h: 24.0, action: Action::Gear });

    // sessions
    let mut cy = y + HEADER_H;
    let now = now_unix();
    if n == 0 {
        g.text("No agents running right now.", Font::Body, x + 16.0, cy, w - 32.0, ROW_H, Align::Left, t.dim);
        cy += ROW_H;
    } else {
        let (first, shown) = visible_rows(a);
        for (i, s) in a.store.sessions.iter().enumerate().skip(first).take(shown) {
            let act = Action::Session(i);
            let close = Action::Dismiss(i);
            let selected = a.selected.as_deref() == Some(s.id.as_str());
            let hot = hov(&act) || hov(&close) || hov(&Action::Approve(i)) || hov(&Action::Deny(i));
            let rh = row_height(s);
            if hot || selected {
                g.fill_rr(x + 6.0, cy + 1.0, w - 12.0, rh - 2.0, 12.0, if selected { t.chip } else { t.hover });
            }
            g.fill_rr(x + 14.0, cy + 8.0, 24.0, 24.0, 8.0, t.chip);
            g.stroke_rr(x + 14.5, cy + 8.5, 23.0, 23.0, 7.5, t.line, 1.0);
            g.logo(s.agent.logo(), x + 14.0 + 4.5, cy + 8.0 + 4.5, 15.0, t.text);
            let el = match s.finished {
                Some(f) if !s.active() => format!("{} ago", fmt_dur(now.saturating_sub(f))),
                _ => fmt_dur(now.saturating_sub(s.started)),
            };
            if hot {
                // hovering a row swaps the elapsed time for a dismiss button
                let (bx, by) = (x + w - 32.0, cy + 3.0);
                g.fill_rr(bx, by, 18.0, 18.0, 9.0, if hov(&close) { t.track } else { t.chip });
                g.line(bx + 6.0, by + 6.0, bx + 12.0, by + 12.0, if hov(&close) { t.text } else { t.dim }, 1.3);
                g.line(bx + 12.0, by + 6.0, bx + 6.0, by + 12.0, if hov(&close) { t.text } else { t.dim }, 1.3);
            } else {
                g.text(&el, Font::Small, x + w - 94.0, cy + 4.0, 80.0, 16.0, Align::Right, t.dim);
            }
            let st = match s.state {
                State::Working => "working",
                State::NeedsInput => "needs input",
                State::Done => "done",
                State::Error => "error",
            };
            let sw = g.measure(st, Font::Small);
            g.text(st, Font::Small, x + w - 94.0, cy + 20.0, 80.0, 16.0, Align::Right, state_color(s.state));
            let under = if selected { Some(t.chip) } else if hot { Some(t.hover) } else { None };
            status_dot(a, g, x + w - 14.0 - sw - 8.0, cy + 28.5, 3.0, s, under);
            g.text(s.name(), Font::Bold, x + 48.0, cy + 3.0, w - 48.0 - 100.0, 18.0, Align::Left, t.text);
            g.text(&s.step, Font::Small, x + 48.0, cy + 20.0, w - 48.0 - 108.0, 16.0, Align::Left, t.dim);
            // subagents running inside this session
            for (k, sub) in s.subs.iter().take(MAX_SUBS).enumerate() {
                let sy = cy + ROW_H - 2.0 + k as f32 * SUB_H;
                // elbow connector from the session's icon down to the line
                g.line(x + 26.0, sy - 1.0, x + 26.0, sy + 8.0, t.track, 1.0);
                g.line(x + 26.0, sy + 8.0, x + 36.0, sy + 8.0, t.track, 1.0);
                let hidden = s.subs.len() - MAX_SUBS.min(s.subs.len());
                let text = if k == MAX_SUBS - 1 && hidden > 0 {
                    format!("+ {} more subagents", hidden + 1)
                } else {
                    let kind = if sub.kind.is_empty() { "subagent" } else { sub.kind.as_str() };
                    if sub.step.is_empty() { kind.to_string() } else { format!("{kind} · {}", sub.step) }
                };
                g.dot(x + 44.0, sy + 8.0, 2.5, hex(BLUE));
                g.text(&text, Font::Small, x + 53.0, sy, w - 53.0 - 16.0, SUB_H, Align::Left, t.dim);
            }
            hits.push(Hit { x: x + 6.0, y: cy, w: w - 12.0, h: rh, action: act });
            hits.push(Hit { x: x + w - 36.0, y: cy, w: 26.0, h: 24.0, action: close });
            if s.pending.is_some() {
                // answer the permission request right here
                let by = cy + rh - DECIDE_H + 2.0;
                for (label, action, color, bx, bw) in [("Approve", Action::Approve(i), GREEN, x + 48.0, 80.0), ("Deny", Action::Deny(i), RED, x + 136.0, 62.0)] {
                    let over = hov(&action);
                    g.fill_rr(bx, by, bw, 22.0, 11.0, hex(color).a(if over { 0.42 } else { 0.24 }));
                    g.stroke_rr(bx + 0.5, by + 0.5, bw - 1.0, 21.0, 10.5, hex(color).a(if over { 1.0 } else { 0.65 }), 1.0);
                    g.text(label, Font::Bold, bx, by - 0.5, bw, 22.0, Align::Center, t.text);
                    hits.push(Hit { x: bx, y: by, w: bw, h: 22.0, action });
                }
            }
            cy += rh;
        }
        if shown < n {
            // scroll hint
            let frac = first as f32 / (n - 1).max(1) as f32;
            let track_h = list_height(a) - 12.0;
            let thumb = (track_h * shown as f32 / n as f32).max(14.0);
            g.fill_rr(x + w - 5.0, y + HEADER_H + 6.0 + (track_h - thumb) * frac, 2.5, thumb, 1.2, t.track);
        }
    }

    // usage
    cy += 4.0;
    g.line(x + 16.0, cy, x + w - 16.0, cy, t.line, 1.0);
    cy += 4.0;
    hits.push(Hit { x: x + 6.0, y: cy, w: w - 12.0, h: 48.0, action: Action::Usage });
    if hov(&Action::Usage) {
        g.fill_rr(x + 6.0, cy + 2.0, w - 12.0, 44.0, 12.0, t.hover);
    }
    let has_row = |u: &Option<UsageWindow>| u.as_ref().map(|u| u.has_bar() || !u.active).unwrap_or(false);
    if !has_row(&a.usage.five_hour) && !has_row(&a.usage.week) {
        // no percentages to show: say why in one line instead of raw token counts
        let first = if a.usage.loading { "Usage limits: loading…" } else { "Usage limits: not available right now" };
        let why = a.usage.note.split(';').next().unwrap_or("").trim();
        let second = if a.usage.loading || why.is_empty() { "Click to retry".to_string() } else { format!("{why} · click to retry") };
        g.text(first, Font::Small, x + 16.0, cy + 6.0, w - 32.0, 18.0, Align::Left, t.text);
        g.text(&second, Font::Small, x + 16.0, cy + 24.0, w - 32.0, 18.0, Align::Left, t.dim);
    } else {
        usage_row(g, t, x, cy + 4.0, w, "5h", a.usage.five_hour.as_ref(), a.usage.source);
        usage_row(g, t, x, cy + 24.0, w, "Week", a.usage.week.as_ref(), a.usage.source);
    }
    cy += 48.0;
    // a meta line: which plan the numbers are scaled to, and how old they are
    let mut meta: Vec<String> = Vec::new();
    if !a.usage.plan.is_empty() {
        meta.push(format!("{} plan", a.usage.plan));
    }
    if let Some(age) = usage_stale(a) {
        let why = if a.usage.note.contains("rate limited") { ", endpoint is rate limiting" } else { "" };
        meta.push(format!("as of {} ago{why} · click to refresh", fmt_dur(age)));
    }
    if !meta.is_empty() {
        g.text(&meta.join(" · "), Font::Small, x + 16.0, cy - 2.0, w - 32.0, 14.0, Align::Left, t.dim);
        cy += 14.0;
    }
    cy += 8.0;

    // toggles
    let narrow = w < 340.0;
    let bw = if narrow { w - 32.0 } else { (w - 16.0 * 2.0 - 8.0) / 2.0 };
    let (rc_txt, rc_on) = a.rc_label();
    toggle(g, t, x + 16.0, cy, bw, "\u{E8EA}", "Remote Control", rc_txt, rc_on, hex(BLUE), hov(&Action::Remote));
    hits.push(Hit { x: x + 16.0, y: cy, w: bw, h: 36.0, action: Action::Remote });
    // side by side, or stacked on the narrow side layout
    let (ax, ay) = if narrow { (x + 16.0, cy + 42.0) } else { (x + 16.0 + bw + 8.0, cy) };
    toggle(g, t, ax, ay, bw, "\u{E706}", "Keep Awake", a.awake_label(), a.awake, hex(AMBER), hov(&Action::Awake));
    hits.push(Hit { x: ax, y: ay, w: bw, h: 36.0, action: Action::Awake });
    // travel mode: full width under the two toggles
    let ty = ay + 42.0;
    let on = a.cfg.travel_mode;
    let state = if on { "On · click to switch off" } else if narrow { "Off" } else { "Off · Remote Control, Keep Awake, hotspot reminder" };
    toggle(g, t, x + 16.0, ty, w - 32.0, "\u{E709}", "Travel Mode", state, on, hex(VIOLET), hov(&Action::Travel));
    hits.push(Hit { x: x + 16.0, y: ty, w: w - 32.0, h: 36.0, action: Action::Travel });
}

#[allow(clippy::too_many_arguments)]
fn usage_row(g: &Gfx, t: &Theme, x: f32, y: f32, w: f32, label: &str, u: Option<&UsageWindow>, source: &str) {
    g.text(label, Font::Small, x + 16.0, y, 36.0, 18.0, Align::Left, t.dim);
    let Some(u) = u else {
        g.text("n/a", Font::Small, x + 54.0, y, 100.0, 18.0, Align::Left, t.dim);
        return;
    };
    let narrow = w < 340.0;
    let bar_x = x + 54.0;
    let bar_w = if narrow { 64.0 } else { 104.0 };
    let mut tx = bar_x;
    if !u.active {
        g.text("idle · no active block", Font::Small, bar_x, y, 180.0, 18.0, Align::Left, t.dim);
        return;
    }
    if let Some(p) = u.pct {
        g.fill_rr(bar_x, y + 7.0, bar_w, 4.0, 2.0, t.track);
        let c = if p < 60.0 { t.text.a(0.85) } else { usage_color(p, t) };
        g.fill_rr(bar_x, y + 7.0, (bar_w * p / 100.0).clamp(4.0, bar_w), 4.0, 2.0, c);
        let est = if source == "estimate" { "~" } else { "" };
        g.text(&format!("{est}{p:.0}% used"), Font::Bold, bar_x + bar_w + 8.0, y, 70.0, 18.0, Align::Left, if p < 60.0 { t.text } else { c });
        tx = bar_x + bar_w + 78.0;
    } else if let Some(tok) = u.tokens {
        g.text(&format!("~{} tokens (local estimate)", fmt_tokens(tok)), Font::Small, bar_x, y, 180.0, 18.0, Align::Left, t.text);
        tx = bar_x + 180.0;
    }
    if let Some(r) = u.resets_at {
        let now = now_unix();
        let s = if r <= now {
            "resetting".to_string()
        } else if narrow {
            format!("in {}", fmt_dur(r - now))
        } else {
            format!("resets in {}", fmt_dur(r - now))
        };
        g.text(&s, Font::Small, tx, y, x + w - 16.0 - tx, 18.0, Align::Right, t.dim);
    }
}

#[allow(clippy::too_many_arguments)]
fn toggle(g: &Gfx, t: &Theme, x: f32, y: f32, w: f32, icon: &str, label: &str, state: &str, on: bool, accent: Color, hover: bool) {
    let bg = if on { accent.a(0.18) } else if hover { t.chip } else { t.hover };
    g.fill_rr(x, y, w, 36.0, 12.0, bg);
    let edge = if on { accent.a(if hover { 0.9 } else { 0.55 }) } else { t.line };
    g.stroke_rr(x + 0.5, y + 0.5, w - 1.0, 35.0, 11.5, edge, 1.0);
    g.text(icon, Font::Icon, x + 8.0, y, 22.0, 36.0, Align::Center, if on { accent } else { t.dim });
    g.text(label, Font::Bold, x + 34.0, y + 2.0, w - 40.0, 17.0, Align::Left, t.text);
    g.text(state, Font::Small, x + 34.0, y + 17.0, w - 40.0, 15.0, Align::Left, if on { accent } else { t.dim });
}

fn settings(a: &App, g: &Gfx, t: &Theme, x: f32, y: f32, w: f32, hits: &mut Vec<Hit>) {
    hits.push(Hit { x, y, w, h: HEADER_H, action: Action::Header });
    if a.hover.as_ref() == Some(&Action::Back) {
        g.fill_rr(x + 10.0, y + 5.0, 26.0, 24.0, 7.0, t.hover);
    }
    g.text("\u{E72B}", Font::Icon, x + 10.0, y + 5.0, 26.0, 24.0, Align::Center, t.dim);
    hits.push(Hit { x: x + 10.0, y: y + 5.0, w: 26.0, h: 24.0, action: Action::Back });
    g.text("Settings", Font::Bold, x + 42.0, y, 200.0, HEADER_H, Align::Left, t.text);

    let mut cy = y + HEADER_H;
    for (id, label) in SETTINGS {
        let act = Action::Set(id);
        if a.hover.as_ref() == Some(&act) {
            g.fill_rr(x + 6.0, cy, w - 12.0, SET_ROW_H, 7.0, t.hover);
        }
        g.text(label, Font::Body, x + 16.0, cy, 190.0, SET_ROW_H, Align::Left, t.text);
        let (val, on) = a.setting_value(id);
        let vw = (g.measure(&val, Font::Small) + 16.0).min(170.0);
        let vx = x + w - 16.0 - vw;
        g.fill_rr(vx, cy + 3.0, vw, SET_ROW_H - 6.0, 9.0, if on { hex(BLUE).a(0.28) } else { t.chip });
        g.text(&val, Font::Small, vx + 4.0, cy + 2.5, vw - 8.0, SET_ROW_H - 6.0, Align::Center, if on { t.text } else { t.dim });
        hits.push(Hit { x: x + 6.0, y: cy, w: w - 12.0, h: SET_ROW_H, action: act });
        cy += SET_ROW_H;
    }
}

pub fn hit_test(hits: &[Hit], x: f32, y: f32) -> Option<Action> {
    hits.iter().rev().find(|h| x >= h.x && x < h.x + h.w && y >= h.y && y < h.y + h.h).map(|h| h.action.clone())
}
