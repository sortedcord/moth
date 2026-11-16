//! The cues of a WebM file: where its keyframes are, in time and in the file.
//!
//! `matroskademux` reads these itself to seek, so the HTTP source doesn't need them for that. It
//! needs them to stop and resume a stream in place (see [`super::http_source::Flow`]): to pick
//! up again at the cluster with the keyframe before the current position, without a seek.
//!
//! Only cues stored before the first cluster are found: where DASH WebM keeps them (YouTube's,
//! and yt-dlp's downloads of it). Other WebM files keep them at the end, and stay unindexed.

use std::time::Duration;

const EBML: u32 = 0x1A45_DFA3;
const SEGMENT: u32 = 0x1853_8067;
const INFO: u32 = 0x1549_A966;
const TIMESTAMP_SCALE: u32 = 0x2A_D7B1;
const CUES: u32 = 0x1C53_BB6B;
const CUE_POINT: u32 = 0xBB;
const CUE_TIME: u32 = 0xB3;
const CUE_TRACK_POSITIONS: u32 = 0xB7;
const CUE_CLUSTER_POSITION: u32 = 0xF1;
const CLUSTER: u32 = 0x1F43_B675;

/// Nanoseconds per timestamp unit when the file doesn't say.
const DEFAULT_TIMESTAMP_SCALE: u64 = 1_000_000;

#[derive(Clone, Debug, PartialEq)]
pub struct CueIndex {
    /// (time, file offset of the cluster), by time.
    cues: Vec<(Duration, u64)>,
}

impl CueIndex {
    /// File offset of the cluster to start reading at to play from `position`: the last cue at
    /// or before it.
    pub fn cluster_offset(&self, position: Duration) -> u64 {
        let after = self.cues.partition_point(|(time, _)| *time <= position);
        self.cues[after.saturating_sub(1)].1
    }
}

#[derive(Debug, PartialEq)]
pub enum Probe {
    /// Not WebM, or no cues before the media.
    Unindexed,
    /// Can't tell yet: the answer lies within this many bytes from the start of the file.
    NeedsBytes(u64),
    Indexed(CueIndex),
}

/// Look for cues in `prefix`, the first bytes of a stream.
pub fn probe(prefix: &[u8]) -> Probe {
    match prefix.get(..4) {
        None => return Probe::NeedsBytes(4),
        Some(magic) if magic != EBML.to_be_bytes() => return Probe::Unindexed,
        Some(_) => {}
    }
    let Some(header) = Element::read(prefix, 0) else { return Probe::NeedsBytes(Element::MAX_HEADER as u64) };
    if header.id != EBML {
        return Probe::Unindexed;
    }
    let Some(segment_at) = header.end() else { return Probe::Unindexed };
    let Some(segment) = Element::read(prefix, segment_at) else {
        return Probe::NeedsBytes(segment_at as u64 + Element::MAX_HEADER as u64);
    };
    if segment.id != SEGMENT {
        return Probe::Unindexed;
    }
    // Cluster positions count from the start of the segment's contents.
    let segment_start = segment.data;
    let mut scale = DEFAULT_TIMESTAMP_SCALE;
    let mut at = segment_start;
    loop {
        let Some(element) = Element::read(prefix, at) else {
            return Probe::NeedsBytes(at as u64 + Element::MAX_HEADER as u64);
        };
        // Media before any cues: there's no index up front.
        if element.id == CLUSTER {
            return Probe::Unindexed;
        }
        let Some(end) = element.end() else { return Probe::Unindexed };
        match element.id {
            INFO | CUES if end > prefix.len() => return Probe::NeedsBytes(end as u64),
            INFO => {
                if let Some(value) = children(&prefix[element.data..end]).find(|(id, _)| *id == TIMESTAMP_SCALE) {
                    scale = uint(value.1).filter(|scale| *scale > 0).unwrap_or(DEFAULT_TIMESTAMP_SCALE);
                }
            }
            CUES => {
                return match cues(&prefix[element.data..end], scale, segment_start as u64) {
                    Some(index) => Probe::Indexed(index),
                    None => Probe::Unindexed,
                };
            }
            _ => {}
        }
        at = end;
    }
}

