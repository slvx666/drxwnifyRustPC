//! HLS audio: media playlists with optional AES-128 segments (VK) and fragmented MP4 (SoundCloud CMAF).
//!
//! The segments are fetched one after another and turned into one continuous stream:
//! - MPEG-TS segments (VK): decrypted, then the audio elementary stream (MP3 or ADTS AAC) is pulled out
//!   (port of `TsAudioDemuxer.kt`);
//! - fMP4 segments: the init segment (`#EXT-X-MAP`) and fragments are joined; symphonia reads fragmented MP4.
//!
//! IV rule (RFC 8216): the `IV=` attribute, or else the segment's media sequence number as a 128-bit big-endian value.

use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
use std::collections::HashMap;
use std::io::Read;

type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

/// Downloads a whole small resource (playlist, key, segment). Implemented by the app with the core HTTP client.
pub trait Fetcher: Send + Sync + 'static {
    fn get(&self, url: &str) -> std::io::Result<Vec<u8>>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct Key {
    pub url: String,
    pub iv: Option<[u8; 16]>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub url: String,
    pub duration_s: f64,
    pub key: Option<Key>,
    pub sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MediaPlaylist {
    pub segments: Vec<Segment>,
    /// `#EXT-X-MAP` init segment (fMP4).
    pub init: Option<String>,
}

impl MediaPlaylist {
    pub fn duration_s(&self) -> f64 {
        self.segments.iter().map(|s| s.duration_s).sum()
    }
    /// True for fragmented MP4 (CMAF) playlists.
    pub fn is_fmp4(&self) -> bool {
        self.init.is_some()
            || self
                .segments
                .first()
                .is_some_and(|s| s.url.split('?').next().unwrap_or("").ends_with(".m4s"))
    }
}

fn resolve(base: &str, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    let base_no_query = base.split('?').next().unwrap_or(base);
    if let Some(rest) = href.strip_prefix('/') {
        // Absolute path on the same host.
        if let Some(scheme_end) = base_no_query.find("://") {
            let host_end = base_no_query[scheme_end + 3..]
                .find('/')
                .map(|i| scheme_end + 3 + i)
                .unwrap_or(base_no_query.len());
            return format!("{}/{}", &base_no_query[..host_end], rest);
        }
    }
    match base_no_query.rfind('/') {
        Some(i) => format!("{}/{}", &base_no_query[..i], href),
        None => href.to_string(),
    }
}

fn attr<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let pat = format!("{name}=");
    let start = line.find(&pat)? + pat.len();
    let rest = &line[start..];
    if let Some(q) = rest.strip_prefix('"') {
        q.split('"').next()
    } else {
        rest.split(',').next()
    }
}

fn hex16(s: &str) -> Option<[u8; 16]> {
    let s = s.trim_start_matches("0x").trim_start_matches("0X");
    let mut out = [0u8; 16];
    let s = format!("{s:0>32}");
    for i in 0..16 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Parses a media playlist. Encrypted playlists are accepted only with plain AES-128 (SAMPLE-AES/DRM are refused).
pub fn parse_media_playlist(text: &str, base_url: &str) -> Result<MediaPlaylist, String> {
    let mut pl = MediaPlaylist::default();
    let mut seq: u64 = 0;
    let mut key: Option<Key> = None;
    let mut pending_dur: Option<f64> = None;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(v) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            seq = v.trim().parse().unwrap_or(0);
        } else if line.starts_with("#EXT-X-KEY") {
            match attr(line, "METHOD") {
                Some("NONE") => key = None,
                Some("AES-128") => {
                    let url = attr(line, "URI").ok_or("key without URI")?;
                    key = Some(Key {
                        url: resolve(base_url, url),
                        iv: attr(line, "IV").and_then(hex16),
                    });
                }
                Some(other) => return Err(format!("unsupported encryption {other}")),
                None => return Err("key without METHOD".into()),
            }
        } else if line.starts_with("#EXT-X-MAP") {
            pl.init = attr(line, "URI").map(|u| resolve(base_url, u));
        } else if let Some(v) = line.strip_prefix("#EXTINF:") {
            pending_dur = v.split(',').next().and_then(|d| d.trim().parse().ok());
        } else if line.starts_with("#EXT-X-STREAM-INF") {
            return Err("master playlist; pick a variant first".into());
        } else if !line.starts_with('#') {
            pl.segments.push(Segment {
                url: resolve(base_url, line),
                duration_s: pending_dur.take().unwrap_or(0.0),
                key: key.clone(),
                sequence: seq,
            });
            seq += 1;
        }
    }
    if pl.segments.is_empty() {
        return Err("playlist has no segments".into());
    }
    Ok(pl)
}

