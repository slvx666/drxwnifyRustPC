//! Android SharedPreferences XML files (`shared_prefs/<name>.xml` in a backup).
//!
//! Android writes them with `XmlUtils.writeMapXml`:
//! ```xml
//! <?xml version='1.0' encoding='utf-8' standalone='yes' ?>
//! <map>
//!     <string name="key">value</string>
//!     <boolean name="key" value="true" />
//!     <int name="key" value="1" />  <long .../>  <float .../>
//!     <set name="key"><string>a</string></set>
//!     <null name="key" />
//! </map>
//! ```
//! The friends profile with the Nostr private key (`friends.xml`, `sk`), chat cursors (`chat.xml`) and the VK login live
//! here. The format is small and fixed, so a tiny parser is used instead of an XML library (Mini size budget).

use crate::error::{Result, malformed};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum SpValue {
    String(String),
    Bool(bool),
    Int(i32),
    Long(i64),
    Float(f32),
    Set(Vec<String>),
    Null,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SharedPrefs {
    pub values: BTreeMap<String, SpValue>,
}

impl SharedPrefs {
    pub fn get_str(&self, key: &str) -> Option<&str> {
        match self.values.get(key)? {
            SpValue::String(s) => Some(s),
            _ => None,
        }
    }
    pub fn get_i64(&self, key: &str) -> Option<i64> {
        match self.values.get(key)? {
            SpValue::Long(v) => Some(*v),
            SpValue::Int(v) => Some(*v as i64),
            _ => None,
        }
    }

    pub fn parse(xml: &str) -> Result<Self> {
        let mut out = SharedPrefs::default();
        let mut p = Parser { s: xml, i: 0 };
        p.skip_prolog();
        p.expect_open("map")?;
        loop {
            p.ws();
            if p.eat("</map>") {
                break;
            }
            if p.eat("<map/>") || p.done() {
                break;
            }
            let (tag, attrs, self_closing) = p.open_tag()?;
            let name = attr(&attrs, "name")
                .ok_or_else(|| malformed("shared_prefs", format!("<{tag}> without name")))?;
            let value = match tag.as_str() {
                "string" => {
                    if self_closing {
                        SpValue::String(String::new())
                    } else {
                        SpValue::String(p.text_until("</string>")?)
                    }
                }
                "set" => {
                    let mut items = Vec::new();
                    if !self_closing {
                        loop {
                            p.ws();
                            if p.eat("</set>") {
                                break;
                            }
                            let (t, _, sc) = p.open_tag()?;
                            if t != "string" {
                                return Err(malformed(
                                    "shared_prefs",
                                    format!("<{t}> inside <set>"),
                                ));
                            }
                            items.push(if sc {
                                String::new()
                            } else {
                                p.text_until("</string>")?
                            });
                        }
                    }
                    SpValue::Set(items)
                }
                "null" => SpValue::Null,
                "boolean" | "int" | "long" | "float" => {
                    let v = attr(&attrs, "value")
                        .ok_or_else(|| malformed("shared_prefs", "missing value"))?;
                    let bad = || malformed("shared_prefs", format!("bad {tag} value {v}"));
                    if !self_closing {
                        p.text_until(&format!("</{tag}>"))?;
                    }
                    match tag.as_str() {
                        "boolean" => SpValue::Bool(v == "true"),
                        "int" => SpValue::Int(v.parse().map_err(|_| bad())?),
                        "long" => SpValue::Long(v.parse().map_err(|_| bad())?),
                        _ => SpValue::Float(v.parse().map_err(|_| bad())?),
                    }
                }
                other => return Err(malformed("shared_prefs", format!("unknown tag <{other}>"))),
            };
            out.values.insert(name, value);
        }
        Ok(out)
    }

    /// Writes the file the way Android does, so the phone can read a PC backup.
    pub fn to_xml(&self) -> String {
        let mut o =
            String::from("<?xml version='1.0' encoding='utf-8' standalone='yes' ?>\n<map>\n");
        for (k, v) in &self.values {
            let k = escape(k);
            match v {
                SpValue::String(s) => {
                    o += &format!("    <string name=\"{k}\">{}</string>\n", escape(s))
                }
                SpValue::Bool(b) => o += &format!("    <boolean name=\"{k}\" value=\"{b}\" />\n"),
                SpValue::Int(i) => o += &format!("    <int name=\"{k}\" value=\"{i}\" />\n"),
                SpValue::Long(l) => o += &format!("    <long name=\"{k}\" value=\"{l}\" />\n"),
                SpValue::Float(f) => o += &format!("    <float name=\"{k}\" value=\"{f}\" />\n"),
                SpValue::Null => o += &format!("    <null name=\"{k}\" />\n"),
                SpValue::Set(items) => {
                    o += &format!("    <set name=\"{k}\">\n");
                    for s in items {
                        o += &format!("        <string>{}</string>\n", escape(s));
                    }
                    o += "    </set>\n";
                }
            }
        }
        o += "</map>\n";
        o
    }
}

fn attr(attrs: &[(String, String)], key: &str) -> Option<String> {
    attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
}

fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            c if (c as u32) < 0x20 && c != '\n' && c != '\t' && c != '\r' => {
                o.push_str(&format!("&#{};", c as u32))
            }
            c => o.push(c),
        }
    }
    o
}

