//! The segment index of a fragmented MP4, which the HTTP source needs to seek in one.
//!
//! GStreamer's `qtdemux` seeks by itself only in MP4s whose `moov` lists every sample. A
//! fragmented file (`moov`, then `moof`+`mdat` fragments: what DASH sites serve) that arrives as a
//! byte stream it won't seek in: it leaves that to whatever feeds it, expecting a DASH client that
//! answers a seek by sending the fragment the position is in. The index from times to fragments
//! is the `sidx` box between the `moov` and the first fragment. `qtdemux` reads it only for the
//! duration, so the source reads it here and plays the DASH client (see [`super::http_source`]).

use std::time::Duration;

/// Where the fragments of a fragmented MP4 are, in time and in the file.
#[derive(Clone, Debug, PartialEq)]
pub struct SegmentIndex {
    fragments: Vec<Fragment>,
    /// When the last fragment ends.
    pub duration: Duration,
    /// How far into the media the presentation starts: the track's edit list skips this much
    /// (H.264 streams typically start one frame in, to make room for frame reordering).
    pub presentation_offset: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Fragment {
    start: Duration,
    offset: u64,
}

impl SegmentIndex {
    /// File offset of the fragment to start reading at to play from `position`: the last one
    /// that starts at or before it. Fragments start with a keyframe.
    pub fn fragment_offset(&self, position: Duration) -> u64 {
        let after = self.fragments.partition_point(|fragment| fragment.start <= position);
        self.fragments[after.saturating_sub(1)].offset
    }
}

#[derive(Debug, PartialEq)]
pub enum Probe {
    /// Not a fragmented MP4, or one without an index this can use.
    Unindexed,
    /// Can't tell yet: the answer lies within this many bytes from the start of the file.
    NeedsBytes(u64),
    Indexed(SegmentIndex),
}

/// Look for a segment index in `prefix`, the first bytes of a stream.
pub fn probe(prefix: &[u8]) -> Probe {
    let mut position = 0;
    let mut fragmented_movie = None;
    loop {
        let Some(header) = BoxHeader::read(&prefix[position..]) else {
            return Probe::NeedsBytes((position + BoxHeader::MAX_LEN) as u64);
        };
        if position == 0 && header.kind != *b"ftyp" && header.kind != *b"styp" {
            return Probe::Unindexed;
        }
        // A size of 0 means "to the end of the file", which only a last box can say.
        let Some(end) = header.size.checked_add(position as u64).filter(|_| header.size >= header.len as u64) else {
            return Probe::Unindexed;
        };
        let complete = || prefix.get(position + header.len..end as usize);
        match &header.kind {
            b"moov" => {
                let Some(movie) = complete() else { return Probe::NeedsBytes(end) };
                fragmented_movie = child(movie, b"mvex").map(|_| movie);
            }
            b"sidx" => {
                let Some(movie) = fragmented_movie else { return Probe::Unindexed };
                let Some(index) = complete() else { return Probe::NeedsBytes(end) };
                return match segment_index(index, end, presentation_offset(movie)) {
                    Some(index) => Probe::Indexed(index),
                    None => Probe::Unindexed,
                };
            }
            // Media already: there's no index before it.
            b"moof" | b"mdat" => return Probe::Unindexed,
            _ => {}
        }
        if end > usize::MAX as u64 {
            return Probe::Unindexed;
        }
        position = end as usize;
        if position > prefix.len() {
            return Probe::NeedsBytes(end + BoxHeader::MAX_LEN as u64);
        }
    }
}

struct BoxHeader {
    kind: [u8; 4],
    /// Of the whole box, header included.
    size: u64,
    len: usize,
}

impl BoxHeader {
    const MAX_LEN: usize = 16;

