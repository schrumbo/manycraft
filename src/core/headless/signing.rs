//! Signed chat as the game does it for an account with a certified key: the chat session announced after login, the
//! last-seen window of other players' messages, and the SHA256withRSA signature over a message or a command's message arguments.

use anyhow::{Context, Result};
use uuid::Uuid;

use super::crypto::SigningKey;
use super::wire::Out;
use crate::core::auth::ChatCertificate;

/// How many received messages the server remembers for the last-seen window.
const WINDOW: usize = 20;

/// One message signature as the server sends and expects it.
pub type Signature = [u8; 256];

/// What a chat packet tells the server about the messages it has seen.
#[derive(Debug, PartialEq, Eq)]
pub struct Update {
    pub offset: i32,
    pub acknowledged: [u8; 3],
    pub checksum: u8,
    pub signatures: Vec<Signature>,
}

/// The signatures of the messages received lately, which every chat packet acknowledges, and how many arrived since the last acknowledgement.
pub struct Seen {
    entries: [Option<Signature>; WINDOW],
    tail: usize,
    offset: i32,
    last: Option<Signature>,
}

impl Default for Seen {
    fn default() -> Self {
        Self { entries: [None; WINDOW], tail: 0, offset: 0, last: None }
    }
}

impl Seen {
    /// Notes a received signed message; the same signature twice in a row counts once.
    pub fn add(&mut self, signature: Signature) {
        if self.last == Some(signature) {
            return;
        }
        self.last = Some(signature);
        self.entries[self.tail] = Some(signature);
        self.tail = (self.tail + 1) % WINDOW;
        self.offset += 1;
    }

    /// Messages received since the last acknowledgement.
    pub fn offset(&self) -> i32 {
        self.offset
    }

    /// Takes the count of unacknowledged messages for a bare acknowledgement.
    pub fn take_offset(&mut self) -> i32 {
        std::mem::take(&mut self.offset)
    }

    /// Acknowledges every message in the window, oldest first.
    pub fn update(&mut self) -> Update {
        let mut acknowledged = [0u8; 3];
        let mut signatures = Vec::new();
        for j in 0..WINDOW {
            if let Some(signature) = self.entries[(self.tail + j) % WINDOW] {
                acknowledged[j / 8] |= 1 << (j % 8);
                signatures.push(signature);
            }
        }
        Update { offset: self.take_offset(), acknowledged, checksum: checksum(&signatures), signatures }
    }
}

fn checksum(signatures: &[Signature]) -> u8 {
    let sum = signatures.iter().fold(1i32, |sum, s| sum.wrapping_mul(31).wrapping_add(s.iter().fold(1i32, |h, b| h.wrapping_mul(31).wrapping_add(i32::from(*b as i8)))));
    match sum as i8 {
        0 => 1,
        b => b as u8,
    }
}

/// The bytes the game signs for one message: format 1, sender, session, position in the chain, salt, seconds, text, last seen.
pub fn payload(sender: Uuid, session: Uuid, index: i32, salt: [u8; 8], millis: i64, content: &str, seen: &[Signature]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1i32.to_be_bytes());
    out.extend_from_slice(sender.as_bytes());
    out.extend_from_slice(session.as_bytes());
    out.extend_from_slice(&index.to_be_bytes());
    out.extend_from_slice(&salt);
    out.extend_from_slice(&millis.div_euclid(1000).to_be_bytes());
    out.extend_from_slice(&i32::try_from(content.len()).unwrap_or(i32::MAX).to_be_bytes());
    out.extend_from_slice(content.as_bytes());
    out.extend_from_slice(&i32::try_from(seen.len()).unwrap_or(i32::MAX).to_be_bytes());
    for signature in seen {
        out.extend_from_slice(signature);
    }
    out
}

/// A chat session of one connection: signs in an unbroken chain, each signature one step further.
pub struct Signer {
    key: SigningKey,
    session: Uuid,
    sender: Uuid,
    index: i32,
    certificate: (i64, Vec<u8>, Vec<u8>),
}

impl Signer {
    pub fn new(certificate: &ChatCertificate, sender: Uuid) -> Result<Self> {
        let mut id = [0u8; 16];
        super::crypto::random(&mut id)?;
        Ok(Self { key: SigningKey::from_pem(&certificate.private_key)?, session: Uuid::from_bytes(id), sender, index: 0, certificate: (certificate.expires_at, certificate.public_key.clone(), certificate.signature.clone()) })
    }

    /// The body of the packet that tells the server this session's public key and Mojang's signature of it.
    pub fn announcement(&self) -> Out {
        let mut out = Out::default();
        out.uuid(self.session).i64(self.certificate.0).bytes(&self.certificate.1).bytes(&self.certificate.2);
        out
    }

    /// Signs `content` as the next link of the chain.
    pub fn sign(&mut self, content: &str, salt: [u8; 8], millis: i64, seen: &[Signature]) -> Result<Signature> {
        let signature = self.key.sign(&payload(self.sender, self.session, self.index, salt, millis, content, seen));
        self.index = self.index.checked_add(1).context("the chat chain is used up")?;
        signature.try_into().map_err(|_| anyhow::anyhow!("the chat key does not make 256-byte signatures"))
    }
}

