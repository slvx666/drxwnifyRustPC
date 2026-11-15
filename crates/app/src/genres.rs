//! Genres of artists, as Android's `portrait/Genres.kt`: neither catalog gives useful ones, so they come from
//! Last.fm's listener tags (`artist.gettoptags`), then MusicBrainz; tags that are not genres ("seen live",
//! countries, years) are dropped and spellings merged. Kept on disk for a month (`artist_genres.json`, a week
//! for "none found"). Also a genre's most tagged artists (`tag.gettopartists`, kept a week) — GenrePlaylists.
//!
//! The Last.fm API key is the app's own; it is not in the repository: `DRX_LASTFM_KEY` at build time (the build
//! script reads it from the untracked `local.env`) or at run time. Without it only MusicBrainz answers.

use crate::App;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

const TTL_MS: i64 = 30 * 86_400_000;
const MISS_TTL_MS: i64 = 7 * 86_400_000;
const ARTISTS_TTL_MS: i64 = 7 * 86_400_000;
const MAX_TAGS: usize = 5;
const FILE: &str = "artist_genres.json";
const LASTFM: &str = "https://ws.audioscrobbler.com/2.0/";
const MUSICBRAINZ_UA: &str = "Drxwnify/2.0 ( https://drxwnify.pages.dev )";

fn lastfm_key() -> Option<String> {
    std::env::var("DRX_LASTFM_KEY")
        .ok()
        .or_else(|| option_env!("DRX_LASTFM_KEY").map(str::to_string))
        .filter(|k| !k.is_empty())
}

const NOT_GENRES: &[&str] = &[
    "seen live", "favorite", "favourite", "vocalist", "female", "male", "love", "awesome", "beautiful",
    "best", "my ", "albums i own", "spotify", "under 2000", "check out", "amazing", "cool", "good",
    "sexy", "cover", "soundtrack of my", "listen", "artist", "band", "singer", "songwriter", "russian",
    "american", "british", "german", "japanese", "french", "swedish", "finnish", "norwegian", "polish",
    "ukrainian", "canadian", "australian", "uk", "usa", "russia", "moscow", "london", "tiktok", "meme",
];

const GENRE_WORDS: &[&str] = &[
    "rock", "metal", "pop", "core", "wave", "gaze", "hop", "rap", "jazz", "techno", "house", "trance",
    "step", "bass", "punk", "folk", "soul", "funk", "blues", "ambient", "electro", "drum", "dnb",
    "industrial", "noise", "grunge", "emo", "indie", "alternative", "psych", "prog", "post-", "black",
    "death", "doom", "sludge", "stoner", "thrash", "grind", "garage", "disco", "synth", "idm", "glitch",
    "lo-fi", "lofi", "chill", "trip", "breakbeat", "breaks", "jungle", "footwork", "juke", "dub",
    "reggae", "ska", "r&b", "rnb", "classical", "orchestral", "soundtrack", "chanson", "k-pop",
    "j-pop", "phonk", "drill", "trap", "grime", "hyperpop", "plunderphonics", "vapor", "witch house",
    "minimal", "experimental", "avant", "country", "bluegrass", "latin", "salsa", "cumbia",
    "reggaeton", "afro", "bossa", "slowcore", "math", "screamo", "gothic", "goth", "ebm", "hardstyle",
    "gabber", "hardcore", "shoegaze", "dream", "darkwave", "coldwave", "electronic", "dance", "edm",
    "downtempo", "trip-hop", "acoustic", "singer-songwriter", "new age", "world", "gospel", "opera",
    "neoclassical", "art pop", "bedroom", "cloud rap", "emo rap", "jersey club", "bossa nova",
    "nu jazz", "fusion", "swing", "big band", "chiptune", "8-bit", "vgm", "anime", "city pop",
    "krautrock", "zeuhl", "ethereal", "dark ambient", "drone", "musique concrete", "breakcore",
    "speedcore", "digital hardcore", "witch", "horrorcore", "crunk", "g-funk", "boom bap",
];

