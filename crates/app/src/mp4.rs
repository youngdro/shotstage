//! A small MP4 (ISO BMFF) writer for one video track of already-encoded frames: H.264 (`avc1`,
//! with its `avcC` record) or VP9 (`vp09`). The browser encodes the frames (WebCodecs) and this
//! puts them in a file every player and video model accepts. The index (`moov`) goes before the
//! frames, so the file starts playing before it has fully loaded.

// only the browser encodes video itself (the desktop hands frames to ffmpeg)
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

/// The codec of the frames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Codec {
    /// H.264; the `AVCDecoderConfigurationRecord` (WebCodecs `decoderConfig.description`).
    Avc(Vec<u8>),
    /// VP9 profile 0, 8 bit, 4:2:0.
    Vp9,
}

/// One encoded frame.
pub struct Sample {
    pub data: Vec<u8>,
    pub key: bool,
}

/// Movie time units per second.
const TIMESCALE: u32 = 90_000;

fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(body.len() + 8);
    b.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
    b.extend_from_slice(kind);
    b.extend_from_slice(body);
    b
}

/// A full box: version 0, 24-bit `flags`.
fn full(kind: &[u8; 4], flags: u32, body: &[u8]) -> Vec<u8> {
    versioned(kind, 0, flags, body)
}

fn versioned(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut b = (flags & 0x00ff_ffff).to_be_bytes().to_vec();
    b[0] = version;
    b.extend_from_slice(body);
    boxed(kind, &b)
}

fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

fn u16b(v: u16) -> [u8; 2] {
    v.to_be_bytes()
}

