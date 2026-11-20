//! Playlist import from a file (the phone's import_playlist: CSV and M3U, the formats PlaylistExporter writes and
//! other apps give): each line becomes a track of a new playlist of mine. A line's id (YouTube video, catalog id) is
//! kept; a line with only a title and artist plays through the resolver like any catalog track.

use crate::App;
use drx_catalog::model::{AlbumRef, ArtistRef, Track};

fn track(id: &str, title: &str, artist: &str, album: &str, duration_s: u64) -> Option<Track> {
    let title = title.trim();
    if title.is_empty() {
        return None;
    }
    let artist = artist.trim();
    let id = id.trim();
    let id = if id.is_empty() {
        // A stable id per title and artist, so importing twice does not double the songs.
        let h = format!("{}|{}", title.to_lowercase(), artist.to_lowercase())
            .bytes()
            .fold(1469598103934665603u64, |h, b| {
                (h ^ b as u64).wrapping_mul(1099511628211)
            });
        format!("import:{h:016x}")
    } else {
        id.to_string()
    };
    Some(Track {
        id,
        title: title.to_string(),
        artists: artist
            .split(';')
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(|a| ArtistRef {
                id: String::new(),
                name: a.to_string(),
            })
            .collect(),
        album: Some(album.trim())
            .filter(|a| !a.is_empty())
            .map(|a| AlbumRef {
                id: String::new(),
                name: a.to_string(),
                images: Vec::new(),
            }),
        duration_ms: duration_s * 1000,
        isrc: None,
        plays: None,
        explicit: false,
    })
}

/// One CSV line into cells (quotes, doubled quotes inside them).
fn csv_cells(line: &str) -> Vec<String> {
    let (mut cells, mut cur, mut quoted) = (Vec::new(), String::new(), false);
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => cells.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    cells.push(cur);
    cells
}

/// A YouTube video id in a link or a bare id.
fn youtube_id(s: &str) -> Option<String> {
    let s = s.trim();
    if let Some(i) = s.find("v=") {
        return Some(
            s[i + 2..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect(),
        );
    }
    if let Some(i) = s.find("youtu.be/") {
        return Some(
            s[i + 9..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect(),
        );
    }
    None
}

/// The tracks of a CSV (header with Title / Artist columns) or M3U text.
pub fn parse_playlist(text: &str) -> Vec<Track> {
    let text = text.trim_start_matches('\u{feff}');
    let mut out = Vec::new();
    if text.trim_start().starts_with("#EXTM3U") || text.contains("#EXTINF") {
        let (mut title, mut artist, mut secs) = (String::new(), String::new(), 0u64);
        for line in text.lines().map(str::trim) {
            if let Some(info) = line.strip_prefix("#EXTINF:") {
                let (d, name) = info.split_once(',').unwrap_or(("0", info));
                secs = d.trim().parse::<f64>().unwrap_or(0.0).max(0.0) as u64;
                match name.rsplit_once(" - ") {
                    Some((a, t)) => (artist, title) = (a.to_string(), t.to_string()),
                    None => (artist, title) = (String::new(), name.to_string()),
                }
            } else if let Some(id) = line.strip_prefix("#YTM:") {
                out.extend(track(id, &title, &artist, "", secs));
                title.clear();
            } else if !line.is_empty() && !line.starts_with('#') {
                let id = youtube_id(line).unwrap_or_default();
                let fallback = std::path::Path::new(line)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let t = if title.is_empty() {
                    fallback
                } else {
                    std::mem::take(&mut title)
                };
                out.extend(track(&id, &t, &artist, "", secs));
            }
        }
        return out;
    }
    let mut lines = text.lines();
    let Some(header) = lines.next() else {
        return out;
    };
    let head: Vec<String> = csv_cells(header)
        .into_iter()
        .map(|h| h.trim().to_lowercase())
        .collect();
    let col = |names: &[&str]| {
        head.iter()
            .position(|h| names.iter().any(|n| h == n || h.starts_with(n)))
    };
    let (Some(t), a) = (
        col(&["title", "track name", "track", "название", "трек"]),
        col(&["artist", "исполнитель"]),
    ) else {
        return out;
    };
    let (al, id) = (
        col(&["album", "альбом"]),
        col(&["youtube video id", "video id", "id"]),
    );
    for line in lines.filter(|l| !l.trim().is_empty()) {
        let cells = csv_cells(line);
        let get = |i: Option<usize>| {
            i.and_then(|i| cells.get(i))
                .map(String::as_str)
                .unwrap_or("")
        };
        out.extend(track(get(id), get(Some(t)), get(a), get(al), 0));
    }
    out
}

impl App {
    /// Imports a CSV or M3U file as a new playlist named after the file. Returns its name and how many tracks.
    pub async fn import_playlist_file(
        &self,
        path: &std::path::Path,
    ) -> crate::AppResult<(String, usize)> {
        let bytes = std::fs::read(path).map_err(|e| crate::AppError::Other(e.to_string()))?;
        let text = String::from_utf8_lossy(&bytes);
        let tracks = parse_playlist(&text);
        if tracks.is_empty() {
            return Err(crate::AppError::Other(
                "в файле не нашлось треков (нужен CSV или M3U)".into(),
            ));
        }
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Импорт".into());
        let id = self.create_playlist(&name).await?;
        let n = self.add_to_playlist(&id, tracks).await?;
        Ok((name, n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_a_file_into_a_new_playlist() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::start(
            drx_core::paths::Profile::at(dir.path()),
            crate::EditionProfile::mini(),
        )
        .unwrap();
        let f = dir.path().join("Моя подборка.m3u");
        std::fs::write(
            &f,
            "#EXTM3U
#EXTINF:200,The Weeknd - Blinding Lights
#YTM:4NRXx6U8ABQ
#EXTINF:100,A - B
#YTM:
",
        )
        .unwrap();
        let (name, n) = app
            .runtime()
            .block_on(app.import_playlist_file(&f))
            .unwrap();
        assert_eq!((name.as_str(), n), ("Моя подборка", 2));
        let entries = app.runtime().block_on(app.library_entries()).unwrap();
        assert!(
            entries.iter().any(|e| e.title == "Моя подборка"),
            "the playlist is in the library"
        );
    }

    #[test]
    fn reads_our_csv_and_m3u() {
        let csv = "Title,Artist,Album,YouTube Video ID\n\"Song, with comma\",A; B,Alb,dQw4w9WgXcQ\nPlain,C,,\n";
        let t = parse_playlist(csv);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].title, "Song, with comma");
        assert_eq!(t[0].artists.len(), 2);
        assert_eq!(t[0].id, "dQw4w9WgXcQ");
        assert!(t[1].id.starts_with("import:"));
        let m3u = "#EXTM3U\n#EXTINF:200,The Weeknd - Blinding Lights\n#YTM:4NRXx6U8ABQ\n#EXTINF:100,Solo\nhttps://www.youtube.com/watch?v=abcdefghijk&t=1\n";
        let t = parse_playlist(m3u);
        assert_eq!(t.len(), 2);
        assert_eq!(
            (
                t[0].title.as_str(),
                t[0].artists[0].name.as_str(),
                t[0].duration_ms
            ),
            ("Blinding Lights", "The Weeknd", 200_000)
        );
        assert_eq!(t[1].id, "abcdefghijk");
    }
}
