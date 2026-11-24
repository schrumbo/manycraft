//! The protocol's building blocks: VarInts, strings, UUIDs and network NBT, read from and written to byte buffers.

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value};
use uuid::Uuid;

/// Vanilla's `NbtAccounter`: containers nest at most this deep, and one value costs at most this many accounted bytes.
const MAX_NBT_DEPTH: usize = 512;
const MAX_NBT_BYTES: usize = 2 << 20;

/// A packet body being written.
#[derive(Default)]
pub struct Out(pub Vec<u8>);

impl Out {
    pub fn varint(&mut self, value: i32) -> &mut Self {
        varint(&mut self.0, value);
        self
    }

    pub fn string(&mut self, value: &str) -> &mut Self {
        self.varint(len(value.len()));
        self.0.extend_from_slice(value.as_bytes());
        self
    }

    pub fn bytes(&mut self, value: &[u8]) -> &mut Self {
        self.varint(len(value.len()));
        self.0.extend_from_slice(value);
        self
    }

    pub fn raw(&mut self, value: &[u8]) -> &mut Self {
        self.0.extend_from_slice(value);
        self
    }

    pub fn uuid(&mut self, value: Uuid) -> &mut Self {
        self.raw(value.as_bytes())
    }

    pub fn bool(&mut self, value: bool) -> &mut Self {
        self.0.push(u8::from(value));
        self
    }

    pub fn u8(&mut self, value: u8) -> &mut Self {
        self.0.push(value);
        self
    }

    pub fn u16(&mut self, value: u16) -> &mut Self {
        self.raw(&value.to_be_bytes())
    }

    pub fn i16(&mut self, value: i16) -> &mut Self {
        self.raw(&value.to_be_bytes())
    }

    pub fn i32(&mut self, value: i32) -> &mut Self {
        self.raw(&value.to_be_bytes())
    }

    pub fn i64(&mut self, value: i64) -> &mut Self {
        self.raw(&value.to_be_bytes())
    }

    pub fn f32(&mut self, value: f32) -> &mut Self {
        self.raw(&value.to_be_bytes())
    }

    pub fn f64(&mut self, value: f64) -> &mut Self {
        self.raw(&value.to_be_bytes())
    }
}

fn len(n: usize) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

/// Appends `value` as a VarInt.
pub fn varint(out: &mut Vec<u8>, value: i32) {
    let mut v = value as u32;
    loop {
        if v & !0x7f == 0 {
            out.push(v as u8);
            return;
        }
        out.push((v & 0x7f | 0x80) as u8);
        v >>= 7;
    }
}

/// How many bytes `value` takes as a VarInt.
pub fn varint_len(value: i32) -> usize {
    let v = value as u32;
    (1..5).find(|i| v >> (7 * i) == 0).unwrap_or(5)
}

/// Reads a VarInt from the start of `buf`: its value and length, or `None` while it is incomplete.
pub fn varint_prefix(buf: &[u8]) -> Result<Option<(i32, usize)>> {
    let mut value = 0u32;
    for (i, byte) in buf.iter().take(5).enumerate() {
        value |= u32::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            return Ok(Some((value as i32, i + 1)));
        }
    }
    ensure!(buf.len() < 5, "VarInt too long");
    Ok(None)
}

struct Quota(usize);

impl Quota {
    fn take(&mut self, bytes: usize) -> Result<()> {
        self.0 = self.0.checked_sub(bytes).context("NBT is too big")?;
        Ok(())
    }
}

enum Frame {
    List { items: Vec<Value>, inner: u8, left: usize },
    Compound { map: Map<String, Value>, name: String },
}

impl Frame {
    fn finish(self) -> Value {
        match self {
            Frame::List { items, .. } => Value::Array(items),
            Frame::Compound { map, .. } => Value::Object(map),
        }
    }
}