fn decrypt(data: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> std::io::Result<Vec<u8>> {
    let mut buf = data.to_vec();
    let n = Aes128CbcDec::new(key.into(), iv.into())
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|_| std::io::Error::other("AES-128 decrypt failed (bad key or padding)"))?
        .len();
    buf.truncate(n);
    Ok(buf)
}

fn sequence_iv(seq: u64) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[8..].copy_from_slice(&seq.to_be_bytes());
    iv
}

/// Pulls the audio elementary stream (MP3 or ADTS AAC frames) out of MPEG-TS (port of TsAudioDemuxer.kt).
pub fn ts_extract_audio(ts: &[u8]) -> Option<Vec<u8>> {
    const PACKET: usize = 188;
    const AUDIO_TYPES: [u8; 4] = [0x03, 0x04, 0x0F, 0x11];
    let mut pmt_pid: i32 = -1;
    let mut audio_pid: i32 = -1;
    let mut out = Vec::with_capacity(ts.len());
    let mut i = 0;
    while i + PACKET <= ts.len() {
        if ts[i] != 0x47 {
            i += 1;
            continue;
        }
        let end = i + PACKET;
        let payload_start = ts[i + 1] & 0x40 != 0;
        let pid = (((ts[i + 1] & 0x1F) as i32) << 8) | ts[i + 2] as i32;
        let adaptation = (ts[i + 3] >> 4) & 0x3;
        let mut p = i + 4;
        if adaptation == 2 || adaptation == 3 {
            p += 1 + ts[p] as usize;
        }
        if (adaptation == 1 || adaptation == 3) && p < end {
            if pid == 0 && pmt_pid < 0 {
                pmt_pid = pat_pmt_pid(&ts[..end], p, payload_start);
            } else if pid == pmt_pid && audio_pid < 0 {
                audio_pid = pmt_audio_pid(&ts[..end], p, payload_start, &AUDIO_TYPES);
            } else if pid == audio_pid && audio_pid >= 0 {
                if payload_start && p + 9 <= end && ts[p] == 0 && ts[p + 1] == 0 && ts[p + 2] == 1 {
                    p += 9 + ts[p + 8] as usize;
                }
                if p < end {
                    out.extend_from_slice(&ts[p..end]);
                }
            }
        }
        i = end;
    }
    (audio_pid >= 0 && !out.is_empty()).then_some(out)
}

fn pat_pmt_pid(ts: &[u8], start: usize, payload_start: bool) -> i32 {
    let mut p = start;
    if payload_start {
        p += 1 + ts[p] as usize;
    }
    if p + 8 > ts.len() {
        return -1;
    }
    let section_len = (((ts[p + 1] & 0x0F) as usize) << 8) | ts[p + 2] as usize;
    let table_end = (p + 3 + section_len).saturating_sub(4).min(ts.len());
    let mut e = p + 8;
    while e + 4 <= table_end {
        let program = ((ts[e] as u16) << 8) | ts[e + 1] as u16;
        let pid = (((ts[e + 2] & 0x1F) as i32) << 8) | ts[e + 3] as i32;
        if program != 0 {
            return pid;
        }
        e += 4;
    }
    -1
}

fn pmt_audio_pid(ts: &[u8], start: usize, payload_start: bool, types: &[u8]) -> i32 {
    let mut p = start;
    if payload_start {
        p += 1 + ts[p] as usize;
    }
    if p + 12 > ts.len() {
        return -1;
    }
    let section_len = (((ts[p + 1] & 0x0F) as usize) << 8) | ts[p + 2] as usize;
    let info_len = (((ts[p + 10] & 0x0F) as usize) << 8) | ts[p + 11] as usize;
    let table_end = (p + 3 + section_len).saturating_sub(4).min(ts.len());
    let mut e = p + 12 + info_len;
    while e + 5 <= table_end {
        let stream_type = ts[e];
        let pid = (((ts[e + 1] & 0x1F) as i32) << 8) | ts[e + 2] as i32;
        let es_info = (((ts[e + 3] & 0x0F) as usize) << 8) | ts[e + 4] as usize;
        if types.contains(&stream_type) {
            return pid;
        }
        e += 5 + es_info;
    }
    -1
}

/// The joined audio of all segments as one `Read`. Segments are fetched lazily.
pub struct HlsReader {
    fetcher: std::sync::Arc<dyn Fetcher>,
    playlist: MediaPlaylist,
    next: usize,
    buf: Vec<u8>,
    pos: usize,
    keys: HashMap<String, [u8; 16]>,
    init_sent: bool,
    /// AAC configuration of an fMP4 playlist: its segments are turned into ADTS (decodable front to back).
    adts: Option<crate::fmp4::AudioConfig>,
    /// Container hint for the decoder: "mp3", "aac" or "mp4".
    kind: Option<&'static str>,
}

