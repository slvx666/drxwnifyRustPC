//! Covers and avatars: download once to a disk cache, decode at the size the UI shows, keep a small LRU in memory
//! bounded by bytes (Mini: 32 MB, Full: 64 MB — EditionProfile.image_cache_bytes).
//!
//! Yandex cover templates (`…/%%`) are expanded before use; Spotify/VK/YouTube URLs are used as they come.

use crate::http::Http;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// A decoded square image, RGBA8.
#[derive(Debug)]
pub struct Pixels {
    pub size: u32,
    pub rgba: Vec<u8>,
}

struct Lru {
    map: HashMap<(String, u32), Arc<Pixels>>,
    order: VecDeque<(String, u32)>,
    bytes: usize,
    limit: usize,
}

impl Lru {
    fn get(&mut self, k: &(String, u32)) -> Option<Arc<Pixels>> {
        let v = self.map.get(k)?.clone();
        if let Some(pos) = self.order.iter().position(|x| x == k) {
            let key = self.order.remove(pos).expect("position is valid");
            self.order.push_back(key);
        }
        Some(v)
    }
    fn put(&mut self, k: (String, u32), v: Arc<Pixels>) {
        self.bytes += v.rgba.len();
        if let Some(old) = self.map.insert(k.clone(), v) {
            self.bytes -= old.rgba.len();
        } else {
            self.order.push_back(k);
        }
        while self.bytes > self.limit {
            let Some(old) = self.order.pop_front() else {
                break;
            };
            if let Some(p) = self.map.remove(&old) {
                self.bytes -= p.rgba.len();
            }
        }
    }
}

#[derive(Clone)]
pub struct Images {
    http: Http,
    dir: PathBuf,
    lru: Arc<Mutex<Lru>>,
    pending: Arc<Mutex<HashSet<(String, u32)>>>,
    /// At most a few downloads/decodes at once: a decode of a 640 px cover briefly needs ~2 MB.
    gate: Arc<tokio::sync::Semaphore>,
}

/// Prefix of a local picture's address: `file:` + absolute path.
pub const LOCAL_PREFIX: &str = "file:";

fn file_name(url: &str) -> String {
    // FNV-1a: stable, no extra dependency.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in url.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// Yandex cover templates end in `%%`; ask for 200×200 (list tiles) or the requested size class.
pub fn normalize_url(url: &str, size: u32) -> String {
    if url.contains("%%") {
        let px = if size <= 200 {
            200
        } else if size <= 400 {
            400
        } else {
            1000
        };
        let u = url.replace("%%", &format!("{px}x{px}"));
        return if u.starts_with("http") {
            u
        } else {
            format!("https://{u}")
        };
    }
    if url.contains("googleusercontent.com") || url.contains("ggpht.com") {
        if let Some(at) = url.rfind("=w") {
            let tail = &url[at + 2..];
            if !tail.contains('/') && tail.split('-').next().is_some_and(|w| w.chars().all(|c| c.is_ascii_digit())) {
                let target = if size <= 64 { 120 } else if size <= 200 { 300 } else if size <= 400 { 600 } else { 800 };
                return format!("{}=w{target}-h{target}-l90-rj", &url[..at]);
            }
        }
    }
    if url.contains("i.scdn.co/image/") && size > 64 {
        let rep = if size <= 300 { "ab67616d00001e02" } else { "ab67616d0000b273" };
        return url.replace("ab67616d00004851", rep);
    }
    url.to_string()
}

/// The biggest variant of `url` its service serves under a predictable address (CoverSaver.largest): the picture
/// shown in a row is often a 300 px thumbnail of a much bigger original.
pub fn largest(url: &str) -> String {
    if url.contains("i.scdn.co/image/") {
        // Spotify: 300 px (…1e02…) / 64 px (…4851…) → 640 px (…b273…).
        return url
            .replace("ab67616d00001e02", "ab67616d0000b273")
            .replace("ab67616d00004851", "ab67616d0000b273");
    }
    if url.contains("googleusercontent.com") || url.contains("ggpht.com") {
        // Google / YouTube Music artwork carries its size at the end ("=w544-h544…").
        if let Some(at) = url.rfind("=w") {
            let tail = &url[at + 2..];
            if !tail.contains('/') && tail.split('-').next().is_some_and(|w| w.chars().all(|c| c.is_ascii_digit())) {
                return format!("{}=w1200-h1200-l90-rj", &url[..at]);
            }
        }
        return url.to_string();
    }
    if url.contains("i.ytimg.com/vi/") {
        for small in ["/hqdefault.jpg", "/mqdefault.jpg", "/sddefault.jpg", "/default.jpg"] {
            if let Some(base) = url.strip_suffix(small) {
                return format!("{base}/maxresdefault.jpg");
            }
        }
        return url.to_string();
    }
    if url.contains("%%") {
        return normalize_url(url, 1000);
    }
    url.to_string()
}

/// The shorter side of a local picture (`file:` address) in pixels, read from its header; None for web images.
pub fn local_side(url: &str) -> Option<u32> {
    let path = url.strip_prefix(LOCAL_PREFIX)?;
    let (w, h) = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.into_dimensions().ok()?;
    Some(w.min(h))
}

/// A file extension for an image by its first bytes (jpg when unknown).
pub fn image_extension(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        "png"
    } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "webp"
    } else if bytes.starts_with(b"GIF8") {
        "gif"
    } else {
        "jpg"
    }
}

