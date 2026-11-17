//! Developer timing; nothing happens unless asked for.
//!
//! `DRX_PERF=<file>`: every page opened writes how long its handler held the UI thread and when its first frame
//! was on screen; every 10 s, how many frames were drawn and how many pixels (an idle window should draw none).
//! `DRX_PERF_TOUR=<rounds>`: Mini walks its pages by itself, one every 1.5 s (timing and memory checks without
//! input); `DRX_PERF_TOUR_MS` changes the step.
//!
//! Always on, nearly free: a page that held the UI thread for more than [`SLOW`] writes one line with its steps to
//! `perf.log` in the profile (kept under 64 KB) — the owner's own runs show where a stall came from.
//! The heap is counted (an atomic add per allocation) for the memory lines of the developer log.

use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

struct Pending {
    name: String,
    start: Instant,
    handler: Option<Duration>,
}

static LOG: Mutex<Option<std::fs::File>> = Mutex::new(None);
/// The profile's `perf.log` for slow pages.
static SLOW_LOG: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);
const SLOW: Duration = Duration::from_millis(150);

/// Counts heap bytes in use (the system allocator underneath).
struct Counting;
static HEAP: AtomicU64 = AtomicU64::new(0);
static HEAP_PEAK: AtomicU64 = AtomicU64::new(0);

