//! Saved accounts: metadata in `accounts.json`, refresh tokens in the OS keychain.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{net, paths};

const FILE: &str = "accounts.json";
const KEYCHAIN_SERVICE: &str = "Manycraft";

/// How an account signs in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountKind {
    /// A Microsoft account with a refresh token in the keychain.
    #[default]
    Microsoft,
    /// Only a name, no sign-in, for servers in offline mode.
    Offline,
}

/// A Minecraft account. The refresh token of a Microsoft account is not part of it; it lives in the keychain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// Minecraft profile id; for offline accounts the vanilla offline UUID of the name.
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub kind: AccountKind,
    /// Proxy for login and game connection, e.g. `socks5://user:pass@host:1080`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
}

impl Account {
    /// An offline account for `name`, validated like vanilla (3 to 16 letters, digits or `_`).
    pub fn offline(name: &str) -> Result<Self> {
        validate_offline_name(name)?;
        Ok(Self { id: offline_uuid(name), name: name.to_owned(), kind: AccountKind::Offline, proxy: None })
    }
}

/// The UUID a vanilla server in offline mode gives the player `name`: UUID v3 of `OfflinePlayer:<name>`.
pub fn offline_uuid(name: &str) -> Uuid {
    let mut bytes = md5(format!("OfflinePlayer:{name}").as_bytes());
    bytes[6] = bytes[6] & 0x0f | 0x30;
    bytes[8] = bytes[8] & 0x3f | 0x80;
    Uuid::from_bytes(bytes)
}

/// Accepts what vanilla accepts as a player name: 3 to 16 characters from `A-Za-z0-9_`.
pub fn validate_offline_name(name: &str) -> Result<()> {
    if !(3..=16).contains(&name.len()) || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        bail!("a name has 3 to 16 characters: letters, digits and _");
    }
    Ok(())
}

/// Loads all saved accounts; an absent file is an empty list, an unreadable one an error.
pub fn load() -> Result<Vec<Account>> {
    load_from(&paths::data_dir())
}

/// Saves `accounts`, refusing to replace an accounts file that cannot be read.
pub fn save(accounts: &[Account]) -> Result<()> {
    save_to(&paths::data_dir(), accounts)
}

/// Adds `account`, or replaces the saved account with the same id in place.
pub fn upsert(accounts: &mut Vec<Account>, account: Account) {
    match accounts.iter_mut().find(|a| a.id == account.id) {
        Some(slot) => *slot = account,
        None => accounts.push(account),
    }
}

/// Stores the refresh token for account `id` in the OS keychain.
pub fn store_token(id: Uuid, token: &str) -> Result<()> {
    entry(id)?.set_secret(token.as_bytes()).context("cannot save the sign-in in the system keychain")
}

/// Reads the refresh token for account `id`; `None` when the keychain has none.
pub fn read_token(id: Uuid) -> Result<Option<String>> {
    match entry(id)?.get_secret() {
        Ok(bytes) => Ok(Some(String::from_utf8(bytes).context("the stored sign-in is unreadable")?)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e).context("cannot read the sign-in from the system keychain"),
    }
}

/// Removes the refresh token for account `id`; a missing token is not an error.
pub fn delete_token(id: Uuid) -> Result<()> {
    match entry(id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e).context("cannot remove the sign-in from the system keychain"),
    }
}

