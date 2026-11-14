//! Catalogs: Spotify, Yandex, VK as a catalog, YouTube Music; main account, cross-catalog lookup, like mode,
//! recommendations, home feed (android/docs/PORTING-2.3.0.md sections 2-5).

pub mod model;
pub mod spotify;

/// JSON navigation helpers: missing or wrong-typed fields read as None (Spotify reshuffles fields often).
pub(crate) mod js {
    use serde_json::Value;

    pub fn obj<'a>(v: &'a Value, k: &str) -> Option<&'a Value> {
        v.get(k).filter(|x| x.is_object())
    }
    pub fn arr<'a>(v: &'a Value, k: &str) -> &'a [Value] {
        v.get(k)
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
    pub fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
        v.get(k).and_then(Value::as_str)
    }
    pub fn i(v: &Value, k: &str) -> Option<i64> {
        v.get(k).and_then(|x| {
            x.as_i64()
                .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
        })
    }
    /// Follows a path of object keys.
    pub fn path<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
        let mut cur = v;
        for k in keys {
            cur = cur.get(*k).filter(|x| !x.is_null())?;
        }
        Some(cur)
    }
}