// SAFETY: forwards to the system allocator unchanged; only counts sizes.
unsafe impl std::alloc::GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: std::alloc::Layout) -> *mut u8 {
        let p = unsafe { std::alloc::System.alloc(l) };
        if !p.is_null() {
            let now = HEAP.fetch_add(l.size() as u64, Ordering::Relaxed) + l.size() as u64;
            HEAP_PEAK.fetch_max(now, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: std::alloc::Layout) {
        unsafe { std::alloc::System.dealloc(p, l) };
        HEAP.fetch_sub(l.size() as u64, Ordering::Relaxed);
    }
    unsafe fn alloc_zeroed(&self, l: std::alloc::Layout) -> *mut u8 {
        let p = unsafe { std::alloc::System.alloc_zeroed(l) };
        if !p.is_null() {
            let now = HEAP.fetch_add(l.size() as u64, Ordering::Relaxed) + l.size() as u64;
            HEAP_PEAK.fetch_max(now, Ordering::Relaxed);
        }
        p
    }
    unsafe fn realloc(&self, p: *mut u8, l: std::alloc::Layout, new: usize) -> *mut u8 {
        let q = unsafe { std::alloc::System.realloc(p, l, new) };
        if !q.is_null() {
            if new >= l.size() {
                let now = HEAP.fetch_add((new - l.size()) as u64, Ordering::Relaxed) + (new - l.size()) as u64;
                HEAP_PEAK.fetch_max(now, Ordering::Relaxed);
            } else {
                HEAP.fetch_sub((l.size() - new) as u64, Ordering::Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Heap bytes in use now and at most.
pub fn heap() -> (u64, u64) {
    (HEAP.load(Ordering::Relaxed), HEAP_PEAK.load(Ordering::Relaxed))
}

thread_local! {
    /// Steps of the page being opened (name, time since it began), written out only when it was slow.
    static STEPS: std::cell::RefCell<Vec<(&'static str, Duration)>> = const { std::cell::RefCell::new(Vec::new()) };
    static PAGE_START: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// Marks a step inside a page opening (for the slow-page line).
pub fn step(name: &'static str) {
    if let Some(t) = PAGE_START.with(|p| p.get()) {
        STEPS.with(|s| s.borrow_mut().push((name, t.elapsed())));
    }
}

/// Where slow pages are written (the profile folder).
pub fn set_profile(root: &std::path::Path) {
    *SLOW_LOG.lock().expect("perf") = Some(root.join("perf.log"));
}

fn slow_line(name: &str, took: Duration) {
    let steps = STEPS.with(|s| {
        s.borrow().iter().map(|(n, t)| format!("{n} {:.0}", t.as_secs_f64() * 1000.0)).collect::<Vec<_>>().join(", ")
    });
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let line = format!("{secs} slow page {name}: {:.0} ms [{steps}]", took.as_secs_f64() * 1000.0);
    log(&line);
    let Some(path) = SLOW_LOG.lock().expect("perf").clone() else { return };
    // Keep the file small: start over past 64 KB.
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 64 * 1024) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{line}");
    }
}
static PENDING: Mutex<Option<Pending>> = Mutex::new(None);
static FRAMES: AtomicU64 = AtomicU64::new(0);
static PIXELS: AtomicU64 = AtomicU64::new(0);
static DRAW_US: AtomicU64 = AtomicU64::new(0);

pub fn on() -> bool {
    LOG.lock().map(|l| l.is_some()).unwrap_or(false)
}

pub fn log(line: &str) {
    if let Ok(mut l) = LOG.lock()
        && let Some(f) = l.as_mut()
    {
        let _ = writeln!(f, "{line}");
    }
}

fn on_frame(draw: Duration, px: u64) {
    FRAMES.fetch_add(1, Ordering::Relaxed);
    PIXELS.fetch_add(px, Ordering::Relaxed);
    DRAW_US.fetch_add(draw.as_micros() as u64, Ordering::Relaxed);
    let done = {
        let mut p = PENDING.lock().expect("perf");
        match p.as_ref() {
            Some(x) if x.handler.is_some() => p.take(),
            _ => None,
        }
    };
    if let Some(p) = done {
        log(&format!(
            "page {:<10} handler {:>6.1} ms  first frame {:>6.1} ms  (draw {:.1} ms, {} kpx)",
            p.name,
            p.handler.unwrap_or_default().as_secs_f64() * 1000.0,
            p.start.elapsed().as_secs_f64() * 1000.0,
            draw.as_secs_f64() * 1000.0,
            px / 1000
        ));
    }
}

/// Starts timing when `DRX_PERF` is set.
pub fn init() {
    let Some(path) = std::env::var_os("DRX_PERF") else { return };
    let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(path) else { return };
    *LOG.lock().expect("perf") = Some(f);
    let _ = i_slint_backend_winit::drx_hooks::FRAME.set(on_frame);
    log(&format!("--- start pid {}", std::process::id()));
    let t = Box::leak(Box::new(slint::Timer::default()));
    t.start(slint::TimerMode::Repeated, Duration::from_secs(10), || {
        let f = FRAMES.swap(0, Ordering::Relaxed);
        let px = PIXELS.swap(0, Ordering::Relaxed);
        let us = DRAW_US.swap(0, Ordering::Relaxed);
        let (h, peak) = heap();
        let (covers, cover_bytes) = crate::covers::stats();
        log(&format!(
            "10 s: {f} frames, {} kpx, draw {:.1} ms | heap {:.1} MB (peak {:.1}), covers {covers} = {:.1} MB",
            px / 1000,
            us as f64 / 1000.0,
            h as f64 / 1048576.0,
            peak as f64 / 1048576.0,
            cover_bytes as f64 / 1048576.0
        ));
    });
}

/// Times one page opening: the handler ends when the guard drops, the frame is the next one drawn after that.
pub struct PageGuard {
    name: &'static str,
    start: Instant,
    dev: bool,
}

pub fn page(name: &'static str) -> PageGuard {
    let start = Instant::now();
    PAGE_START.with(|p| p.set(Some(start)));
    STEPS.with(|s| s.borrow_mut().clear());
    let dev = on();
    if dev {
        *PENDING.lock().expect("perf") = Some(Pending { name: name.into(), start, handler: None });
    }
    PageGuard { name, start, dev }
}

impl Drop for PageGuard {
    fn drop(&mut self) {
        let took = self.start.elapsed();
        PAGE_START.with(|p| p.set(None));
        if took > SLOW {
            slow_line(self.name, took);
        } else if self.dev && STEPS.with(|s| !s.borrow().is_empty()) {
            let steps = STEPS.with(|s| {
                s.borrow().iter().map(|(n, t)| format!("{n} {:.1}", t.as_secs_f64() * 1000.0)).collect::<Vec<_>>().join(", ")
            });
            log(&format!("  steps {}: {steps}", self.name));
        }
        if self.dev
            && let Some(p) = PENDING.lock().expect("perf").as_mut()
        {
            p.handler = Some(took);
        }
    }
}

/// `DRX_PERF_TOUR`: walks the pages by itself.
pub fn start_tour(ui: &crate::MainWindow, app: &drx_app::App) {
    let Some(rounds) = std::env::var("DRX_PERF_TOUR").ok().and_then(|v| v.parse::<u32>().ok()) else { return };
    let step_ms = std::env::var("DRX_PERF_TOUR_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(1500u64);
    let (ui_w, app) = (ui.as_weak(), app.clone());
    let n = std::cell::Cell::new(0u32);
    let t = Box::leak(Box::new(slint::Timer::default()));
    use slint::ComponentHandle;
    t.start(slint::TimerMode::Repeated, Duration::from_millis(step_ms), move || {
        let Some(ui) = ui_w.upgrade() else { return };
        use crate::nav::Page;
        use drx_app::ListSpec;
        let pages = [
            Page::home(),
            Page::settings(),
            Page::list(ListSpec::LikedSongs, "Любимые треки", "liked", None),
            Page::history(),
            Page::stats(),
            Page::friends(),
            Page::list(ListSpec::Top { period: app.top_period(), size: app.top_size() }, "Мой топ", "top", None),
            Page::list(ListSpec::Downloads, "Загрузки", "", None),
        ];
        let i = n.get();
        if i >= rounds * pages.len() as u32 {
            if i == rounds * pages.len() as u32 {
                log("--- tour done");
                n.set(i + 1);
            }
            return;
        }
        n.set(i + 1);
        crate::nav::go_page(&ui, &app, pages[(i as usize) % pages.len()].clone());
    });
}