const ALIASES: &[(&str, &str)] = &[
    ("hip hop", "hip-hop"), ("hiphop", "hip-hop"), ("rnb", "r&b"), ("rhythm and blues", "r&b"),
    ("drum n bass", "drum and bass"), ("drum & bass", "drum and bass"), ("dnb", "drum and bass"),
    ("drum'n'bass", "drum and bass"), ("lofi", "lo-fi"), ("lo fi", "lo-fi"), ("electronica", "electronic"),
    ("post rock", "post-rock"), ("post punk", "post-punk"), ("post metal", "post-metal"),
    ("post hardcore", "post-hardcore"), ("trip hop", "trip-hop"), ("nu-metal", "nu metal"),
    ("alt rock", "alternative rock"), ("alternative", "alternative rock"), ("dream-pop", "dream pop"),
    ("synth pop", "synthpop"), ("synth-pop", "synthpop"), ("edm", "electronic dance"),
    ("black-metal", "black metal"), ("death-metal", "death metal"), ("witchhouse", "witch house"),
    ("shoegazer", "shoegaze"), ("blackgaze", "blackgaze"), ("rap", "hip-hop"),
];

const UPPER: &[&str] = &["idm", "ebm", "edm", "r&b", "uk", "dnb", "vgm", "8-bit"];

/// Words that make a subgenre of the genre after them ("Symphonic Deathcore" is Deathcore).
const MODIFIERS: &[&str] = &[
    "progressive", "symphonic", "technical", "brutal", "blackened", "melodic", "atmospheric", "modern",
    "epic", "slam", "orchestral", "cinematic", "raw", "old", "school", "old-school", "dissonant", "mathy",
    "experimental", "avant-garde",
];

