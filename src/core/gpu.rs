//! The graphics card Windows gives a Java launcher, through its per-app preference in
//! `HKCU\Software\Microsoft\DirectX\UserGpuPreferences`. Manycraft writes a preference only for an instance whose
//! user picked one, remembers what was there before, and puts that back once no instance wants a choice any more.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::options::Gpu;

const LEDGER: &str = "gpu-preferences.json";

/// The registry data that asks Windows for `choice`.
pub fn value(choice: Gpu) -> &'static str {
    match choice {
        Gpu::PowerSaving => "GpuPreference=1;",
        Gpu::HighPerformance => "GpuPreference=2;",
    }
}

/// Per-app preferences, keyed by the full path of the program.
pub trait Preferences {
    fn get(&self, program: &str) -> Option<String>;
    fn set(&mut self, program: &str, data: &str) -> Result<()>;
    fn remove(&mut self, program: &str) -> Result<()>;
}

/// What stood in the registry before Manycraft wrote a preference, and the instances that asked for one, per program.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger(BTreeMap<String, Entry>);

#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    before: Option<String>,
    by: BTreeSet<String>,
}

/// Asks for `choice` for `program` on behalf of `instance`. Without a choice, the instance lets go of the program, and
/// what was there before comes back once no instance asks any more.
pub fn apply_to(prefs: &mut dyn Preferences, ledger: &mut Ledger, program: &str, instance: &str, choice: Option<Gpu>) -> Result<()> {
    let Some(choice) = choice else {
        if ledger.0.get_mut(program).is_some_and(|e| e.by.remove(instance)) {
            restore_unused(prefs, ledger)?;
        }
        return Ok(());
    };
    let entry = ledger.0.entry(program.to_owned()).or_insert_with(|| Entry { before: prefs.get(program), by: BTreeSet::new() });
    entry.by.insert(instance.to_owned());
    if prefs.get(program).as_deref() != Some(value(choice)) {
        prefs.set(program, value(choice))?;
    }
    Ok(())
}

/// Lets `instance` go of every program it asked a graphics card for.
pub fn release_from(prefs: &mut dyn Preferences, ledger: &mut Ledger, instance: &str) -> Result<()> {
    let mut changed = false;
    for entry in ledger.0.values_mut() {
        changed |= entry.by.remove(instance);
    }
    if changed { restore_unused(prefs, ledger) } else { Ok(()) }
}

fn restore_unused(prefs: &mut dyn Preferences, ledger: &mut Ledger) -> Result<()> {
    let unused: Vec<String> = ledger.0.iter().filter(|(_, e)| e.by.is_empty()).map(|(p, _)| p.clone()).collect();
    for program in unused {
        match ledger.0.get(&program).and_then(|e| e.before.clone()) {
            Some(data) => prefs.set(&program, &data)?,
            None => prefs.remove(&program)?,
        }
        ledger.0.remove(&program);
    }
    Ok(())
}

fn with_ledger(change: impl FnOnce(&mut dyn Preferences, &mut Ledger) -> Result<()>, skip_when_empty: bool) -> Result<()> {
    if !cfg!(windows) {
        return Ok(());
    }
    let path = super::paths::data_dir().join(LEDGER);
    let mut ledger: Ledger = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    if skip_when_empty && ledger.0.is_empty() {
        return Ok(());
    }
    let before = serde_json::to_vec(&ledger)?;
    let result = change(&mut registry::Registry, &mut ledger);
    let after = serde_json::to_vec(&ledger)?;
    if after != before {
        super::net::write_atomic(&path, &after)?;
    }
    result
}

/// Applies the graphics card choice of `instance` to the Java launcher it starts with. Does nothing outside Windows.
pub fn apply(java: &Path, instance: &str, choice: Option<Gpu>) -> Result<()> {
    let program = java.to_string_lossy().into_owned();
    with_ledger(|prefs, ledger| apply_to(prefs, ledger, &program, instance, choice), choice.is_none())
}

/// Takes back the graphics card choice of `instance` right away, for when the user resets it or deletes the instance.
pub fn release(instance: &str) -> Result<()> {
    with_ledger(|prefs, ledger| release_from(prefs, ledger, instance), true)
}

