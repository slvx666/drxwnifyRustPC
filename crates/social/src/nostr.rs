//! Nostr events, keys and encryption, byte-compatible with Android's `friends/Nostr.kt` and `Secp256k1.kt`
//! (spec/CRITICAL.md §1.1): event id = sha256 of the NIP-01 array with only `" \ \n \r \t \b \f` (and other control
//! characters as `\u00xx`) escaped; Schnorr BIP-340 signatures over x-only keys; NIP-04 = AES-256-CBC with the raw
//! x coordinate of the ECDH point, `base64(ct) + "?iv=" + base64(iv)`; NIP-19 npub/nsec in bech32.

use aes::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use k256::schnorr::signature::hazmat::{PrehashSigner, PrehashVerifier};
use k256::schnorr::{Signature, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// NIP-78: app data, replaceable per (author, `d` tag).
pub const KIND_APP_DATA: u32 = 30078;
/// NIP-01 profile metadata.
pub const KIND_METADATA: u32 = 0;
/// NIP-04 direct message.
pub const KIND_DM: u32 = 4;
/// NIP-09 deletion.
pub const KIND_DELETE: u32 = 5;
/// «Печатает…» (ephemeral, encrypted like a DM).
pub const KIND_TYPING: u32 = 21004;

#[derive(Debug, thiserror::Error)]
pub enum NostrError {
    #[error("bad key")]
    Key,
    #[error("bad data: {0}")]
    Data(&'static str),
}

/// A signed event as relays carry it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub pubkey: String,
    pub created_at: i64,
    pub kind: u32,
    pub tags: Vec<Vec<String>>,
    pub content: String,
    pub sig: String,
}

impl Event {
    /// The first value of a tag (`d`, `p`, `e`…).
    pub fn tag(&self, name: &str) -> Option<&str> {
        self.tags
            .iter()
            .find(|t| t.len() >= 2 && t[0] == name)
            .map(|t| t[1].as_str())
    }

    /// The id is the hash of the content (cheap).
    pub fn has_valid_id(&self) -> bool {
        id_of(&self.pubkey, self.created_at, self.kind, &self.tags, &self.content) == self.id
    }

    /// The id matches and the author signed it.
    pub fn is_valid(&self) -> bool {
        if !self.has_valid_id() {
            return false;
        }
        let (Ok(id), Ok(pk), Ok(sig)) = (hex::decode(&self.id), hex::decode(&self.pubkey), hex::decode(&self.sig))
        else {
            return false;
        };
        let (Ok(vk), Ok(sig)) = (VerifyingKey::from_bytes(&pk), Signature::try_from(sig.as_slice())) else {
            return false;
        };
        vk.verify_prehash(&id, &sig).is_ok()
    }
}

fn push_json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The NIP-01 event id (lower-case hex).
pub fn id_of(pubkey: &str, created_at: i64, kind: u32, tags: &[Vec<String>], content: &str) -> String {
    let mut s = String::from("[0,");
    push_json_string(&mut s, pubkey);
    s.push_str(&format!(",{created_at},{kind},["));
    for (i, tag) in tags.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push('[');
        for (j, v) in tag.iter().enumerate() {
            if j > 0 {
                s.push(',');
            }
            push_json_string(&mut s, v);
        }
        s.push(']');
    }
    s.push_str("],");
    push_json_string(&mut s, content);
    s.push(']');
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// A Nostr identity: the private key and its x-only public key.
#[derive(Clone)]
pub struct Keys {
    signing: SigningKey,
}

impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Keys({})", self.public_hex())
    }
}

impl Keys {
    /// From the 64-hex private key the phone keeps (`friends.sk`).
    pub fn from_hex(sk: &str) -> Result<Keys, NostrError> {
        let b = hex::decode(sk.trim()).map_err(|_| NostrError::Key)?;
        Ok(Keys {
            signing: SigningKey::from_bytes(&b).map_err(|_| NostrError::Key)?,
        })
    }

    /// A new random identity.
    pub fn generate() -> Keys {
        loop {
            let mut b = [0u8; 32];
            getrandom::getrandom(&mut b).expect("system randomness");
            if let Ok(signing) = SigningKey::from_bytes(&b) {
                return Keys { signing };
            }
        }
    }

    pub fn secret_hex(&self) -> String {
        hex::encode(self.signing.to_bytes())
    }

    pub fn public_hex(&self) -> String {
        hex::encode(self.signing.verifying_key().to_bytes())
    }

    /// Signs a new event made now.
    pub fn sign(&self, kind: u32, tags: Vec<Vec<String>>, content: String) -> Event {
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.sign_at(created_at, kind, tags, content)
    }

