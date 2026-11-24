//! Packet framing on one TCP connection: VarInt lengths, zlib above the server's threshold and AES-CFB8 once
//! encryption starts. Packets the client does not read are dropped after inflating only their id, and their encrypted
//! bytes are stepped over without decrypting.

use std::io::{BufReader, Read, Write};
use std::net::TcpStream;

use anyhow::{Context, Result, bail, ensure};
use flate2::write::ZlibEncoder;
use flate2::{Compression, Decompress, FlushDecompress, Status};

use super::crypto::Cfb8;
use super::wire::{self, varint_prefix};

/// The largest frame the protocol allows: a three-byte VarInt length.
const MAX_FRAME: usize = (1 << 21) - 1;
/// The largest packet the client inflates in full; servers send at most 8 MiB.
const MAX_PACKET: usize = 8 << 20;
const KEEP_SCRATCH: usize = 32 << 10;
const ID_STEP: usize = 64;

/// The reading half.
pub struct Reader<R = TcpStream> {
    input: BufReader<R>,
    cipher: Option<Cfb8>,
    threshold: Option<usize>,
    inflate: Decompress,
    frame: Vec<u8>,
    body: Vec<u8>,
}

/// One packet the caller asked for.
pub struct Packet<'a> {
    pub id: i32,
    pub body: &'a [u8],
}

impl<R: Read> Reader<R> {
    pub fn new(input: R) -> Self {
        Self { input: BufReader::with_capacity(16 << 10, input), cipher: None, threshold: None, inflate: Decompress::new(true), frame: Vec::new(), body: Vec::new() }
    }

    pub fn encrypt(&mut self, key: &[u8; 16]) {
        self.cipher = Some(Cfb8::new(key));
    }

    pub fn compress(&mut self, threshold: Option<usize>) {
        self.threshold = threshold;
    }

    /// The next packet whose id `wanted` accepts; other packets are dropped on the way.
    pub fn next(&mut self, wanted: impl Fn(i32) -> bool) -> Result<Packet<'_>> {
        loop {
            if let Some(id) = self.frame(&wanted)? {
                return Ok(Packet { id, body: &self.body });
            }
        }
    }

    fn frame(&mut self, wanted: &impl Fn(i32) -> bool) -> Result<Option<i32>> {
        let len = self.length()?;
        ensure!(len <= MAX_FRAME, "frame of {len} bytes");
        if self.frame.capacity() > KEEP_SCRATCH && len <= KEEP_SCRATCH {
            self.frame = Vec::new();
        }
        if self.body.capacity() > KEEP_SCRATCH && len <= KEEP_SCRATCH {
            self.body = Vec::new();
        }
        self.frame.resize(len, 0);
        self.input.read_exact(&mut self.frame)?;
        let mut clear = 0;
        let mut reveal = |frame: &mut [u8], clear: &mut usize, upto: usize| {
            let upto = upto.min(frame.len());
            if let Some(cipher) = self.cipher.as_mut()
                && upto > *clear
            {
                cipher.decrypt(&mut frame[*clear..upto]);
            }
            *clear = (*clear).max(upto);
        };
        reveal(&mut self.frame, &mut clear, 10);
        let (data_len, head) = match self.threshold {
            Some(_) => varint_prefix(&self.frame)?.context("frame without a length")?,
            None => (0, 0),
        };
        let id = if data_len == 0 {
            let (id, n) = varint_prefix(&self.frame[head..clear])?.context("frame without an id")?;
            if wanted(id) {
                reveal(&mut self.frame, &mut clear, len);
                self.body.clear();
                self.body.extend_from_slice(&self.frame[head + n..]);
            }
            id
        } else {
            let data_len = usize::try_from(data_len).context("negative packet length")?;
            ensure!(data_len <= MAX_PACKET, "packet of {data_len} bytes");
            if let Some(threshold) = self.threshold {
                ensure!(data_len >= threshold, "compressed packet of {data_len} bytes below the threshold {threshold}");
            }
            self.inflate.reset(true);
            let mut first = [0u8; 5];
            let mut got = 0;
            let mut fed = head;
            reveal(&mut self.frame, &mut clear, head + ID_STEP);
            let id = loop {
                let before = (self.inflate.total_in(), self.inflate.total_out());
                let status = self.inflate.decompress(&self.frame[fed..clear], &mut first[got..], FlushDecompress::None)?;
                fed += (self.inflate.total_in() - before.0) as usize;
                got += (self.inflate.total_out() - before.1) as usize;
                if let Some((id, _)) = varint_prefix(&first[..got])? {
                    break id;
                }
                ensure!(status != Status::StreamEnd && clear < len, "packet ends before its id");
                let more = clear + ID_STEP;
                reveal(&mut self.frame, &mut clear, more);
            };
            if wanted(id) {
                reveal(&mut self.frame, &mut clear, len);
                self.inflate.reset(true);
                self.body.clear();
                self.body.resize(data_len, 0);
                let status = self.inflate.decompress(&self.frame[head..], &mut self.body, FlushDecompress::Finish)?;
                ensure!(status == Status::StreamEnd && self.inflate.total_out() as usize == data_len, "packet inflates to the wrong size");
                let (_, n) = varint_prefix(&self.body)?.context("packet without an id")?;
                self.body.drain(..n);
            }
            id
        };
        if let Some(cipher) = self.cipher.as_mut()
            && clear < len
        {
            cipher.skip(&self.frame[clear..]);
        }
        Ok(wanted(id).then_some(id))
    }

    fn length(&mut self) -> Result<usize> {
        let mut value = 0u32;
        for i in 0..3 {
            let mut byte = [0u8];
            self.input.read_exact(&mut byte)?;
            if let Some(cipher) = self.cipher.as_mut() {
                cipher.decrypt(&mut byte);
            }
            value |= u32::from(byte[0] & 0x7f) << (7 * i);
            if byte[0] & 0x80 == 0 {
                return Ok(value as usize);
            }
        }
        bail!("frame length too long")
    }
}