impl HlsReader {
    pub fn new(fetcher: std::sync::Arc<dyn Fetcher>, playlist: MediaPlaylist) -> Self {
        Self {
            fetcher,
            playlist,
            next: 0,
            buf: Vec::new(),
            pos: 0,
            keys: HashMap::new(),
            init_sent: false,
            adts: None,
            kind: None,
        }
    }

    /// Fetches the first segment to learn the format. Returns the decoder hint.
    pub fn probe(&mut self) -> std::io::Result<&'static str> {
        if self.kind.is_none() && self.pos >= self.buf.len() {
            self.fill()?;
        }
        Ok(self.kind.unwrap_or("mp3"))
    }

    pub fn duration_s(&self) -> f64 {
        self.playlist.duration_s()
    }

    fn fill(&mut self) -> std::io::Result<bool> {
        if self.playlist.is_fmp4() && !self.init_sent {
            self.init_sent = true;
            if let Some(init) = self.playlist.init.clone() {
                let bytes = self.fetcher.get(&init)?;
                match crate::fmp4::parse_init(&bytes) {
                    Some(cfg) => {
                        // AAC: the init carries no samples; segments become ADTS frames.
                        self.adts = Some(cfg);
                        self.kind = Some("aac");
                    }
                    None => {
                        self.buf = bytes;
                        self.pos = 0;
                        self.kind = Some("mp4");
                        return Ok(true);
                    }
                }
            }
        }
        let Some(seg) = self.playlist.segments.get(self.next).cloned() else {
            return Ok(false);
        };
        self.next += 1;
        let mut data = self.fetcher.get(&seg.url)?;
        if let Some(k) = &seg.key {
            let key = match self.keys.get(&k.url) {
                Some(k) => *k,
                None => {
                    let raw = self.fetcher.get(&k.url)?;
                    let key: [u8; 16] = raw
                        .get(..16)
                        .and_then(|s| s.try_into().ok())
                        .ok_or_else(|| std::io::Error::other("bad AES key"))?;
                    self.keys.insert(k.url.clone(), key);
                    key
                }
            };
            data = decrypt(
                &data,
                &key,
                &k.iv.unwrap_or_else(|| sequence_iv(seg.sequence)),
            )?;
        }
        if let Some(cfg) = self.adts {
            self.buf = crate::fmp4::transmux(&data, cfg)
                .ok_or_else(|| std::io::Error::other("bad fMP4 segment"))?;
        } else if self.playlist.is_fmp4() {
            self.kind = Some("mp4");
            self.buf = data;
        } else if data.first() == Some(&0x47) {
            let audio = ts_extract_audio(&data)
                .ok_or_else(|| std::io::Error::other("TS segment without audio"))?;
            if self.kind.is_none() {
                self.kind = Some(
                    if audio.len() > 1 && audio[0] == 0xFF && audio[1] & 0xF6 == 0xF0 {
                        "aac"
                    } else {
                        "mp3"
                    },
                );
            }
            self.buf = audio;
        } else {
            // Raw ADTS/MP3 segments.
            self.kind.get_or_insert(
                if data.len() > 1 && data[0] == 0xFF && data[1] & 0xF6 == 0xF0 {
                    "aac"
                } else {
                    "mp3"
                },
            );
            self.buf = data;
        }
        self.pos = 0;
        Ok(true)
    }
}