fn unescape(s: &str) -> Result<String> {
    if !s.contains('&') {
        return Ok(s.to_string());
    }
    let mut o = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        o.push_str(&rest[..pos]);
        let after = &rest[pos + 1..];
        let end = after
            .find(';')
            .ok_or_else(|| malformed("shared_prefs", "unterminated entity"))?;
        let ent = &after[..end];
        let ch = match ent {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ if ent.starts_with("#x") || ent.starts_with("#X") => char::from_u32(
                u32::from_str_radix(&ent[2..], 16)
                    .map_err(|_| malformed("shared_prefs", "bad entity"))?,
            )
            .ok_or_else(|| malformed("shared_prefs", "bad char"))?,
            _ if ent.starts_with('#') => char::from_u32(
                ent[1..]
                    .parse()
                    .map_err(|_| malformed("shared_prefs", "bad entity"))?,
            )
            .ok_or_else(|| malformed("shared_prefs", "bad char"))?,
            _ => return Err(malformed("shared_prefs", format!("unknown entity &{ent};"))),
        };
        o.push(ch);
        rest = &after[end + 1..];
    }
    o.push_str(rest);
    Ok(o)
}

/// Tag name, attributes, and whether it was self-closing (`<x />`).
type OpenTag = (String, Vec<(String, String)>, bool);

struct Parser<'a> {
    s: &'a str,
    i: usize,
}

