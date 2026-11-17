//! Files a 0.10 beta writes into the shared data folder, read back by this version, so going back from the beta keeps
//! every account, instance, batch and setting.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use super::account::{Account, AccountKind};
use super::batch::Background;
use super::instance::Loader;
use super::options::{Gpu, Renderer, WindowSize};
use super::phone::Service;
use super::settings::{PreviewRate, Settings};
use super::winstate::{Page, Saved};
use super::{batch, instance, store};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/core/testdata/compat").join(name)
}

fn placed(root: &Path, id: &str, file: &str, name: &str) {
    std::fs::create_dir_all(root.join(id)).unwrap();
    std::fs::copy(fixture(name), root.join(id).join(file)).unwrap();
}

#[test]
fn settings_written_by_the_beta_keep_every_setting_this_version_knows() {
    let s = store::load::<Settings>(&fixture("settings.json")).unwrap().unwrap().sanitized();
    assert!(!s.reconnect.enabled && s.reconnect.backoff_secs == [5, 20] && s.reconnect.patient_secs == [120] && s.reconnect.max_attempts == 9);
    assert!(!s.crash_restart.enabled && s.crash_restart.max_attempts == 2);
    assert!(!s.merged_chat && !s.broadcast && !s.notifications.attention && !s.notifications.whispers);
    assert!(s.phone.enabled && s.phone.service == Service::Ntfy && s.phone.telegram_chat == "12345" && s.phone.reconnected && s.phone.whispers);
    assert!(!s.auto_update && s.prereleases && !s.game_badge && !s.prevent_sleep && s.tray_explained && s.swap_explained);
    assert_eq!(s.preview_rate, PreviewRate::Smooth);
}

#[test]
fn accounts_written_by_the_beta_read_whole() {
    let accounts = store::load::<Vec<Account>>(&fixture("accounts.json")).unwrap().unwrap();
    assert_eq!(accounts.len(), 2);
    assert!(accounts[0].id == Uuid::from_u128(7) && accounts[0].kind == AccountKind::Microsoft && accounts[0].proxy.as_deref() == Some("socks5://127.0.0.1:1080") && accounts[0].color == Some(3));
    assert!(accounts[1].name == "AltOne" && accounts[1].kind == AccountKind::Offline && accounts[1].proxy.is_none());
}

#[test]
fn instances_written_by_the_beta_list_with_their_mods_and_options() {
    let root = tempfile::tempdir().unwrap();
    placed(root.path(), "a1b2", instance::FILE, "instance.json");
    placed(root.path(), "c3d4", instance::FILE, "instance-follows-default.json");
    let all = instance::load_all_in(root.path()).unwrap();
    assert_eq!(all.len(), 2);
    let farm = all.iter().find(|i| i.id == "a1b2").unwrap();
    assert!(farm.loader == Loader::Fabric(Some("0.18.1".into())) && farm.mods.len() == 1 && farm.local_mods == ["own.jar"] && farm.disabled == ["own.jar"]);
    assert!(farm.memory_mb == 2048 && farm.jvm_args == ["-XX:+UseG1GC"] && farm.last_account == Some(Uuid::from_u128(8)));
    assert!(farm.options.window == Some(WindowSize { width: 854, height: 480 }) && farm.options.gpu == Some(Gpu::PowerSaving) && farm.options.renderer == Some(Renderer::Vulkan));
    let plain = all.iter().find(|i| i.id == "c3d4").unwrap();
    assert!(plain.loader == Loader::Vanilla && plain.memory_mb == instance::DEFAULT_MEMORY_MB);
}

#[test]
fn batches_written_by_the_beta_list_with_everything_this_version_knows() {
    let root = tempfile::tempdir().unwrap();
    placed(root.path(), "b1", "batch.json", "batch.json");
    std::fs::create_dir_all(root.path().join(".trash").join("b2-1000")).unwrap();
    std::fs::copy(fixture("batch.json"), root.path().join(".trash").join("b2-1000").join("batch.json")).unwrap();
    let all = batch::load_all_in(root.path()).unwrap();
    assert_eq!(all.len(), 1, "the beta's Papierkorb is no batch here");
    let b = &all[0];
    assert!(b.id == "b1" && b.instance_id == "a1b2" && b.server.as_deref() == Some("127.0.0.1:25565") && b.accounts.len() == 2);
    assert!(b.join_commands == ["/home afk"] && b.join_delay_ms == 1500 && b.memory_override == Some(3072) && b.launch_parallel == 2 && b.full_speed);
    assert_eq!(b.background, Background::Custom { fps: 10, sight: 4 });
}

#[test]
fn window_and_measured_memory_written_by_the_beta_read_whole() {
    let saved: Saved = serde_json::from_slice(&std::fs::read(fixture("window.json")).unwrap()).unwrap();
    assert!(saved.maximized && saved.page == Page::Instance && saved.batch.as_deref() == Some("a1b2") && !saved.sidebar && saved.frame.is_some());
    let measured: HashMap<String, u32> = serde_json::from_slice(&std::fs::read(fixture("measured.json")).unwrap()).unwrap();
    assert_eq!((measured.get("b1"), measured.get("b1+full")), (Some(&1400), Some(&1900)));
}