fn push(stack: &mut Vec<Frame>, frame: Frame) -> Result<()> {
    ensure!(stack.len() < MAX_NBT_DEPTH, "NBT nested too deep");
    stack.push(frame);
    Ok(())
}

/// A packet body being read.
pub struct In<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> In<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|end| *end <= self.buf.len()).context("packet ends early")?;
        let out = &self.buf[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.take(N)?.try_into()?)
    }

    pub fn varint(&mut self) -> Result<i32> {
        let (value, n) = varint_prefix(&self.buf[self.pos..])?.context("packet ends early")?;
        self.pos += n;
        Ok(value)
    }

    pub fn varlong(&mut self) -> Result<i64> {
        let mut value = 0u64;
        for i in 0..10 {
            let byte = self.u8()?;
            value |= u64::from(byte & 0x7f) << (7 * i);
            if byte & 0x80 == 0 {
                return Ok(value as i64);
            }
        }
        bail!("varlong too long")
    }

    fn length(&mut self) -> Result<usize> {
        let n = self.varint()?;
        usize::try_from(n).with_context(|| format!("negative length {n}"))
    }

    /// How many bytes are still unread.
    pub fn left(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// An element count behind a VarInt: never negative and never more than the bytes left. Elements wider in memory than on
    /// the wire still need a cap of their own.
    pub fn list(&mut self) -> Result<usize> {
        let n = self.length()?;
        ensure!(n <= self.left(), "list of {n} past the packet");
        Ok(n)
    }

    pub fn string(&mut self) -> Result<String> {
        let n = self.length()?;
        Ok(std::str::from_utf8(self.take(n)?)?.to_owned())
    }

    pub fn bytes(&mut self) -> Result<&'a [u8]> {
        let n = self.length()?;
        self.take(n)
    }

    pub fn uuid(&mut self) -> Result<Uuid> {
        Ok(Uuid::from_bytes(self.array()?))
    }

    pub fn bool(&mut self) -> Result<bool> {
        Ok(self.u8()? != 0)
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }

    pub fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_be_bytes(self.array()?))
    }

    pub fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.array()?))
    }

    pub fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_be_bytes(self.array()?))
    }

    pub fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_be_bytes(self.array()?))
    }

    pub fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_be_bytes(self.array()?))
    }

    /// Network NBT (a nameless root tag), as JSON: compounds become objects, lists and arrays become arrays. Read without
    /// recursion, under the depth and size limits of the game's own `NbtAccounter`.
    pub fn nbt(&mut self) -> Result<Value> {
        let mut quota = Quota(MAX_NBT_BYTES);
        let mut stack: Vec<Frame> = Vec::new();
        let mut tag = self.u8()?;
        loop {
            let mut value = match tag {
                0 => Value::Null,
                1 => {
                    quota.take(9)?;
                    Value::from(self.array::<1>()?[0] as i8)
                }
                2 => {
                    quota.take(10)?;
                    Value::from(i16::from_be_bytes(self.array()?))
                }
                3 => {
                    quota.take(12)?;
                    Value::from(self.i32()?)
                }
                4 => {
                    quota.take(16)?;
                    Value::from(self.i64()?)
                }
                5 => {
                    quota.take(12)?;
                    Value::from(f64::from(self.f32()?))
                }
                6 => {
                    quota.take(16)?;
                    Value::from(self.f64()?)
                }
                7 => self.skip_array(1, &mut quota)?,
                8 => Value::String(self.nbt_string(&mut quota)?),
                9 => {
                    quota.take(36)?;
                    let inner = self.u8()?;
                    let n = self.count()?;
                    quota.take(4 * n)?;
                    ensure!(inner != 0 || n == 0, "list of end tags");
                    ensure!(stack.len() < MAX_NBT_DEPTH, "NBT nested too deep");
                    if n > 0 {
                        push(&mut stack, Frame::List { items: Vec::with_capacity(n.min(16)), inner, left: n })?;
                        tag = inner;
                        continue;
                    }
                    Value::Array(Vec::new())
                }
                10 => {
                    quota.take(48)?;
                    push(&mut stack, Frame::Compound { map: Map::new(), name: String::new() })?;
                    match self.entry(&mut stack, &mut quota)? {
                        Some(next) => {
                            tag = next;
                            continue;
                        }
                        None => stack.pop().map_or(Value::Null, Frame::finish),
                    }
                }
                11 => self.skip_array(4, &mut quota)?,
                12 => self.skip_array(8, &mut quota)?,
                other => bail!("unknown NBT tag {other}"),
            };
            loop {
                match stack.last_mut() {
                    None => return Ok(value),
                    Some(Frame::List { items, inner, left }) => {
                        items.push(value);
                        *left -= 1;
                        if *left > 0 {
                            tag = *inner;
                            break;
                        }
                        value = stack.pop().map_or(Value::Null, Frame::finish);
                    }
                    Some(Frame::Compound { map, name }) => {
                        quota.take(36)?;
                        map.insert(std::mem::take(name), value);
                        if let Some(next) = self.entry(&mut stack, &mut quota)? {
                            tag = next;
                            break;
                        }
                        value = stack.pop().map_or(Value::Null, Frame::finish);
                    }
                }
            }
        }
    }

    /// An optional network NBT value behind a presence flag.
    pub fn optional_nbt(&mut self) -> Result<Option<Value>> {
        if self.bool()? { self.nbt().map(Some) } else { Ok(None) }
    }

    /// The type of the next entry of the compound on top of `stack`, with its name stored there; `None` at its end.
    fn entry(&mut self, stack: &mut [Frame], quota: &mut Quota) -> Result<Option<u8>> {
        let tag = self.u8()?;
        if tag == 0 {
            return Ok(None);
        }
        let key = self.nbt_string(quota)?;
        quota.take(28)?;
        if let Some(Frame::Compound { name, .. }) = stack.last_mut() {
            *name = key;
        }
        Ok(Some(tag))
    }

    fn nbt_string(&mut self, quota: &mut Quota) -> Result<String> {
        let s = self.mutf8()?;
        quota.take(36usize.saturating_add(2 * s.encode_utf16().count()))?;
        Ok(s)
    }

    fn skip_array(&mut self, width: usize, quota: &mut Quota) -> Result<Value> {
        let n = self.count()?;
        let bytes = n.checked_mul(width).context("NBT array too long")?;
        quota.take(24 + bytes)?;
        self.take(bytes)?;
        Ok(Value::Null)
    }

    fn count(&mut self) -> Result<usize> {
        let n = self.i32()?;
        let n = usize::try_from(n).with_context(|| format!("negative NBT length {n}"))?;
        ensure!(n <= self.buf.len() - self.pos, "NBT length {n} past the packet");
        Ok(n)
    }

    fn mutf8(&mut self) -> Result<String> {
        let n = usize::from(u16::from_be_bytes(self.array()?));
        Ok(mutf8(self.take(n)?))
    }
}

