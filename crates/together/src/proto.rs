//! The messages of `metroproto/listentogether.proto`, written and read by hand: a few dozen fields do not need a
//! code generator (and protoc at build time). Field numbers and types follow the .proto exactly.

use std::io::{Read, Write};

// ---- writing -------------------------------------------------------------------------------------------------------

#[derive(Default)]
pub struct Writer(pub Vec<u8>);

impl Writer {
    fn varint(&mut self, mut v: u64) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                self.0.push(b);
                return;
            }
            self.0.push(b | 0x80);
        }
    }
    fn key(&mut self, field: u32, wire: u8) {
        self.varint(u64::from(field) << 3 | u64::from(wire));
    }
    /// proto3: default values (empty, 0, false) are not written.
    pub fn string(&mut self, field: u32, s: &str) {
        if !s.is_empty() {
            self.bytes(field, s.as_bytes());
        }
    }
    pub fn bytes(&mut self, field: u32, b: &[u8]) {
        if !b.is_empty() {
            self.key(field, 2);
            self.varint(b.len() as u64);
            self.0.extend_from_slice(b);
        }
    }
    pub fn int64(&mut self, field: u32, v: i64) {
        if v != 0 {
            self.key(field, 0);
            self.varint(v as u64);
        }
    }
    pub fn uint64(&mut self, field: u32, v: u64) {
        if v != 0 {
            self.key(field, 0);
            self.varint(v);
        }
    }
    pub fn boolean(&mut self, field: u32, v: bool) {
        if v {
            self.key(field, 0);
            self.varint(1);
        }
    }
    pub fn float(&mut self, field: u32, v: f32) {
        if v != 0.0 {
            self.key(field, 5);
            self.0.extend_from_slice(&v.to_le_bytes());
        }
    }
    /// A sub-message (always written when present, even empty).
    pub fn message(&mut self, field: u32, m: &Writer) {
        self.key(field, 2);
        self.varint(m.0.len() as u64);
        self.0.extend_from_slice(&m.0);
    }
}

// ---- reading -------------------------------------------------------------------------------------------------------

pub enum Value<'a> {
    Varint(u64),
    Fixed64(u64),
    Bytes(&'a [u8]),
    Fixed32(u32),
}

/// The fields of one message in order; unknown wire types end the reading (the rest is ignored).
pub struct Fields<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Fields<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, at: 0 }
    }
    fn varint(&mut self) -> Option<u64> {
        let mut v: u64 = 0;
        for shift in (0..64).step_by(7) {
            let b = *self.buf.get(self.at)?;
            self.at += 1;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }
}

impl<'a> Iterator for Fields<'a> {
    type Item = (u32, Value<'a>);
    fn next(&mut self) -> Option<Self::Item> {
        if self.at >= self.buf.len() {
            return None;
        }
        let key = self.varint()?;
        let field = (key >> 3) as u32;
        let value = match key & 7 {
            0 => Value::Varint(self.varint()?),
            1 => {
                let b = self.buf.get(self.at..self.at + 8)?;
                self.at += 8;
                Value::Fixed64(u64::from_le_bytes(b.try_into().ok()?))
            }
            2 => {
                let n = usize::try_from(self.varint()?).ok()?;
                let b = self.buf.get(self.at..self.at.checked_add(n)?)?;
                self.at += n;
                Value::Bytes(b)
            }
            5 => {
                let b = self.buf.get(self.at..self.at + 4)?;
                self.at += 4;
                Value::Fixed32(u32::from_le_bytes(b.try_into().ok()?))
            }
            _ => return None,
        };
        Some((field, value))
    }
}

impl Value<'_> {
    pub fn str(&self) -> String {
        match self {
            Value::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
            _ => String::new(),
        }
    }
    pub fn i64(&self) -> i64 {
        match self {
            Value::Varint(v) | Value::Fixed64(v) => *v as i64,
            Value::Fixed32(v) => i64::from(*v),
            Value::Bytes(_) => 0,
        }
    }
    pub fn u64(&self) -> u64 {
        self.i64() as u64
    }
    pub fn bool(&self) -> bool {
        self.i64() != 0
    }
    pub fn f32(&self) -> f32 {
        match self {
            Value::Fixed32(v) => f32::from_bits(*v),
            _ => 0.0,
        }
    }
    pub fn bytes(&self) -> &[u8] {
        match self {
            Value::Bytes(b) => b,
            _ => &[],
        }
    }
}

