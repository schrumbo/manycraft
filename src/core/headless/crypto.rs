//! Login encryption: AES-128-CFB8 for the stream, RSA PKCS#1 v1.5 for the shared secret, and the server hash the
//! session server checks.

use aes::Aes128;
use aes::cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray};
use anyhow::{Context, Result, ensure};
use num_bigint::BigUint;
use sha1::{Digest, Sha1};

/// One direction of an AES-128-CFB8 stream; the key doubles as the IV, as the game does it.
pub struct Cfb8 {
    aes: Aes128,
    register: [u8; 16],
}

impl Cfb8 {
    pub fn new(key: &[u8; 16]) -> Self {
        Self { aes: Aes128::new(GenericArray::from_slice(key)), register: *key }
    }

    fn pad(&self) -> u8 {
        let mut block = GenericArray::from(self.register);
        self.aes.encrypt_block(&mut block);
        block[0]
    }

    fn shift(&mut self, cipher: u8) {
        self.register.copy_within(1.., 0);
        self.register[15] = cipher;
    }

    pub fn encrypt(&mut self, data: &mut [u8]) {
        for byte in data {
            *byte ^= self.pad();
            self.shift(*byte);
        }
    }

    pub fn decrypt(&mut self, data: &mut [u8]) {
        for byte in data {
            let cipher = *byte;
            *byte ^= self.pad();
            self.shift(cipher);
        }
    }

    /// Moves past `cipher` without decrypting it: the register only needs the last 16 bytes of ciphertext.
    pub fn skip(&mut self, cipher: &[u8]) {
        if cipher.len() >= 16 {
            self.register.copy_from_slice(&cipher[cipher.len() - 16..]);
        } else {
            for byte in cipher {
                self.shift(*byte);
            }
        }
    }
}

/// The hash the client and server both send to the session server: SHA-1 as a signed hexadecimal number.
pub fn server_hash(server_id: &str, secret: &[u8], public_key: &[u8]) -> String {
    let mut sha = Sha1::new();
    sha.update(server_id.as_bytes());
    sha.update(secret);
    sha.update(public_key);
    signed_hex(&sha.finalize())
}

fn signed_hex(digest: &[u8]) -> String {
    let negative = digest.first().is_some_and(|b| b & 0x80 != 0);
    let mut bytes = digest.to_vec();
    if negative {
        let mut carry = true;
        for b in bytes.iter_mut().rev() {
            *b = !*b;
            if carry {
                let (v, overflow) = b.overflowing_add(1);
                *b = v;
                carry = overflow;
            }
        }
    }
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let trimmed = hex.trim_start_matches('0');
    let digits = if trimmed.is_empty() { "0" } else { trimmed };
    if negative { format!("-{digits}") } else { digits.to_owned() }
}

/// Encrypts `data` for the RSA public key `spki` (DER SubjectPublicKeyInfo) with PKCS#1 v1.5 padding.
pub fn rsa_encrypt(spki: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    let (modulus, exponent) = rsa_key(spki)?;
    let n = BigUint::from_bytes_be(modulus);
    let k = modulus.iter().skip_while(|b| **b == 0).count();
    ensure!(k >= data.len() + 11, "RSA key too small");
    let mut block = vec![0u8; k];
    block[1] = 2;
    let pad = &mut block[2..k - data.len() - 1];
    random(pad)?;
    for b in pad.iter_mut() {
        while *b == 0 {
            let mut one = [0u8];
            random(&mut one)?;
            *b = one[0];
        }
    }
    block[k - data.len()..].copy_from_slice(data);
    let c = BigUint::from_bytes_be(&block).modpow(&BigUint::from_bytes_be(exponent), &n).to_bytes_be();
    let mut out = vec![0u8; k - c.len()];
    out.extend_from_slice(&c);
    Ok(out)
}

/// Fills `out` from the system's secure random source.
pub fn random(out: &mut [u8]) -> Result<()> {
    getrandom::fill(out).map_err(|e| anyhow::anyhow!("no secure random source: {e}"))
}

fn rsa_key(spki: &[u8]) -> Result<(&[u8], &[u8])> {
    let mut outer = Der(der(&mut Der(spki), 0x30)?);
    der(&mut outer, 0x30)?;
    let bits = der(&mut outer, 0x03)?;
    let (unused, key) = bits.split_first().context("empty key")?;
    ensure!(*unused == 0, "odd key bit string");
    let mut key = Der(der(&mut Der(key), 0x30)?);
    let n = der(&mut key, 0x02)?;
    let e = der(&mut key, 0x02)?;
    Ok((n, e))
}

struct Der<'a>(&'a [u8]);

