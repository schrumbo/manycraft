//! Keeps the computer from going to sleep on its own while games run. The display may still turn off, and a sleep the user chooses is never blocked. The operating system owns the hold, so it ends with the app, however the app ends.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::OnceLock;

use super::i18n::tr;

/// Whether the system should be kept awake: the setting is on and at least one game runs or waits to start again.
pub fn wanted(setting: bool, games: bool) -> bool {
    setting && games
}

struct Keeper<H> {
    held: Option<H>,
    gave_up: bool,
}

impl<H> Keeper<H> {
    const fn new() -> Self {
        Self { held: None, gave_up: false }
    }

    /// Takes the hold when it is wanted and not held yet, drops it when it is not wanted. A failed take is not tried again until the want went away once.
    fn apply(&mut self, wanted: bool, take: impl FnOnce() -> Option<H>) {
        if !wanted {
            self.held = None;
            self.gave_up = false;
        } else if self.held.is_none() && !self.gave_up {
            self.held = take();
            self.gave_up = self.held.is_none();
        }
    }

    #[cfg(test)]
    fn holds(&self) -> bool {
        self.held.is_some()
    }
}

static WISH: AtomicBool = AtomicBool::new(false);
static WORKER: OnceLock<Sender<bool>> = OnceLock::new();

/// Holds the system awake or lets it sleep again. Cheap to call after every change: it only acts when the wish changed, and the work runs on its own thread.
pub fn set(wanted: bool) {
    if WISH.swap(wanted, Ordering::SeqCst) == wanted {
        return;
    }
    let worker = WORKER.get_or_init(|| {
        let (send, receive) = mpsc::channel::<bool>();
        let spawned = std::thread::Builder::new().name("awake".into()).spawn(move || {
            let mut keeper = Keeper::new();
            while let Ok(mut wanted) = receive.recv() {
                while let Ok(newer) = receive.try_recv() {
                    wanted = newer;
                }
                keeper.apply(wanted, || imp::take(&tr("Manycraft: Spiele laufen")));
            }
        });
        if let Err(e) = spawned {
            eprintln!("awake: the system could not be kept awake: {e}");
        }
        send
    });
    let _ = worker.send(wanted);
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Power::{PowerClearRequest, PowerCreateRequest, PowerRequestSystemRequired, PowerSetRequest};
    use windows_sys::Win32::System::Threading::{POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0};

    pub struct Hold(HANDLE);

    pub fn take(reason: &str) -> Option<Hold> {
        let mut text: Vec<u16> = reason.encode_utf16().chain(Some(0)).collect();
        let context = REASON_CONTEXT { Version: 0, Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING, Reason: REASON_CONTEXT_0 { SimpleReasonString: text.as_mut_ptr() } };
        let handle = unsafe { PowerCreateRequest(&context) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            eprintln!("awake: the system could not be kept awake: {}", std::io::Error::last_os_error());
            return None;
        }
        if unsafe { PowerSetRequest(handle, PowerRequestSystemRequired) } == 0 {
            eprintln!("awake: the system could not be kept awake: {}", std::io::Error::last_os_error());
            unsafe { CloseHandle(handle) };
            return None;
        }
        Some(Hold(handle))
    }

    impl Drop for Hold {
        fn drop(&mut self) {
            unsafe {
                PowerClearRequest(self.0, PowerRequestSystemRequired);
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;

    use objc2_foundation::NSString;

    const LEVEL_ON: u32 = 255;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(kind: *const c_void, level: u32, name: *const c_void, id: *mut u32) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
    }

    pub struct Hold(u32);

    pub fn take(reason: &str) -> Option<Hold> {
        let kind = NSString::from_str("PreventUserIdleSystemSleep");
        let name = NSString::from_str(reason);
        let mut id = 0u32;
        let code = unsafe { IOPMAssertionCreateWithName(&*kind as *const NSString as *const c_void, LEVEL_ON, &*name as *const NSString as *const c_void, &mut id) };
        if code != 0 {
            eprintln!("awake: the system could not be kept awake (IOKit {code})");
            return None;
        }
        Some(Hold(id))
    }

    impl Drop for Hold {
        fn drop(&mut self) {
            unsafe { IOPMAssertionRelease(self.0) };
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod imp {
    use std::process::{Child, Command, Stdio};
    use std::time::Duration;

    const CHECK: Duration = Duration::from_millis(150);

    /// A `systemd-inhibit` child that holds the lock while it waits on a pipe; it ends when the pipe closes, so a crashed app leaves nothing behind.
    pub struct Hold(Child);

    pub fn take(reason: &str) -> Option<Hold> {
        let child = Command::new("systemd-inhibit")
            .args(["--what=idle:sleep", "--who=Manycraft", "--mode=block", "--why", reason, "cat"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn();
        match child {
            Ok(mut child) => {
                std::thread::sleep(CHECK);
                match child.try_wait() {
                    Ok(Some(status)) => {
                        eprintln!("awake: the system could not be kept awake: systemd-inhibit ended with {status}");
                        None
                    }
                    _ => Some(Hold(child)),
                }
            }
            Err(e) => {
                eprintln!("awake: the system could not be kept awake: {e}");
                None
            }
        }
    }

    impl Drop for Hold {
        fn drop(&mut self) {
            drop(self.0.stdin.take());
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn only_a_running_game_with_the_setting_on_wants_the_hold() {
        assert!(wanted(true, true));
        assert!(!wanted(true, false));
        assert!(!wanted(false, true));
        assert!(!wanted(false, false));
    }

    #[test]
    fn the_hold_is_taken_once_and_dropped_when_not_wanted() {
        let takes = Cell::new(0);
        let mut keeper = Keeper::new();
        let take = || {
            takes.set(takes.get() + 1);
            Some(())
        };
        keeper.apply(false, take);
        assert!(!keeper.holds() && takes.get() == 0);
        keeper.apply(true, take);
        keeper.apply(true, take);
        assert!(keeper.holds() && takes.get() == 1);
        keeper.apply(false, take);
        assert!(!keeper.holds());
        keeper.apply(true, take);
        assert!(keeper.holds() && takes.get() == 2);
    }

    #[test]
    fn dropping_the_hold_releases_it() {
        struct Flag<'a>(&'a Cell<bool>);
        impl Drop for Flag<'_> {
            fn drop(&mut self) {
                self.0.set(true);
            }
        }
        let released = Cell::new(false);
        let mut keeper = Keeper::new();
        keeper.apply(true, || Some(Flag(&released)));
        assert!(!released.get());
        keeper.apply(false, || None);
        assert!(released.get());
    }

    #[test]
    fn a_failed_take_is_not_retried_until_the_want_went_away() {
        let tries = Cell::new(0);
        let mut keeper: Keeper<()> = Keeper::new();
        let fail = || {
            tries.set(tries.get() + 1);
            None
        };
        keeper.apply(true, fail);
        keeper.apply(true, fail);
        keeper.apply(true, fail);
        assert!(!keeper.holds() && tries.get() == 1);
        keeper.apply(false, fail);
        keeper.apply(true, fail);
        assert_eq!(tries.get(), 2);
    }
}
