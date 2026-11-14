//! Checks a downloaded installer against its SHA-256 and the release signature before anything runs it.

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use minisign_verify::{PublicKey, Signature};
use sha2::{Digest, Sha256};

/// The public half of the key every release file is signed with in CI.
pub const RELEASE_KEY: &str = "RWRbJqLBcsELLUfxnCzjjWf8RnLlRHGlZubizV3WPi+8fG39+E2P1znA";

/// Fails unless the file at `path` has the SHA-256 in `sha256` (a `sha256sum` line) and a valid minisign signature `minisig` by `key`.
pub fn file(path: &Path, sha256: &str, minisig: &str, key: &str) -> Result<()> {
    let expected = sha256.split_whitespace().next().unwrap_or_default();
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("the checksum file is not a SHA-256");
    }
    let key = PublicKey::from_base64(key).map_err(|e| anyhow::anyhow!("bad public key: {e}"))?;
    let signature = Signature::decode(minisig).map_err(|e| anyhow::anyhow!("unreadable signature: {e}"))?;
    let mut stream = key.verify_stream(&signature).map_err(|e| anyhow::anyhow!("signature does not fit the release key: {e}"))?;
    let mut sha = Sha256::new();
    let mut file = std::fs::File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut buf = vec![0; 256 * 1024];
    loop {
        let n = file.read(&mut buf).with_context(|| format!("cannot read {}", path.display()))?;
        if n == 0 {
            break;
        }
        sha.update(&buf[..n]);
        stream.update(&buf[..n]);
    }
    let got: String = sha.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if !got.eq_ignore_ascii_case(expected) {
        bail!("SHA-256 mismatch");
    }
    stream.finalize().map_err(|e| anyhow::anyhow!("signature mismatch: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/core/testdata/update");
    const SHA: &str = "bb0cabcbff009359250bafb84c7be5a7ac81f84340ffc6db268645997b17c308  asset.bin\n";

    fn fixture_key() -> String {
        std::fs::read_to_string(format!("{DIR}/fixture.pub")).unwrap().lines().nth(1).unwrap().to_owned()
    }

    fn signature() -> String {
        std::fs::read_to_string(format!("{DIR}/asset.bin.minisig")).unwrap()
    }

    #[test]
    fn a_signed_file_with_the_right_checksum_passes() {
        file(Path::new(&format!("{DIR}/asset.bin")), SHA, &signature(), &fixture_key()).unwrap();
    }

    #[test]
    fn a_changed_byte_fails_both_checks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("asset.bin");
        let mut bytes = std::fs::read(format!("{DIR}/asset.bin")).unwrap();
        bytes[0] ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        assert!(file(&path, SHA, &signature(), &fixture_key()).unwrap_err().to_string().contains("SHA-256"));
        let sha: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        assert!(file(&path, &sha, &signature(), &fixture_key()).unwrap_err().to_string().contains("signature"));
    }

    #[test]
    fn the_wrong_key_or_a_broken_sidecar_fails() {
        let path = format!("{DIR}/asset.bin");
        let path = Path::new(&path);
        assert!(file(path, SHA, &signature(), RELEASE_KEY).is_err());
        assert!(file(path, "nope", &signature(), &fixture_key()).is_err());
        assert!(file(path, SHA, "garbage", &fixture_key()).is_err());
        assert!(file(&path.with_extension("missing"), SHA, &signature(), &fixture_key()).is_err());
    }

    #[test]
    fn the_release_key_parses() {
        assert!(PublicKey::from_base64(RELEASE_KEY).is_ok());
    }
}