fn cues(cues: &[u8], scale: u64, segment_start: u64) -> Option<CueIndex> {
    let mut points: Vec<(Duration, u64)> = children(cues)
        .filter(|(id, _)| *id == CUE_POINT)
        .filter_map(|(_, point)| {
            let mut time = None;
            let mut cluster = None;
            for (id, value) in children(point) {
                match id {
                    CUE_TIME => time = uint(value),
                    CUE_TRACK_POSITIONS if cluster.is_none() => {
                        cluster =
                            children(value).find(|(id, _)| *id == CUE_CLUSTER_POSITION).and_then(|(_, v)| uint(v));
                    }
                    _ => {}
                }
            }
            let nanos = u128::from(time?) * u128::from(scale);
            Some((Duration::from_nanos(u64::try_from(nanos).ok()?), segment_start.checked_add(cluster?)?))
        })
        .collect();
    points.sort_by_key(|(time, _)| *time);
    (!points.is_empty()).then_some(CueIndex { cues: points })
}

/// An element's header: its ID, and where its contents are.
struct Element {
    id: u32,
    /// Offset of the contents.
    data: usize,
    /// Size of the contents; `None` for "unknown" (until the end of the file).
    size: Option<u64>,
}

impl Element {
    /// A 4-byte ID and an 8-byte size.
    const MAX_HEADER: usize = 12;

    fn read(bytes: &[u8], at: usize) -> Option<Self> {
        let (id, id_len) = vint(bytes.get(at..)?, false)?;
        let (size, size_len) = vint(bytes.get(at + id_len..)?, true)?;
        // All value bits set means the size is unknown.
        let unknown = size == (1 << (7 * size_len)) - 1;
        Some(Self { id: u32::try_from(id).ok()?, data: at + id_len + size_len, size: (!unknown).then_some(size) })
    }

    fn end(&self) -> Option<usize> {
        self.data.checked_add(usize::try_from(self.size?).ok()?)
    }
}

/// An EBML variable-length integer and its length. IDs keep their length marker; sizes don't.
fn vint(bytes: &[u8], strip_marker: bool) -> Option<(u64, usize)> {
    let first = *bytes.first()?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 {
        return None;
    }
    // The marker is the first set bit; with 8 bytes, the whole first byte is marker and zeros.
    let mut value = u64::from(if strip_marker { first & (0xFFu16 >> len) as u8 } else { first });
    for byte in bytes.get(1..len)? {
        value = value << 8 | u64::from(*byte);
    }
    Some((value, len))
}

fn uint(bytes: &[u8]) -> Option<u64> {
    (bytes.len() <= 8).then(|| bytes.iter().fold(0, |value, byte| value << 8 | u64::from(*byte)))
}

