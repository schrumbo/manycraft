//! Login encryption: AES-128-CFB8 for the stream, RSA PKCS#1 v1.5 for the shared secret, and the server hash the
//! session server checks.

use aes::Aes128;
use aes::cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray};
use anyhow::{Context, Result, ensure};
use base64::Engine;
use num_bigint::BigUint;
use sha1::{Digest, Sha1};
use sha2::Sha256;

const MAX_MODULUS: usize = 512;
const MAX_EXPONENT: usize = 8;

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

/// The DER prefix of a SHA-256 `DigestInfo`, which PKCS#1 v1.5 signatures wrap the hash in.
const SHA256_DIGEST_INFO: [u8; 19] = [0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05, 0x00, 0x04, 0x20];

/// An account's RSA key for signing chat: SHA256withRSA, PKCS#1 v1.5. Not `Debug`, so it never lands in a log.
pub struct SigningKey {
    n: BigUint,
    d: BigUint,
    len: usize,
}

impl SigningKey {
    /// Reads a PEM private key, PKCS#8 or PKCS#1 whatever its label says; Mojang labels PKCS#8 keys "RSA PRIVATE KEY".
    pub fn from_pem(pem: &str) -> Result<Self> {
        let body: String = pem.lines().filter(|l| !l.starts_with("-----")).flat_map(str::chars).filter(|c| !c.is_whitespace()).collect();
        let der_bytes = base64::engine::general_purpose::STANDARD.decode(body).context("the private key is not base64")?;
        let mut outer = Der(der(&mut Der(&der_bytes), 0x30)?);
        der(&mut outer, 0x02)?;
        let mut key = match outer.0.first() {
            Some(0x30) => {
                der(&mut outer, 0x30)?;
                Der(der(&mut Der(der(&mut outer, 0x04)?), 0x30)?)
            }
            _ => outer,
        };
        if matches!(key.0.first(), Some(0x02)) && key.0.get(1) == Some(&1) && key.0.get(2) == Some(&0) {
            der(&mut key, 0x02)?;
        }
        let n = der(&mut key, 0x02)?;
        der(&mut key, 0x02)?;
        let d = der(&mut key, 0x02)?;
        ensure!(n.len() <= MAX_MODULUS + 1, "RSA key larger than any server accepts");
        let n = BigUint::from_bytes_be(n);
        let len = usize::try_from(n.bits().div_ceil(8))?;
        ensure!(len >= SHA256_DIGEST_INFO.len() + 32 + 11, "RSA key too small");
        Ok(Self { n, d: BigUint::from_bytes_be(d), len })
    }

    /// The SHA256withRSA signature of `message`, as long as the key's modulus.
    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        let hash = Sha256::digest(message);
        let mut block = vec![0xff; self.len];
        block[0] = 0;
        block[1] = 1;
        let tail = SHA256_DIGEST_INFO.len() + hash.len();
        block[self.len - tail - 1] = 0;
        block[self.len - tail..self.len - hash.len()].copy_from_slice(&SHA256_DIGEST_INFO);
        block[self.len - hash.len()..].copy_from_slice(&hash);
        let signed = BigUint::from_bytes_be(&block).modpow(&self.d, &self.n).to_bytes_be();
        let mut out = vec![0; self.len - signed.len()];
        out.extend_from_slice(&signed);
        out
    }
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
    ensure!(n.len() <= MAX_MODULUS + 1 && e.len() <= MAX_EXPONENT, "RSA key larger than any server sends");
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

    const KEY_PKCS8: &str = "-----BEGIN PRIVATE KEY-----
