//! The data files this pre-release writes, in the shape the fixtures under `testdata/compat` pin down. The release
//! before it reads the same fixtures back in its own tests, so a change of shape here fails one side or the other.

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::account::Account;
use super::batch::Batch;
use super::instance::Instance;
use super::settings::Settings;
use super::winstate::Saved;

fn written_back<T: DeserializeOwned + Serialize>(name: &str) -> (Value, Value) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/core/testdata/compat").join(name);
    let raw: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let typed: T = serde_json::from_value(raw.clone()).unwrap();
    (serde_json::to_value(&typed).unwrap(), raw)
}

#[test]
fn every_data_file_is_written_exactly_in_the_shape_the_release_before_reads() {
    let pairs = [
        written_back::<Settings>("settings.json"),
        written_back::<Vec<Account>>("accounts.json"),
        written_back::<Instance>("instance.json"),
        written_back::<Instance>("instance-follows-default.json"),
        written_back::<Batch>("batch.json"),
        written_back::<Saved>("window.json"),
        written_back::<HashMap<String, u32>>("measured.json"),
    ];
    for (i, (written, fixture)) in pairs.into_iter().enumerate() {
        assert_eq!(written, fixture, "fixture {i}");
    }
}
