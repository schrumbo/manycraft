//! Notifications of the operating system: shown silently, one per tag, and a click on one comes back as its tag.

use crate::core::drive;

/// The name the notifications go out under.
#[cfg(any(target_os = "windows", target_os = "macos"))]
fn aumid() -> &'static str {
    crate::core::channel::app_id()
}

/// Shows a notification with `title` and `body` that replaces the one of the same `tag`; a click on it sends `tag` to `clicks`.
pub fn show(tag: &str, title: &str, body: &str, clicks: &async_channel::Sender<String>) {
    if drive::fenced("notify", format!("{title} | {body}")) {
        return;
    }
    if let Err(e) = os::show(tag, title, body, clicks) {
        eprintln!("notification: {e}");
    }
}

#[cfg(target_os = "windows")]
mod os {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    use winrt::Data::Xml::Dom::XmlDocument;
    use winrt::Foundation::TypedEventHandler;
    use winrt::UI::Notifications::{ToastNotification, ToastNotificationManager, ToastNotifier};
    use winrt::core::{HSTRING, IInspectable, h};

    thread_local! {
        static NOTIFIER: RefCell<Option<ToastNotifier>> = const { RefCell::new(None) };
        static SHOWN: RefCell<HashMap<String, ToastNotification>> = RefCell::new(HashMap::new());
    }

    pub fn show(tag: &str, title: &str, body: &str, clicks: &async_channel::Sender<String>) -> winrt::core::Result<()> {
        let notifier = notifier()?;
        let toast = ToastNotification::CreateToastNotification(&document(title, body)?)?;
        let mut hasher = DefaultHasher::new();
        tag.hash(&mut hasher);
        toast.SetTag(&HSTRING::from(format!("{:016x}", hasher.finish())))?;
        let (clicks, clicked) = (clicks.clone(), tag.to_owned());
        toast.Activated(&TypedEventHandler::<ToastNotification, IInspectable>::new(move |_, _| {
            let _ = clicks.try_send(clicked.clone());
            Ok(())
        }))?;
        if let Some(before) = SHOWN.with_borrow_mut(|s| s.remove(tag)) {
            let _ = notifier.Hide(&before);
        }
        notifier.Show(&toast)?;
        SHOWN.with_borrow_mut(|s| s.insert(tag.to_owned(), toast));
        Ok(())
    }

    fn notifier() -> winrt::core::Result<ToastNotifier> {
        if let Some(n) = NOTIFIER.with_borrow(Clone::clone) {
            return Ok(n);
        }
        let named = windows_registry::CURRENT_USER.create(format!(r"Software\Classes\AppUserModelId\{}", super::aumid())).and_then(|key| key.set_string("DisplayName", crate::core::channel::name()));
        if let Err(e) = named {
            eprintln!("notification name: {e}");
        }
        let n = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(super::aumid()))?;
        NOTIFIER.with_borrow_mut(|slot| *slot = Some(n.clone()));
        Ok(n)
    }

    fn document(title: &str, body: &str) -> winrt::core::Result<XmlDocument> {
        let doc = XmlDocument::new()?;
        let toast = doc.CreateElement(h!("toast"))?;
        doc.AppendChild(&toast)?;
        let visual = doc.CreateElement(h!("visual"))?;
        toast.AppendChild(&visual)?;
        let binding = doc.CreateElement(h!("binding"))?;
        binding.SetAttribute(h!("template"), h!("ToastGeneric"))?;
        visual.AppendChild(&binding)?;
        for text in [title, body] {
            let line = doc.CreateElement(h!("text"))?;
            line.SetInnerText(&HSTRING::from(text))?;
            binding.AppendChild(&line)?;
        }
        let audio = doc.CreateElement(h!("audio"))?;
        audio.SetAttribute(h!("silent"), h!("true"))?;
        toast.AppendChild(&audio)?;
        Ok(doc)
    }
}

#[cfg(not(target_os = "windows"))]
mod os {
    #[cfg(not(target_os = "macos"))]
    static SHOWN: std::sync::Mutex<Option<std::collections::HashMap<String, u32>>> = std::sync::Mutex::new(None);

    pub fn show(tag: &str, title: &str, body: &str, clicks: &async_channel::Sender<String>) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            static APP: std::sync::Once = std::sync::Once::new();
            APP.call_once(|| {
                let _ = notify_rust::set_application(super::aumid());
            });
        }
        let mut note = notify_rust::Notification::new();
        note.appname(crate::core::channel::name()).summary(title).body(body).action("default", "default");
        #[cfg(not(target_os = "macos"))]
        if let Some(id) = SHOWN.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_default().get(tag) {
            note.id(*id);
        }
        let (tag, clicks) = (tag.to_owned(), clicks.clone());
        std::thread::Builder::new()
            .name("notification".into())
            .spawn(move || match note.show() {
                Ok(shown) => {
                    #[cfg(not(target_os = "macos"))]
                    SHOWN.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_default().insert(tag.clone(), shown.id());
                    shown.wait_for_action(|action| {
                        if action == "default" {
                            let _ = clicks.try_send(tag);
                        }
                    })
                }
                Err(e) => eprintln!("notification: {e}"),
            })
            .map(drop)
            .map_err(|e| e.to_string())
    }
}