/// The (ID, contents) of each element directly inside `container` (itself an element's contents).
fn children(container: &[u8]) -> impl Iterator<Item = (u32, &[u8])> {
    let mut at = 0;
    std::iter::from_fn(move || {
        let element = Element::read(container, at)?;
        let end = element.end().filter(|end| *end <= container.len())?;
        at = end;
        Some((element.id, &container[element.data..end]))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An element with a 1-byte size (contents under 127 bytes) or an 8-byte one.
    fn element(id: u32, contents: &[u8]) -> Vec<u8> {
        let mut bytes: Vec<u8> = id.to_be_bytes().into_iter().skip_while(|byte| *byte == 0).collect();
        if contents.len() < 127 {
            bytes.push(0x80 | contents.len() as u8);
        } else {
            bytes.push(0x01);
            bytes.extend_from_slice(&(contents.len() as u64).to_be_bytes()[1..]);
        }
        bytes.extend_from_slice(contents);
        bytes
    }

    fn uint_element(id: u32, value: u64) -> Vec<u8> {
        element(id, &value.to_be_bytes()[4..])
    }

    fn cue(time: u64, cluster: u64) -> Vec<u8> {
        let positions = [uint_element(0xF7, 1), uint_element(CUE_CLUSTER_POSITION, cluster)].concat();
        element(CUE_POINT, &[uint_element(CUE_TIME, time), element(CUE_TRACK_POSITIONS, &positions)].concat())
    }

    fn file(segment: &[Vec<u8>], unknown_size: bool) -> (Vec<u8>, u64) {
        let contents = segment.concat();
        let mut bytes = element(EBML, &element(0x4282, b"webm"));
        bytes.extend_from_slice(&[0x18, 0x53, 0x80, 0x67, 0x01]);
        if unknown_size {
            bytes.extend_from_slice(&[0xFF; 7]);
        } else {
            bytes.extend_from_slice(&(contents.len() as u64).to_be_bytes()[1..]);
        }
        let segment_start = bytes.len() as u64;
        bytes.extend(contents);
        (bytes, segment_start)
    }

    fn segment(cues: &[Vec<u8>]) -> Vec<Vec<u8>> {
        vec![
            element(INFO, &uint_element(TIMESTAMP_SCALE, 1_000_000)),
            element(CUES, &cues.concat()),
            element(CLUSTER, &[0; 16]),
        ]
    }

    #[test]
    fn finds_clusters_by_time() {
        for unknown_size in [false, true] {
            let (bytes, start) = file(&segment(&[cue(0, 100), cue(3250, 900), cue(7000, 2000)]), unknown_size);
            let Probe::Indexed(index) = probe(&bytes) else { panic!("expected an index") };
            assert_eq!(index.cluster_offset(Duration::ZERO), start + 100);
            assert_eq!(index.cluster_offset(Duration::from_millis(3249)), start + 100);
            assert_eq!(index.cluster_offset(Duration::from_millis(3250)), start + 900);
            assert_eq!(index.cluster_offset(Duration::MAX), start + 2000);
        }
    }

    #[test]
    fn applies_the_timestamp_scale() {
        let mut parts = segment(&[cue(0, 10), cue(100, 20)]);
        parts[0] = element(INFO, &uint_element(TIMESTAMP_SCALE, 10_000_000));
        let (bytes, start) = file(&parts, false);
        let Probe::Indexed(index) = probe(&bytes) else { panic!("expected an index") };
        assert_eq!(index.cluster_offset(Duration::from_millis(999)), start + 10);
        assert_eq!(index.cluster_offset(Duration::from_secs(1)), start + 20);
    }

    #[test]
    fn asks_for_the_rest_of_the_cues() {
        let (bytes, _) = file(&segment(&[cue(0, 100), cue(3250, 900)]), false);
        let cues_end = bytes.len() - element(CLUSTER, &[0; 16]).len();
        assert_eq!(probe(&bytes[..3]), Probe::NeedsBytes(4));
        assert_eq!(probe(&bytes[..cues_end - 1]), Probe::NeedsBytes(cues_end as u64));
        assert!(matches!(probe(&bytes[..cues_end]), Probe::Indexed(_)));
    }

    #[test]
    fn leaves_other_files_alone() {
        // MP4.
        assert_eq!(probe(b"\0\0\0\x18ftypdash\0\0\0\0iso6"), Probe::Unindexed);
        // Cues after the media.
        let parts = vec![element(INFO, &[]), element(CLUSTER, &[0; 16]), element(CUES, &cue(0, 1))];
        assert_eq!(probe(&file(&parts, false).0), Probe::Unindexed);
        // No cue points.
        assert_eq!(probe(&file(&segment(&[]), false).0), Probe::Unindexed);
    }

    /// The real thing, when the dev media is there (`scripts/fetch-dev-media.sh`): YouTube's
    /// DASH WebM, checked against the cluster offsets `ffprobe` reports for its keyframes.
    #[test]
    fn reads_youtube_webm() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/target/dev-media/spring/248.webm");
        let Ok(bytes) = std::fs::read(path) else { return };
        let Probe::Indexed(index) = probe(&bytes[..1 << 20]) else { panic!("expected an index") };
        assert_eq!(index.cluster_offset(Duration::ZERO), 1762);
        assert_eq!(index.cluster_offset(Duration::from_millis(48_190)), 3_835_448);
        assert_eq!(index.cluster_offset(Duration::from_millis(48_250)), 4_890_037);
    }

    #[test]
    fn reads_variable_length_integers() {
        assert_eq!(vint(&[0x81], true), Some((1, 1)));
        assert_eq!(vint(&[0x40, 0x02], true), Some((2, 2)));
        assert_eq!(vint(&[0x1A, 0x45, 0xDF, 0xA3], false), Some((0x1A45_DFA3, 4)));
        assert_eq!(vint(&[0x01, 0, 0, 0, 0, 0, 0x01, 0x00], true), Some((256, 8)));
        assert_eq!(vint(&[0x00], true), None);
    }
}