    pub fn sign_at(&self, created_at: i64, kind: u32, tags: Vec<Vec<String>>, content: String) -> Event {
        let pubkey = self.public_hex();
        let id = id_of(&pubkey, created_at, kind, &tags, &content);
        let digest = hex::decode(&id).expect("hex id");
        let sig: Signature = self.signing.sign_prehash(&digest).expect("schnorr sign");
        Event {
            id,
            pubkey,
            created_at,
            kind,
            tags,
            content,
            sig: hex::encode(sig.to_bytes()),
        }
    }

    /// The NIP-04 shared secret with a public key: the x coordinate of their point times our key, not hashed.
    fn shared_secret(&self, pubkey_hex: &str) -> Result<[u8; 32], NostrError> {
        let x = hex::decode(pubkey_hex).map_err(|_| NostrError::Key)?;
        if x.len() != 32 {
            return Err(NostrError::Key);
        }
        // x-only key: the point with even y (BIP-340).
        let mut sec1 = vec![0x02];
        sec1.extend_from_slice(&x);
        let pk = k256::PublicKey::from_sec1_bytes(&sec1).map_err(|_| NostrError::Key)?;
        let sk = k256::SecretKey::from_bytes(&self.signing.to_bytes()).map_err(|_| NostrError::Key)?;
        let shared = k256::ecdh::diffie_hellman(sk.to_nonzero_scalar(), pk.as_affine());
        let mut out = [0u8; 32];
        out.copy_from_slice(shared.raw_secret_bytes());
        Ok(out)
    }

    /// NIP-04 encryption to `to_pubkey`.
    pub fn encrypt(&self, to_pubkey: &str, plain: &str) -> Result<String, NostrError> {
        let key = self.shared_secret(to_pubkey)?;
        let mut iv = [0u8; 16];
        getrandom::getrandom(&mut iv).expect("system randomness");
        let ct = cbc::Encryptor::<aes::Aes256>::new(&key.into(), &iv.into())
            .encrypt_padded_vec_mut::<Pkcs7>(plain.as_bytes());
        Ok(format!("{}?iv={}", B64.encode(ct), B64.encode(iv)))
    }

    /// NIP-04 decryption of what `from_pubkey` sent.
    pub fn decrypt(&self, from_pubkey: &str, payload: &str) -> Result<String, NostrError> {
        let (data, iv) = payload.split_once("?iv=").ok_or(NostrError::Data("no iv"))?;
        let key = self.shared_secret(from_pubkey)?;
        let ct = B64.decode(data.trim()).map_err(|_| NostrError::Data("base64"))?;
        let iv = B64.decode(iv.trim()).map_err(|_| NostrError::Data("base64"))?;
        if iv.len() != 16 {
            return Err(NostrError::Data("iv"));
        }
        let iv: [u8; 16] = iv.try_into().map_err(|_| NostrError::Data("iv"))?;
        let plain = cbc::Decryptor::<aes::Aes256>::new(&key.into(), &iv.into())
            .decrypt_padded_vec_mut::<Pkcs7>(&ct)
            .map_err(|_| NostrError::Data("padding"))?;
        String::from_utf8(plain).map_err(|_| NostrError::Data("utf-8"))
    }

    pub fn npub(&self) -> String {
        bech32_encode("npub", &hex::decode(self.public_hex()).expect("hex"))
    }

    pub fn nsec(&self) -> String {
        bech32_encode("nsec", &self.signing.to_bytes())
    }
}

/// The hex public key of an `npub…` (or a hex key as it is).
pub fn pubkey_from_text(text: &str) -> Option<String> {
    let t = text.trim();
    if t.len() == 64 && t.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some(t.to_lowercase());
    }
    let (hrp, data) = bech32_decode(t)?;
    (hrp == "npub" && data.len() == 32).then(|| hex::encode(data))
}

const CHARSET: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
const GENERATOR: [u32; 5] = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3];

fn polymod(values: &[u8]) -> u32 {
    let mut chk: u32 = 1;
    for &v in values {
        let top = chk >> 25;
        chk = ((chk & 0x1ffffff) << 5) ^ v as u32;
        for (i, g) in GENERATOR.iter().enumerate() {
            if (top >> i) & 1 == 1 {
                chk ^= g;
            }
        }
    }
    chk
}

fn hrp_expand(hrp: &str) -> Vec<u8> {
    let mut v: Vec<u8> = hrp.bytes().map(|c| c >> 5).collect();
    v.push(0);
    v.extend(hrp.bytes().map(|c| c & 31));
    v
}