/// The display form of a tag when it is a genre (Genres.normalize), else None.
pub fn normalize(raw: &str, artist: &str) -> Option<String> {
    let mut tag = raw.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    if tag.is_empty() || tag == artist || tag.chars().count() > 30 {
        return None;
    }
    let digits = tag.trim_end_matches('s');
    if (2..=4).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if NOT_GENRES.iter().any(|n| tag.contains(n)) {
        return None;
    }
    if let Some((_, to)) = ALIASES.iter().find(|(from, _)| *from == tag) {
        tag = to.to_string();
    }
    if !GENRE_WORDS.iter().any(|w| tag.contains(w)) {
        return None;
    }
    let cap = |part: &str| -> String {
        if UPPER.contains(&part) {
            part.to_uppercase()
        } else {
            let mut c = part.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        }
    };
    Some(
        tag.split(' ')
            .map(|word| {
                if UPPER.contains(&word) {
                    word.to_uppercase()
                } else {
                    word.split('-').map(cap).collect::<Vec<_>>().join("-")
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// The genre a tag belongs to: its leading modifiers dropped (Genres.family).
pub fn family(tag: &str) -> String {
    let words: Vec<&str> = tag.split(' ').collect();
    let mut i = 0;
    while i + 1 < words.len() && MODIFIERS.contains(&words[i].to_lowercase().as_str()) {
        i += 1;
    }
    words[i..].join(" ")
}

/// The genre tags among (name, weight), merged, in display form, strongest first (Genres.pick).
fn pick(mut tags: Vec<(String, i64)>, artist: &str, min_count: i64) -> Vec<String> {
    let own = artist.trim().to_lowercase();
    tags.retain(|(_, c)| *c >= min_count);
    tags.sort_by_key(|t| std::cmp::Reverse(t.1));
    let mut out: Vec<String> = Vec::new();
    for (name, _) in tags {
        if let Some(g) = normalize(&name, &own)
            && !out.contains(&g)
        {
            out.push(g);
        }
        if out.len() >= MAX_TAGS {
            break;
        }
    }
    out
}

/// Last.fm's tag spelling of a genre (GenrePlaylists.lastFmTag).
fn lastfm_tag(genre: &str) -> String {
    match genre.trim().to_lowercase().as_str() {
        "r&b" => "rnb".into(),
        "electronic dance" => "edm".into(),
        g => g.to_string(),
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct Entry {
    g: Vec<String>,
    t: i64,
}

#[derive(Serialize, Deserialize)]
struct TopArtists {
    at: i64,
    names: Vec<String>,
}

#[derive(Default)]
pub(crate) struct GenreCache {
    loaded: bool,
    map: HashMap<String, Entry>,
    last_musicbrainz: Option<std::time::Instant>,
}

fn key_of(artist: &str) -> String {
    artist.trim().to_lowercase()
}

impl App {
    fn genres_file(&self) -> std::path::PathBuf {
        self.profile().root().join(FILE)
    }

    fn genre_cache(&self) -> std::sync::MutexGuard<'_, GenreCache> {
        let mut g = self.inner.genres.lock().expect("genres");
        if !g.loaded {
            g.loaded = true;
            if let Ok(b) = std::fs::read(self.genres_file()) {
                g.map = serde_json::from_slice(&b).unwrap_or_default();
            }
        }
        g
    }

    /// What is known of an artist's genres now, without the network.
    pub fn cached_genres(&self, artist: &str) -> Vec<String> {
        self.genre_cache()
            .map
            .get(&key_of(artist))
            .map(|e| e.g.clone())
            .unwrap_or_default()
    }

    /// Looks up every artist not known yet (one at a time: the services limit the rate).
    pub async fn ensure_genres(&self, artists: &[String]) {
        let now = crate::songs::now_ms();
        let stale = |e: Option<&Entry>| match e {
            None => true,
            Some(e) => now - e.t > if e.g.is_empty() { MISS_TTL_MS } else { TTL_MS },
        };
        let missing: Vec<String> = {
            let c = self.genre_cache();
            let mut seen = std::collections::HashSet::new();
            artists
                .iter()
                .map(|a| a.trim().to_string())
                .filter(|a| !a.is_empty() && seen.insert(key_of(a)))
                .filter(|a| stale(c.map.get(&key_of(a))))
                .collect()
        };
        if missing.is_empty() {
            return;
        }
        let mut changed = false;
        for name in missing {
            let lastfm = self.genres_from_lastfm(&name).await;
            let mb = if lastfm.as_ref().is_none_or(|g| g.is_empty()) {
                self.genres_from_musicbrainz(&name).await
            } else {
                None
            };
            // Neither answered (no network): not remembered as "no genres", asked again later.
            if lastfm.is_none() && mb.is_none() {
                continue;
            }
            let found = lastfm.filter(|g| !g.is_empty()).or(mb).unwrap_or_default();
            self.genre_cache().map.insert(
                key_of(&name),
                Entry {
                    g: found,
                    t: crate::songs::now_ms(),
                },
            );
            changed = true;
        }
        if changed {
            let bytes = serde_json::to_vec(&self.genre_cache().map).unwrap_or_default();
            let _ = drx_core::paths::write_atomic(&self.genres_file(), &bytes);
        }
    }

    async fn lastfm_get(&self, params: &[(&str, &str)]) -> Option<serde_json::Value> {
        let key = lastfm_key()?;
        let mut q: Vec<(&str, &str)> = params.to_vec();
        q.push(("api_key", key.as_str()));
        q.push(("format", "json"));
        let req = self
            .inner
            .http
            .request(drx_core::http::Method::GET, LASTFM)
            .query(&q)
            .timeout(Duration::from_secs(10));
        self.inner.http.send(req).await.ok()?.json().await.ok()
    }

    async fn genres_from_lastfm(&self, artist: &str) -> Option<Vec<String>> {
        let v = self
            .lastfm_get(&[("method", "artist.gettoptags"), ("artist", artist), ("autocorrect", "1")])
            .await?;
        let tags = v
            .get("toptags")
            .and_then(|t| t.get("tag"))
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        Some(pick(
            tags.iter()
                .filter_map(|t| {
                    Some((
                        t.get("name")?.as_str()?.to_string(),
                        t.get("count").and_then(|c| c.as_i64()).unwrap_or(0),
                    ))
                })
                .collect(),
            artist,
            8,
        ))
    }

    async fn genres_from_musicbrainz(&self, artist: &str) -> Option<Vec<String>> {
        // MusicBrainz asks for at most one request a second.
        let wait = {
            let mut c = self.genre_cache();
            let w = c
                .last_musicbrainz
                .map(|t| Duration::from_millis(1100).saturating_sub(t.elapsed()))
                .unwrap_or_default();
            c.last_musicbrainz = Some(std::time::Instant::now() + w);
            w
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
        let query = format!("artist:\"{}\"", artist.replace('"', ""));
        let req = self
            .inner
            .http
            .request(drx_core::http::Method::GET, "https://musicbrainz.org/ws/2/artist/")
            .query(&[("query", query.as_str()), ("limit", "1"), ("fmt", "json")])
            .header("User-Agent", MUSICBRAINZ_UA)
            .timeout(Duration::from_secs(10));
        let v: serde_json::Value = self.inner.http.send(req).await.ok()?.json().await.ok()?;
        let Some(first) = v.get("artists").and_then(|a| a.get(0)) else {
            return Some(Vec::new());
        };
        if first.get("score").and_then(|s| s.as_i64()).unwrap_or(0) < 90 {
            return Some(Vec::new());
        }
        let tags = first.get("tags").and_then(|t| t.as_array()).cloned().unwrap_or_default();
        Some(pick(
            tags.iter()
                .filter_map(|t| {
                    Some((
                        t.get("name")?.as_str()?.to_string(),
                        t.get("count").and_then(|c| c.as_i64()).unwrap_or(0),
                    ))
                })
                .collect(),
            artist,
            1,
        ))
    }

    /// The genre's most tagged artists on Last.fm (up to 250), as normalized names (letters and digits).
    pub async fn genre_top_artists(&self, genre: &str) -> std::collections::HashSet<String> {
        let tag = lastfm_tag(genre);
        let safe: String = tag.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
        let file = self.profile().cache_dir().join(format!("genre_artists_{safe}.json"));
        let now = crate::songs::now_ms();
        let kept: Option<TopArtists> = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok());
        let names = match kept {
            Some(k) if now - k.at < ARTISTS_TTL_MS && !k.names.is_empty() => k.names,
            kept => {
                let fresh: Option<Vec<String>> = self
                    .lastfm_get(&[("method", "tag.gettopartists"), ("tag", &tag), ("limit", "250")])
                    .await
                    .map(|v| {
                        v.get("topartists")
                            .and_then(|t| t.get("artist"))
                            .and_then(|a| a.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|x| x.get("name")?.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default()
                    });
                match fresh {
                    Some(n) if !n.is_empty() => {
                        let _ = drx_core::paths::write_atomic(
                            &file,
                            &serde_json::to_vec(&TopArtists { at: now, names: n.clone() }).unwrap_or_default(),
                        );
                        n
                    }
                    _ => kept.map(|k| k.names).unwrap_or_default(),
                }
            }
        };
        names.iter().map(|n| norm(n)).collect()
    }
}

/// Letters and digits in lower case.
pub(crate) fn norm(s: &str) -> String {
    s.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_become_genres_like_android() {
        assert_eq!(normalize("post rock", ""), Some("Post-Rock".into()));
        assert_eq!(normalize("hip hop", ""), Some("Hip-Hop".into()));
        assert_eq!(normalize("seen live", ""), None);
        assert_eq!(normalize("90s", ""), None);
        assert_eq!(normalize("female vocalists", ""), None);
        assert_eq!(normalize("idm", ""), Some("IDM".into()));
        assert_eq!(normalize("Deathcore", "deathcore"), None);
        assert_eq!(normalize("melodic death metal", ""), Some("Melodic Death Metal".into()));
    }

    #[test]
    fn pick_orders_and_merges() {
        let tags = vec![
            ("hip hop".to_string(), 50),
            ("rap".to_string(), 90),
            ("seen live".to_string(), 100),
            ("trap".to_string(), 3),
        ];
        assert_eq!(pick(tags, "x", 8), vec!["Hip-Hop".to_string()]);
    }

    #[test]
    fn family_drops_modifiers() {
        assert_eq!(family("Symphonic Deathcore"), "Deathcore");
        assert_eq!(family("Progressive Metal"), "Metal");
        assert_eq!(family("Metal"), "Metal");
    }
}