impl Images {
    /// The original file of a picture in its largest size (falls back to `url` itself when the large one is
    /// missing — YouTube has no maxres thumbnail for some videos). For saving, not for showing.
    pub async fn original(&self, url: &str) -> Option<Vec<u8>> {
        if let Some(path) = url.strip_prefix(LOCAL_PREFIX) {
            return tokio::fs::read(path).await.ok();
        }
        let big = largest(url);
        let real = normalize_url(url, 1000);
        for u in [big, real] {
            let got = async {
                self.http
                    .send(self.http.request(crate::http::Method::GET, &u))
                    .await
                    .ok()?
                    .bytes()
                    .await
                    .ok()
            }
            .await;
            if let Some(b) = got.filter(|b| image::guess_format(b).is_ok()) {
                return Some(b.to_vec());
            }
        }
        None
    }

    pub fn new(http: Http, cache_dir: PathBuf, memory_limit: usize) -> Self {
        Self {
            http,
            dir: cache_dir.join("images"),
            lru: Arc::new(Mutex::new(Lru {
                map: HashMap::new(),
                order: VecDeque::new(),
                bytes: 0,
                limit: memory_limit,
            })),
            pending: Arc::default(),
            gate: Arc::new(tokio::sync::Semaphore::new(3)),
        }
    }

    /// The image if it is already decoded at this size.
    pub fn cached(&self, url: &str, size: u32) -> Option<Arc<Pixels>> {
        self.lru.lock().expect("lru").get(&(url.to_string(), size))
    }

    /// Takes the image out of the memory cache (the UI keeps its own copy, so the pixels are not held twice).
    pub fn take(&self, url: &str, size: u32) -> Option<Arc<Pixels>> {
        let mut lru = self.lru.lock().expect("lru");
        let key = (url.to_string(), size);
        let p = lru.map.remove(&key)?;
        lru.bytes -= p.rgba.len();
        if let Some(pos) = lru.order.iter().position(|x| *x == key) {
            lru.order.remove(pos);
        }
        Some(p)
    }

