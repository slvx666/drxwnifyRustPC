//! A friend's musical portrait (Android portrait/Portrait.kt): three rolling periods, the last 12 months and three
//! genre tags, sent as the `portrait` field — PortraitCodec: `gz1:` + base64(gzip(JSON)), or plain JSON.
//! Read only: the portrait is built on the phone (from its listening history and likes) once a day.

use crate::friends::TrackRef;
use base64::Engine;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Read;

/// One artist of a portrait: listening time, how many of their tracks are liked, a cover.
#[derive(Debug, Clone, PartialEq)]
pub struct PortraitArtist {
    pub name: String,
    pub minutes: i64,
    pub likes: i64,
    pub cover: Option<String>,
}

/// One track of a portrait: how many times it was chosen, whether it is liked.
#[derive(Debug, Clone, PartialEq)]
pub struct PortraitTrack {
    pub track: TrackRef,
    pub plays: i64,
    pub liked: bool,
}

/// What someone listened to over a period; `by_likes` when there was too little listening and the picture comes
/// from that period's likes alone.
#[derive(Debug, Clone, PartialEq)]
pub struct Portrait {
    pub minutes: i64,
    pub tracks: i64,
    pub new_likes: i64,
    pub by_likes: bool,
    pub artists: Vec<PortraitArtist>,
    /// Genre and percent, strongest first; "" = everything else.
    pub genres: Vec<(String, i64)>,
    pub top_tracks: Vec<PortraitTrack>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProfilePortrait {
    pub at: i64,
    pub tags: Vec<String>,
    pub week: Option<Portrait>,
    pub month: Option<Portrait>,
    pub year: Option<Portrait>,
    /// "2026-10" → that month (sorted).
    pub months: BTreeMap<String, Portrait>,
}

const PREFIX: &str = "gz1:";

/// PortraitCodec.unpack.
pub fn unpack(content: &str) -> Option<ProfilePortrait> {
    let text = match content.strip_prefix(PREFIX) {
        Some(b64) => {
            let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
            let mut s = String::new();
            flate2::read::GzDecoder::new(bytes.as_slice())
                .read_to_string(&mut s)
                .ok()?;
            s
        }
        None => content.to_string(),
    };
    let o: Value = serde_json::from_str(&text).ok()?;
    let months = o
        .get("months")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter(|(_, v)| v.is_object())
                .map(|(k, v)| (k.clone(), from_json(v)))
                .collect()
        })
        .unwrap_or_default();
    Some(ProfilePortrait {
        at: o.get("at").and_then(Value::as_i64).unwrap_or(0),
        tags: o
            .get("tags")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .filter(|t| !t.trim().is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        week: o.get("w").filter(|v| v.is_object()).map(from_json),
        month: o.get("m").filter(|v| v.is_object()).map(from_json),
        year: o.get("y").filter(|v| v.is_object()).map(from_json),
        months,
    })
}

fn from_json(o: &Value) -> Portrait {
    let rows = |k: &str| -> Vec<Vec<Value>> {
        o.get(k)
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|r| r.as_array().cloned()).collect())
            .unwrap_or_default()
    };
    let s = |r: &[Value], i: usize| r.get(i).and_then(Value::as_str).unwrap_or("").to_string();
    let n = |r: &[Value], i: usize| r.get(i).and_then(Value::as_i64).unwrap_or(0);
    let ne = |v: String| (!v.is_empty()).then_some(v);
    Portrait {
        minutes: o.get("min").and_then(Value::as_i64).unwrap_or(0),
        tracks: o.get("n").and_then(Value::as_i64).unwrap_or(0),
        new_likes: o.get("likes").and_then(Value::as_i64).unwrap_or(0),
        by_likes: o.get("bl").and_then(Value::as_bool).unwrap_or(false),
        artists: rows("a")
            .iter()
            .map(|r| PortraitArtist {
                name: s(r, 0),
                minutes: n(r, 1),
                likes: n(r, 2),
                cover: ne(s(r, 3)),
            })
            .collect(),
        genres: rows("g").iter().map(|r| (s(r, 0), n(r, 1))).collect(),
        top_tracks: rows("t")
            .iter()
            .map(|r| PortraitTrack {
                track: TrackRef {
                    title: s(r, 0),
                    artist: s(r, 1),
                    catalog_id: ne(s(r, 2)),
                    media_id: ne(s(r, 3)),
                    cover: ne(s(r, 4)),
                    duration_ms: n(r, 5) * 1000,
                    at: 0,
                },
                plays: n(r, 6),
                liked: r.get(7).and_then(Value::as_bool).unwrap_or(false),
            })
            .collect(),
    }
}

