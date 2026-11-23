//! Why a connection dropped, read from the disconnect message the way the companion mod reads its disconnect screen.

use crate::core::ipc::DisconnectCause;

const WHITELIST: [&str; 3] = ["whitelist", "white-list", "white list"];
const FULL: [&str; 3] = ["server is full", "server full", "is full"];
const AUTH: [&str; 6] = ["failed to verify username", "invalid session", "failed to log in", "login failed", "session expired", "public key"];
const KICKED: [&str; 5] = ["kicked", "idle", "afk", "flying", "spam"];
const DUPLICATE: [&str; 5] = ["another location", "already online", "already connected", "already logged in", "logged in from"];
const DOWN: [&str; 17] = ["connection refused", "timed out", "timeout", "connection lost", "connection closed", "connection reset", "server closed", "shutting down", "restarting", "restart", "stopping", "no route", "unknown host", "unresolved", "unreachable", "end of stream", "authentication servers are down"];
const BAN_WORDS: [&str; 7] = ["banned", "tempbanned", "ipbanned", "blacklisted", "ban", "tempban", "ipban"];
const BAN_PARTS: [&str; 2] = ["bannt", "gesperrt"];

/// The cause of a drop with translation `keys` (outermost first) and the shown `text`.
pub fn classify(keys: &[String], text: &str) -> DisconnectCause {
    if let Some(cause) = keys.iter().find_map(|k| by_key(k)) {
        return cause;
    }
    if keys.iter().any(|k| k.starts_with("disconnect.loginFailed")) {
        return DisconnectCause::Auth;
    }
    by_text(&text.to_lowercase())
}

fn by_key(key: &str) -> Option<DisconnectCause> {
    use DisconnectCause::*;
    if key.starts_with("multiplayer.disconnect.banned") || key == "disconnect.loginFailedInfo.userBanned" {
        return Some(Banned);
    }
    match key {
        "multiplayer.disconnect.not_whitelisted" => return Some(Whitelist),
        "multiplayer.requiredTexturePrompt.disconnect" => return Some(PackDeclined),
        "multiplayer.disconnect.server_full" => return Some(ServerFull),
        "multiplayer.disconnect.authservers_down" | "disconnect.loginFailedInfo.serversUnavailable" => return Some(ServerDown),
        _ => {}
    }
    if key.starts_with("disconnect.loginFailedInfo.") || key == "multiplayer.disconnect.unverified_username" || key.contains("public_key") {
        return Some(Auth);
    }
    Some(match key {
        "multiplayer.disconnect.kicked" | "multiplayer.disconnect.idling" | "multiplayer.disconnect.flying" | "disconnect.kicked" => Kicked,
        "multiplayer.disconnect.duplicate_login" | "multiplayer.disconnect.name_taken" => Duplicate,
        "multiplayer.disconnect.server_shutdown" | "multiplayer.disconnect.slow_login" | "disconnect.timeout" | "disconnect.lost" | "disconnect.closed" | "disconnect.endOfStream" => ServerDown,
        _ => return None,
    })
}

fn by_text(text: &str) -> DisconnectCause {
    let any = |needles: &[&str]| needles.iter().any(|n| text.contains(n));
    if text.split(|c: char| !c.is_ascii_lowercase()).any(|word| BAN_WORDS.contains(&word)) || any(&BAN_PARTS) {
        DisconnectCause::Banned
    } else if any(&WHITELIST) {
        DisconnectCause::Whitelist
    } else if any(&FULL) {
        DisconnectCause::ServerFull
    } else if any(&AUTH) {
        DisconnectCause::Auth
    } else if any(&DUPLICATE) {
        DisconnectCause::Duplicate
    } else if any(&KICKED) {
        DisconnectCause::Kicked
    } else if any(&DOWN) {
        DisconnectCause::ServerDown
    } else {
        DisconnectCause::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ipc::DisconnectCause::*;

    fn keys(k: &[&str]) -> Vec<String> {
        k.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn keys_decide_before_text() {
        assert_eq!(classify(&keys(&["multiplayer.disconnect.banned.reason"]), "whatever"), Banned);
        assert_eq!(classify(&keys(&["multiplayer.disconnect.server_shutdown"]), "Server closed"), ServerDown);
        assert_eq!(classify(&keys(&["multiplayer.disconnect.duplicate_login"]), ""), Duplicate);
        assert_eq!(classify(&keys(&["multiplayer.requiredTexturePrompt.disconnect"]), ""), PackDeclined);
        assert_eq!(classify(&keys(&["disconnect.loginFailedInfo.invalidSession"]), ""), Auth);
        assert_eq!(classify(&keys(&["disconnect.loginFailedInfo.serversUnavailable"]), ""), ServerDown);
        assert_eq!(classify(&keys(&["some.plugin.key", "multiplayer.disconnect.kicked"]), ""), Kicked);
        assert_eq!(classify(&keys(&["disconnect.loginFailed"]), ""), Auth);
    }

    #[test]
    fn text_falls_back_like_the_mod() {
        assert_eq!(classify(&[], "You are BANNED from this server"), Banned);
        assert_eq!(classify(&[], "Du wurdest gebannt"), Banned);
        assert_eq!(classify(&[], "Bandwidth exceeded"), Unknown, "ban only as a whole word");
        assert_eq!(classify(&[], "You are not on the whitelist"), Whitelist);
        assert_eq!(classify(&[], "The server is full!"), ServerFull);
        assert_eq!(classify(&[], "You logged in from another location"), Duplicate);
        assert_eq!(classify(&[], "Kicked for being AFK"), Kicked);
        assert_eq!(classify(&[], "Server is restarting"), ServerDown);
        assert_eq!(classify(&[], "Connection refused: no further information"), ServerDown);
        assert_eq!(classify(&[], "Bye"), Unknown);
    }
}