fn u32b(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

/// The identity matrix of `mvhd` / `tkhd`.
fn matrix() -> Vec<u8> {
    [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000].iter().flat_map(|v| v.to_be_bytes()).collect()
}

/// The whole file: `ftyp`, `moov`, `mdat`.
pub fn write(codec: &Codec, width: u32, height: u32, fps: f32, samples: &[Sample]) -> Vec<u8> {
    let fps = if fps.is_finite() && fps > 0.0 { fps } else { 24.0 };
    let delta = (TIMESCALE as f32 / fps).round().max(1.0) as u32;
    let duration = delta as u64 * samples.len() as u64;
    let ftyp = boxed(b"ftyp", &cat(&[b"isom".to_vec(), u32b(512).to_vec(), b"isomiso2avc1mp41".to_vec()]));
    // the index needs the frames' offset, which needs the index's size: build it twice
    let moov_len = moov(codec, width, height, delta, duration, samples, 0).len();
    let first = (ftyp.len() + moov_len + 8) as u32;
    let moov = moov(codec, width, height, delta, duration, samples, first);
    let mdat_len: usize = samples.iter().map(|s| s.data.len()).sum();
    let mut out = Vec::with_capacity(first as usize + mdat_len);
    out.extend_from_slice(&ftyp);
    out.extend_from_slice(&moov);
    out.extend_from_slice(&((mdat_len + 8) as u32).to_be_bytes());
    out.extend_from_slice(b"mdat");
    for s in samples {
        out.extend_from_slice(&s.data);
    }
    out
}

fn moov(codec: &Codec, width: u32, height: u32, delta: u32, duration: u64, samples: &[Sample], first: u32) -> Vec<u8> {
    let dur = duration.min(u32::MAX as u64) as u32;
    let mvhd = full(
        b"mvhd",
        0,
        &cat(&[
            u32b(0).to_vec(),
            u32b(0).to_vec(),
            u32b(TIMESCALE).to_vec(),
            u32b(dur).to_vec(),
            u32b(0x0001_0000).to_vec(), // rate 1.0
            u16b(0x0100).to_vec(),      // volume 1.0
            vec![0; 10],
            matrix(),
            vec![0; 24],
            u32b(2).to_vec(), // next track id
        ]),
    );
    let tkhd = full(
        b"tkhd",
        3, // enabled, in movie
        &cat(&[
            u32b(0).to_vec(),
            u32b(0).to_vec(),
            u32b(1).to_vec(), // track id
            u32b(0).to_vec(),
            u32b(dur).to_vec(),
            vec![0; 8],
            u16b(0).to_vec(), // layer
            u16b(0).to_vec(), // group
            u16b(0).to_vec(), // volume (video)
            u16b(0).to_vec(),
            matrix(),
            u32b(width << 16).to_vec(),
            u32b(height << 16).to_vec(),
        ]),
    );
    let mdhd = full(
        b"mdhd",
        0,
        &cat(&[
            u32b(0).to_vec(),
            u32b(0).to_vec(),
            u32b(TIMESCALE).to_vec(),
            u32b(dur).to_vec(),
            u16b(0x55c4).to_vec(), // language "und"
            u16b(0).to_vec(),
        ]),
    );
    let hdlr = full(b"hdlr", 0, &cat(&[u32b(0).to_vec(), b"vide".to_vec(), vec![0; 12], b"Video\0".to_vec()]));
    let vmhd = full(b"vmhd", 1, &[0; 8]);
    let dinf = boxed(b"dinf", &full(b"dref", 0, &cat(&[u32b(1).to_vec(), full(b"url ", 1, &[])])));
    let stbl = boxed(
        b"stbl",
        &cat(&[
            full(b"stsd", 0, &cat(&[u32b(1).to_vec(), sample_entry(codec, width, height)])),
            full(b"stts", 0, &cat(&[u32b(1).to_vec(), u32b(samples.len() as u32).to_vec(), u32b(delta).to_vec()])),
            {
                let keys: Vec<u8> =
                    samples.iter().enumerate().filter(|(_, s)| s.key).flat_map(|(i, _)| u32b(i as u32 + 1)).collect();
                full(b"stss", 0, &cat(&[u32b((keys.len() / 4) as u32).to_vec(), keys]))
            },
            // every frame in one chunk
            full(
                b"stsc",
                0,
                &cat(&[u32b(1).to_vec(), u32b(1).to_vec(), u32b(samples.len() as u32).to_vec(), u32b(1).to_vec()]),
            ),
            full(
                b"stsz",
                0,
                &cat(&[
                    u32b(0).to_vec(),
                    u32b(samples.len() as u32).to_vec(),
                    samples.iter().flat_map(|s| u32b(s.data.len() as u32)).collect(),
                ]),
            ),
            full(b"stco", 0, &cat(&[u32b(1).to_vec(), u32b(first).to_vec()])),
        ]),
    );
    let minf = boxed(b"minf", &cat(&[vmhd, dinf, stbl]));
    let mdia = boxed(b"mdia", &cat(&[mdhd, hdlr, minf]));
    let trak = boxed(b"trak", &cat(&[tkhd, mdia]));
    boxed(b"moov", &cat(&[mvhd, trak]))
}

fn sample_entry(codec: &Codec, width: u32, height: u32) -> Vec<u8> {
    let (kind, config) = match codec {
        Codec::Avc(record) => (b"avc1", boxed(b"avcC", record)),
        // profile 0, level 3.1 (the decoder ignores it), 8 bit 4:2:0 colocated, BT.709
        Codec::Vp9 => (b"vp09", versioned(b"vpcC", 1, 0, &[0, 31, 0x82, 1, 1, 1, 0, 0])),
    };
    boxed(
        kind,
        &cat(&[
            vec![0; 6],
            u16b(1).to_vec(), // data reference index
            vec![0; 16],
            u16b(width as u16).to_vec(),
            u16b(height as u16).to_vec(),
            u32b(0x0048_0000).to_vec(), // 72 dpi
            u32b(0x0048_0000).to_vec(),
            u32b(0).to_vec(),
            u16b(1).to_vec(),      // frames per sample
            vec![0; 32],           // compressor name
            u16b(0x0018).to_vec(), // depth
            u16b(0xffff).to_vec(), // pre-defined -1
            config,
        ]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (kind, start of body, end) of the boxes in `b`.
    fn children(b: &[u8]) -> Vec<(String, usize, usize)> {
        let mut out = vec![];
        let mut i = 0;
        while i + 8 <= b.len() {
            let len = u32::from_be_bytes(b[i..i + 4].try_into().unwrap()) as usize;
            assert!(len >= 8 && i + len <= b.len(), "box at {i} has length {len}");
            out.push((String::from_utf8_lossy(&b[i + 4..i + 8]).into_owned(), i + 8, i + len));
            i += len;
        }
        assert_eq!(i, b.len(), "boxes fill their parent");
        out
    }

    fn find<'a>(b: &'a [u8], path: &[&str]) -> &'a [u8] {
        let mut cur = b;
        for (depth, name) in path.iter().enumerate() {
            let (_, s, e) = children(cur)
                .into_iter()
                .find(|(k, ..)| k == name)
                .unwrap_or_else(|| panic!("no {name} at depth {depth}"));
            cur = &cur[s..e];
        }
        cur
    }

    #[test]
    fn frames_land_where_the_index_says() {
        let samples: Vec<Sample> =
            (0..5u8).map(|i| Sample { data: vec![i; 10 + i as usize], key: i % 3 == 0 }).collect();
        let file = write(&Codec::Avc(vec![1, 0x64, 0, 0x28, 0xff, 0xe0, 0]), 640, 360, 24.0, &samples);
        let top: Vec<String> = children(&file).into_iter().map(|c| c.0).collect();
        assert_eq!(top, ["ftyp", "moov", "mdat"]);
        let stbl = find(&file, &["moov", "trak", "mdia", "minf", "stbl"]);
        let stco = find(stbl, &["stco"]);
        let offset = u32::from_be_bytes(stco[8..12].try_into().unwrap()) as usize;
        // the sizes in stsz walk the frames in mdat
        let stsz = find(stbl, &["stsz"]);
        assert_eq!(u32::from_be_bytes(stsz[8..12].try_into().unwrap()), 5);
        let mut at = offset;
        for (i, s) in samples.iter().enumerate() {
            let size = u32::from_be_bytes(stsz[12 + i * 4..16 + i * 4].try_into().unwrap()) as usize;
            assert_eq!(&file[at..at + size], &s.data[..]);
            at += size;
        }
        assert_eq!(at, file.len());
        // key frames 1 and 4 (1-based)
        let stss = find(stbl, &["stss"]);
        assert_eq!(&stss[4..], &[0, 0, 0, 2, 0, 0, 0, 1, 0, 0, 0, 4]);
        // 5 frames of 3750 ticks at 90 kHz
        let mvhd = find(&file, &["moov", "mvhd"]);
        assert_eq!(u32::from_be_bytes(mvhd[16..20].try_into().unwrap()), 5 * 3750);
        let avc1 = find(stbl, &["stsd"]);
        assert_eq!(&avc1[12..16], b"avc1");
        let entry = &avc1[8..];
        assert_eq!(u16::from_be_bytes(entry[32..34].try_into().unwrap()), 640);
        assert_eq!(u16::from_be_bytes(entry[34..36].try_into().unwrap()), 360);
        let config = children(&entry[8 + 78..]);
        assert_eq!(config[0].0, "avcC");
    }

    #[test]
    fn vp9_has_its_config() {
        let file = write(&Codec::Vp9, 320, 240, 30.0, &[Sample { data: vec![9; 4], key: true }]);
        let stsd = find(&file, &["moov", "trak", "mdia", "minf", "stbl", "stsd"]);
        assert_eq!(&stsd[12..16], b"vp09");
        let config = children(&stsd[8 + 8 + 78..]);
        assert_eq!(config[0].0, "vpcC");
    }
}