impl<'a> Parser<'a> {
    fn rest(&self) -> &'a str {
        &self.s[self.i..]
    }
    fn done(&self) -> bool {
        self.i >= self.s.len()
    }
    fn ws(&mut self) {
        let t = self.rest().trim_start();
        self.i = self.s.len() - t.len();
    }
    fn eat(&mut self, lit: &str) -> bool {
        if self.rest().starts_with(lit) {
            self.i += lit.len();
            true
        } else {
            false
        }
    }
    fn skip_prolog(&mut self) {
        self.ws();
        if self.rest().starts_with("<?")
            && let Some(e) = self.rest().find("?>")
        {
            self.i += e + 2;
        }
        self.ws();
    }
    fn expect_open(&mut self, name: &str) -> Result<()> {
        self.ws();
        if self.eat(&format!("<{name}/>")) || self.eat(&format!("<{name} />")) {
            // An empty map: put the cursor at a closing tag the caller will accept.
            self.s = "</map>";
            self.i = 0;
            return Ok(());
        }
        if self.eat(&format!("<{name}>")) {
            Ok(())
        } else {
            Err(malformed("shared_prefs", format!("expected <{name}>")))
        }
    }
    /// Parses `<tag a="b" ...>` or `<tag ... />`.
    fn open_tag(&mut self) -> Result<OpenTag> {
        self.ws();
        if !self.eat("<") {
            return Err(malformed("shared_prefs", "expected a tag"));
        }
        let end = self
            .rest()
            .find('>')
            .ok_or_else(|| malformed("shared_prefs", "unterminated tag"))?;
        let inner = &self.rest()[..end];
        self.i += end + 1;
        let (inner, self_closing) = match inner.strip_suffix('/') {
            Some(x) => (x, true),
            None => (inner, false),
        };
        let inner = inner.trim();
        let (tag, mut attrs_src) = match inner.find(char::is_whitespace) {
            Some(p) => (&inner[..p], inner[p..].trim_start()),
            None => (inner, ""),
        };
        let mut attrs = Vec::new();
        while !attrs_src.is_empty() {
            let eq = attrs_src
                .find('=')
                .ok_or_else(|| malformed("shared_prefs", "bad attribute"))?;
            let key = attrs_src[..eq].trim().to_string();
            let after = attrs_src[eq + 1..].trim_start();
            let quote = after
                .chars()
                .next()
                .ok_or_else(|| malformed("shared_prefs", "bad attribute"))?;
            if quote != '"' && quote != '\'' {
                return Err(malformed("shared_prefs", "unquoted attribute"));
            }
            let close = after[1..]
                .find(quote)
                .ok_or_else(|| malformed("shared_prefs", "unterminated attribute"))?;
            attrs.push((key, unescape(&after[1..1 + close])?));
            attrs_src = after[close + 2..].trim_start();
        }
        Ok((tag.to_string(), attrs, self_closing))
    }
    fn text_until(&mut self, close: &str) -> Result<String> {
        let end = self
            .rest()
            .find(close)
            .ok_or_else(|| malformed("shared_prefs", format!("missing {close}")))?;
        let text = unescape(&self.rest()[..end])?;
        self.i += end + close.len();
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <string name="sk">00ff&amp;&lt;x&gt;</string>
    <boolean name="on" value="true" />
    <int name="n" value="-5" />
    <long name="cursor_abc" value="1731500000" />
    <float name="vol" value="0.8" />
    <set name="ids">
        <string>a</string>
        <string>Привет &quot;夜&quot;</string>
    </set>
    <string name="empty"></string>
    <null name="gone" />
</map>
"#;

    #[test]
    fn parses_android_xml() {
        let p = SharedPrefs::parse(SAMPLE).unwrap();
        assert_eq!(p.get_str("sk"), Some("00ff&<x>"));
        assert_eq!(p.values["on"], SpValue::Bool(true));
        assert_eq!(p.values["n"], SpValue::Int(-5));
        assert_eq!(p.get_i64("cursor_abc"), Some(1731500000));
        assert_eq!(
            p.values["ids"],
            SpValue::Set(vec!["a".into(), "Привет \"夜\"".into()])
        );
        assert_eq!(p.get_str("empty"), Some(""));
        assert_eq!(p.values["gone"], SpValue::Null);
    }

    #[test]
    fn roundtrip() {
        let p = SharedPrefs::parse(SAMPLE).unwrap();
        assert_eq!(SharedPrefs::parse(&p.to_xml()).unwrap(), p);
    }

    #[test]
    fn empty_map() {
        assert!(
            SharedPrefs::parse("<?xml version='1.0' ?>\n<map />")
                .unwrap()
                .values
                .is_empty()
        );
    }

    /// Parses every shared_prefs file of a real backup. Set DRX_TEST_SHARED_PREFS_DIR. Not run in CI.
    #[test]
    fn real_android_files() {
        let Ok(dir) = std::env::var("DRX_TEST_SHARED_PREFS_DIR") else {
            return;
        };
        for e in std::fs::read_dir(dir).unwrap() {
            let path = e.unwrap().path();
            let xml = std::fs::read_to_string(&path).unwrap();
            let p =
                SharedPrefs::parse(&xml).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
            assert_eq!(
                SharedPrefs::parse(&p.to_xml()).unwrap(),
                p,
                "{}",
                path.display()
            );
        }
    }
}
