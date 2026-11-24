//! Demo data (`--demo`) and off-screen snapshots (`--snapshot file.bmp`) for visual checks.
use crate::app::App;
use crate::gfx::Gfx;
use crate::model::{Agent, AgentEvent, EventKind, Usage, UsageWindow};
use crate::util::now_unix;

pub fn fill(a: &mut App) {
    let now = now_unix();
    let mk = |agent, id: &str, dir: &str, kind, ago: u64| {
        let mut e = AgentEvent::new(agent, id, dir, kind);
        e.at = now - ago;
        e
    };
    let evs = vec![
        mk(Agent::Claude, "d1", "C:\\dev\\webshop", EventKind::Working { step: "start".into() }, 754),
        mk(Agent::Claude, "d1", "C:\\dev\\webshop", EventKind::Working { step: "Bash: npx vitest run".into() }, 3),
        mk(Agent::Claude, "d2", "C:\\dev\\notch", EventKind::Working { step: "start".into() }, 190),
        mk(Agent::Claude, "d2", "C:\\dev\\notch", EventKind::NeedsInput { reason: "Permission: Bash(cargo build)".into() }, 12),
        mk(Agent::Codex, "d3", "C:\\dev\\api-server", EventKind::Working { step: "apply_patch src/routes.ts".into() }, 61),
        mk(Agent::T3Code, "d4", "C:\\dev\\landing", EventKind::Working { step: "start".into() }, 2400),
        mk(Agent::T3Code, "d4", "C:\\dev\\landing", EventKind::Done, 95),
        mk(Agent::Claude, "d5", "C:\\dev\\infra", EventKind::Working { step: "start".into() }, 400),
        mk(Agent::Claude, "d5", "C:\\dev\\infra", EventKind::Error { message: "API Error: rate limit reached".into() }, 200),
    ];
    for e in evs {
        a.store.apply(e);
    }
    // two subagents under the first session
    for (id, kind, step) in [("s1", "Explore", "Grep \"checkout\" in src"), ("s2", "general-purpose", "Bash: npx tsc --noEmit")] {
        let mut e = AgentEvent::new(Agent::Claude, "d1", "", EventKind::Info);
        e.sub = Some(crate::model::SubEvent { id: id.into(), kind: kind.into(), step: Some(step.into()), ended: false });
        a.store.apply(e);
    }
    a.usage = Usage {
        five_hour: Some(UsageWindow { pct: Some(72.0), tokens: None, resets_at: Some(now + 2 * 3600 + 13 * 60), active: true }),
        week: Some(UsageWindow { pct: Some(34.0), tokens: None, resets_at: Some(now + 3 * 86400 + 5 * 3600), active: true }),
        source: "api",
        note: String::new(),
        fetched_at: now,
        loading: false,
    };
}

/// Write the canvas, composited over mid-grey, as a 32-bit BMP.
pub fn save_bmp(g: &Gfx, path: &str) -> i32 {
    let (w, h) = (g.w, g.h);
    let src = g.pixels();
    let mut out = Vec::with_capacity(54 + src.len());
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(54 + src.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&(-h).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0u8; 24]);
    let bg = [0x90u32, 0x80, 0x70];
    for p in src.chunks_exact(4) {
        let inv = 255 - p[3] as u32;
        for c in 0..3 {
            out.push((p[c] as u32 + bg[c] * inv / 255).min(255) as u8);
        }
        out.push(255);
    }
    match std::fs::write(path, out) {
        Ok(_) => 0,
        Err(e) => {
            crate::logln!("snapshot failed: {e}");
            1
        }
    }
}