/// A genre's own hue (PortraitUi.hueOf): Java's String.hashCode of the lowercase name, spread by the golden ratio.
pub fn hue_of(genre: &str) -> f32 {
    let mut h: i32 = 0;
    for u in genre.to_lowercase().encode_utf16() {
        h = h.wrapping_mul(31).wrapping_add(u as i32);
    }
    let h = (h as i64) & 0xffff_ffff;
    ((h as f64 * 0.618_033_988_7) % 1.0 * 360.0) as f32
}

fn hue_distance(a: f32, b: f32) -> f32 {
    let d = (a - b).abs() % 360.0;
    if d > 180.0 { 360.0 - d } else { d }
}

/// Hues for genres shown side by side (PortraitUi.huesFor): each its own, pushed around when two look alike.
pub fn hues_for(genres: &[String]) -> Vec<(String, f32)> {
    let mut out: Vec<(String, f32)> = Vec::new();
    for g in genres {
        if g.trim().is_empty() || out.iter().any(|(x, _)| x == g) {
            continue;
        }
        let mut hue = hue_of(g);
        let mut tries = 0;
        while out.iter().any(|(_, h)| hue_distance(*h, hue) < 38.0) && tries < 9 {
            hue = (hue + 41.0) % 360.0;
            tries += 1;
        }
        out.push((g.clone(), hue));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn unpacks_gzip_and_plain() {
        let json = r#"{"v":1,"at":5,"tags":["Deathcore",""],"w":{"min":90,"n":12,"likes":3,"bl":false,
            "a":[["Lorna Shore",60,4,"https://i/x"]],"g":[["Deathcore",70],["",30]],
            "t":[["To the Hellfire","Lorna Shore","sp1","","https://c",662,5,true]]},
            "months":{"2026-10":{"min":10,"n":1,"likes":0,"bl":true,"a":[],"g":[],"t":[]}}}"#;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(json.as_bytes()).unwrap();
        let packed = format!(
            "gz1:{}",
            base64::engine::general_purpose::STANDARD.encode(gz.finish().unwrap())
        );
        for content in [packed.as_str(), json] {
            let p = unpack(content).unwrap();
            assert_eq!(p.tags, vec!["Deathcore"]);
            let w = p.week.unwrap();
            assert_eq!(
                (w.minutes, w.tracks, w.new_likes, w.by_likes),
                (90, 12, 3, false)
            );
            assert_eq!(w.artists[0].cover.as_deref(), Some("https://i/x"));
            assert_eq!(w.genres[1], (String::new(), 30));
            let t = &w.top_tracks[0];
            assert_eq!((t.track.duration_ms, t.plays, t.liked), (662_000, 5, true));
            assert_eq!(t.track.media_id, None);
            assert!(p.month.is_none());
            assert!(p.months["2026-10"].by_likes);
        }
    }

    #[test]
    fn hues_match_java_hashcode() {
        // "a".hashCode() = 97 → 97 × φ mod 1 × 360.
        let want = ((97.0f64 * 0.618_033_988_7) % 1.0 * 360.0) as f32;
        assert!((hue_of("A") - want).abs() < 1e-3);
        let hs = hues_for(&[
            "Deathcore".into(),
            "Deathcore".into(),
            String::new(),
            "Metalcore".into(),
        ]);
        assert_eq!(hs.len(), 2);
        assert!(hue_distance(hs[0].1, hs[1].1) >= 38.0 || hs[1].1 != hue_of("Metalcore"));
    }
}
