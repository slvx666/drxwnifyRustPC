//! Fragmented-MP4 AAC (CMAF HLS segments, SoundCloud "aac_160k") → plain ADTS AAC (port of `Fmp4AdtsTransmuxer.kt`).
//! Joined fMP4 segments have no seek table and no length; ADTS frames are self-describing, so the decoder gets a
//! stream it can read front to back, and export can write it as AAC directly.

pub const ADTS_HEADER_SIZE: usize = 7;
const FREQUENCIES: [u32; 13] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioConfig {
    pub object_type: u8,
    pub frequency_index: u8,
    pub channel_config: u8,
}

struct Box_ {
    kind: [u8; 4],
    start: usize,
    header: usize,
    end: usize,
}

impl Box_ {
    fn payload(&self) -> usize {
        self.start + self.header
    }
}

fn u32_at(b: &[u8], o: usize) -> u64 {
    b.get(o..o + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]) as u64)
        .unwrap_or(0)
}
fn u16_at(b: &[u8], o: usize) -> u32 {
    b.get(o..o + 2)
        .map(|s| u16::from_be_bytes([s[0], s[1]]) as u32)
        .unwrap_or(0)
}

/// Child boxes in [from, to); stops at a truncated box.
fn boxes(b: &[u8], from: usize, to: usize) -> Vec<Box_> {
    let to = to.min(b.len());
    let mut out = Vec::new();
    let mut o = from;
    while o + 8 <= to {
        let mut size = u32_at(b, o);
        let kind = [b[o + 4], b[o + 5], b[o + 6], b[o + 7]];
        let mut header = 8;
        if size == 1 {
            if o + 16 > to {
                break;
            }
            size = (u32_at(b, o + 8) << 32) | u32_at(b, o + 12);
            header = 16;
        } else if size == 0 {
            size = (to - o) as u64;
        }
        if size < header as u64 {
            break;
        }
        let end = o as u64 + size;
        out.push(Box_ {
            kind,
            start: o,
            header,
            end: end.min(usize::MAX as u64) as usize,
        });
        if end > to as u64 {
            break;
        }
        o = end as usize;
    }
    out
}

fn child(b: &[u8], parent: &Box_, kind: &[u8; 4]) -> Option<Box_> {
    boxes(b, parent.payload(), parent.end)
        .into_iter()
        .find(|x| &x.kind == kind)
}

fn frequency_index(rate: u32) -> u8 {
    FREQUENCIES.iter().position(|&f| f == rate).unwrap_or(4) as u8
}

/// AAC configuration from an init segment (moov/…/stsd/mp4a/esds), or None if it isn't AAC audio.
pub fn parse_init(init: &[u8]) -> Option<AudioConfig> {
    let moov = boxes(init, 0, init.len())
        .into_iter()
        .find(|x| &x.kind == b"moov")?;
    for trak in boxes(init, moov.payload(), moov.end)
        .into_iter()
        .filter(|x| &x.kind == b"trak")
    {
        let Some(stbl) = child(init, &trak, b"mdia")
            .and_then(|m| child(init, &m, b"minf"))
            .and_then(|m| child(init, &m, b"stbl"))
        else {
            continue;
        };
        let Some(stsd) = child(init, &stbl, b"stsd") else {
            continue;
        };
        let Some(mp4a) = boxes(init, stsd.payload() + 8, stsd.end)
            .into_iter()
            .find(|x| &x.kind == b"mp4a")
        else {
            continue;
        };
        let base = mp4a.payload();
        if base + 28 > init.len() {
            continue;
        }
        let channels = u16_at(init, base + 16).clamp(1, 7) as u8;
        let rate = (u32_at(init, base + 24) >> 16) as u32;
        let fallback = AudioConfig {
            object_type: 2,
            frequency_index: frequency_index(rate),
            channel_config: channels,
        };
        let Some(esds) = boxes(init, base + 28, mp4a.end)
            .into_iter()
            .find(|x| &x.kind == b"esds")
        else {
            return Some(fallback);
        };
        return Some(
            parse_esds(init, esds.payload() + 4, esds.end.min(init.len())).unwrap_or(fallback),
        );
    }
    None
}

