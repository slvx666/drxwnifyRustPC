//! Writing Opus into an Ogg file (`.opus`) without re-encoding.
//!
//! Used by export: YouTube gives Opus inside WebM; Telegram shows `.opus` files as music in both the desktop and
//! Android clients (`docs/research/telegram-audio.md`). Tags go into `OpusTags` (Vorbis comments: TITLE, ARTIST,
//! ALBUM) and the cover into `METADATA_BLOCK_PICTURE` (a base64 FLAC picture block), which FFmpeg (Telegram Desktop)
//! and Android's MediaMetadataRetriever read.

use std::io::Write;

/// Cover art for `METADATA_BLOCK_PICTURE`.
pub struct Picture<'a> {
    pub mime: &'a str,
    pub data: &'a [u8],
    pub width: u32,
    pub height: u32,
}

#[derive(Default)]
pub struct Tags<'a> {
    pub title: Option<&'a str>,
    pub artist: Option<&'a str>,
    pub album: Option<&'a str>,
    pub picture: Option<Picture<'a>>,
}

/// One Opus packet and the 48 kHz sample position at its end (Ogg granule position, includes pre-skip).
pub struct OpusPacket<'a> {
    pub data: &'a [u8],
    pub granule_end: u64,
}

fn opus_head(channels: u8, pre_skip: u16, input_rate: u32) -> Vec<u8> {
    let mut h = Vec::with_capacity(19);
    h.extend_from_slice(b"OpusHead");
    h.push(1); // version
    h.push(channels);
    h.extend_from_slice(&pre_skip.to_le_bytes());
    h.extend_from_slice(&input_rate.to_le_bytes());
    h.extend_from_slice(&0i16.to_le_bytes()); // output gain
    h.push(0); // channel mapping family 0: mono or stereo
    h
}

fn opus_tags(tags: &Tags) -> Vec<u8> {
    let vendor = concat!("Drxwnify ", env!("CARGO_PKG_VERSION"));
    let mut comments: Vec<String> = Vec::new();
    if let Some(t) = tags.title {
        comments.push(format!("TITLE={t}"));
    }
    if let Some(a) = tags.artist {
        comments.push(format!("ARTIST={a}"));
    }
    if let Some(a) = tags.album {
        comments.push(format!("ALBUM={a}"));
    }
    if let Some(p) = &tags.picture {
        comments.push(format!(
            "METADATA_BLOCK_PICTURE={}",
            base64(&flac_picture(p))
        ));
    }
    let mut v = Vec::new();
    v.extend_from_slice(b"OpusTags");
    v.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    v.extend_from_slice(vendor.as_bytes());
    v.extend_from_slice(&(comments.len() as u32).to_le_bytes());
    for c in &comments {
        v.extend_from_slice(&(c.len() as u32).to_le_bytes());
        v.extend_from_slice(c.as_bytes());
    }
    v
}

/// FLAC METADATA_BLOCK_PICTURE body (big endian), picture type 3 = front cover.
fn flac_picture(p: &Picture) -> Vec<u8> {
    let mut v = Vec::with_capacity(32 + p.mime.len() + p.data.len());
    v.extend_from_slice(&3u32.to_be_bytes());
    v.extend_from_slice(&(p.mime.len() as u32).to_be_bytes());
    v.extend_from_slice(p.mime.as_bytes());
    v.extend_from_slice(&0u32.to_be_bytes()); // description length
    v.extend_from_slice(&p.width.to_be_bytes());
    v.extend_from_slice(&p.height.to_be_bytes());
    v.extend_from_slice(&24u32.to_be_bytes()); // colour depth
    v.extend_from_slice(&0u32.to_be_bytes()); // palette colours
    v.extend_from_slice(&(p.data.len() as u32).to_be_bytes());
    v.extend_from_slice(p.data);
    v
}

pub(crate) fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Writes a complete Ogg Opus file.
pub fn write<'p, W: Write>(
    w: W,
    channels: u8,
    pre_skip: u16,
    input_rate: u32,
    tags: &Tags,
    packets: impl IntoIterator<Item = OpusPacket<'p>>,
) -> std::io::Result<()> {
    use ogg::writing::{PacketWriteEndInfo, PacketWriter};
    let serial = 0x4458_5257; // "DXRW"
    let mut pw = PacketWriter::new(w);
    pw.write_packet(
        opus_head(channels, pre_skip, input_rate),
        serial,
        PacketWriteEndInfo::EndPage,
        0,
    )?;
    pw.write_packet(opus_tags(tags), serial, PacketWriteEndInfo::EndPage, 0)?;
    let mut iter = packets.into_iter().peekable();
    let mut in_page = 0usize;
    while let Some(p) = iter.next() {
        in_page += 1;
        let info = if iter.peek().is_none() {
            PacketWriteEndInfo::EndStream
        } else if in_page >= 50 {
            // About one second per page at 20 ms frames: keeps seeking precise.
            in_page = 0;
            PacketWriteEndInfo::EndPage
        } else {
            PacketWriteEndInfo::NormalPacket
        };
        pw.write_packet(p.data.to_vec(), serial, info, p.granule_end)?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Encodes `frames` of a 440 Hz stereo sine at 48 kHz into an Ogg Opus file (test helper).
    pub(crate) fn encode_sine_ogg_opus(frames: usize) -> Vec<u8> {
        let mut enc =
            opus::Encoder::new(48_000, opus::Channels::Stereo, opus::Application::Audio).unwrap();
        let pre_skip = enc.get_lookahead().unwrap() as u16;
        let frame = 960; // 20 ms
        let mut pcm = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let s = (i as f32 / 48_000.0 * 440.0 * std::f32::consts::TAU).sin() * 0.5;
            pcm.push(s);
            pcm.push(s);
        }
        // Extra silence so the encoder flushes its look-ahead and every real sample comes out.
        pcm.resize(
            (frames + pre_skip as usize).div_ceil(frame) * frame * 2,
            0.0,
        );
        let mut encoded: Vec<(Vec<u8>, u64)> = Vec::new();
        for (n, chunk) in pcm.chunks(frame * 2).enumerate() {
            let data = enc.encode_vec_float(chunk, 4000).unwrap();
            let end = ((n + 1) * frame) as u64 + pre_skip as u64;
            encoded.push((data, end));
        }
        // The last granule marks the real end, so decoders trim the padding.
        if let Some(last) = encoded.last_mut() {
            last.1 = frames as u64 + pre_skip as u64;
        }
        let mut out = Vec::new();
        let tags = Tags {
            title: Some("Тест 夜"),
            artist: Some("Drxwnify"),
            album: None,
            picture: None,
        };
        write(
            &mut out,
            2,
            pre_skip,
            48_000,
            &tags,
            encoded.iter().map(|(d, g)| OpusPacket {
                data: d,
                granule_end: *g,
            }),
        )
        .unwrap();
        out
    }

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn writes_valid_head_and_tags() {
        let file = encode_sine_ogg_opus(4800);
        assert_eq!(&file[..4], b"OggS");
        assert!(file.windows(8).any(|w| w == b"OpusHead"));
        assert!(file.windows(8).any(|w| w == b"OpusTags"));
        let title = "TITLE=Тест 夜".as_bytes();
        assert!(file.windows(title.len()).any(|w| w == title));
    }
}