#[cfg(test)]
pub(super) const TEST_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQCfkXywqAbUe07p
hWzTIUS0AHAPD0O2yYPwuMo9N/mQz01O0QvwBMz1tmri690YqZKqcQEXo+o88ogU
5Om8jCUTwxzL5A4pJ4erjbuVwBwXlHZGtbwS9HEoN5gNSiW1HhcvqowcYKNxR0c5
EsDJq8H/mEHMiNrm5kahbs3CN0YLjcphFnvRaayYJVgidw0FOTKfaXXyXuz1bYUP
4MdvvLxhcKIDqi5neAUjlVgbXXDOejWXoStjOe3x70ZZxit8Rs5NCvoybADc8T1p
gFx1PFc4e1ZoqysBUBoboYjM+eg9m15D4Vv0AmF5G7KPKEYtFuYe0C8Aj6spoBfi
lPWUEvHLAgMBAAECggEAOgZGINkhDrA+TFWb8pSE7q3AEgHbfkL+KxGPXT6XDm0L
sC79V8zvGSS57oWVcyvqrPcFzippebs599scEdemfe6z8IKgEaARyjTycyyWKo7u
iZz5ZK0l3tmEfcSqPJfSF5+9bm5H0N/6CsuCdnNySrb7qlxTuaYgJgDsaoT+ACH+
4ybppH+lIfsdXqG2H0jsjfPdDIYOsP/nR+mKYiAv29a3KG2ZZA6N5/RR4s6wIesS
IpaRTyNgTXwj2iKXoPRFjCWE8WzfaxHLyxGM3pvSY2XIh0m7Bwru6D9V+DHy9g04
U3266OrFLVLT3EIn7WnqCkRTNKElCqD9JMeE0Ei40QKBgQDNzVWHwOUtlFqr0OZp
0AZZeMIuXIQ1XZQW02mJkiOKPbHLwP+5b2SvndNUkANs+sS2ICKOZFc3QiX2bxiU
mlhJLa+EP6Uu2tWOmgwfVgT7pYnBeq6OfzM5cWHZJJyS0IFqAwNiv/eFg7OB0Kmt
6R0HRRoz9XZUQC9opL6oCQcFewKBgQDGfTrO9PolvcnJ21C0CmrzVK3EkLwP85oS
HpKOPHHlZzy31lhjqtBMacGV/ATpUztAnlX2AJozJr1N5O0hoHcUKNuhA5LpMLXa
z7gLIGa0F3/JSSmtJ9IUvD/5T3mGTXroelCtshJubmqh23taoy9S2M36j3mtlNnu
vDHMMGeL8QKBgHDZBu1JeGFl19Ie2jlrOBAXh2KkmFoq1ekuQ9CLsbE74yI20gJ8
cllZ0WetEDPRzcuQE2LA4pIKU4atpGzXtVFrG7MIsyreE/v9vGfyYwkZ3E+bVdwN
I6l74ouO58wZSN04icK8QJ2VefjU0jXqfWBfI98Z2aiZCWX38BNz38XBAoGBAI3h
ai4RUtWzihAuIsvvPm/dMbD2PFeyq4kULXJappdyaiaGovvrV6WnWudZFDGsewg/
pfaIi7DQmjSZ1R9aUoQoiIXZxiPF29gi7s2VRZSpz4AUOiQ5+KM1xQzy0E0+CQMw
m4btaNtjNfmop84sI2G289WMxU9cvUM3ZuksGHghAoGAZK5o7/LuGJ2FtSvkCK23
x6HNpu8LNvYJOuy1PgUyLX/MdIxHc1tY7FG/FIWRGwqufLMEH1pgo+mqvgjgc25v
J0zMk+otOTJJmEn0aID91LvMX0ePRqqjrqkwqvxHlos6IN5slAaN3rgJ6Um05KRM
R5Kiau6Idmd077IA6MLGdPk=
-----END PRIVATE KEY-----";