fn convert_bits(data: &[u8], from: u32, to: u32, pad: bool) -> Option<Vec<u8>> {
    let (mut acc, mut bits) = (0u32, 0u32);
    let maxv = (1u32 << to) - 1;
    let mut out = Vec::new();
    for &b in data {
        let v = b as u32;
        if v >> from != 0 {
            return None;
        }
        acc = (acc << from) | v;
        bits += from;
        while bits >= to {
            bits -= to;
            out.push(((acc >> bits) & maxv) as u8);
        }
    }
    if pad {
        if bits > 0 {
            out.push(((acc << (to - bits)) & maxv) as u8);
        }
    } else if bits >= from || ((acc << (to - bits)) & maxv) != 0 {
        return None;
    }
    Some(out)
}

pub fn bech32_encode(hrp: &str, data: &[u8]) -> String {
    let values = convert_bits(data, 8, 5, true).expect("8 to 5 bits");
    let mut input = hrp_expand(hrp);
    input.extend(&values);
    input.extend([0u8; 6]);
    let m = polymod(&input) ^ 1;
    let mut s = format!("{hrp}1");
    for v in values.iter().copied().chain((0..6).map(|i| ((m >> (5 * (5 - i))) & 31) as u8)) {
        s.push(CHARSET[v as usize] as char);
    }
    s
}

pub fn bech32_decode(text: &str) -> Option<(String, Vec<u8>)> {
    let s = text.trim().to_lowercase();
    let pos = s.rfind('1')?;
    if pos < 1 || pos + 7 > s.len() {
        return None;
    }
    let hrp = &s[..pos];
    let values: Vec<u8> = s[pos + 1..]
        .bytes()
        .map(|c| CHARSET.iter().position(|x| *x == c).map(|i| i as u8))
        .collect::<Option<_>>()?;
    let mut check = hrp_expand(hrp);
    check.extend(&values);
    if polymod(&check) != 1 {
        return None;
    }
    let data = convert_bits(&values[..values.len() - 6], 5, 8, false)?;
    Some((hrp.to_string(), data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bip340_vector_0_public_key() {
        // BIP-340 test vector 0: secret key 3 → its x-only public key.
        let k = Keys::from_hex("0000000000000000000000000000000000000000000000000000000000000003").unwrap();
        assert_eq!(k.public_hex(), "f9308a019258c31049344f85f89d5229b531c845836f99b08601f113bce036f9");
    }

    #[test]
    fn id_escapes_like_android() {
        // Only NIP-01's characters are escaped; non-ASCII stays as is.
        let id = id_of("ab", 1, 1, &[vec!["t".into(), "drxwnify".into()]], "a\"b\\c\nЁ");
        let mut s = String::new();
        push_json_string(&mut s, "a\"b\\c\nЁ\u{1}");
        assert_eq!(s, "\"a\\\"b\\\\c\\nЁ\\u0001\"");
        assert_eq!(id.len(), 64);
    }

    #[test]
    fn signed_events_verify() {
        let k = Keys::generate();
        let e = k.sign(KIND_APP_DATA, vec![vec!["d".into(), "drxw:profile".into()]], "{\"name\":\"x\"}".into());
        assert!(e.is_valid());
        let mut bad = e.clone();
        bad.content.push(' ');
        assert!(!bad.is_valid());
        // Through JSON and back.
        let back: Event = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert!(back.is_valid());
    }

    #[test]
    fn nip04_both_ways() {
        let (a, b) = (Keys::generate(), Keys::generate());
        let msg = "{\"t\":\"drxw-chat\",\"text\":\"Привет 👋\"}";
        let ct = a.encrypt(&b.public_hex(), msg).unwrap();
        assert!(ct.contains("?iv="));
        assert_eq!(b.decrypt(&a.public_hex(), &ct).unwrap(), msg);
        assert!(Keys::generate().decrypt(&a.public_hex(), &ct).is_err() || Keys::generate().decrypt(&a.public_hex(), &ct).unwrap() != msg);
    }

    #[test]
    fn npub_round_trip() {
        let k = Keys::generate();
        let npub = k.npub();
        assert!(npub.starts_with("npub1"));
        assert_eq!(pubkey_from_text(&npub).unwrap(), k.public_hex());
        // NIP-19 example key.
        assert_eq!(
            pubkey_from_text("npub10elfcs4fr0l0r8af98jlmgdh9c8tcxjvz9qkw038js35mp4dma8qzvjptg").unwrap(),
            "7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e"
        );
    }
}