#[cfg(windows)]
mod registry {
    use anyhow::{Result, bail};
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW};

    const KEY: &str = r"Software\Microsoft\DirectX\UserGpuPreferences";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    pub struct Registry;

    impl super::Preferences for Registry {
        fn get(&self, program: &str) -> Option<String> {
            let (key, value) = (wide(KEY), wide(program));
            let mut size = 0u32;
            let found = unsafe { RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), std::ptr::null_mut(), &mut size) };
            if found != ERROR_SUCCESS || size == 0 {
                return None;
            }
            let mut buf = vec![0u16; (size as usize).div_ceil(2)];
            let read = unsafe { RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut size) };
            if read != ERROR_SUCCESS {
                return None;
            }
            let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            Some(String::from_utf16_lossy(&buf[..end]))
        }

        fn set(&mut self, program: &str, data: &str) -> Result<()> {
            let data = wide(data);
            let bytes = u32::try_from(data.len() * 2).unwrap_or(0);
            let status = unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, wide(KEY).as_ptr(), wide(program).as_ptr(), REG_SZ, data.as_ptr().cast(), bytes) };
            if status != ERROR_SUCCESS {
                bail!("cannot set the graphics card preference ({status})");
            }
            Ok(())
        }

        fn remove(&mut self, program: &str) -> Result<()> {
            let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, wide(KEY).as_ptr(), wide(program).as_ptr()) };
            if status != ERROR_SUCCESS && status != ERROR_FILE_NOT_FOUND {
                bail!("cannot remove the graphics card preference ({status})");
            }
            Ok(())
        }
    }
}

#[cfg(not(windows))]
mod registry {
    pub struct Registry;

    impl super::Preferences for Registry {
        fn get(&self, _: &str) -> Option<String> {
            None
        }

        fn set(&mut self, _: &str, _: &str) -> anyhow::Result<()> {
            Ok(())
        }

        fn remove(&mut self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[derive(Default)]
    struct Fake(HashMap<String, String>);

    impl Preferences for Fake {
        fn get(&self, program: &str) -> Option<String> {
            self.0.get(program).cloned()
        }

        fn set(&mut self, program: &str, data: &str) -> Result<()> {
            self.0.insert(program.to_owned(), data.to_owned());
            Ok(())
        }

        fn remove(&mut self, program: &str) -> Result<()> {
            self.0.remove(program);
            Ok(())
        }
    }

    const JAVA: &str = r"C:\mc\runtimes\epsilon\bin\javaw.exe";

    #[test]
    fn the_registry_values_are_windows_own() {
        assert_eq!(value(Gpu::PowerSaving), "GpuPreference=1;");
        assert_eq!(value(Gpu::HighPerformance), "GpuPreference=2;");
    }

    #[test]
    fn no_choice_never_touches_a_preference_manycraft_did_not_write() {
        let mut prefs = Fake::default();
        prefs.set(JAVA, "GpuPreference=1;").unwrap();
        let mut ledger = Ledger::default();
        apply_to(&mut prefs, &mut ledger, JAVA, "a", None).unwrap();
        release_from(&mut prefs, &mut ledger, "a").unwrap();
        assert_eq!(prefs.get(JAVA).as_deref(), Some("GpuPreference=1;"));
        assert_eq!(ledger, Ledger::default());
    }

    #[test]
    fn a_choice_is_written_and_reverting_removes_it() {
        let (mut prefs, mut ledger) = (Fake::default(), Ledger::default());
        apply_to(&mut prefs, &mut ledger, JAVA, "a", Some(Gpu::HighPerformance)).unwrap();
        assert_eq!(prefs.get(JAVA).as_deref(), Some("GpuPreference=2;"));
        apply_to(&mut prefs, &mut ledger, JAVA, "a", Some(Gpu::PowerSaving)).unwrap();
        assert_eq!(prefs.get(JAVA).as_deref(), Some("GpuPreference=1;"));
        apply_to(&mut prefs, &mut ledger, JAVA, "a", None).unwrap();
        assert_eq!(prefs.get(JAVA), None);
        assert_eq!(ledger, Ledger::default());
    }

    #[test]
    fn reverting_puts_back_what_the_user_had_set() {
        let (mut prefs, mut ledger) = (Fake::default(), Ledger::default());
        prefs.set(JAVA, "GpuPreference=0;").unwrap();
        apply_to(&mut prefs, &mut ledger, JAVA, "a", Some(Gpu::HighPerformance)).unwrap();
        apply_to(&mut prefs, &mut ledger, JAVA, "a", Some(Gpu::HighPerformance)).unwrap();
        release_from(&mut prefs, &mut ledger, "a").unwrap();
        assert_eq!(prefs.get(JAVA).as_deref(), Some("GpuPreference=0;"));
        assert_eq!(ledger, Ledger::default());
    }

    #[test]
    fn a_shared_launcher_keeps_the_choice_while_another_instance_still_asks() {
        let (mut prefs, mut ledger) = (Fake::default(), Ledger::default());
        apply_to(&mut prefs, &mut ledger, JAVA, "a", Some(Gpu::HighPerformance)).unwrap();
        apply_to(&mut prefs, &mut ledger, JAVA, "b", Some(Gpu::HighPerformance)).unwrap();
        release_from(&mut prefs, &mut ledger, "a").unwrap();
        apply_to(&mut prefs, &mut ledger, JAVA, "c", None).unwrap();
        assert_eq!(prefs.get(JAVA).as_deref(), Some("GpuPreference=2;"));
        apply_to(&mut prefs, &mut ledger, JAVA, "b", None).unwrap();
        assert_eq!(prefs.get(JAVA), None);
    }
}