fn parse_esds(b: &[u8], from: usize, to: usize) -> Option<AudioConfig> {
    let mut o = from;
    let read_len = |o: &mut usize| -> usize {
        let mut len = 0usize;
        for _ in 0..4 {
            if *o >= to {
                return len;
            }
            let v = b[*o];
            *o += 1;
            len = (len << 7) | (v & 0x7F) as usize;
            if v & 0x80 == 0 {
                return len;
            }
        }
        len
    };
    while o < to {
        let tag = b[o];
        o += 1;
        let len = read_len(&mut o);
        match tag {
            0x03 => {
                if o + 3 > to {
                    return None;
                }
                let flags = b[o + 2];
                o += 3;
                if flags & 0x80 != 0 {
                    o += 2;
                }
                if flags & 0x40 != 0 {
                    o += 1 + *b.get(o).unwrap_or(&0) as usize;
                }
                if flags & 0x20 != 0 {
                    o += 2;
                }
            }
            0x04 => o += 13,
            0x05 => {
                if len < 2 || o + 2 > to {
                    return None;
                }
                let v = u16_at(b, o);
                let mut object_type = (v >> 11) as u8;
                let mut freq = ((v >> 7) & 0xF) as u8;
                let mut channel = ((v >> 3) & 0xF) as u8;
                if freq == 15 {
                    if o + 5 > to {
                        return None;
                    }
                    let rate = ((u32_at(b, o) >> 7) & 0xFF_FFFF) as u32;
                    freq = frequency_index(rate);
                    channel = ((u16_at(b, o + 3) >> 11) & 0xF) as u8;
                }
                // HE-AAC/PS signal the LC core in ADTS.
                if !(1..=4).contains(&object_type) {
                    object_type = 2;
                }
                return Some(AudioConfig {
                    object_type,
                    frequency_index: freq,
                    channel_config: channel.clamp(1, 7),
                });
            }
            _ => o += len,
        }
    }
    None
}

/// Sample sizes of the fragment and where its data starts.
fn parse_fragment(seg: &[u8]) -> Option<(Vec<usize>, usize)> {
    let top = boxes(seg, 0, seg.len());
    let moof = top.iter().find(|x| &x.kind == b"moof")?;
    let traf = child(seg, moof, b"traf")?;
    let tfhd = child(seg, &traf, b"tfhd")?;
    let tf_flags = (u32_at(seg, tfhd.payload()) & 0xFF_FFFF) as u32;
    let mut p = tfhd.payload() + 8;
    let mut base = moof.start as i64;
    if tf_flags & 0x1 != 0 {
        base = ((u32_at(seg, p) << 32) | u32_at(seg, p + 4)) as i64;
        p += 8;
    }
    if tf_flags & 0x2 != 0 {
        p += 4;
    }
    if tf_flags & 0x8 != 0 {
        p += 4;
    }
    let default_size = if tf_flags & 0x10 != 0 {
        u32_at(seg, p) as usize
    } else {
        0
    };
    let mut sizes = Vec::new();
    let mut data_start: Option<i64> = None;
    for trun in boxes(seg, traf.payload(), traf.end)
        .into_iter()
        .filter(|x| &x.kind == b"trun")
    {
        let flags = (u32_at(seg, trun.payload()) & 0xFF_FFFF) as u32;
        let mut q = trun.payload() + 4;
        let count = u32_at(seg, q) as usize;
        q += 4;
        if flags & 0x1 != 0 {
            let off = u32_at(seg, q) as u32 as i32 as i64;
            data_start.get_or_insert(base + off);
            q += 4;
        }
        if flags & 0x4 != 0 {
            q += 4;
        }
        for _ in 0..count {
            if flags & 0x100 != 0 {
                q += 4;
            }
            let size = if flags & 0x200 != 0 {
                let s = u32_at(seg, q) as usize;
                q += 4;
                s
            } else {
                default_size
            };
            if flags & 0x400 != 0 {
                q += 4;
            }
            if flags & 0x800 != 0 {
                q += 4;
            }
            sizes.push(size);
        }
    }
    if sizes.is_empty() {
        return None;
    }
    let start = match data_start {
        Some(s) => usize::try_from(s).ok()?,
        None => top
            .iter()
            .find(|x| &x.kind == b"mdat" && x.start >= moof.end)?
            .payload(),
    };
    Some((sizes, start))
}