// ---- messages ------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrackInfo {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Milliseconds.
    pub duration: i64,
    pub thumbnail: String,
    pub suggested_by: String,
}

impl TrackInfo {
    pub fn write(&self) -> Writer {
        let mut w = Writer::default();
        w.string(1, &self.id);
        w.string(2, &self.title);
        w.string(3, &self.artist);
        w.string(4, &self.album);
        w.int64(5, self.duration);
        w.string(6, &self.thumbnail);
        w.string(7, &self.suggested_by);
        w
    }
    pub fn read(b: &[u8]) -> Self {
        let mut t = Self::default();
        for (f, v) in Fields::new(b) {
            match f {
                1 => t.id = v.str(),
                2 => t.title = v.str(),
                3 => t.artist = v.str(),
                4 => t.album = v.str(),
                5 => t.duration = v.i64(),
                6 => t.thumbnail = v.str(),
                7 => t.suggested_by = v.str(),
                _ => {}
            }
        }
        t
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UserInfo {
    pub user_id: String,
    pub username: String,
    pub is_host: bool,
    pub is_connected: bool,
}

impl UserInfo {
    pub fn read(b: &[u8]) -> Self {
        let mut u = Self::default();
        for (f, v) in Fields::new(b) {
            match f {
                1 => u.user_id = v.str(),
                2 => u.username = v.str(),
                3 => u.is_host = v.bool(),
                4 => u.is_connected = v.bool(),
                _ => {}
            }
        }
        u
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RoomState {
    pub room_code: String,
    pub host_id: String,
    pub users: Vec<UserInfo>,
    pub current_track: Option<TrackInfo>,
    pub is_playing: bool,
    pub position: i64,
    pub last_update: i64,
    pub volume: f32,
    pub queue: Vec<TrackInfo>,
    pub revision: u64,
}

impl RoomState {
    pub fn read(b: &[u8]) -> Self {
        let mut s = Self::default();
        for (f, v) in Fields::new(b) {
            match f {
                1 => s.room_code = v.str(),
                2 => s.host_id = v.str(),
                3 => s.users.push(UserInfo::read(v.bytes())),
                4 => s.current_track = Some(TrackInfo::read(v.bytes())),
                5 => s.is_playing = v.bool(),
                6 => s.position = v.i64(),
                7 => s.last_update = v.i64(),
                8 => s.volume = v.f32(),
                9 => s.queue.push(TrackInfo::read(v.bytes())),
                10 => s.revision = v.u64(),
                _ => {}
            }
        }
        s
    }
}

/// PlaybackActionPayload: what the host sends, and what the server relays to guests as `sync_playback`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlaybackAction {
    pub action: String,
    pub track_id: String,
    pub position: i64,
    pub track_info: Option<TrackInfo>,
    pub insert_next: bool,
    pub queue: Vec<TrackInfo>,
    pub queue_title: String,
    pub volume: f32,
    pub server_time: i64,
    pub revision: u64,
    pub captured_at_server_time: i64,
}

impl PlaybackAction {
    pub fn write(&self) -> Writer {
        let mut w = Writer::default();
        w.string(1, &self.action);
        w.string(2, &self.track_id);
        w.int64(3, self.position);
        if let Some(t) = &self.track_info {
            w.message(4, &t.write());
        }
        w.boolean(5, self.insert_next);
        for t in &self.queue {
            w.message(6, &t.write());
        }
        w.string(7, &self.queue_title);
        w.float(8, self.volume);
        w.int64(9, self.server_time);
        w.uint64(10, self.revision);
        w.int64(11, self.captured_at_server_time);
        w
    }
    pub fn read(b: &[u8]) -> Self {
        let mut a = Self::default();
        for (f, v) in Fields::new(b) {
            match f {
                1 => a.action = v.str(),
                2 => a.track_id = v.str(),
                3 => a.position = v.i64(),
                4 => a.track_info = Some(TrackInfo::read(v.bytes())),
                5 => a.insert_next = v.bool(),
                6 => a.queue.push(TrackInfo::read(v.bytes())),
                7 => a.queue_title = v.str(),
                8 => a.volume = v.f32(),
                9 => a.server_time = v.i64(),
                10 => a.revision = v.u64(),
                11 => a.captured_at_server_time = v.i64(),
                _ => {}
            }
        }
        a
    }
}

/// SyncStatePayload (answer to `request_sync`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyncState {
    pub current_track: Option<TrackInfo>,
    pub is_playing: bool,
    pub position: i64,
    pub last_update: i64,
    pub queue: Vec<TrackInfo>,
    pub volume: f32,
}

impl SyncState {
    pub fn read(b: &[u8]) -> Self {
        let mut s = Self::default();
        for (f, v) in Fields::new(b) {
            match f {
                1 => s.current_track = Some(TrackInfo::read(v.bytes())),
                2 => s.is_playing = v.bool(),
                3 => s.position = v.i64(),
                4 => s.last_update = v.i64(),
                5 => s.queue.push(TrackInfo::read(v.bytes())),
                6 => s.volume = v.f32(),
                _ => {}
            }
        }
        s
    }
}

/// A message of only string fields 1, 2, 3… (most payloads: room_created, join_request, user_joined, error…).
pub fn strings(b: &[u8]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (f, v) in Fields::new(b) {
        let i = f as usize;
        if i == 0 || i > 16 {
            continue;
        }
        if out.len() < i {
            out.resize(i, String::new());
        }
        if let Value::Bytes(_) = v {
            out[i - 1] = v.str();
        }
    }
    out
}

/// Writes string fields 1, 2, 3… (create_room, join_room, approve_join, kick_user…).
pub fn write_strings(values: &[&str]) -> Writer {
    let mut w = Writer::default();
    for (i, s) in values.iter().enumerate() {
        w.string(i as u32 + 1, s);
    }
    w
}

// ---- the envelope --------------------------------------------------------------------------------------------------

/// Above this many bytes a payload is sent gzipped when that is smaller (MessageCodec.COMPRESSION_THRESHOLD).
const COMPRESSION_THRESHOLD: usize = 100;

/// Envelope { type = 1, payload = 2, compressed = 3 }.
pub fn envelope(kind: &str, payload: &[u8], compress: bool) -> Vec<u8> {
    let mut body = payload.to_vec();
    let mut compressed = false;
    if compress && body.len() > COMPRESSION_THRESHOLD {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        if gz.write_all(&body).is_ok()
            && let Ok(z) = gz.finish()
            && z.len() < body.len()
        {
            body = z;
            compressed = true;
        }
    }
    let mut w = Writer::default();
    w.string(1, kind);
    w.bytes(2, &body);
    w.boolean(3, compressed);
    w.0
}

/// The type and the (unpacked) payload of an envelope.
pub fn open(data: &[u8]) -> Option<(String, Vec<u8>)> {
    let (mut kind, mut payload, mut compressed) = (String::new(), Vec::new(), false);
    for (f, v) in Fields::new(data) {
        match f {
            1 => kind = v.str(),
            2 => payload = v.bytes().to_vec(),
            3 => compressed = v.bool(),
            _ => {}
        }
    }
    if kind.is_empty() {
        return None;
    }
    if compressed {
        let mut out = Vec::new();
        if flate2::read::GzDecoder::new(payload.as_slice()).read_to_end(&mut out).is_ok() {
            payload = out;
        }
    }
    Some((kind, payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let a = PlaybackAction {
            action: "change_track".into(),
            track_id: "abc".into(),
            position: 123_456,
            track_info: Some(TrackInfo { id: "abc".into(), title: "Т".into(), duration: 200_000, ..Default::default() }),
            queue: vec![TrackInfo { id: "q".into(), ..Default::default() }; 3],
            volume: 0.5,
            revision: 7,
            ..Default::default()
        };
        let bytes = envelope("playback_action", &a.write().0, true);
        let (kind, payload) = open(&bytes).unwrap();
        assert_eq!(kind, "playback_action");
        assert_eq!(PlaybackAction::read(&payload), a);
        assert_eq!(strings(&write_strings(&["ROOM1234", "", "tok"]).0), vec!["ROOM1234", "", "tok"]);
    }

    #[test]
    fn matches_protobuf_bytes() {
        // Envelope{type:"ping"} as protobuf-java writes it: 0a 04 'p' 'i' 'n' 'g'.
        assert_eq!(envelope("ping", &[], false), b"\x0a\x04ping".to_vec());
        // Negative int64 is a 10-byte varint.
        let mut w = Writer::default();
        w.int64(3, -1);
        assert_eq!(w.0.len(), 11);
        assert_eq!(Fields::new(&w.0).next().unwrap().1.i64(), -1);
    }
}
