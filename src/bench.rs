//! `--bench [seconds]`: log working set and CPU once a second, then a summary.
//! Output goes to %LOCALAPPDATA%\Notch\logs\bench.log (and stdout when redirected).
use std::io::Write;
use std::time::Instant;
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

/// PROCESS_MEMORY_COUNTERS_EX2 (Windows 11 22H2+).
#[repr(C)]
#[derive(Default)]
struct Pmc2 {
    cb: u32,
    page_fault_count: u32,
    peak_working_set: usize,
    working_set: usize,
    quota_peak_paged: usize,
    quota_paged: usize,
    quota_peak_nonpaged: usize,
    quota_nonpaged: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
    private_usage: usize,
    private_working_set: usize,
    shared_commit: u64,
}

pub struct Sample {
    pub working_set: f64,
    /// 0 when the OS does not report it.
    pub private_ws: f64,
    pub cpu_time: f64,
}

pub fn sample() -> Sample {
    unsafe {
        let p = GetCurrentProcess();
        let mut m = Pmc2 { cb: std::mem::size_of::<Pmc2>() as u32, ..Default::default() };
        if !K32GetProcessMemoryInfo(p, &mut m as *mut _ as *mut PROCESS_MEMORY_COUNTERS, m.cb).as_bool() {
            // older Windows: EX layout only (no private working set)
            m = Pmc2 { cb: (std::mem::size_of::<Pmc2>() - 16) as u32, ..Default::default() };
            let _ = K32GetProcessMemoryInfo(p, &mut m as *mut _ as *mut PROCESS_MEMORY_COUNTERS, m.cb);
            m.private_working_set = 0;
        }
        let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        let _ = GetProcessTimes(p, &mut c, &mut e, &mut k, &mut u);
        let ft = |f: FILETIME| ((f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64) as f64 / 1e7;
        Sample { working_set: m.working_set as f64 / 1048576.0, private_ws: m.private_working_set as f64 / 1048576.0, cpu_time: ft(k) + ft(u) }
    }
}

static FRAMES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static DRAW_MICROS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Record the drawing part of a pulse frame (the rest is presenting it).
pub fn draw(d: std::time::Duration) {
    DRAW_MICROS.fetch_add(d.as_micros() as u64, std::sync::atomic::Ordering::Relaxed);
}

static FRAME_MICROS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Record one rendered frame (cheap; always on).
pub fn frame(d: std::time::Duration) {
    FRAMES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    FRAME_MICROS.fetch_add(d.as_micros() as u64, std::sync::atomic::Ordering::Relaxed);
}

pub struct Bench {
    secs: u32,
    n: u32,
    last_cpu: f64,
    last_t: Instant,
    cpu_sum: f64,
    cpu_max: f64,
    ws_max: f64,
    pws_max: f64,
    file: Option<std::fs::File>,
    cores: f64,
}

impl Bench {
    pub fn new(secs: u32) -> Bench {
        let file = std::fs::File::create(crate::log::dir().join("bench.log")).ok();
        let s = sample();
        let cores = std::thread::available_parallelism().map(|n| n.get() as f64).unwrap_or(1.0);
        let mut b = Bench { secs, n: 0, last_cpu: s.cpu_time, last_t: Instant::now(), cpu_sum: 0.0, cpu_max: 0.0, ws_max: 0.0, pws_max: 0.0, file, cores };
        b.out(&format!("# notch --bench {secs}s, {cores} logical cores. cpu = share of the whole machine (Task Manager), cpu1 = share of one core."));
        b
    }

    fn out(&mut self, line: &str) {
        if let Some(f) = self.file.as_mut() {
            let _ = writeln!(f, "{line}");
        }
        let _ = writeln!(std::io::stdout(), "{line}");
    }

    /// Called once a second. Returns true when the run is finished.
    pub fn tick(&mut self) -> bool {
        let s = sample();
        let dt = self.last_t.elapsed().as_secs_f64().max(0.001);
        let one = (s.cpu_time - self.last_cpu) / dt * 100.0;
        self.last_cpu = s.cpu_time;
        self.last_t = Instant::now();
        self.n += 1;
        self.cpu_sum += one;
        self.cpu_max = self.cpu_max.max(one);
        self.ws_max = self.ws_max.max(s.working_set);
        self.pws_max = self.pws_max.max(s.private_ws);
        let frames = FRAMES.swap(0, std::sync::atomic::Ordering::Relaxed);
        let micros = FRAME_MICROS.swap(0, std::sync::atomic::Ordering::Relaxed);
        let draw = DRAW_MICROS.swap(0, std::sync::atomic::Ordering::Relaxed);
        self.out(&format!(
            "t={:>3}s  working_set={:>6.2} MB  private_ws={:>6.2} MB  cpu={:>5.2}%  cpu1={:>5.2}%  frames={:>3}  render={:>5.1} ms (pulse draw {:>4.1} ms)",
            self.n,
            s.working_set,
            s.private_ws,
            one / self.cores,
            one,
            frames,
            micros as f64 / 1000.0,
            draw as f64 / 1000.0
        ));
        if self.n >= self.secs {
            let avg = self.cpu_sum / self.n as f64;
            self.out(&format!(
                "SUMMARY  avg_cpu={:.3}%  peak_cpu={:.2}%  avg_cpu1={:.3}%  peak_cpu1={:.2}%  peak_working_set={:.2} MB  peak_private_ws={:.2} MB",
                avg / self.cores,
                self.cpu_max / self.cores,
                avg,
                self.cpu_max,
                self.ws_max,
                self.pws_max
            ));
            true
        } else {
            false
        }
    }
}