pub fn write_header(out: &mut Vec<u8>, frame_len: usize, c: AudioConfig) {
    out.push(0xFF);
    out.push(0xF1); // MPEG-4, layer 0, no CRC
    out.push(
        (((c.object_type - 1) & 0x3) << 6)
            | ((c.frequency_index & 0xF) << 2)
            | ((c.channel_config >> 2) & 0x1),
    );
    out.push(((c.channel_config & 0x3) << 6) | ((frame_len >> 11) & 0x3) as u8);
    out.push(((frame_len >> 3) & 0xFF) as u8);
    out.push((((frame_len & 0x7) << 5) as u8) | 0x1F);
    out.push(0xFC);
}

/// ADTS frames for a complete media segment. An empty segment is a short gap, not an error.
pub fn transmux(seg: &[u8], c: AudioConfig) -> Option<Vec<u8>> {
    if seg.is_empty() {
        return Some(Vec::new());
    }
    let (sizes, start) = parse_fragment(seg)?;
    let mut out = Vec::with_capacity(sizes.iter().map(|s| s + ADTS_HEADER_SIZE).sum());
    let mut src = start;
    for size in sizes {
        if src + size > seg.len() {
            return None;
        }
        write_header(&mut out, size + ADTS_HEADER_SIZE, c);
        out.extend_from_slice(&seg[src..src + size]);
        src += size;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn transmuxes_a_fragment() {
        // tfhd: flags 0x20000 (default-base-is-moof), track 1. trun: flags 0x201 (data offset + sizes), 2 samples.
        let tfhd = bx(b"tfhd", &[0, 2, 0, 0, 0, 0, 0, 1]);
        let samples: [&[u8]; 2] = [&[1, 2, 3], &[4, 5]];
        let trun_body = |data_off: u32| {
            let mut t = vec![0, 0, 0x02, 0x01];
            t.extend_from_slice(&2u32.to_be_bytes());
            t.extend_from_slice(&data_off.to_be_bytes());
            t.extend_from_slice(&3u32.to_be_bytes());
            t.extend_from_slice(&2u32.to_be_bytes());
            t
        };
        // moof size is fixed regardless of the offset value; compute it, then the data offset = moof size + 8.
        let probe = bx(
            b"moof",
            &bx(
                b"traf",
                &[tfhd.clone(), bx(b"trun", &trun_body(0))].concat(),
            ),
        );
        let moof = bx(
            b"moof",
            &bx(
                b"traf",
                &[tfhd, bx(b"trun", &trun_body(probe.len() as u32 + 8))].concat(),
            ),
        );
        let seg = [moof, bx(b"mdat", &samples.concat())].concat();
        let cfg = AudioConfig {
            object_type: 2,
            frequency_index: 4,
            channel_config: 2,
        };
        let out = transmux(&seg, cfg).unwrap();
        assert_eq!(out.len(), 5 + 2 * ADTS_HEADER_SIZE);
        assert_eq!(&out[..2], &[0xFF, 0xF1]);
        assert_eq!(&out[7..10], &[1, 2, 3]);
        assert_eq!(&out[17..19], &[4, 5]);
        // Frame length field of the first frame = 3 + 7.
        let len =
            (((out[3] & 0x3) as usize) << 11) | ((out[4] as usize) << 3) | ((out[5] >> 5) as usize);
        assert_eq!(len, 10);
    }
}