/// The writing half.
pub struct Writer<W = TcpStream> {
    output: W,
    cipher: Option<Cfb8>,
    threshold: Option<usize>,
}

impl<W: Write> Writer<W> {
    pub fn new(output: W) -> Self {
        Self { output, cipher: None, threshold: None }
    }

    pub fn encrypt(&mut self, key: &[u8; 16]) {
        self.cipher = Some(Cfb8::new(key));
    }

    pub fn compress(&mut self, threshold: Option<usize>) {
        self.threshold = threshold;
    }

    pub fn output(&self) -> &W {
        &self.output
    }

    /// Sends packet `id` with `body`.
    pub fn send(&mut self, id: i32, body: &[u8]) -> Result<()> {
        let mut packet = Vec::with_capacity(body.len() + 5);
        wire::varint(&mut packet, id);
        packet.extend_from_slice(body);
        let mut frame = Vec::with_capacity(packet.len() + 8);
        match self.threshold {
            Some(threshold) if packet.len() >= threshold => {
                let mut deflate = ZlibEncoder::new(Vec::with_capacity(packet.len() / 2 + 64), Compression::default());
                deflate.write_all(&packet)?;
                let zipped = deflate.finish()?;
                let data_len = i32::try_from(packet.len())?;
                wire::varint(&mut frame, i32::try_from(wire::varint_len(data_len) + zipped.len())?);
                wire::varint(&mut frame, data_len);
                frame.extend_from_slice(&zipped);
            }
            Some(_) => {
                wire::varint(&mut frame, i32::try_from(packet.len() + 1)?);
                frame.push(0);
                frame.extend_from_slice(&packet);
            }
            None => {
                wire::varint(&mut frame, i32::try_from(packet.len())?);
                frame.extend_from_slice(&packet);
            }
        }
        ensure!(frame.len() <= MAX_FRAME + 3, "packet too large to send");
        if let Some(cipher) = self.cipher.as_mut() {
            cipher.encrypt(&mut frame);
        }
        self.output.write_all(&frame)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn pair(threshold: Option<usize>, key: Option<[u8; 16]>) -> (Writer<Vec<u8>>, impl Fn(Vec<u8>) -> Reader<Cursor<Vec<u8>>>) {
        let mut w = Writer::new(Vec::new());
        w.compress(threshold);
        if let Some(k) = key {
            w.encrypt(&k);
        }
        (w, move |bytes| {
            let mut r = Reader::new(Cursor::new(bytes));
            r.compress(threshold);
            if let Some(k) = key {
                r.encrypt(&k);
            }
            r
        })
    }

    fn noisy(n: usize) -> Vec<u8> {
        let mut x = 0x1234_5678u32;
        (0..n).map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        }).collect()
    }

    #[test]
    fn packets_round_trip_through_every_framing() {
        let big = noisy(70_000);
        let packets: Vec<(i32, Vec<u8>)> = vec![(0, vec![]), (5, b"short".to_vec()), (0x7f, vec![0; 300]), (300, big.clone()), (46, noisy(40_000)), (7, vec![1, 2, 3])];
        for threshold in [None, Some(0), Some(256)] {
            for key in [None, Some([3u8; 16])] {
                let (mut w, reader) = pair(threshold, key);
                for (id, body) in &packets {
                    w.send(*id, body).unwrap();
                }
                let mut r = reader(w.output);
                for (id, body) in &packets {
                    let p = r.next(|_| true).unwrap();
                    assert_eq!((p.id, p.body), (*id, body.as_slice()), "{threshold:?} {key:?}");
                }
                assert!(r.next(|_| true).is_err(), "the stream ends");
            }
        }
    }

    #[test]
    fn unwanted_packets_are_skipped_and_the_stream_stays_in_step() {
        for threshold in [None, Some(64)] {
            let (mut w, reader) = pair(threshold, Some([9u8; 16]));
            w.send(46, &noisy(50_000)).unwrap();
            w.send(46, &vec![0u8; 20_000]).unwrap();
            w.send(2, b"keep").unwrap();
            w.send(46, &noisy(10)).unwrap();
            w.send(3, &noisy(1000)).unwrap();
            let mut r = reader(w.output);
            let p = r.next(|id| id != 46).unwrap();
            assert_eq!((p.id, p.body), (2, b"keep".as_slice()));
            let p = r.next(|id| id != 46).unwrap();
            assert_eq!(p.id, 3);
            assert_eq!(p.body, noisy(1000).as_slice());
        }
    }

    #[test]
    fn a_compressed_packet_below_the_threshold_or_too_large_is_refused() {
        let (mut w, _) = pair(Some(0), None);
        w.send(1, &[0; 10]).unwrap();
        let mut r = Reader::new(Cursor::new(w.output));
        r.compress(Some(256));
        assert!(r.next(|_| true).is_err());
        let mut lying = Vec::new();
        wire::varint(&mut lying, 6);
        wire::varint(&mut lying, i32::MAX);
        lying.extend_from_slice(&[0x78, 0x9c]);
        let mut r = Reader::new(Cursor::new(lying));
        r.compress(Some(0));
        assert!(r.next(|_| true).is_err());
    }
}