/// Java's modified UTF-8: UTF-8 with NUL as two bytes and characters outside the BMP as surrogate pairs.
fn mutf8(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes)
        && !bytes.contains(&0xED)
        && !bytes.windows(2).any(|w| w == [0xC0, 0x80])
    {
        return s.to_owned();
    }
    let mut units = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let (unit, n) = match b {
            0x00..=0x7f => (u16::from(b), 1),
            0xc0..=0xdf if i + 1 < bytes.len() => ((u16::from(b & 0x1f) << 6) | u16::from(bytes[i + 1] & 0x3f), 2),
            0xe0..=0xef if i + 2 < bytes.len() => ((u16::from(b & 0x0f) << 12) | (u16::from(bytes[i + 1] & 0x3f) << 6) | u16::from(bytes[i + 2] & 0x3f), 3),
            _ => (0xfffd, 1),
        };
        units.push(unit);
        i += n;
    }
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_match_the_protocol_examples() {
        for (value, bytes) in [(0, vec![0x00]), (1, vec![0x01]), (127, vec![0x7f]), (128, vec![0x80, 0x01]), (255, vec![0xff, 0x01]), (25565, vec![0xdd, 0xc7, 0x01]), (2_097_151, vec![0xff, 0xff, 0x7f]), (i32::MAX, vec![0xff, 0xff, 0xff, 0xff, 0x07]), (-1, vec![0xff, 0xff, 0xff, 0xff, 0x0f]), (i32::MIN, vec![0x80, 0x80, 0x80, 0x80, 0x08])] {
            let mut out = Vec::new();
            varint(&mut out, value);
            assert_eq!(out, bytes, "{value}");
            assert_eq!(varint_len(value), bytes.len());
            assert_eq!(varint_prefix(&bytes).unwrap(), Some((value, bytes.len())));
            assert_eq!(varint_prefix(&bytes[..bytes.len() - 1]).unwrap(), None);
        }
        assert!(varint_prefix(&[0xff; 6]).is_err());
    }

    #[test]
    fn a_body_round_trips() {
        let id = Uuid::new_v4();
        let mut out = Out::default();
        out.varint(300).string("héllo").uuid(id).bool(true).i64(-5).f32(1.5).f64(-2.25).bytes(&[1, 2, 3]).u16(25565).i32(7);
        let mut r = In::new(&out.0);
        assert_eq!(r.varint().unwrap(), 300);
        assert_eq!(r.string().unwrap(), "héllo");
        assert_eq!(r.uuid().unwrap(), id);
        assert!(r.bool().unwrap());
        assert_eq!(r.i64().unwrap(), -5);
        assert_eq!(r.f32().unwrap(), 1.5);
        assert_eq!(r.f64().unwrap(), -2.25);
        assert_eq!(r.bytes().unwrap(), &[1, 2, 3]);
        assert_eq!(r.take(2).unwrap(), &25565u16.to_be_bytes());
        assert_eq!(r.i32().unwrap(), 7);
        assert!(r.u8().is_err(), "reading past the end fails");
    }

    fn nbt_string(out: &mut Vec<u8>, s: &str) {
        out.extend_from_slice(&(s.len() as u16).to_be_bytes());
        out.extend_from_slice(s.as_bytes());
    }

    #[test]
    fn network_nbt_reads_as_json() {
        let mut b = vec![10];
        b.push(8);
        nbt_string(&mut b, "text");
        nbt_string(&mut b, "hi");
        b.push(1);
        nbt_string(&mut b, "bold");
        b.push(1);
        b.push(9);
        nbt_string(&mut b, "extra");
        b.push(8);
        b.extend_from_slice(&2i32.to_be_bytes());
        nbt_string(&mut b, "a");
        nbt_string(&mut b, "b");
        b.push(0);
        let value = In::new(&b).nbt().unwrap();
        assert_eq!(value, serde_json::json!({"text": "hi", "bold": 1, "extra": ["a", "b"]}));
        let mut bare = vec![8];
        nbt_string(&mut bare, "plain");
        assert_eq!(In::new(&bare).nbt().unwrap(), Value::String("plain".into()));
        assert!(In::new(&[9, 1, 0x7f, 0xff, 0xff, 0xff]).nbt().is_err(), "a list longer than the packet fails");
    }

    fn lists(depth: usize) -> Vec<u8> {
        let mut out = vec![9];
        for _ in 1..depth {
            out.extend([9, 0, 0, 0, 1]);
        }
        out.extend([0, 0, 0, 0, 0]);
        out
    }

    fn compounds(depth: usize) -> Vec<u8> {
        let mut out = vec![10];
        for _ in 1..depth {
            out.push(10);
            nbt_string(&mut out, "a");
        }
        out.extend(std::iter::repeat_n(0, depth));
        out
    }

    #[test]
    fn nbt_nests_as_deep_as_the_game_allows_and_no_deeper() {
        for (name, build) in [("lists", lists as fn(usize) -> Vec<u8>), ("compounds", compounds)] {
            assert!(In::new(&build(MAX_NBT_DEPTH)).nbt().is_ok(), "{name} at the limit");
            match In::new(&build(MAX_NBT_DEPTH + 1)).nbt() {
                Err(e) => assert!(e.to_string().contains("too deep"), "{name} past it: {e}"),
                Ok(_) => panic!("{name} nested past the limit were accepted"),
            }
        }
        let deep = In::new(&compounds(3)).nbt().unwrap();
        assert_eq!(deep, serde_json::json!({"a": {"a": {}}}));
        assert_eq!(In::new(&lists(3)).nbt().unwrap(), serde_json::json!([[[]]]));
    }

    #[test]
    fn the_deepest_nbt_fits_a_connection_thread() {
        let ok = std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(|| {
                for build in [lists as fn(usize) -> Vec<u8>, compounds] {
                    let value = In::new(&build(MAX_NBT_DEPTH)).nbt().unwrap();
                    drop(value);
                }
            })
            .unwrap()
            .join();
        assert!(ok.is_ok(), "reading and dropping the deepest allowed NBT overflowed a 512 KB stack");
    }

    #[test]
    fn hostile_nbt_is_bounded_in_size_and_arrays() {
        let mut array = vec![7, 0, 0, 0x10, 0];
        array.resize(array.len() + 0x1000, 0);
        let mut r = In::new(&array);
        assert_eq!(r.nbt().unwrap(), Value::Null);
        assert_eq!(r.pos, array.len());
        let n = MAX_NBT_BYTES / 4 + 1;
        let mut wide = vec![9, 1];
        wide.extend((n as i32).to_be_bytes());
        wide.resize(wide.len() + n, 0);
        assert!(In::new(&wide).nbt().err().is_some_and(|e| e.to_string().contains("too big")), "a list that costs more than the game's quota");
        let mut empties = vec![9, 10];
        empties.extend((200_000i32).to_be_bytes());
        empties.resize(empties.len() + 200_000, 0);
        assert!(In::new(&empties).nbt().is_err(), "tiny values add up to the quota");
        let mut ok = vec![9, 10];
        ok.extend((10_000i32).to_be_bytes());
        ok.resize(ok.len() + 10_000, 0);
        assert_eq!(In::new(&ok).nbt().unwrap().as_array().map(Vec::len), Some(10_000));
    }

    #[test]
    fn strings_in_compounds_and_lists_survive_the_iterative_reader() {
        let mut b = vec![10];
        for (i, name) in ["x", "y"].into_iter().enumerate() {
            b.push(9);
            nbt_string(&mut b, name);
            b.push(10);
            b.extend(2i32.to_be_bytes());
            for _ in 0..2 {
                b.push(8);
                nbt_string(&mut b, "k");
                nbt_string(&mut b, &format!("v{i}"));
                b.push(3);
                nbt_string(&mut b, "n");
                b.extend(7i32.to_be_bytes());
                b.push(0);
            }
        }
        b.push(8);
        nbt_string(&mut b, "last");
        nbt_string(&mut b, "end");
        b.push(0);
        let value = In::new(&b).nbt().unwrap();
        assert_eq!(value, serde_json::json!({"x": [{"k": "v0", "n": 7}, {"k": "v0", "n": 7}], "y": [{"k": "v1", "n": 7}, {"k": "v1", "n": 7}], "last": "end"}));
    }

    #[test]
    fn modified_utf8_decodes_nul_and_supplementary_characters() {
        assert_eq!(mutf8(&[0x61, 0xC0, 0x80, 0x62]), "a\0b");
        assert_eq!(mutf8(&[0xED, 0xA0, 0xBD, 0xED, 0xB8, 0x80]), "😀");
        assert_eq!(mutf8("grün".as_bytes()), "grün");
    }
}