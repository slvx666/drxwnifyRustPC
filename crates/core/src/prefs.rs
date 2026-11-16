//! App settings in the Android DataStore format (`settings.preferences_pb`).
//!
//! Android keeps almost all settings (and the account sessions) in one protobuf file:
//! ```text
//! message PreferenceMap { map<string, Value> preferences = 1; }
//! message Value { oneof { bool boolean = 1; float float = 2; int32 integer = 3; int64 long = 4;
//!                         string string = 5; StringSet string_set = 6; double double = 7; bytes bytes = 8; } }
//! message StringSet { repeated string strings = 1; }
//! ```
//! The PC stores settings in the same file format so backups go both ways without conversion.
//! Key names are the ones in `android/.../constants/PreferenceKeys.kt`.
//! A hand-written codec keeps the protobuf dependency (and its size) out of Mini.

use crate::error::{Result, malformed};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum PrefValue {
    Bool(bool),
    Float(f32),
    Int(i32),
    Long(i64),
    String(String),
    StringSet(Vec<String>),
    Double(f64),
    Bytes(Vec<u8>),
}

/// All settings, sorted by key.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Prefs {
    pub values: BTreeMap<String, PrefValue>,
}

impl Prefs {
    pub fn get(&self, key: &str) -> Option<&PrefValue> {
        self.values.get(key)
    }
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        match self.get(key)? {
            PrefValue::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn get_str(&self, key: &str) -> Option<&str> {
        match self.get(key)? {
            PrefValue::String(s) => Some(s),
            _ => None,
        }
    }
    pub fn get_i64(&self, key: &str) -> Option<i64> {
        match self.get(key)? {
            PrefValue::Long(v) => Some(*v),
            PrefValue::Int(v) => Some(*v as i64),
            _ => None,
        }
    }
    pub fn get_f32(&self, key: &str) -> Option<f32> {
        match self.get(key)? {
            PrefValue::Float(v) => Some(*v),
            PrefValue::Double(v) => Some(*v as f32),
            _ => None,
        }
    }
    pub fn set(&mut self, key: impl Into<String>, value: PrefValue) {
        self.values.insert(key.into(), value);
    }
    pub fn remove(&mut self, key: &str) -> Option<PrefValue> {
        self.values.remove(key)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut out = Prefs::default();
        let mut r = Reader { b: bytes, i: 0 };
        while !r.done() {
            let (field, wt) = r.tag()?;
            if field == 1 && wt == 2 {
                let entry = r.bytes()?;
                let (k, v) = decode_entry(entry)?;
                out.values.insert(k, v);
            } else {
                r.skip(wt)?;
            }
        }
        Ok(out)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (k, v) in &self.values {
            let mut entry = Vec::new();
            put_len(&mut entry, 1, k.as_bytes());
            put_len(&mut entry, 2, &encode_value(v));
            put_len(&mut out, 1, &entry);
        }
        out
    }
}

fn decode_entry(b: &[u8]) -> Result<(String, PrefValue)> {
    let mut r = Reader { b, i: 0 };
    let (mut key, mut val) = (None, None);
    while !r.done() {
        let (field, wt) = r.tag()?;
        match (field, wt) {
            (1, 2) => key = Some(utf8(r.bytes()?)?),
            (2, 2) => val = Some(decode_value(r.bytes()?)?),
            _ => r.skip(wt)?,
        }
    }
    match (key, val) {
        (Some(k), Some(v)) => Ok((k, v)),
        (Some(k), None) => Err(malformed(
            "preferences_pb",
            format!("key {k} without value"),
        )),
        _ => Err(malformed("preferences_pb", "entry without key")),
    }
}

fn decode_value(b: &[u8]) -> Result<PrefValue> {
    let mut r = Reader { b, i: 0 };
    let mut v = None;
    while !r.done() {
        let (field, wt) = r.tag()?;
        v = Some(match (field, wt) {
            (1, 0) => PrefValue::Bool(r.varint()? != 0),
            (2, 5) => PrefValue::Float(f32::from_le_bytes(r.fixed::<4>()?)),
            (3, 0) => PrefValue::Int(r.varint()? as i32),
            (4, 0) => PrefValue::Long(r.varint()? as i64),
            (5, 2) => PrefValue::String(utf8(r.bytes()?)?),
            (6, 2) => {
                let mut set = Vec::new();
                let mut s = Reader {
                    b: r.bytes()?,
                    i: 0,
                };
                while !s.done() {
                    let (f, w) = s.tag()?;
                    if f == 1 && w == 2 {
                        set.push(utf8(s.bytes()?)?)
                    } else {
                        s.skip(w)?
                    }
                }
                PrefValue::StringSet(set)
            }
            (7, 1) => PrefValue::Double(f64::from_le_bytes(r.fixed::<8>()?)),
            (8, 2) => PrefValue::Bytes(r.bytes()?.to_vec()),
            _ => {
                r.skip(wt)?;
                continue;
            }
        });
    }
    v.ok_or_else(|| malformed("preferences_pb", "empty value"))
}

fn encode_value(v: &PrefValue) -> Vec<u8> {
    let mut o = Vec::new();
    match v {
        PrefValue::Bool(b) => put_varint_field(&mut o, 1, *b as u64),
        PrefValue::Float(f) => {
            put_tag(&mut o, 2, 5);
            o.extend_from_slice(&f.to_le_bytes());
        }
        // int32 negative values are encoded sign-extended to 64 bits, as protobuf requires.
        PrefValue::Int(i) => put_varint_field(&mut o, 3, *i as i64 as u64),
        PrefValue::Long(l) => put_varint_field(&mut o, 4, *l as u64),
        PrefValue::String(s) => put_len(&mut o, 5, s.as_bytes()),
        PrefValue::StringSet(set) => {
            let mut inner = Vec::new();
            for s in set {
                put_len(&mut inner, 1, s.as_bytes());
            }
            put_len(&mut o, 6, &inner);
        }
        PrefValue::Double(d) => {
            put_tag(&mut o, 7, 1);
            o.extend_from_slice(&d.to_le_bytes());
        }
        PrefValue::Bytes(b) => put_len(&mut o, 8, b),
    }
    o
}

fn utf8(b: &[u8]) -> Result<String> {
    String::from_utf8(b.to_vec()).map_err(|_| malformed("preferences_pb", "invalid UTF-8"))
}

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Reader<'a> {
    fn done(&self) -> bool {
        self.i >= self.b.len()
    }
    fn varint(&mut self) -> Result<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = *self
                .b
                .get(self.i)
                .ok_or_else(|| malformed("preferences_pb", "truncated varint"))?;
            self.i += 1;
            v |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(malformed("preferences_pb", "varint too long"))
    }
    fn tag(&mut self) -> Result<(u32, u8)> {
        let t = self.varint()?;
        Ok(((t >> 3) as u32, (t & 7) as u8))
    }
    fn bytes(&mut self) -> Result<&'a [u8]> {
        let n = self.varint()? as usize;
        let end = self.i.checked_add(n).filter(|&e| e <= self.b.len());
        let end = end.ok_or_else(|| malformed("preferences_pb", "length past end"))?;
        let s = &self.b[self.i..end];
        self.i = end;
        Ok(s)
    }
    fn fixed<const N: usize>(&mut self) -> Result<[u8; N]> {
        let s = self
            .b
            .get(self.i..self.i + N)
            .ok_or_else(|| malformed("preferences_pb", "truncated fixed"))?;
        self.i += N;
        Ok(s.try_into().expect("slice length is N"))
    }
    fn skip(&mut self, wt: u8) -> Result<()> {
        match wt {
            0 => self.varint().map(|_| ()),
            1 => self.fixed::<8>().map(|_| ()),
            2 => self.bytes().map(|_| ()),
            5 => self.fixed::<4>().map(|_| ()),
            _ => Err(malformed(
                "preferences_pb",
                format!("unsupported wire type {wt}"),
            )),
        }
    }
}

