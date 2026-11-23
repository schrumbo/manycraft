//! The protocol's building blocks: VarInts, strings, UUIDs and network NBT, read from and written to byte buffers.

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value};
use uuid::Uuid;

const MAX_NBT_DEPTH: usize = 64;
const MAX_NBT_NODES: usize = 65_536;

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

/// A packet body being read.
pub struct In<'a> {
    buf: &'a [u8],
    pos: usize,
    nodes: usize,
}

impl<'a> In<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0, nodes: 0 }
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

    fn length(&mut self) -> Result<usize> {
        let n = self.varint()?;
        usize::try_from(n).with_context(|| format!("negative length {n}"))
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

    /// Network NBT (a nameless root tag), as JSON: compounds become objects, lists and arrays become arrays.
    pub fn nbt(&mut self) -> Result<Value> {
        let tag = self.u8()?;
        self.nodes = MAX_NBT_NODES;
        self.tag(tag, 0)
    }

    /// An optional network NBT value behind a presence flag.
    pub fn optional_nbt(&mut self) -> Result<Option<Value>> {
        if self.bool()? { self.nbt().map(Some) } else { Ok(None) }
    }

    fn tag(&mut self, tag: u8, depth: usize) -> Result<Value> {
        ensure!(depth < MAX_NBT_DEPTH, "NBT nested too deep");
        ensure!(self.nodes > 0, "NBT has too many values");
        self.nodes -= 1;
        Ok(match tag {
            0 => Value::Null,
            1 => Value::from(self.array::<1>()?[0] as i8),
            2 => Value::from(i16::from_be_bytes(self.array()?)),
            3 => Value::from(self.i32()?),
            4 => Value::from(self.i64()?),
            5 => Value::from(f64::from(self.f32()?)),
            6 => Value::from(self.f64()?),
            7 => self.skip_array(1)?,
            8 => Value::String(self.mutf8()?),
            9 => {
                let inner = self.u8()?;
                let n = self.count()?;
                ensure!(inner != 0 || n == 0, "list of end tags");
                let mut items = Vec::with_capacity(n.min(1024));
                for _ in 0..n {
                    items.push(self.tag(inner, depth + 1)?);
                }
                Value::Array(items)
            }
            10 => {
                let mut map = Map::new();
                loop {
                    let inner = self.u8()?;
                    if inner == 0 {
                        break;
                    }
                    let name = self.mutf8()?;
                    let value = self.tag(inner, depth + 1)?;
                    map.insert(name, value);
                }
                Value::Object(map)
            }
            11 => self.skip_array(4)?,
            12 => self.skip_array(8)?,
            other => bail!("unknown NBT tag {other}"),
        })
    }

    fn skip_array(&mut self, width: usize) -> Result<Value> {
        let n = self.count()?;
        self.take(n.checked_mul(width).context("NBT array too long")?)?;
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

    #[test]
    fn hostile_nbt_is_bounded_in_depth_values_and_arrays() {
        fn lists(depth: usize) -> Vec<u8> {
            let mut out = vec![9];
            for _ in 1..depth {
                out.extend([9, 0, 0, 0, 1]);
            }
            out.extend([0, 0, 0, 0, 0]);
            out
        }
        let ok = std::thread::Builder::new().stack_size(512 * 1024).spawn(|| In::new(&lists(MAX_NBT_DEPTH)).nbt().is_ok()).unwrap().join().unwrap();
        assert!(ok, "the deepest allowed NBT fits on a connection thread");
        assert!(In::new(&lists(MAX_NBT_DEPTH + 1)).nbt().is_err());
        let mut array = vec![7, 0, 0, 0x10, 0];
        array.resize(array.len() + 0x1000, 0);
        let mut r = In::new(&array);
        assert_eq!(r.nbt().unwrap(), Value::Null);
        assert_eq!(r.pos, array.len());
        let n = MAX_NBT_NODES + 1;
        let mut wide = vec![9, 1];
        wide.extend((n as i32).to_be_bytes());
        wide.resize(wide.len() + n, 0);
        assert!(In::new(&wide).nbt().is_err());
    }

    #[test]
    fn modified_utf8_decodes_nul_and_supplementary_characters() {
        assert_eq!(mutf8(&[0x61, 0xC0, 0x80, 0x62]), "a\0b");
        assert_eq!(mutf8(&[0xED, 0xA0, 0xBD, 0xED, 0xB8, 0x80]), "😀");
        assert_eq!(mutf8("grün".as_bytes()), "grün");
    }
}