fn md5(data: &[u8]) -> [u8; 16] {
    const SHIFTS: [u32; 16] = [7, 12, 17, 22, 5, 9, 14, 20, 4, 11, 16, 23, 6, 10, 15, 21];
    let mut state = [0x6745_2301u32, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend(((data.len() as u64) * 8).to_le_bytes());
    for chunk in message.chunks_exact(64) {
        let mut words = [0u32; 16];
        for (word, bytes) in words.iter_mut().zip(chunk.chunks_exact(4)) {
            *word = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
        let [mut a, mut b, mut c, mut d] = state;
        for i in 0..64 {
            let (mix, index) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let constant = ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32;
            let sum = mix.wrapping_add(a).wrapping_add(constant).wrapping_add(words[index]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(sum.rotate_left(SHIFTS[i / 16 * 4 + i % 4]));
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d]) {
            *slot = slot.wrapping_add(value);
        }
    }
    let mut out = [0; 16];
    for (chunk, word) in out.chunks_exact_mut(4).zip(state) {
        chunk.copy_from_slice(&word.to_le_bytes());
    }
    out
}

fn entry(id: Uuid) -> Result<keyring::Entry> {
    keyring::Entry::new(KEYCHAIN_SERVICE, &id.to_string()).context("system keychain is not available")
}

fn load_from(dir: &Path) -> Result<Vec<Account>> {
    let path = dir.join(FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    serde_json::from_slice(&bytes).with_context(|| format!("{} is damaged; it was left untouched, fix or move it", path.display()))
}

fn save_to(dir: &Path, accounts: &[Account]) -> Result<()> {
    load_from(dir)?;
    let mut ids = std::collections::HashSet::new();
    if let Some(dup) = accounts.iter().find(|a| !ids.insert(a.id)) {
        bail!("account {} appears twice", dup.id);
    }
    let bytes = serde_json::to_vec_pretty(accounts).context("cannot encode accounts")?;
    net::write_atomic(&dir.join(FILE), &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(n: u128, name: &str, proxy: Option<&str>) -> Account {
        Account { id: Uuid::from_u128(n), name: name.into(), kind: AccountKind::Microsoft, proxy: proxy.map(Into::into) }
    }

    #[test]
    fn missing_file_is_an_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_from(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn round_trip_keeps_every_field() {
        let dir = tempfile::tempdir().unwrap();
        let accounts = vec![account(1, "Alpha", None), account(2, "Beta", Some("socks5://u:p@127.0.0.1:1080"))];
        save_to(dir.path(), &accounts).unwrap();
        assert_eq!(load_from(dir.path()).unwrap(), accounts);
        save_to(dir.path(), &accounts[..1]).unwrap();
        assert_eq!(load_from(dir.path()).unwrap(), &accounts[..1]);
    }

    #[test]
    fn saved_file_uses_dashed_ids_and_omits_absent_proxy() {
        let dir = tempfile::tempdir().unwrap();
        save_to(dir.path(), &[account(1, "Alpha", None)]).unwrap();
        let text = std::fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert!(text.contains("00000000-0000-0000-0000-000000000001"));
        assert!(!text.contains("proxy"));
    }

    #[test]
    fn file_without_proxy_key_loads() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), r#"[{"id":"069a79f4-44e9-4726-a5be-fca90e38aaf5","name":"Notch"}]"#).unwrap();
        let loaded = load_from(dir.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].proxy, None);
    }

    #[test]
    fn corrupt_file_is_an_error_and_stays_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        std::fs::write(&path, b"[{\"id\": broken").unwrap();
        assert!(load_from(dir.path()).is_err());
        assert!(save_to(dir.path(), &[account(1, "Alpha", None)]).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"[{\"id\": broken");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn unreadable_file_is_an_error_not_an_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(FILE)).unwrap();
        assert!(load_from(dir.path()).is_err());
        assert!(save_to(dir.path(), &[account(1, "Alpha", None)]).is_err());
        assert!(dir.path().join(FILE).is_dir());
    }

    #[test]
    fn wrong_shape_counts_as_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), br#"{"accounts":[]}"#).unwrap();
        assert!(load_from(dir.path()).is_err());
    }

    #[test]
    fn duplicate_ids_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(save_to(dir.path(), &[account(1, "A", None), account(1, "B", None)]).is_err());
        assert!(!dir.path().join(FILE).exists());
    }

    #[test]
    fn upsert_replaces_by_id_and_appends_new() {
        let mut accounts = vec![account(1, "Old", None), account(2, "Two", None)];
        upsert(&mut accounts, account(1, "New", Some("http://p:1")));
        upsert(&mut accounts, account(3, "Three", None));
        assert_eq!(accounts, vec![account(1, "New", Some("http://p:1")), account(2, "Two", None), account(3, "Three", None)]);
    }

    fn hex(bytes: [u8; 16]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn md5_matches_rfc_1321_vectors() {
        assert_eq!(hex(md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(hex(md5(b"message digest")), "f96b697d7cb7938d525a2f31aaf161d0");
        assert_eq!(hex(md5(b"The quick brown fox jumps over the lazy dog")), "9e107d9d372bb6826bd81d3542a419d6");
        let long = "1234567890".repeat(8);
        assert_eq!(hex(md5(long.as_bytes())), "57edf4a22be3c955ac49da2e2107b67a");
    }

    #[test]
    fn offline_uuid_matches_vanilla() {
        assert_eq!(offline_uuid("Notch").to_string(), "b50ad385-829d-3141-a216-7e7d7539ba7f");
        assert_ne!(offline_uuid("Notch"), offline_uuid("notch"));
        assert_eq!(offline_uuid("Notch").get_version_num(), 3);
    }

    #[test]
    fn offline_names_are_validated_like_vanilla() {
        for ok in ["abc", "Notch", "a_b_c_d_e_f_g_h_", "Player_123"] {
            assert!(validate_offline_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "ab", "a_b_c_d_e_f_g_h_i", "has space", "dash-ed", "näme", "a.b", "abc\n"] {
            assert!(validate_offline_name(bad).is_err(), "{bad:?}");
        }
        assert!(Account::offline("ab").is_err());
    }

    #[test]
    fn offline_account_has_vanilla_id_and_no_proxy() {
        let account = Account::offline("Notch").unwrap();
        assert_eq!(account.kind, AccountKind::Offline);
        assert_eq!(account.id.to_string(), "b50ad385-829d-3141-a216-7e7d7539ba7f");
        assert_eq!((account.name.as_str(), account.proxy.as_deref()), ("Notch", None));
    }

    #[test]
    fn kind_defaults_to_microsoft_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), r#"[{"id":"069a79f4-44e9-4726-a5be-fca90e38aaf5","name":"Notch"}]"#).unwrap();
        assert_eq!(load_from(dir.path()).unwrap()[0].kind, AccountKind::Microsoft);
        let accounts = vec![account(1, "Alpha", None), Account::offline("Beta").unwrap()];
        save_to(dir.path(), &accounts).unwrap();
        let text = std::fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert!(text.contains(r#""kind": "offline""#) && text.contains(r#""kind": "microsoft""#), "{text}");
        assert_eq!(load_from(dir.path()).unwrap(), accounts);
    }

    #[test]
    fn token_round_trip_in_the_system_keychain() {
        let id = Uuid::new_v4();
        if store_token(id, "first").is_err() {
            return;
        }
        assert_eq!(read_token(id).unwrap().as_deref(), Some("first"));
        let long = "x".repeat(2000);
        store_token(id, &long).unwrap();
        assert_eq!(read_token(id).unwrap(), Some(long));
        delete_token(id).unwrap();
        assert_eq!(read_token(id).unwrap(), None);
        delete_token(id).unwrap();
    }

    #[test]
    fn keychain_entry_uses_service_and_account_id() {
        let id = Uuid::from_u128(7);
        let entry = entry(id).unwrap();
        assert!(format!("{entry:?}").contains(&id.to_string()));
        assert!(format!("{entry:?}").contains(KEYCHAIN_SERVICE));
    }
}
