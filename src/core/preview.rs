//! Preview frames the companion mod writes into a shared file under a seqlock, and the manager reads.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const PREVIEW_MAGIC: u32 = 0x4D43_5056;
const PREVIEW_HEADER: usize = 16;
const MAX_PREVIEW_SIDE: u32 = 4096;

/// One preview frame: RGBA8 rows, top-down.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preview {
    pub seq: u32,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Reads the preview file the mod writes, only as far as the frame reaches. Returns `None` while the file is missing, incomplete or being rewritten.
pub fn read_preview(path: &Path) -> Option<Preview> {
    read_preview_from(&mut std::fs::File::open(path).ok()?)
}

/// Reads one preview frame from an already open preview file, so a poller can keep the handle instead of reopening it every frame.
pub fn read_preview_from(file: &mut std::fs::File) -> Option<Preview> {
    file.seek(SeekFrom::Start(0)).ok()?;
    let mut bytes = vec![0; PREVIEW_HEADER];
    file.read_exact(&mut bytes).ok()?;
    let word = |i: usize| bytes.get(i..i + 4).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes);
    let side = |i: usize| word(i).filter(|s| *s <= MAX_PREVIEW_SIDE);
    if word(0)? != PREVIEW_MAGIC || word(4)? % 2 != 0 {
        return None;
    }
    let len = side(8)? as usize * side(12)? as usize * 4;
    bytes.resize(PREVIEW_HEADER + len, 0);
    file.read_exact(&mut bytes[PREVIEW_HEADER..]).ok()?;
    let preview = parse_preview(&bytes)?;
    let mut seq = [0; 4];
    file.seek(SeekFrom::Start(4)).ok()?;
    file.read_exact(&mut seq).ok()?;
    (u32::from_le_bytes(seq) == preview.seq).then_some(preview)
}

/// Remembers the last frame shown per instance, so a frame that repeats the last one is not drawn again.
#[derive(Default)]
pub struct PreviewChanges(HashMap<String, (u32, u64)>);

impl PreviewChanges {
    /// Whether `preview` differs from the last frame passed for `key`. A `seq` that did not rise means a new game process or a read again and counts as a change.
    pub fn changed(&mut self, key: &str, preview: &Preview) -> bool {
        let mut hasher = std::hash::DefaultHasher::new();
        (preview.width, preview.height, &preview.rgba).hash(&mut hasher);
        let print = hasher.finish();
        let fresh = self.0.get(key).is_none_or(|&(seq, last)| last != print || preview.seq <= seq);
        self.0.insert(key.to_owned(), (preview.seq, print));
        fresh
    }
}

fn parse_preview(bytes: &[u8]) -> Option<Preview> {
    let word = |i: usize| bytes.get(i..i + 4).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes);
    if word(0)? != PREVIEW_MAGIC {
        return None;
    }
    let (seq, width, height) = (word(4)?, word(8)?, word(12)?);
    if seq % 2 == 1 || width == 0 || height == 0 || width > MAX_PREVIEW_SIDE || height > MAX_PREVIEW_SIDE {
        return None;
    }
    let len = width as usize * height as usize * 4;
    let rgba = bytes.get(PREVIEW_HEADER..PREVIEW_HEADER + len)?.to_vec();
    Some(Preview { seq, width, height, rgba })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_file_is_parsed_and_validated() {
        let mut bytes = Vec::new();
        for word in [PREVIEW_MAGIC, 8, 2, 1] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preview.bin");
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(read_preview(&path).unwrap(), Preview { seq: 8, width: 2, height: 1, rgba: vec![1, 2, 3, 4, 5, 6, 7, 8] });
        assert!(parse_preview(&bytes[..bytes.len() - 1]).is_none());
        let mut huge = bytes.clone();
        huge[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse_preview(&huge).is_none());
        assert!(parse_preview(&[0; 16]).is_none());
        let mut writing = bytes.clone();
        writing[4..8].copy_from_slice(&9u32.to_le_bytes());
        assert!(parse_preview(&writing).is_none());
        assert!(read_preview(&dir.path().join("missing")).is_none());
    }

    #[test]
    fn a_repeated_frame_is_no_change_until_the_game_restarts() {
        let frame = |seq, rgba: &[u8]| Preview { seq, width: 1, height: 1, rgba: rgba.to_vec() };
        let mut changes = PreviewChanges::default();
        assert!(changes.changed("a", &frame(2, &[1, 2, 3, 4])));
        assert!(!changes.changed("a", &frame(4, &[1, 2, 3, 4])));
        assert!(changes.changed("a", &frame(4, &[1, 2, 3, 4])));
        assert!(changes.changed("b", &frame(4, &[1, 2, 3, 4])));
        assert!(changes.changed("a", &frame(6, &[1, 2, 3, 5])));
        assert!(!changes.changed("a", &frame(8, &[1, 2, 3, 5])));
        assert!(changes.changed("a", &frame(2, &[1, 2, 3, 5])));
        assert!(!changes.changed("a", &frame(4, &[1, 2, 3, 5])));
        let wide = Preview { seq: 6, width: 2, height: 1, rgba: vec![1, 2, 3, 5] };
        assert!(changes.changed("a", &wide));
    }
}
