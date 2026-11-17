//! Finished cover pictures (corners baked in) on the UI thread, bounded by bytes.
//!
//! Before, every row asked for its cover again each time Slint read the row, and every answer was a fresh copy of
//! the pixels from the core's cache: the same cover lived in memory several times and the heap kept growing with
//! scrolling. Now the core hands a decoded cover over once (`App::take_image`), the finished `slint::Image` is kept
//! here and shared by every row that shows it (cloning an image is a reference count).

use drx_app::App;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

/// Bytes of pixels kept here. Rows hold their pictures while they are on screen, so this is about what comes back
/// when scrolling or returning to a page.
const LIMIT: usize = 16 << 20;

type Key = (String, u32, u32);

#[derive(Default)]
struct Cache {
    map: HashMap<Key, (slint::Image, usize)>,
    /// Least recently used first.
    order: VecDeque<Key>,
    bytes: usize,
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::default();
}

/// The cover at `size` px with `radius` px corners; None (and a load started) when it is not decoded yet.
pub fn get(
    app: &App,
    url: &str,
    size: u32,
    radius: f32,
    round: impl FnOnce(&mut [slint::Rgba8Pixel], usize, f32),
) -> Option<slint::Image> {
    let radius = if radius >= 0.5 { radius } else { 0.0 };
    let key: Key = (url.to_string(), size, radius.to_bits());
    let hit = CACHE.with(|c| {
        let mut c = c.borrow_mut();
        let img = c.map.get(&key)?.0.clone();
        // Move to the back only when it is not there already (cheap for the rows on screen).
        if c.order.back() != Some(&key)
            && let Some(pos) = c.order.iter().rposition(|k| *k == key)
        {
            let k = c.order.remove(pos).expect("position is valid");
            c.order.push_back(k);
        }
        Some(img)
    });
    if hit.is_some() {
        return hit;
    }
    let Some(p) = app.take_image(url, size) else {
        app.request_image(url, size);
        return None;
    };
    let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&p.rgba, p.size, p.size);
    if radius > 0.0 {
        round(buf.make_mut_slice(), p.size as usize, radius.min(p.size as f32 / 2.0));
    }
    let bytes = p.rgba.len();
    drop(p);
    let img = slint::Image::from_rgba8(buf);
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        c.bytes += bytes;
        if let Some((_, old)) = c.map.insert(key.clone(), (img.clone(), bytes)) {
            c.bytes -= old;
        } else {
            c.order.push_back(key);
        }
        while c.bytes > LIMIT {
            let Some(k) = c.order.pop_front() else { break };
            if let Some((_, b)) = c.map.remove(&k) {
                c.bytes -= b;
            }
        }
    });
    Some(img)
}

/// Whether the cover is ready to show at `size` (in any corner radius, or waiting in the core).
pub fn ready(app: &App, url: &str, size: u32) -> bool {
    CACHE.with(|c| c.borrow().map.keys().any(|(u, s, _)| *s == size && u == url)) || app.image(url, size).is_some()
}

/// What is kept now: (pictures, bytes) — for the developer log.
pub fn stats() -> (usize, usize) {
    CACHE.with(|c| {
        let c = c.borrow();
        (c.map.len(), c.bytes)
    })
}
