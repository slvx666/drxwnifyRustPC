//! Listening history and statistics from the `event` table (Android HistoryScreen, StatsScreen). Statistics count
//! this app's listening only (2.3.0: «Статистика считает только прослушивания в приложении»).

use crate::lists::SongRow;
use crate::{App, AppResult};

/// Statistics period (Android StatPeriod).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatPeriod {
    Week,
    #[default]
    Month,
    Months3,
    Months6,
    Year,
    All,
    /// Android Wrapped: the phone fixes it to 2025 (WrappedConstants.YEAR); the PC shows the current year — its
    /// history starts later, and last year's summary would be empty.
    Wrapped,
}

/// Wrapped's year: this one (UTC).
pub fn wrapped_year() -> i64 {
    civil_year(now_ms().div_euclid(86_400_000))
}

/// Wrapped's playlist name (WrappedConstants.PLAYLIST_NAME with the year).
pub fn wrapped_playlist() -> String {
    format!("Drxwnify {}", wrapped_year())
}

/// Year of a day number (days since 1970-01-01; H. Hinnant's civil_from_days).
fn civil_year(z: i64) -> i64 {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    yoe + era * 400 + i64::from(mp >= 10)
}

/// Days since 1970-01-01 of January 1st of `y` (days_from_civil).
fn jan1(y: i64) -> i64 {
    let y = y - 1;
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = 306; // March-based day of January 1st
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

impl StatPeriod {
    pub const ALL: [StatPeriod; 7] = [
        StatPeriod::Week,
        StatPeriod::Month,
        StatPeriod::Months3,
        StatPeriod::Months6,
        StatPeriod::Year,
        StatPeriod::All,
        StatPeriod::Wrapped,
    ];

    pub fn label(self) -> String {
        if self == StatPeriod::Wrapped {
            return format!("Итоги {}", wrapped_year());
        }
        self.short_label().to_string()
    }

    fn short_label(self) -> &'static str {
        match self {
            StatPeriod::Week => "7 дней",
            StatPeriod::Month => "Месяц",
            StatPeriod::Months3 => "3 месяца",
            StatPeriod::Months6 => "Полгода",
            StatPeriod::Year => "Год",
            StatPeriod::All => "Всё время",
            StatPeriod::Wrapped => "Итоги года",
        }
    }

    /// [from, to) in Unix ms.
    pub fn range(self) -> (i64, i64) {
        match self {
            // January 1st of this year .. of the next (UTC).
            StatPeriod::Wrapped => {
                let y = wrapped_year();
                (jan1(y) * 86_400_000, jan1(y + 1) * 86_400_000)
            }
            p => (p.days().map(|d| now_ms() - d * 86_400_000).unwrap_or(0), i64::MAX),
        }
    }

    fn days(self) -> Option<i64> {
        match self {
            StatPeriod::Week => Some(7),
            StatPeriod::Month => Some(30),
            StatPeriod::Months3 => Some(91),
            StatPeriod::Months6 => Some(182),
            StatPeriod::Year => Some(365),
            StatPeriod::All | StatPeriod::Wrapped => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StatSong {
    pub song: SongRow,
    pub plays: u32,
    pub ms: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StatItem {
    pub id: String,
    pub title: String,
    /// Albums: their artists.
    pub subtitle: String,
    pub thumbnail: Option<String>,
    pub plays: u32,
    pub ms: i64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stats {
    pub total_ms: i64,
    pub plays: u32,
    pub songs: Vec<StatSong>,
    pub artists: Vec<StatItem>,
    pub albums: Vec<StatItem>,
    /// The first listening ever (Unix ms), «с 14 марта 2025».
    pub since_ms: Option<i64>,
    /// Different songs and artists heard in the period (Wrapped's totals).
    pub unique_songs: u32,
    pub unique_artists: u32,
}

/// One history row: when, which event (to remove it), the song.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    pub event_id: i64,
    pub at_ms: i64,
    pub played_ms: i64,
    pub song: SongRow,
}

const ARTISTS: &str = "(SELECT group_concat(a.name, ', ') FROM (SELECT ar.name FROM song_artist_map m JOIN artist ar ON ar.id = m.artistId WHERE m.songId = s.id ORDER BY m.position) a)";
const FIRST_ARTIST: &str =
    "(SELECT m.artistId FROM song_artist_map m WHERE m.songId = s.id ORDER BY m.position LIMIT 1)";

fn song_row(r: &rusqlite::Row, at: usize) -> rusqlite::Result<SongRow> {
    Ok(SongRow {
        id: r.get(at)?,
        title: r.get(at + 1)?,
        artists: r.get::<_, Option<String>>(at + 2)?.unwrap_or_default(),
        album: r.get(at + 3)?,
        duration_s: r.get(at + 4)?,
        thumbnail: r.get(at + 5)?,
        liked: r.get(at + 6)?,
        artist_id: r.get(at + 7)?,
        album_id: r.get(at + 8)?,
        plays: None,
    })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl App {
    /// Statistics for a period: totals and the top songs, artists and albums by listening time.
    pub async fn stats(&self, period: StatPeriod) -> AppResult<Stats> {
        let (from, to) = period.range();
        let stats: Stats = self
            .store()
            .call(move |c| {
                let (total_ms, plays): (i64, i64) = c.query_row(
                    "SELECT coalesce(sum(playTime), 0), count(*) FROM event WHERE timestamp >= ?1 AND timestamp < ?2",
                    [from, to],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                let unique_songs: i64 = c.query_row(
                    "SELECT count(DISTINCT songId) FROM event WHERE timestamp >= ?1 AND timestamp < ?2",
                    [from, to],
                    |r| r.get(0),
                )?;
                let unique_artists: i64 = c.query_row(
                    "SELECT count(DISTINCT m.artistId) FROM event e JOIN song_artist_map m ON m.songId = e.songId AND m.position = 0
                     WHERE e.timestamp >= ?1 AND e.timestamp < ?2",
                    [from, to],
                    |r| r.get(0),
                )?;
                let since_ms: Option<i64> = c.query_row("SELECT min(timestamp) FROM event", [], |r| r.get(0))?;
                let mut st = c.prepare(&format!(
                    "SELECT e.cnt, e.ms, s.id, s.title, {ARTISTS}, s.albumName, s.duration, s.thumbnailUrl, s.liked, {FIRST_ARTIST}, s.albumId
                     FROM (SELECT songId, count(*) AS cnt, sum(playTime) AS ms FROM event WHERE timestamp >= ?1 AND timestamp < ?2
                           GROUP BY songId ORDER BY ms DESC LIMIT 50) e
                     JOIN song s ON s.id = e.songId ORDER BY e.ms DESC"
                ))?;
                let songs = st
                    .query_map([from, to], |r| {
                        Ok(StatSong {
                            plays: r.get::<_, i64>(0)? as u32,
                            ms: r.get(1)?,
                            song: song_row(r, 2)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                // The first artist of each song gets its time (as Android's mostPlayedArtists).
                let mut st = c.prepare(
                    "SELECT ar.id, ar.name,
                            coalesce(
                                ar.thumbnailUrl,
                                (SELECT s2.thumbnailUrl FROM event e2
                                 JOIN song s2 ON s2.id = e2.songId
                                 JOIN song_artist_map sam ON sam.songId = s2.id
                                 WHERE sam.artistId = ar.id AND s2.thumbnailUrl IS NOT NULL AND s2.thumbnailUrl != ''
                                 ORDER BY e2.playTime DESC LIMIT 1),
                                (SELECT s3.thumbnailUrl FROM song s3
                                 JOIN song_artist_map sam2 ON sam2.songId = s3.id
                                 WHERE sam2.artistId = ar.id AND s3.thumbnailUrl IS NOT NULL AND s3.thumbnailUrl != ''
                                 LIMIT 1)
                            ) AS thumb,
                            count(*), sum(e.playTime) FROM event e
                     JOIN song_artist_map m ON m.songId = e.songId AND m.position = 0
                     JOIN artist ar ON ar.id = m.artistId
                     WHERE e.timestamp >= ?1 AND e.timestamp < ?2 GROUP BY ar.id ORDER BY sum(e.playTime) DESC LIMIT 30",
                )?;
                let artists = st
                    .query_map([from, to], |r| {
                        Ok(StatItem {
                            id: r.get(0)?,
                            title: r.get(1)?,
                            subtitle: String::new(),
                            thumbnail: r.get(2)?,
                            plays: r.get::<_, i64>(3)? as u32,
                            ms: r.get(4)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                // Albums by the songs' album name and first artist (most played songs have no album row).
                let mut st = c.prepare(&format!(
                    "SELECT max(s.albumId), s.albumName, max(s.thumbnailUrl), {ARTISTS}, count(*), sum(e.playTime)
                     FROM event e JOIN song s ON s.id = e.songId
                     WHERE e.timestamp >= ?1 AND e.timestamp < ?2 AND s.albumName IS NOT NULL AND s.albumName != ''
                     GROUP BY coalesce(s.albumId, s.albumName) ORDER BY sum(e.playTime) DESC LIMIT 30"
                ))?;
                let albums = st
                    .query_map([from, to], |r| {
                        Ok(StatItem {
                            // Empty when the songs only name their album (it cannot be opened then).
                            id: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                            title: r.get(1)?,
                            thumbnail: r.get(2)?,
                            subtitle: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                            plays: r.get::<_, i64>(4)? as u32,
                            ms: r.get(5)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Stats {
                    total_ms,
                    plays: plays as u32,
                    songs,
                    artists,
                    albums,
                    since_ms,
                    unique_songs: unique_songs as u32,
                    unique_artists: unique_artists as u32,
                })
            })
            .await?;

        // Background resolve high-res Spotify artist avatars for artists missing photos
        let this = self.clone();
        let missing: Vec<String> = stats
            .artists
            .iter()
            .filter(|a| a.id.starts_with("SP_"))
            .map(|a| a.id.clone())
            .collect();
        tokio::spawn(async move {
            for id in missing {
                let has_artist_thumb: bool = this
                    .store()
                    .call({
                        let id = id.clone();
                        move |c| {
                            let t: Option<String> = c
                                .query_row("SELECT thumbnailUrl FROM artist WHERE id = ?1", [&id], |r| r.get(0))
                                .unwrap_or(None);
                            Ok(t.is_some())
                        }
                    })
                    .await
                    .unwrap_or(true);
                if !has_artist_thumb {
                    if let Some(sp_id) = id.strip_prefix("SP_") {
                        if let Ok(page) = this.spotify_or_public().artist(sp_id).await {
                            if let Some(img) = page.images.first() {
                                let thumb = img.url.clone();
                                let _ = this.store().call({
                                    let id = id.clone();
                                    move |c| {
                                        c.execute("UPDATE artist SET thumbnailUrl = ?1 WHERE id = ?2", [&thumb, &id])?;
                                        Ok(())
                                    }
                                }).await;
                            }
                        }
                    }
                }
            }
        });

        Ok(stats)
    }

    /// Wrapped's playlist (WrappedManager.createPlaylist): every song of 2025, the most listened first, as a new
    /// playlist «Drxwnify 2025». Returns its id and how many songs went in.
    pub async fn save_wrapped_playlist(&self) -> AppResult<(String, usize)> {
        let (from, to) = StatPeriod::Wrapped.range();
        let rows: Vec<SongRow> = self
            .store()
            .call(move |c| {
                let mut st = c.prepare(&format!(
                    "SELECT s.id, s.title, {ARTISTS}, s.albumName, s.duration, s.thumbnailUrl, s.liked, {FIRST_ARTIST}, s.albumId
                     FROM (SELECT songId, sum(playTime) AS ms FROM event WHERE timestamp >= ?1 AND timestamp < ?2 GROUP BY songId) e
                     JOIN song s ON s.id = e.songId ORDER BY e.ms DESC"
                ))?;
                let rows = st.query_map([from, to], |r| song_row(r, 0))?.collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await?;
        if rows.is_empty() {
            return Err(crate::AppError::Other("за этот год прослушиваний нет".into()));
        }
        let id = self.create_playlist(&wrapped_playlist()).await?;
        let tracks = rows.into_iter().map(|r| crate::playable_of(r).track).collect();
        let n = self.add_to_playlist(&id, tracks).await?;
        Ok((id, n))
    }

    /// The listening history, newest first, `range` of it.
    pub async fn history(&self, offset: usize, limit: usize) -> AppResult<Vec<HistoryEntry>> {
        Ok(self
            .store()
            .call(move |c| {
                let mut st = c.prepare_cached(&format!(
                    "SELECT e.id, e.timestamp, e.playTime, s.id, s.title, {ARTISTS}, s.albumName, s.duration, s.thumbnailUrl, s.liked, {FIRST_ARTIST}, s.albumId
                     FROM event e JOIN song s ON s.id = e.songId ORDER BY e.timestamp DESC LIMIT ?1 OFFSET ?2"
                ))?;
                let rows = st
                    .query_map([limit as i64, offset as i64], |r| {
                        Ok(HistoryEntry {
                            event_id: r.get(0)?,
                            at_ms: r.get(1)?,
                            played_ms: r.get(2)?,
                            song: song_row(r, 3)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await?)
    }

    /// Removes one listening from the history (and so from the statistics).
    pub async fn remove_history_event(&self, event_id: i64) -> AppResult<()> {
        self.store()
            .call(move |c| {
                c.execute("DELETE FROM event WHERE id = ?1", [event_id])?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    /// Plays songs from a statistics or history list.
    pub fn play_rows(&self, rows: Vec<SongRow>, index: usize) {
        let items: Vec<crate::playback::Playable> = rows.into_iter().map(crate::playable_of).collect();
        self.remember(items.iter().map(|p| &p.track));
        self.inner.playback.play_list(items, index);
    }
}

/// The history's day groups (HistoryScreen): «Сегодня», «Вчера», «На этой неделе», «На прошлой неделе», then months.
pub fn history_group(at_ms: i64, now_ms: i64, utc_offset_s: i64) -> String {
    let day = |ms: i64| (ms / 1000 + utc_offset_s).div_euclid(86_400);
    let (d, today) = (day(at_ms), day(now_ms));
    // Monday-based weeks: 1970-01-01 was a Thursday.
    let weekday = |d: i64| (d + 3).rem_euclid(7);
    let week_start = today - weekday(today);
    match today - d {
        0 => "Сегодня".into(),
        1 => "Вчера".into(),
        _ if d >= week_start => "На этой неделе".into(),
        _ if d >= week_start - 7 => "На прошлой неделе".into(),
        _ => {
            let (y, m) = civil(d);
            const MONTHS: [&str; 12] = [
                "Январь",
                "Февраль",
                "Март",
                "Апрель",
                "Май",
                "Июнь",
                "Июль",
                "Август",
                "Сентябрь",
                "Октябрь",
                "Ноябрь",
                "Декабрь",
            ];
            let (ty, _) = civil(today);
            if y == ty {
                MONTHS[m as usize - 1].to_string()
            } else {
                format!("{} {y}", MONTHS[m as usize - 1])
            }
        }
    }
}

/// (year, month) of a Unix day.
fn civil(days: i64) -> (i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m)
}

#[cfg(test)]
mod year_tests {
    use super::*;

    #[test]
    fn year_bounds() {
        assert_eq!(jan1(2025) * 86_400_000, 1_735_689_600_000);
        assert_eq!(jan1(2026) * 86_400_000, 1_767_225_600_000);
        assert_eq!(civil_year(jan1(2026)), 2026);
        assert_eq!(civil_year(jan1(2026) - 1), 2025);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups() {
        // 2026-11-14 (Saturday) 12:00 UTC.
        let now = 1_794_657_600_000;
        let h = |d: i64| history_group(now - d * 86_400_000, now, 0);
        assert_eq!(h(0), "Сегодня");
        assert_eq!(h(1), "Вчера");
        assert_eq!(h(4), "На этой неделе"); // Tuesday
        assert_eq!(h(8), "На прошлой неделе");
        assert_eq!(h(40), "Октябрь");
        assert_eq!(h(400), "Октябрь 2025");
    }
}