MIICdgIBADANBgkqhkiG9w0BAQEFAASCAmAwggJcAgEAAoGBAPMes6w6vXk66li/
u0/RuatVtVTE+JdJxDvzbsN19at+mQs1/dFU/bHmohTQJZgl3XZ4x/nAUCXdOaND
Gf6KLc6OtFHR98VQmLTyQyxaZHYGLhOyNszC5d5M6ap45bKYb5aWKWEcv/jh0rHq
bwCy2gNFMss6nCFzYENpbYAtO5tfAgMBAAECgYBLtd7sHqrMqv05wBOb6srwQVOj
32cu7ejvZoKy2fQ0gMs2Q40UeGw8z37gDmX2uKv77ecQrsQG1prRbd8VdoCGvGU/
oL++XqPVQojKKsHu62cLc9NdJdM/H+przSLiCssO1miVBpbfhFACPCgP+/rNuGZv
rbA2qABIty6bylD9WQJBAPsiWDoCZPLVuZDnRl/WBe3qyfNwsQRdBH2aPXUT/z5g
iVcSuhS2OHHzW9so6azaYckAWIvUR8ZReGHPH2lwiusCQQD31JsNqXMARcL3t5Ua
oPsvbl09Si3Xl21Ofo4l104nrtKf+BdRi0/hyScE/zescNO06q+Ky3VgJ4hK5dni
umxdAkEAsfmEBxBdVE8s5ElS8WYu645iMrkhalM1dIITnd5fHGizzFaEM1t0IyVx
ap7TA+H8PMOW/PmjKCPiqYZs33+XowJAcSHXqjPpMH2oE+b1JNPJ9ov9+kWLibgx
iLyS7UlM/yZ6NdZN5saer6ZgrpJG+uwCaLsdaVGZgkxpli84+Jw7bQJAWvo73Gjo
a/v1l+FBtwLMQpfKhVn+Oe+JOAfMLHMW741Y928nssHXaPzTfMToXjqZEeh3Ssdv
gQcbr/IfwCSwNw==
-----END PRIVATE KEY-----";
    const KEY_PKCS1: &str = "-----BEGIN RSA PRIVATE KEY-----
MIICXAIBAAKBgQDzHrOsOr15OupYv7tP0bmrVbVUxPiXScQ7827DdfWrfpkLNf3R
VP2x5qIU0CWYJd12eMf5wFAl3TmjQxn+ii3OjrRR0ffFUJi08kMsWmR2Bi4TsjbM
wuXeTOmqeOWymG+WlilhHL/44dKx6m8AstoDRTLLOpwhc2BDaW2ALTubXwIDAQAB
AoGAS7Xe7B6qzKr9OcATm+rK8EFTo99nLu3o72aCstn0NIDLNkONFHhsPM9+4A5l
9rir++3nEK7EBtaa0W3fFXaAhrxlP6C/vl6j1UKIyirB7utnC3PTXSXTPx/qa80i
4grLDtZolQaW34RQAjwoD/v6zbhmb62wNqgASLcum8pQ/VkCQQD7Ilg6AmTy1bmQ
50Zf1gXt6snzcLEEXQR9mj11E/8+YIlXEroUtjhx81vbKOms2mHJAFiL1EfGUXhh
zx9pcIrrAkEA99SbDalzAEXC97eVGqD7L25dPUot15dtTn6OJddOJ67Sn/gXUYtP
4cknBP83rHDTtOqvist1YCeISuXZ4rpsXQJBALH5hAcQXVRPLORJUvFmLuuOYjK5
IWpTNXSCE53eXxxos8xWhDNbdCMlcWqe0wPh/DzDlvz5oygj4qmGbN9/l6MCQHEh
16oz6TB9qBPm9STTyfaL/fpFi4m4MYi8ku1JTP8mejXWTebGnq+mYK6SRvrsAmi7
HWlRmYJMaZYvOPicO20CQFr6O9xo6Gv79ZfhQbcCzEKXyoVZ/jnviTgHzCxzFu+N
WPdvJ7LB12j803zE6F46mRHod0rHb4EHG6/yH8AksDc=
-----END RSA PRIVATE KEY-----";
    const SIGNED: &str = "91011e1b3ae391a9fb9d19376514a5042916d69a1e86ebe369b9f862378c05f34cd513178e553656c584ce7fd8b4b0177778deabe65e56a2ccee01d4b56bc057b208aa221e8f8a137ed03e6f6894b648b3f798e25decec504f961a47aaf433a00b8ce1313f9e8e5da01f28dabe4d3bd2955a5b0825a11bf0217b24d482c873b7";

    #[test]
    fn signing_matches_openssl_for_both_private_key_layouts() {
        for pem in [KEY_PKCS8, KEY_PKCS1, &KEY_PKCS8.replace("BEGIN PRIVATE", "BEGIN RSA PRIVATE").replace("END PRIVATE", "END RSA PRIVATE")] {
            let key = SigningKey::from_pem(pem).unwrap();
            assert_eq!(key.sign(b"manycraft signed chat").iter().map(|b| format!("{b:02x}")).collect::<String>(), SIGNED);
        }
        assert!(SigningKey::from_pem("-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----").is_err());
    }

    #[test]
    fn oversized_keys_are_refused() {
        let key = [der_item(0x02, &[0x7f; 64]), der_item(0x02, &[1; MAX_EXPONENT + 1])].concat();
        let mut bits = vec![0];
        bits.extend(der_item(0x30, &key));
        let spki = der_item(0x30, &[der_item(0x30, &[0x05, 0x00]), der_item(0x03, &bits)].concat());
        assert!(rsa_encrypt(&spki, &[0; 16]).is_err());
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
