use crate::core::auth::AuthError;
use crate::core::i18n::{tr, trf};
use crate::core::skin_change::SkinError;
use crate::core::wardrobe::{AddError, Step};
use crate::ui::forms::{self, Problem};

fn skin_text(e: &SkinError) -> String {
    match e {
        SkinError::NotPng => tr("Das ist keine PNG-Datei. Wähl ein Skin-Bild im PNG-Format."),
        SkinError::WrongSize(w, h) => trf("Ein Skin ist 64 × 64 oder 64 × 32 Pixel groß, dieses Bild %lld × %lld.", &[w, h]),
        SkinError::TooLarge => tr("Die Datei ist zu groß für einen Skin."),
        SkinError::RateLimited => tr("Microsoft lässt gerade keine Änderung zu, versuch es in einer Minute nochmal."),
        SkinError::SignInAgain => tr("Die Anmeldung ist abgelaufen. Melde dich erneut an."),
        SkinError::SkinRejected => tr("Minecraft hat diesen Skin abgelehnt. Versuch es mit einer anderen Datei."),
        SkinError::CapeRejected => tr("Diesen Umhang kann das Konto gerade nicht tragen."),
        SkinError::Unavailable(_) => tr("Die Minecraft-Dienste antworten gerade nicht. Versuch es gleich noch einmal."),
    }
}

fn auth_text(e: &AuthError) -> String {
    match e {
        AuthError::SignInAgain => tr("Die gespeicherte Anmeldung gilt nicht mehr. Melde dich erneut an."),
        AuthError::NoXboxProfile => tr("Dieses Konto hat noch kein Xbox-Profil. Melde dich einmal auf xbox.com an, dann klappt es."),
        AuthError::RegionBlocked => tr("Xbox Live ist im Land dieses Kontos nicht verfügbar."),
        AuthError::AdultVerification => tr("Dieses Konto muss erst bei Microsoft das Alter bestätigen."),
        AuthError::ChildAccount => tr("Das ist ein Kinderkonto. Ein Erwachsener muss es erst zu einer Microsoft-Familie hinzufügen."),
        AuthError::XboxRejected(code) => trf("Xbox Live hat die Anmeldung abgelehnt (Code %lld).", &[code]),
        other => other.to_string(),
    }
}

fn add_text(e: &AddError) -> String {
    match e {
        AddError::BadLink => tr("Das ist keine Webadresse. Füg einen Link wie https://… ein."),
        AddError::NotAnImage => tr("Unter diesem Link liegt kein Skin-Bild im PNG-Format."),
        AddError::BadName => tr("Ein Spielername hat 3 bis 16 Buchstaben, Ziffern oder _."),
        AddError::NoSuchPlayer => tr("Diesen Spieler gibt es nicht."),
        AddError::NoSkin => tr("Dieser Spieler trägt keinen eigenen Skin."),
        AddError::RateLimited => tr("Zu viele Anfragen. Versuch es in einer Minute nochmal."),
        AddError::Unreachable => tr("Keine Verbindung. Prüfe Internet und Proxy und versuche es noch einmal."),
    }
}

/// Everyday German for whatever went wrong with a skin, a cape or a lookup.
pub fn problem(e: &anyhow::Error) -> Problem {
    if let Some(a) = e.chain().find_map(|c| c.downcast_ref::<AddError>()) {
        return Problem::plain(add_text(a));
    }
    if let Some(s) = e.chain().find_map(|c| c.downcast_ref::<SkinError>()) {
        let detail = matches!(s, SkinError::Unavailable(_)).then(|| format!("{e:#}"));
        return Problem { text: skin_text(s), detail };
    }
    if let Some(a) = e.chain().find_map(|c| c.downcast_ref::<AuthError>()) {
        return Problem::plain(auth_text(a));
    }
    forms::explain(e)
}

/// Whether only a new sign-in of the account helps.
pub fn signs_in_again(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.downcast_ref::<AuthError>() == Some(&AuthError::SignInAgain) || matches!(c.downcast_ref::<SkinError>(), Some(SkinError::SignInAgain)))
}

pub fn done_text(skin: bool, cape: bool) -> String {
    match (skin, cape) {
        (true, true) => tr("Skin und Umhang geändert."),
        (true, false) => tr("Skin geändert."),
        _ => tr("Umhang geändert."),
    }
}

/// What the lane is doing, in a few words.
pub fn step_text(step: Step) -> String {
    match step {
        Step::SignIn => tr("Anmeldung wird geprüft …"),
        Step::Profile => tr("Profil wird geladen …"),
        Step::Capes { done, total } => trf("Umhänge werden geladen (%lld von %lld) …", &[&(done + 1).min(total), &total]),
        Step::Upload => tr("Wird an Minecraft gesendet …"),
        Step::Saving => tr("Wird gesichert …"),
    }
}
