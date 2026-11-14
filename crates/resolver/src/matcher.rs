//! The strict candidate selector: which upload is the catalog track (port of `spotify/SpotifyMapper.kt`,
//! `selectBestMatch` and helpers, with its unit tests). Guiding rule from Android: a WRONG track is worse
//! than NO track — the gates reject rather than ship the closest-but-wrong result.

use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

/// Max catalog-vs-candidate length gap (s) still treated as the same recording.
pub const DURATION_TOLERANCE_S: i64 = 7;
/// More than this off with the right title and artist = another edit (remix/live/extended) → reject.
pub const DURATION_TOLERANCE_WIDE_S: i64 = 30;
pub const MIN_MATCH_THRESHOLD: f64 = 0.35;
const VARIANT_PENALTY_PER_MARKER: f64 = 0.15;
const VIDEO_PENALTY: f64 = 0.06;
const MAX_VARIANT_PENALTY: f64 = 0.30;

static CREDIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)[(\[][^)\]]*(?:\b(?:feat|ft|featuring|with)\b|\bw/)[^)\]]*[)\]]").unwrap()
});
static TRAILING_CREDIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\s+(?:w/|feat\.|ft\.|featuring\s).*$").unwrap());
static BRACKET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[.*?]").unwrap());
static REMASTER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\(.*?remaster.*?\)").unwrap());
static REMIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\(.*?remix.*?\)").unwrap());
static MULTI_SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
static VARIANT_MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(live|en vivo|en directo|ao vivo|karaoke|cover|instrumental|sped up|spedup|slowed|reverb|nightcore|8d|remix|bootleg|mashup|music video|official video|lyric video)\b").unwrap()
});
static FOREIGN_RECORDING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(cover|covered by|karaoke|remix|rmx|bootleg|mashup|nightcore|sped up|spedup|speed up|slowed|reverb|8d|tribute|instrumental|minus|lofi|lo-fi|8-bit|8 bit|(?:female|male|girl|boy|piano|acoustic|rock|metal|orchestral|russian|english) version|кавер|ремикс|минус|караоке)\b").unwrap()
});
static ARTIST_SPLIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)[,&/]+|\s+(?:feat\.?|ft\.?|x|vs\.?)\s+").unwrap());
static BRACKET_GROUP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[(\[]([^)\]]*)[)\]]").unwrap());
static CREDIT_SEPARATORS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)[,&]|\s+(?:and|x|и)\s+").unwrap());

/// Acts that changed their name, or are commonly written another way. Each line: one act.
const ARTIST_RENAMES: &[&[&str]] = &[
    &["Electric Callboy", "Eskimo Callboy"],
    &["Ghost", "Ghost B.C."],
    &["Ye", "Kanye West"],
    &["Bring Me The Horizon", "BMTH"],
    &["$uicideboy$", "Suicideboys"],
    &["Lorna Shore", "Lorna Shore Band"],
    &["Our Last Night", "OLN"],
];

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub duration_s: Option<i64>,
    pub is_video: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Matched {
    pub id: String,
    pub score: f64,
    pub title: String,
    pub artist: String,
}

fn letter_fold(c: char, out: &mut String) {
    let s = match c {
        'ɇ' => "e",
        'ø' => "o",
        'đ' | 'ð' => "d",
        'ł' | 'ɫ' | 'ƚ' => "l",
        'ħ' => "h",
        'ŧ' => "t",
        'ƀ' => "b",
        'ɍ' => "r",
        'ʉ' | 'ᵾ' => "u",
        'ɏ' => "y",
        'ɨ' | 'ı' => "i",
        'ȼ' => "c",
        'ɉ' => "j",
        'ƶ' => "z",
        'ǥ' => "g",
        'ß' => "ss",
        'æ' => "ae",
        'œ' => "oe",
        'þ' => "th",
        _ => {
            out.push(c);
            return;
        }
    };
    out.push_str(s);
}