fn der<'a>(d: &mut Der<'a>, tag: u8) -> Result<&'a [u8]> {
    let (&t, rest) = d.0.split_first().context("key ends early")?;
    ensure!(t == tag, "unexpected DER tag {t:#x}, wanted {tag:#x}");
    let (&first, mut rest) = rest.split_first().context("key ends early")?;
    let len = if first & 0x80 == 0 {
        usize::from(first)
    } else {
        let n = usize::from(first & 0x7f);
        ensure!((1..=4).contains(&n) && rest.len() >= n, "bad DER length");
        let (len, after) = rest.split_at(n);
        rest = after;
        len.iter().fold(0usize, |acc, b| (acc << 8) | usize::from(*b))
    };
    ensure!(rest.len() >= len, "key ends early");
    let (value, after) = rest.split_at(len);
    d.0 = after;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn cfb8_matches_nist_sp800_38a_f_3_7() {
        let key: [u8; 16] = hex("2b7e151628aed2a6abf7158809cf4f3c").try_into().unwrap();
        let iv: [u8; 16] = hex("000102030405060708090a0b0c0d0e0f").try_into().unwrap();
        let plain = hex("6bc1bee22e409f96e93d7e117393172aae2d");
        let cipher = hex("3b79424c9c0dd436bace9e0ed4586a4f32b9");
        let mut enc = Cfb8 { aes: Aes128::new(GenericArray::from_slice(&key)), register: iv };
        let mut data = plain.clone();
        enc.encrypt(&mut data);
        assert_eq!(data, cipher);
        let mut dec = Cfb8 { aes: Aes128::new(GenericArray::from_slice(&key)), register: iv };
        dec.decrypt(&mut data);
        assert_eq!(data, plain);
    }

    #[test]
    fn skipping_ciphertext_keeps_the_stream_in_step() {
        let key = [7u8; 16];
        let mut plain: Vec<u8> = (0..100u8).collect();
        let mut sender = Cfb8::new(&key);
        sender.encrypt(&mut plain);
        let cipher = plain;
        for cut in [3, 20, 60] {
            let mut reader = Cfb8::new(&key);
            let mut head = cipher[..cut].to_vec();
            reader.decrypt(&mut head);
            reader.skip(&cipher[cut..cut + 5]);
            reader.skip(&cipher[cut + 5..90]);
            let mut tail = cipher[90..].to_vec();
            reader.decrypt(&mut tail);
            assert_eq!(tail, (90..100u8).collect::<Vec<_>>(), "cut {cut}");
        }
    }

    #[test]
    fn server_hashes_match_the_known_signed_sha1_values() {
        assert_eq!(signed_hex(&Sha1::digest(b"Notch")), "4ed1f46bbe04bc756bcb17c0c7ce3e4632f06a48");
        assert_eq!(signed_hex(&Sha1::digest(b"jeb_")), "-7c9d5b0044c130109a5d7b5fb5c317c02b4e28c1");
        assert_eq!(signed_hex(&Sha1::digest(b"simon")), "88e16a1019277b15d58faf0541e11910eb756f6");
        assert_eq!(server_hash("", b"jeb_", b""), "-7c9d5b0044c130109a5d7b5fb5c317c02b4e28c1");
    }

    fn der_item(tag: u8, value: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        if value.len() < 128 {
            out.push(value.len() as u8);
        } else {
            out.push(0x82);
            out.extend_from_slice(&(value.len() as u16).to_be_bytes());
        }
        out.extend_from_slice(value);
        out
    }

    #[test]
    fn rsa_encryption_decrypts_with_the_private_key_to_pkcs1_padding() {
        let p = BigUint::parse_bytes(b"F2A0F2EB4FF2A0B9F5E7F7A8C3E1B9D8DDE1B1C5F8E2F5A1E8F0C2A7D3B1E5C9F7", 16).unwrap();
        let p = next_prime(p);
        let q = next_prime(BigUint::parse_bytes(b"D1C3B5A7E9F1B3C5D7E9A1B3C5D7E9F1A3B5C7D9E1F3A5B7C9D1E3F5A7B9C1D3E5", 16).unwrap());
        let n = &p * &q;
        let e = BigUint::from(65537u32);
        let phi = (&p - 1u32) * (&q - 1u32);
        let d = e.modinv(&phi).unwrap();
        let mut n_bytes = n.to_bytes_be();
        if n_bytes[0] & 0x80 != 0 {
            n_bytes.insert(0, 0);
        }
        let key = [der_item(0x02, &n_bytes), der_item(0x02, &e.to_bytes_be())].concat();
        let mut bits = vec![0];
        bits.extend(der_item(0x30, &key));
        let algorithm = der_item(0x30, &[der_item(0x06, &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01]), vec![0x05, 0x00]].concat());
        let spki = der_item(0x30, &[algorithm, der_item(0x03, &bits)].concat());
        let secret = [9u8; 16];
        let sealed = rsa_encrypt(&spki, &secret).unwrap();
        let k = n.to_bytes_be().len();
        assert_eq!(sealed.len(), k);
        let mut opened = BigUint::from_bytes_be(&sealed).modpow(&d, &n).to_bytes_be();
        opened.insert(0, 0);
        assert_eq!(opened.len(), k);
        assert_eq!(&opened[..2], &[0, 2]);
        let pad = &opened[2..k - secret.len() - 1];
        assert!(pad.len() >= 8 && pad.iter().all(|b| *b != 0));
        assert_eq!(opened[k - secret.len() - 1], 0);
        assert_eq!(&opened[k - secret.len()..], &secret);
        assert_ne!(rsa_encrypt(&spki, &secret).unwrap(), sealed, "padding is random");
    }

    fn next_prime(mut n: BigUint) -> BigUint {
        if &n % 2u32 == BigUint::from(0u32) {
            n += 1u32;
        }
        while !probably_prime(&n) {
            n += 2u32;
        }
        n
    }

    fn probably_prime(n: &BigUint) -> bool {
        let one = BigUint::from(1u32);
        let minus = n - &one;
        let mut d = minus.clone();
        let mut s = 0;
        while &d % 2u32 == BigUint::from(0u32) {
            d >>= 1;
            s += 1;
        }
        [2u32, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37].iter().all(|a| {
            let mut x = BigUint::from(*a).modpow(&d, n);
            if x == one || x == minus {
                return true;
            }
            for _ in 1..s {
                x = x.modpow(&BigUint::from(2u32), n);
                if x == minus {
                    return true;
                }
            }
            false
        })
    }
}