    /// Loads (disk cache or network), decodes and resizes. Returns None for broken images.
    pub async fn load(&self, url: &str, size: u32) -> Option<Arc<Pixels>> {
        let key = (url.to_string(), size);
        if let Some(p) = self.cached(url, size) {
            return Some(p);
        }
        if !self.pending.lock().expect("pending").insert(key.clone()) {
            return None; // already loading
        }
        let result = match self.gate.acquire().await {
            Ok(_permit) => self.load_inner(url, size).await,
            Err(_) => None,
        };
        self.pending.lock().expect("pending").remove(&key);
        let p = Arc::new(result?);
        self.lru.lock().expect("lru").put(key, p.clone());
        Some(p)
    }

    async fn load_inner(&self, url: &str, size: u32) -> Option<Pixels> {
        // A picture of the profile itself (custom playlist covers): read in place, not copied to the cache.
        if let Some(path) = url.strip_prefix(LOCAL_PREFIX) {
            let bytes = tokio::fs::read(path).await.ok()?;
            return tokio::task::spawn_blocking(move || decode_square(&bytes, size))
                .await
                .ok()?;
        }
        let real = normalize_url(url, size);
        let path = self.dir.join(file_name(&real));
        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(_) => {
                let b = self
                    .http
                    .send(self.http.request(crate::http::Method::GET, &real))
                    .await
                    .ok()?
                    .bytes()
                    .await
                    .ok()?
                    .to_vec();
                let _ = tokio::fs::create_dir_all(&self.dir).await;
                let _ = tokio::fs::write(&path, &b).await;
                b
            }
        };
        tokio::task::spawn_blocking(move || decode_square(&bytes, size))
            .await
            .ok()?
    }
}

/// Decodes and center-crops to a square of `size` px.
pub fn decode_square(bytes: &[u8], size: u32) -> Option<Pixels> {
    let img = image::load_from_memory(bytes).ok()?;
    let (w, h) = (img.width(), img.height());
    let side = w.min(h);
    let cropped = img.crop_imm((w - side) / 2, (h - side) / 2, side, side);
    let resized = cropped.resize_exact(size, size, image::imageops::FilterType::CatmullRom);
    Some(Pixels {
        size,
        rgba: resized.to_rgba8().into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn largest_variants() {
        assert_eq!(
            largest("https://i.scdn.co/image/ab67616d00001e02abc"),
            "https://i.scdn.co/image/ab67616d0000b273abc"
        );
        assert_eq!(
            largest("https://lh3.googleusercontent.com/x=w120-h120-l90-rj"),
            "https://lh3.googleusercontent.com/x=w1200-h1200-l90-rj"
        );
        assert_eq!(
            largest("https://i.ytimg.com/vi/abc/hqdefault.jpg"),
            "https://i.ytimg.com/vi/abc/maxresdefault.jpg"
        );
        assert_eq!(largest("avatars.yandex.net/x/%%"), "https://avatars.yandex.net/x/1000x1000");
        assert_eq!(image_extension(&[0x89, b'P', b'N', b'G', 0]), "png");
    }

    #[test]
    fn yandex_template() {
        assert_eq!(
            normalize_url("avatars.yandex.net/x/%%", 56),
            "https://avatars.yandex.net/x/200x200"
        );
        assert_eq!(
            normalize_url("https://i.scdn.co/image/abc", 56),
            "https://i.scdn.co/image/abc"
        );
    }

    #[test]
    fn decodes_and_crops() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(40, 20, image::Rgba([255, 0, 0, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let p = decode_square(&png, 8).unwrap();
        assert_eq!(p.rgba.len(), 8 * 8 * 4);
        assert_eq!(&p.rgba[..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn lru_respects_byte_limit() {
        let mut l = Lru {
            map: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
            limit: 100,
        };
        for i in 0..5 {
            l.put(
                (i.to_string(), 1),
                Arc::new(Pixels {
                    size: 1,
                    rgba: vec![0; 40],
                }),
            );
        }
        assert!(l.bytes <= 100);
        assert!(l.get(&("4".into(), 1)).is_some());
        assert!(l.get(&("0".into(), 1)).is_none());
    }
}