/// SpotifyMapper.normalizeTitle: drop credits, square brackets, (remaster)/(remix); fold letters; NFKD without
/// marks; keep letters and digits of any script; everything else becomes a space.
pub fn normalize(title: &str) -> String {
    let lower = title.to_lowercase();
    let s = CREDIT.replace_all(&lower, "");
    let s = TRAILING_CREDIT.replace_all(&s, "");
    let s = BRACKET.replace_all(&s, "");
    let s = REMASTER.replace_all(&s, "");
    let s = REMIX.replace_all(&s, "");
    let mut folded = String::with_capacity(s.len());
    for c in s.chars() {
        letter_fold(c, &mut folded);
    }
    let mut out = String::with_capacity(folded.len());
    for c in folded.nfkd() {
        if c == '\'' || c == '’' || is_combining_mark(c) {
            continue;
        }
        if c.is_whitespace() || !c.is_alphanumeric() {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    MULTI_SPACE.replace_all(&out, " ").trim().to_string()
}

fn bigrams(s: &str) -> HashSet<(char, char)> {
    let v: Vec<char> = s.chars().collect();
    v.windows(2).map(|w| (w[0], w[1])).collect()
}

fn bigram_similarity(a: &str, b: &str) -> f64 {
    if a == b {
        return 1.0;
    }
    let (x, y) = (bigrams(a), bigrams(b));
    if x.is_empty() || y.is_empty() {
        return 0.0;
    }
    let inter = x.iter().filter(|g| y.contains(g)).count();
    2.0 * inter as f64 / (x.len() + y.len()) as f64
}

fn duration_score(catalog_ms: i64, cand_s: Option<i64>) -> f64 {
    let Some(c) = cand_s else { return 0.5 };
    if catalog_ms <= 0 {
        return 0.5;
    }
    match (catalog_ms / 1000 - c).abs() {
        0..=2 => 1.0,
        3..=5 => 0.8,
        6..=10 => 0.5,
        11..=30 => 0.2,
        _ => 0.0,
    }
}

/// Weighted score: title 0.45, artist 0.35, duration 0.20.
pub fn match_score(
    title: &str,
    artist: &str,
    duration_ms: i64,
    c_title: &str,
    c_artist: &str,
    c_dur: Option<i64>,
) -> f64 {
    bigram_similarity(&normalize(title), &normalize(c_title)) * 0.45
        + bigram_similarity(&normalize(artist), &normalize(c_artist)) * 0.35
        + duration_score(duration_ms, c_dur) * 0.20
}

fn markers(re: &Regex, s: &str) -> HashSet<String> {
    re.find_iter(&s.to_lowercase())
        .map(|m| m.as_str().to_lowercase())
        .collect()
}

/// The candidate is another recording (cover/remix/edit) the catalog title isn't.
pub fn is_foreign_recording(catalog_title: &str, cand_title: &str) -> bool {
    let c = markers(&FOREIGN_RECORDING, cand_title);
    !c.is_empty() && !c.is_subset(&markers(&FOREIGN_RECORDING, catalog_title))
}

/// Drops the " - Variant" suffix of catalog titles ("Bohemian Rhapsody - Remastered 2011"). Catalog side only.
pub fn title_core(title: &str) -> &str {
    title.split(" - ").next().unwrap_or(title)
}

fn strip_own_credits(title: &str, tokens: &[String]) -> String {
    if tokens.is_empty() || (!title.contains('(') && !title.contains('[')) {
        return title.to_string();
    }
    BRACKET_GROUP
        .replace_all(title, |c: &regex::Captures| {
            let names: Vec<String> = CREDIT_SEPARATORS
                .split(&c[1])
                .map(normalize)
                .filter(|n| !n.is_empty())
                .collect();
            let all = !names.is_empty()
                && names.iter().all(|n| {
                    tokens.iter().any(|t| {
                        t == n
                            || (t.chars().count() >= 4
                                && (n.contains(t.as_str()) || t.contains(n.as_str())))
                    })
                });
            if all {
                " ".to_string()
            } else {
                c[0].to_string()
            }
        })
        .into_owned()
}

fn symbols_of(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The candidate's title could be the catalog track (rules out another song by the same artist).
pub fn title_plausibly_matches(cand_title: &str, catalog_title: &str, tokens: &[String]) -> bool {
    let target = normalize(&strip_own_credits(title_core(catalog_title), tokens));
    if target.is_empty() {
        let sym = symbols_of(catalog_title);
        return !sym.is_empty() && symbols_of(cand_title).contains(&sym);
    }
    let cand = normalize(cand_title);
    if cand.is_empty() {
        return false;
    }
    if target.chars().count() >= 4 {
        return cand.contains(&target);
    }
    let wanted: Vec<&str> = target.split(' ').collect();
    let words: Vec<&str> = cand.split(' ').collect();
    words.windows(wanted.len()).any(|w| w == wanted.as_slice())
}

pub fn is_symbol_only_title(title: &str) -> bool {
    !title.trim().is_empty() && normalize(title_core(title)).is_empty()
}

fn latin_part_of(name: &str) -> Option<String> {
    let latin: String = name
        .chars()
        .filter(|c| (*c as u32) < 0x250)
        .collect::<String>()
        .trim()
        .to_string();
    let other = name.chars().any(|c| c.is_alphabetic() && c as u32 >= 0x250);
    (other && latin.chars().filter(|c| c.is_alphabetic()).count() >= 3 && latin != name)
        .then_some(latin)
}

static NORMALIZED_ALIASES: LazyLock<HashMap<String, Vec<String>>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    for group in ARTIST_RENAMES {
        let keys: Vec<String> = group
            .iter()
            .map(|s| normalize(s))
            .filter(|k| !k.is_empty())
            .collect();
        for k in &keys {
            m.insert(
                k.clone(),
                keys.iter().filter(|x| *x != k).cloned().collect(),
            );
        }
    }
    m
});

/// Normalized artist tokens from a multi-artist credit, with old names and the Latin part of mixed names.
pub fn artist_tokens(artists: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in ARTIST_SPLIT.split(artists) {
        let n = normalize(part);
        if n.is_empty() {
            continue;
        }
        let mut add = vec![n.clone()];
        add.extend(NORMALIZED_ALIASES.get(&n).cloned().unwrap_or_default());
        add.extend(latin_part_of(&n));
        for a in add {
            if !out.contains(&a) {
                out.push(a);
            }
        }
    }
    out
}

/// The other names an artist is known under, as written.
pub fn artist_aliases(artist: &str) -> Vec<String> {
    let key = normalize(artist);
    ARTIST_RENAMES
        .iter()
        .find(|g| g.iter().any(|a| normalize(a) == key))
        .map(|g| {
            g.iter()
                .filter(|a| normalize(a) != key)
                .map(|a| a.to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn strip_article(n: &str) -> &str {
    n.strip_prefix("the ").unwrap_or(n).trim()
}

pub fn artist_plausibly_matches(cand_title: &str, cand_artist: &str, tokens: &[String]) -> bool {
    if tokens.is_empty() {
        return true;
    }
    let title_n = normalize(cand_title);
    if tokens.iter().any(|t| title_n.contains(t.as_str())) {
        return true;
    }
    let artist_n = normalize(cand_artist);
    let compact = artist_n.replace(' ', "");
    if tokens.iter().any(|t| {
        let c = t.replace(' ', "");
        c.chars().count() >= 4 && c == compact
    }) {
        return true;
    }
    if !tokens.iter().any(|t| artist_n.contains(t.as_str())) {
        return false;
    }
    let parts: Vec<String> = ARTIST_SPLIT
        .split(cand_artist)
        .map(|p| strip_article(&normalize(p)).to_string())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() <= 1 {
        return true;
    }
    let expected: Vec<&str> = tokens.iter().map(|t| strip_article(t)).collect();
    let is_expected = |p: &str| {
        expected
            .iter()
            .any(|e| p == *e || p.contains(e) || e.contains(p))
    };
    if is_expected(&parts[0]) {
        return true;
    }
    parts.iter().all(|p| is_expected(p))
}

fn latin_key(text: &str) -> String {
    let mut s = String::new();
    for c in text.to_lowercase().chars() {
        let t = match c {
            'а' => "a",
            'б' => "b",
            'в' => "v",
            'г' => "g",
            'д' => "d",
            'е' | 'ё' => "e",
            'ж' => "zh",
            'з' => "z",
            'и' | 'й' => "i",
            'к' => "k",
            'л' => "l",
            'м' => "m",
            'н' => "n",
            'о' => "o",
            'п' => "p",
            'р' => "r",
            'с' => "s",
            'т' => "t",
            'у' => "u",
            'ф' => "f",
            'х' => "h",
            'ц' => "ts",
            'ч' => "ch",
            'ш' | 'щ' => "sh",
            'ъ' | 'ь' => "",
            'ы' => "i",
            'э' => "e",
            'ю' => "iu",
            'я' => "ia",
            'і' | 'ї' => "i",
            'є' => "e",
            'ґ' => "g",
            'ѣ' => "e",
            'ў' => "u",
            _ => {
                s.push(c);
                continue;
            }
        };
        s.push_str(t);
    }
    let s = s
        .replace("kh", "h")
        .replace("sch", "sh")
        .replace("ck", "k")
        .replace('x', "ks");
    s.chars()
        .map(|c| match c {
            'y' | 'j' => 'i',
            'w' => 'v',
            'q' => 'k',
            c => c,
        })
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        .collect()
}

fn bigram_dice(a: &str, b: &str) -> f64 {
    let (x, y): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if x.len() < 2 || y.len() < 2 {
        return if a == b { 1.0 } else { 0.0 };
    }
    let mut cx: HashMap<(char, char), usize> = HashMap::new();
    for w in x.windows(2) {
        *cx.entry((w[0], w[1])).or_default() += 1;
    }
    let mut cy: HashMap<(char, char), usize> = HashMap::new();
    for w in y.windows(2) {
        *cy.entry((w[0], w[1])).or_default() += 1;
    }
    let common: usize = cx
        .iter()
        .map(|(k, n)| (*n).min(*cy.get(k).unwrap_or(&0)))
        .sum();
    2.0 * common as f64 / (x.len() - 1 + y.len() - 1) as f64
}

pub fn artist_transliteration_matches(
    cand_title: &str,
    cand_artist: &str,
    tokens: &[String],
) -> bool {
    if tokens.is_empty() || artist_plausibly_matches(cand_title, cand_artist, tokens) {
        return true;
    }
    let ak = latin_key(cand_artist);
    let tk = latin_key(cand_title);
    tokens.iter().any(|t| {
        let k = latin_key(t);
        let kl = k.len();
        (kl >= 2 && k == ak)
            || (kl >= 3
                && (k == ak
                    || (kl >= 4
                        && (tk.contains(&k)
                            || (ak.len() >= 4 && (ak.contains(&k) || k.contains(&ak)))))
                    || bigram_dice(&k, &ak) >= 0.75))
    })
}

/// The candidate is by one of the artists, written in either script.
pub fn artist_matches(cand_title: &str, cand_artist: &str, tokens: &[String]) -> bool {
    let any_latin = tokens.iter().any(|t| {
        t.chars()
            .any(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    });
    if !any_latin {
        artist_transliteration_matches(cand_title, cand_artist, tokens)
    } else {
        artist_plausibly_matches(cand_title, cand_artist, tokens)
            || (cand_artist
                .chars()
                .any(|c| c.is_alphabetic() && !c.is_ascii_alphabetic())
                && artist_transliteration_matches(cand_title, cand_artist, tokens))
    }
}

pub fn variant_penalty(catalog_title: &str, cand_title: &str) -> f64 {
    let c = markers(&VARIANT_MARKER, cand_title);
    if c.is_empty() {
        return 0.0;
    }
    let own = markers(&VARIANT_MARKER, catalog_title);
    (c.difference(&own).count() as f64 * VARIANT_PENALTY_PER_MARKER).min(MAX_VARIANT_PENALTY)
}

/// The best candidate for a catalog track, or None when none clears the gates (selectBestMatch, strict mode).
pub fn select_best(
    title: &str,
    primary_artist: &str,
    all_artists: &str,
    duration_ms: i64,
    candidates: &[Candidate],
) -> Option<Matched> {
    if candidates.is_empty() {
        return None;
    }
    let off = |c: &Candidate| {
        c.duration_s
            .filter(|_| duration_ms > 0)
            .map(|d| (duration_ms / 1000 - d).abs())
    };
    let score = |c: &Candidate| {
        match_score(
            title,
            primary_artist,
            duration_ms,
            &c.title,
            &c.artist,
            c.duration_s,
        )
    };
    let tokens = artist_tokens(if all_artists.trim().is_empty() {
        primary_artist
    } else {
        all_artists
    });
    let mut title_ok: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| {
            title_plausibly_matches(&c.title, title, &tokens)
                && !is_foreign_recording(title, &c.title)
        })
        .collect();
    if title_ok.is_empty() && is_symbol_only_title(title) {
        title_ok = candidates
            .iter()
            .filter(|c| {
                artist_plausibly_matches(&c.title, &c.artist, &tokens)
                    && !is_foreign_recording(title, &c.title)
                    && off(c).unwrap_or(i64::MAX) <= DURATION_TOLERANCE_S
            })
            .collect();
    }
    let pool: Vec<&Candidate> = title_ok
        .into_iter()
        .filter(|c| artist_matches(&c.title, &c.artist, &tokens))
        .filter(|c| off(c).unwrap_or(0) <= DURATION_TOLERANCE_WIDE_S)
        .collect();
    let adjusted = |c: &Candidate| {
        score(c) - variant_penalty(title, &c.title) - if c.is_video { VIDEO_PENALTY } else { 0.0 }
    };
    let chosen = pool.into_iter().min_by(|a, b| {
        adjusted(b)
            .partial_cmp(&adjusted(a))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(off(a).unwrap_or(i64::MAX).cmp(&off(b).unwrap_or(i64::MAX)))
    })?;
    if off(chosen).is_some_and(|o| o > DURATION_TOLERANCE_WIDE_S) {
        return None;
    }
    let s = score(chosen);
    (s >= MIN_MATCH_THRESHOLD).then(|| Matched {
        id: chosen.id.clone(),
        score: s,
        title: chosen.title.clone(),
        artist: chosen.artist.clone(),
    })
}

#[cfg(test)]
mod tests {
    //! Cases from android `spotify/src/test/.../SpotifyMapperSelectMatchTest.kt`.
    use super::*;

    fn cand(id: &str, title: &str, artist: &str, dur: i64) -> Candidate {
        Candidate {
            id: id.into(),
            title: title.into(),
            artist: artist.into(),
            duration_s: Some(dur),
            is_video: false,
        }
    }
    fn video(id: &str, title: &str, artist: &str, dur: i64) -> Candidate {
        Candidate {
            is_video: true,
            ..cand(id, title, artist, dur)
        }
    }
    fn sel(title: &str, artist: &str, dur: i64, c: &[Candidate]) -> Option<String> {
        select_best(title, artist, artist, dur * 1000, c).map(|m| m.id)
    }

    #[test]
    fn stroked_letters_match_plain_spelling() {
        assert_eq!(
            sel(
                "Extraction",
                "LARCɆNIA ROɆ",
                200,
                &[cand("RIGHT", "Extraction", "Larcenia Roe", 200)]
            )
            .as_deref(),
            Some("RIGHT")
        );
    }

    #[test]
    fn released_audio_wins_over_clip() {
        let c = [
            video("CLIP", "The Sound", "The Plot in You", 245),
            cand("AUDIO", "THE SOUND", "The Plot In You", 249),
        ];
        assert_eq!(
            sel("THE SOUND", "The Plot In You", 245, &c).as_deref(),
            Some("AUDIO")
        );
        let c = [
            video("CLIP", "The Sound", "The Plot in You", 245),
            cand("LIVE", "THE SOUND (Live)", "The Plot In You", 262),
        ];
        assert_eq!(
            sel("THE SOUND", "The Plot In You", 245, &c).as_deref(),
            Some("CLIP")
        );
    }

    #[test]
    fn symbol_only_titles() {
        let c = [
            cand("OTHER", "Punishment Routine", "DOGBITE", 180),
            cand("RIGHT", "./", "DOGBITE", 95),
        ];
        assert_eq!(sel("./", "DOGBITE", 95, &c).as_deref(), Some("RIGHT"));
        let c = [
            cand("LONG", "Untitled", "DOGBITE", 200),
            cand("RIGHT", "Intro", "DOGBITE", 96),
        ];
        assert_eq!(sel("./", "DOGBITE", 95, &c).as_deref(), Some("RIGHT"));
        assert_eq!(
            sel(
                "./",
                "DOGBITE",
                95,
                &[cand("X", "Intro", "Someone Else", 95)]
            ),
            None
        );
    }

    #[test]
    fn strict_gates() {
        assert_eq!(
            sel(
                "Blinding Lights",
                "The Weeknd",
                200,
                &[cand("R", "Blinding Lights", "The Weeknd", 200)]
            )
            .as_deref(),
            Some("R")
        );
        assert_eq!(
            sel(
                "Blinding Lights",
                "The Weeknd",
                200,
                &[cand("L", "Blinding Lights", "The Weeknd", 290)]
            ),
            None,
            "too long"
        );
        assert_eq!(
            sel(
                "Mi Gente",
                "J Balvin",
                189,
                &[cand("W", "Ginza", "J Balvin", 190)]
            ),
            None,
            "other song"
        );
        assert_eq!(
            sel(
                "Imagine",
                "John Lennon",
                183,
                &[cand("W", "Imagine", "Ariana Grande", 183)]
            ),
            None,
            "other artist"
        );
        let c = [
            cand("LIVE", "As It Was (Live)", "Harry Styles", 170),
            cand("STUDIO", "As It Was", "Harry Styles", 167),
        ];
        assert_eq!(
            sel("As It Was", "Harry Styles", 167, &c).as_deref(),
            Some("STUDIO")
        );
    }

    #[test]
    fn covers_and_remixes_are_refused() {
        let c = [cand(
            "COVER",
            "Welcome to the Jungle (female version)",
            "Guns N' Roses",
            270,
        )];
        assert_eq!(sel("Welcome to the Jungle", "Guns N' Roses", 271, &c), None);
        assert!(is_foreign_recording("Song", "Song (кавер)"));
        assert!(!is_foreign_recording("Song (Remix)", "Song (Remix)"));
    }

    #[test]
    fn transliteration_both_ways() {
        // The artist may be transliterated; the title is compared in its own script, as on Android.
        assert_eq!(
            sel(
                "Группа крови",
                "Кино",
                286,
                &[cand("R", "Группа крови", "Kino", 285)]
            )
            .as_deref(),
            Some("R")
        );
        assert_eq!(
            sel("Баста", "Basta", 200, &[cand("R", "Баста", "Баста", 200)]).as_deref(),
            Some("R")
        );
        assert!(artist_transliteration_matches(
            "Пошёл нахуй",
            "Skameyka Zapasnih",
            &artist_tokens("Скамейка Запасных")
        ));
    }

    #[test]
    fn credits_in_brackets_do_not_block() {
        let c = [cand("R", "One Kiss", "Calvin Harris", 214)];
        assert_eq!(
            select_best(
                "One Kiss (with Dua Lipa)",
                "Calvin Harris",
                "Calvin Harris, Dua Lipa",
                214_000,
                &c
            )
            .map(|m| m.id)
            .as_deref(),
            Some("R")
        );
    }

    #[test]
    fn renamed_band() {
        assert!(artist_tokens("Electric Callboy").contains(&"eskimo callboy".to_string()));
        assert_eq!(
            artist_aliases("Eskimo Callboy"),
            vec!["Electric Callboy".to_string()]
        );
    }

    #[test]
    fn other_band_with_the_same_word_is_refused() {
        let c = [cand("W", "Machine", "Mercury & The Architects", 230)];
        assert_eq!(sel("Machine", "Architects", 230, &c), None);
    }
}