fn put_varint(o: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        o.push((v as u8) | 0x80);
        v >>= 7;
    }
    o.push(v as u8);
}
fn put_tag(o: &mut Vec<u8>, field: u32, wt: u8) {
    put_varint(o, ((field as u64) << 3) | wt as u64);
}
fn put_varint_field(o: &mut Vec<u8>, field: u32, v: u64) {
    put_tag(o, field, 0);
    put_varint(o, v);
}
fn put_len(o: &mut Vec<u8>, field: u32, b: &[u8]) {
    put_tag(o, field, 2);
    put_varint(o, b.len() as u64);
    o.extend_from_slice(b);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_types() {
        let mut p = Prefs::default();
        p.set("b", PrefValue::Bool(true));
        p.set("f", PrefValue::Float(0.5));
        p.set("i", PrefValue::Int(-3));
        p.set("l", PrefValue::Long(1_700_000_000_000));
        p.set("s", PrefValue::String("Привет 夜".into()));
        p.set("set", PrefValue::StringSet(vec!["a".into(), "б".into()]));
        p.set("d", PrefValue::Double(-1.25));
        p.set("by", PrefValue::Bytes(vec![0, 255, 7]));
        let back = Prefs::decode(&p.encode()).unwrap();
        assert_eq!(back, p);
    }

    /// Decodes a real Android `settings.preferences_pb` and re-encodes it without loss.
    /// Set DRX_TEST_PREFS_PB to an extracted file. Not run in CI.
    #[test]
    fn real_android_prefs_roundtrip() {
        let Ok(path) = std::env::var("DRX_TEST_PREFS_PB") else {
            return;
        };
        let bytes = std::fs::read(path).unwrap();
        let p = Prefs::decode(&bytes).unwrap();
        assert!(p.values.len() > 10);
        assert_eq!(Prefs::decode(&p.encode()).unwrap(), p);
    }
}