#[cfg(test)]
pub(super) fn test_certificate(public: u8, refresh_after: i64, expires_at: i64) -> ChatCertificate {
    ChatCertificate { private_key: TEST_KEY.into(), public_key: vec![public; 8], signature: vec![public; 4], expires_at, refresh_after }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn signature(seed: u8) -> Signature {
        std::array::from_fn(|i| seed.wrapping_mul(31).wrapping_add((i as u8).wrapping_mul(7)))
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    const SENDER: u128 = 0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10;
    const SESSION: u128 = 0x1112_1314_1516_1718_191a_1b1c_1d1e_1f20;
    const SALT: [u8; 8] = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];

    #[test]
    fn the_signed_bytes_and_checksums_are_the_ones_the_game_makes() {
        let make = |seen: &[Signature]| payload(Uuid::from_u128(SENDER), Uuid::from_u128(SESSION), 3, SALT, 1_796_119_200_123, "hello wörld", seen);
        assert_eq!(hex(&make(&[])), "000000010102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20000000031122334455667788000000006b0e9aa00000000c68656c6c6f2077c3b6726c6400000000");
        assert_eq!(hex(&Sha256::digest(make(&[signature(1), signature(2)]))), "eb0435dd3fe04881d6cca9368d1c7bb788360567864ec5a0dccda0011ba75f82");
        assert_eq!(checksum(&[]), 1);
        assert_eq!(checksum(&[signature(1), signature(2)]), (-31i8) as u8);
    }

    fn test_signer() -> Signer {
        let public = hex_bytes("30820122300d06092a864886f70d01010105000382010f003082010a02820101009f917cb0a806d47b4ee9856cd32144b400700f0f43b6c983f0b8ca3d37f990cf4d4ed10bf004ccf5b66ae2ebdd18a992aa710117a3ea3cf28814e4e9bc8c2513c31ccbe40e292787ab8dbb95c01c17947646b5bc12f4712837980d4a25b51e172faa8c1c60a37147473912c0c9abc1ff9841cc88dae6e646a16ecdc237460b8dca61167bd169ac98255822770d0539329f6975f25eecf56d850fe0c76fbcbc6170a203aa2e6778052395581b5d70ce7a3597a12b6339edf1ef4659c62b7c46ce4d0afa326c00dcf13d69805c753c57387b5668ab2b01501a1ba188ccf9e83d9b5e43e15bf40261791bb28f28462d16e61ed02f008fab29a017e294f59412f1cb0203010001");
        Signer { key: SigningKey::from_pem(TEST_KEY).unwrap(), session: Uuid::from_u128(SESSION), sender: Uuid::from_u128(SENDER), index: 0, certificate: (1_796_119_200_123, public, vec![1, 2, 3, 4, 5]) }
    }

    fn hex_bytes(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn the_session_announcement_is_what_the_games_packet_codec_writes() {
        assert_eq!(hex(&test_signer().announcement().0), "1112131415161718191a1b1c1d1e1f20000001a2310c017ba60230820122300d06092a864886f70d01010105000382010f003082010a02820101009f917cb0a806d47b4ee9856cd32144b400700f0f43b6c983f0b8ca3d37f990cf4d4ed10bf004ccf5b66ae2ebdd18a992aa710117a3ea3cf28814e4e9bc8c2513c31ccbe40e292787ab8dbb95c01c17947646b5bc12f4712837980d4a25b51e172faa8c1c60a37147473912c0c9abc1ff9841cc88dae6e646a16ecdc237460b8dca61167bd169ac98255822770d0539329f6975f25eecf56d850fe0c76fbcbc6170a203aa2e6778052395581b5d70ce7a3597a12b6339edf1ef4659c62b7c46ce4d0afa326c00dcf13d69805c753c57387b5668ab2b01501a1ba188ccf9e83d9b5e43e15bf40261791bb28f28462d16e61ed02f008fab29a017e294f59412f1cb0203010001050102030405");
    }

    #[test]
    fn each_signature_is_one_step_further_down_the_chain() {
        let mut signer = test_signer();
        let key = SigningKey::from_pem(TEST_KEY).unwrap();
        let at = |index| key.sign(&payload(Uuid::from_u128(SENDER), Uuid::from_u128(SESSION), index, SALT, 5_000, "hi", &[signature(1)]));
        assert_eq!(signer.sign("hi", SALT, 5_000, &[signature(1)]).unwrap().to_vec(), at(0));
        assert_eq!(signer.sign("hi", SALT, 5_000, &[signature(1)]).unwrap().to_vec(), at(1));
        assert_ne!(at(0), at(1));
    }

    #[test]
    fn the_window_acknowledges_what_it_holds_oldest_first_and_counts_what_is_new() {
        let mut seen = Seen::default();
        assert_eq!(seen.update(), Update { offset: 0, acknowledged: [0; 3], checksum: 1, signatures: vec![] });
        seen.add(signature(1));
        seen.add(signature(1));
        seen.add(signature(2));
        assert_eq!(seen.offset(), 2, "the same signature twice counts once");
        let update = seen.update();
        assert_eq!((update.offset, update.acknowledged, update.signatures.clone()), (2, [0b0000_0000, 0b0000_0000, 0b0000_1100], vec![signature(1), signature(2)]));
        assert_eq!(update.checksum, (-31i8) as u8);
        assert_eq!(seen.offset(), 0);
        let again = seen.update();
        assert_eq!((again.offset, again.signatures.len()), (0, 2), "what was acknowledged stays in the window");
    }

    #[test]
    fn a_full_window_drops_the_oldest_and_a_bare_acknowledgement_takes_the_count() {
        let mut seen = Seen::default();
        for n in 1..=25u8 {
            seen.add(signature(n));
        }
        assert_eq!(seen.take_offset(), 25);
        let update = seen.update();
        assert_eq!(update.acknowledged, [0xff, 0xff, 0x0f]);
        assert_eq!(update.signatures.first(), Some(&signature(6)));
        assert_eq!(update.signatures.last(), Some(&signature(25)));
    }
}