    fn read(data: &[u8]) -> Option<Self> {
        let size = be_u32(data, 0)?;
        let kind = data.get(4..8)?.try_into().ok()?;
        // A size of 1 says the real, 64-bit size follows the type.
        if size == 1 {
            Some(Self { kind, size: be_u64(data, 8)?, len: 16 })
        } else {
            Some(Self { kind, size: size.into(), len: 8 })
        }
    }
}

fn be_u16(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn be_u32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

fn be_u64(data: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(data.get(at..at + 8)?.try_into().ok()?))
}

/// The contents of every `kind` box directly inside `container` (itself the contents of a box).
fn children<'a>(container: &'a [u8], kind: &'a [u8; 4]) -> impl Iterator<Item = &'a [u8]> {
    let mut rest = container;
    std::iter::from_fn(move || {
        loop {
            let header = BoxHeader::read(rest)?;
            let size = usize::try_from(header.size).ok().filter(|size| *size >= header.len)?;
            let contents = rest.get(header.len..size)?;
            rest = &rest[size..];
            if header.kind == *kind {
                return Some(contents);
            }
        }
    })
}

fn child<'a>(container: &'a [u8], kind: &'a [u8; 4]) -> Option<&'a [u8]> {
    children(container, kind).next()
}

fn duration(units: u64, timescale: u32) -> Duration {
    let nanos = u128::from(units) * 1_000_000_000 / u128::from(timescale);
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

/// Parse the contents of a `sidx` box that ends at file offset `end`.
fn segment_index(sidx: &[u8], end: u64, presentation_offset: Duration) -> Option<SegmentIndex> {
    let version = *sidx.first()?;
    let timescale = be_u32(sidx, 8).filter(|timescale| *timescale > 0)?;
    let (earliest, first_offset, references) = match version {
        0 => (be_u32(sidx, 12)?.into(), be_u32(sidx, 16)?.into(), 20),
        _ => (be_u64(sidx, 12)?, be_u64(sidx, 20)?, 28),
    };
    let count = usize::from(be_u16(sidx, references + 2)?);
    let mut time = earliest;
    let mut offset = end.checked_add(first_offset)?;
    let mut fragments = Vec::with_capacity(count);
    for reference in 0..count {
        let at = references + 4 + reference * 12;
        let (size, length) = (be_u32(sidx, at)?, be_u32(sidx, at + 4)?);
        // The top bit marks a reference to another index rather than to media: a hierarchy of
        // indexes, which nothing moth plays uses.
        if size >> 31 != 0 {
            return None;
        }
        fragments.push(Fragment { start: duration(time, timescale), offset });
        time = time.checked_add(length.into())?;
        offset = offset.checked_add(size.into())?;
    }
    if fragments.is_empty() {
        return None;
    }
    Some(SegmentIndex { fragments, duration: duration(time, timescale), presentation_offset })
}

/// The media time the movie's only track starts presenting at, from its edit list. Zero unless
/// it's as simple as that: `qtdemux` likewise only applies such edit lists to streamed files.
fn presentation_offset(movie: &[u8]) -> Duration {
    let mut tracks = children(movie, b"trak");
    let (Some(track), None) = (tracks.next(), tracks.next()) else { return Duration::ZERO };
    offset_in_track(track).unwrap_or_default()
}

fn offset_in_track(track: &[u8]) -> Option<Duration> {
    let header = child(child(track, b"mdia")?, b"mdhd")?;
    let timescale = be_u32(header, if *header.first()? == 0 { 12 } else { 20 }).filter(|timescale| *timescale > 0)?;

    let edits = child(child(track, b"edts")?, b"elst")?;
    let version = *edits.first()?;
    let count = be_u32(edits, 4)? as usize;
    let mut media_times = (0..count).map(|edit| match version {
        0 => be_u32(edits, 8 + edit * 12 + 4).map(|time| i64::from(time as i32)),
        _ => be_u64(edits, 8 + edit * 20 + 8).map(|time| time as i64),
    });
    // A media time of -1 is an empty edit (a delay before the media starts).
    let mut starts = media_times.by_ref().filter(|time| *time != Some(-1));
    let (Some(start), None) = (starts.next(), starts.next()) else { return None };
    Some(duration(u64::try_from(start?).ok()?, timescale))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxed(kind: &[u8; 4], contents: &[u8]) -> Vec<u8> {
        let mut bytes = ((contents.len() + 8) as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(contents);
        bytes
    }

    /// A track with a 12288 Hz timescale whose edit list starts at media time `media_time`.
    fn track(media_time: Option<i32>) -> Vec<u8> {
        let mut header = vec![0; 12];
        header.extend_from_slice(&12288u32.to_be_bytes());
        let mut track = boxed(b"mdia", &boxed(b"mdhd", &header));
        if let Some(media_time) = media_time {
            let mut edits = vec![0; 4];
            edits.extend_from_slice(&1u32.to_be_bytes());
            edits.extend_from_slice(&5_702_656u32.to_be_bytes());
            edits.extend_from_slice(&media_time.to_be_bytes());
            edits.extend_from_slice(&[0, 1, 0, 0]);
            track.extend(boxed(b"edts", &boxed(b"elst", &edits)));
        }
        boxed(b"trak", &track)
    }

    fn movie(fragmented: bool, tracks: &[Vec<u8>]) -> Vec<u8> {
        let mut movie = boxed(b"mvhd", &[0; 100]);
        tracks.iter().for_each(|track| movie.extend(track));
        if fragmented {
            movie.extend(boxed(b"mvex", &boxed(b"trex", &[0; 24])));
        }
        boxed(b"moov", &movie)
    }

    /// A version 0 `sidx` at 1000 units per second; `references` are (size, duration) pairs.
    fn sidx(first_offset: u32, references: &[(u32, u32)]) -> Vec<u8> {
        let mut index = vec![0; 4];
        index.extend_from_slice(&1u32.to_be_bytes());
        index.extend_from_slice(&1000u32.to_be_bytes());
        index.extend_from_slice(&0u32.to_be_bytes());
        index.extend_from_slice(&first_offset.to_be_bytes());
        index.extend_from_slice(&[0, 0]);
        index.extend_from_slice(&(references.len() as u16).to_be_bytes());
        for (size, length) in references {
            index.extend_from_slice(&size.to_be_bytes());
            index.extend_from_slice(&length.to_be_bytes());
            index.extend_from_slice(&0x9000_0000u32.to_be_bytes());
        }
        boxed(b"sidx", &index)
    }

    fn file(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }

    fn file_type() -> Vec<u8> {
        boxed(b"ftyp", b"dash\0\0\0\0iso6")
    }

    const REFERENCES: [(u32, u32); 3] = [(500, 3250), (700, 3750), (900, 5625)];

    #[test]
    fn finds_fragments_by_time() {
        let header = file(&[file_type(), movie(true, &[track(None)]), sidx(0, &REFERENCES)]);
        let first = header.len() as u64;
        let Probe::Indexed(index) = probe(&file(&[header, boxed(b"moof", &[0; 16])])) else {
            panic!("expected an index")
        };
        assert_eq!(index.duration, Duration::from_millis(12_625));
        assert_eq!(index.presentation_offset, Duration::ZERO);
        assert_eq!(index.fragment_offset(Duration::ZERO), first);
        assert_eq!(index.fragment_offset(Duration::from_millis(3249)), first);
        assert_eq!(index.fragment_offset(Duration::from_millis(3250)), first + 500);
        assert_eq!(index.fragment_offset(Duration::from_millis(6999)), first + 500);
        assert_eq!(index.fragment_offset(Duration::from_secs(7)), first + 1200);
        assert_eq!(index.fragment_offset(Duration::from_secs(3600)), first + 1200);
    }

    #[test]
    fn counts_from_the_first_offset() {
        let bytes = file(&[file_type(), movie(true, &[track(None)]), sidx(32, &REFERENCES)]);
        let Probe::Indexed(index) = probe(&bytes) else { panic!("expected an index") };
        assert_eq!(index.fragment_offset(Duration::ZERO), bytes.len() as u64 + 32);
    }

    #[test]
    fn reads_the_version_1_layout() {
        let mut index = vec![1, 0, 0, 0];
        index.extend_from_slice(&1u32.to_be_bytes());
        index.extend_from_slice(&90_000u32.to_be_bytes());
        index.extend_from_slice(&180_000u64.to_be_bytes());
        index.extend_from_slice(&0u64.to_be_bytes());
        index.extend_from_slice(&[0, 0, 0, 2]);
        for (size, length) in [(1000u32, 450_000u32), (2000, 90_000)] {
            index.extend_from_slice(&size.to_be_bytes());
            index.extend_from_slice(&length.to_be_bytes());
            index.extend_from_slice(&0x9000_0000u32.to_be_bytes());
        }
        let bytes = file(&[file_type(), movie(true, &[track(None)]), boxed(b"sidx", &index)]);
        let Probe::Indexed(index) = probe(&bytes) else { panic!("expected an index") };
        assert_eq!(index.duration, Duration::from_secs(8));
        assert_eq!(index.fragment_offset(Duration::from_secs(6)), bytes.len() as u64);
        assert_eq!(index.fragment_offset(Duration::from_secs(7)), bytes.len() as u64 + 1000);
    }

    #[test]
    fn asks_for_the_rest_of_a_cut_off_header() {
        let bytes = file(&[file_type(), movie(true, &[track(None)]), sidx(0, &REFERENCES)]);
        let movie_end = (file_type().len() + movie(true, &[track(None)]).len()) as u64;
        assert_eq!(probe(&bytes[..4]), Probe::NeedsBytes(16));
        assert_eq!(probe(&bytes[..40]), Probe::NeedsBytes(movie_end));
        assert_eq!(probe(&bytes[..movie_end as usize]), Probe::NeedsBytes(movie_end + 16));
        assert_eq!(probe(&bytes[..bytes.len() - 1]), Probe::NeedsBytes(bytes.len() as u64));
        assert!(matches!(probe(&bytes), Probe::Indexed(_)));
    }

    #[test]
    fn leaves_other_files_alone() {
        // WebM.
        assert_eq!(probe(&[0x1a, 0x45, 0xdf, 0xa3, 0x9f, 0x42, 0x86, 0x81, 1, 0x42, 0xf7, 0x81]), Probe::Unindexed);
        // An MP4 that isn't fragmented: qtdemux seeks in those itself.
        let plain = file(&[file_type(), movie(false, &[track(None)]), boxed(b"mdat", &[0; 64])]);
        assert_eq!(probe(&plain), Probe::Unindexed);
        // Fragmented, but without an index.
        let unindexed = file(&[file_type(), movie(true, &[track(None)]), boxed(b"moof", &[0; 16])]);
        assert_eq!(probe(&unindexed), Probe::Unindexed);
        // An index of indexes.
        let nested = file(&[file_type(), movie(true, &[track(None)]), sidx(0, &[(0x8000_0100, 1000)])]);
        assert_eq!(probe(&nested), Probe::Unindexed);
        let empty = file(&[file_type(), movie(true, &[track(None)]), sidx(0, &[])]);
        assert_eq!(probe(&empty), Probe::Unindexed);
    }

    #[test]
    fn reads_the_edit_list_of_a_single_track() {
        let offset = |tracks: &[Vec<u8>]| {
            let bytes = file(&[file_type(), movie(true, tracks), sidx(0, &REFERENCES)]);
            let Probe::Indexed(index) = probe(&bytes) else { panic!("expected an index") };
            index.presentation_offset
        };
        assert_eq!(offset(&[track(Some(512))]), duration(512, 12288));
        assert_eq!(offset(&[track(Some(0))]), Duration::ZERO);
        assert_eq!(offset(&[track(Some(-1))]), Duration::ZERO);
        assert_eq!(offset(&[track(Some(512)), track(Some(512))]), Duration::ZERO);
    }
}