impl Read for HlsReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.pos >= self.buf.len() {
            if !self.fill()? {
                return Ok(0);
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Feeds an HLS playlist into [`crate::source::CachedSource`]. HLS has no byte ranges, so opening at an offset
/// re-reads from the start and skips; the cache usually has the bytes long before a seek needs them (VK tracks are
/// ~10 MB and download in seconds).
pub struct HlsOpener {
    pub fetcher: std::sync::Arc<dyn Fetcher>,
    pub playlist: MediaPlaylist,
}

impl crate::source::RangeOpener for HlsOpener {
    fn open(&self, offset: u64) -> std::io::Result<(Option<u64>, Box<dyn Read + Send>)> {
        let mut r = HlsReader::new(self.fetcher.clone(), self.playlist.clone());
        if offset > 0 {
            std::io::copy(&mut (&mut r).take(offset), &mut std::io::sink())?;
        }
        Ok((None, Box::new(r)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockEncryptMut;
    use std::sync::Arc;

    type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;

    #[test]
    fn parses_vk_style_playlist() {
        let text = "#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXT-X-MEDIA-SEQUENCE:5\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.pub\"\n#EXTINF:10.0,\nseg-1.ts?x=1\n#EXT-X-KEY:METHOD=NONE\n#EXTINF:4.5,\nhttps://cdn/other/seg-2.ts\n#EXT-X-ENDLIST\n";
        let pl = parse_media_playlist(text, "https://vk.example/audio/index.m3u8?token=1").unwrap();
        assert_eq!(pl.segments.len(), 2);
        assert_eq!(pl.segments[0].url, "https://vk.example/audio/seg-1.ts?x=1");
        assert_eq!(pl.segments[0].sequence, 5);
        assert_eq!(
            pl.segments[0].key.as_ref().unwrap().url,
            "https://vk.example/audio/key.pub"
        );
        assert!(pl.segments[1].key.is_none());
        assert!((pl.duration_s() - 14.5).abs() < 1e-9);
        assert!(
            parse_media_playlist(
                "#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"k\"\n#EXTINF:1,\na.ts",
                "https://x/y"
            )
            .is_err()
        );
    }

    /// A tiny TS: PAT → PMT with an MP3 stream on pid 0x101 → one PES packet carrying `payload`.
    fn make_ts(payload: &[u8]) -> Vec<u8> {
        let mut ts = Vec::new();
        let packet = |pid: u16, start: bool, body: &[u8]| -> Vec<u8> {
            let mut p = vec![
                0x47,
                (if start { 0x40 } else { 0 }) | ((pid >> 8) as u8 & 0x1F),
                pid as u8,
                0x10,
            ];
            p.extend_from_slice(body);
            p.resize(188, 0xFF);
            p
        };
        // PAT: pointer, table id 0, section length 13, ts id, version, section, last, program 1 → PMT pid 0x100, CRC.
        let pat = [
            0x00, 0x00, 0xB0, 0x0D, 0x00, 0x01, 0xC1, 0x00, 0x00, 0x00, 0x01, 0xE1, 0x00, 0, 0, 0,
            0,
        ];
        ts.extend(packet(0, true, &pat));
        // PMT: pointer, table 2, length 18, program 1, ..., PCR pid, info len 0, stream type 3 (MP3) on pid 0x101, CRC.
        let pmt = [
            0x00, 0x02, 0xB0, 0x12, 0x00, 0x01, 0xC1, 0x00, 0x00, 0xE1, 0x01, 0xF0, 0x00, 0x03,
            0xE1, 0x01, 0xF0, 0x00, 0, 0, 0, 0,
        ];
        ts.extend(packet(0x100, true, &pmt));
        // PES: start code, stream id, length, flags, header length 0, then payload (fits one packet).
        let mut pes = vec![0, 0, 1, 0xC0, 0, 0, 0x80, 0x00, 0x00];
        pes.extend_from_slice(payload);
        let mut p = vec![0x47, 0x40 | 0x01, 0x01, 0x10];
        p.extend_from_slice(&pes);
        // Use an adaptation field to pad so no 0xFF filler follows the payload.
        let pad = 188 - p.len();
        let mut q = vec![0x47, 0x40 | 0x01, 0x01, 0x30, (pad - 1) as u8];
        q.extend(std::iter::repeat_n(0xFF, pad - 1));
        q.extend_from_slice(&pes);
        assert_eq!(q.len(), 188);
        ts.extend(q);
        ts
    }

    #[test]
    fn extracts_audio_from_ts() {
        let payload: Vec<u8> = (0..100u8).collect();
        assert_eq!(ts_extract_audio(&make_ts(&payload)).unwrap(), payload);
    }

    struct Mem(HashMap<String, Vec<u8>>);
    impl Fetcher for Mem {
        fn get(&self, url: &str) -> std::io::Result<Vec<u8>> {
            self.0
                .get(url)
                .cloned()
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, url.to_string()))
        }
    }

    #[test]
    fn reads_encrypted_segments_with_sequence_iv() {
        let key = [7u8; 16];
        let a: Vec<u8> = (0..120u8).collect();
        let b: Vec<u8> = (120..220u8).collect();
        let enc = |data: &[u8], seq: u64| {
            let iv = sequence_iv(seq);
            Aes128CbcEnc::new(&key.into(), &iv.into()).encrypt_padded_vec_mut::<Pkcs7>(data)
        };
        let mut files = HashMap::new();
        files.insert("https://h/k".to_string(), key.to_vec());
        files.insert("https://h/1.ts".to_string(), enc(&make_ts(&a), 9));
        files.insert("https://h/2.ts".to_string(), make_ts(&b));
        let text = "#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:9\n#EXT-X-KEY:METHOD=AES-128,URI=\"https://h/k\"\n#EXTINF:1,\n1.ts\n#EXT-X-KEY:METHOD=NONE\n#EXTINF:1,\n2.ts\n";
        let pl = parse_media_playlist(text, "https://h/p.m3u8").unwrap();
        let mut r = HlsReader::new(Arc::new(Mem(files)), pl);
        assert_eq!(r.probe().unwrap(), "mp3");
        let mut all = Vec::new();
        r.read_to_end(&mut all).unwrap();
        assert_eq!(all, [a, b].concat());
    }
}
