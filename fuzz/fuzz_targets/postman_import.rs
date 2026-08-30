#![no_main]

use std::{fs, path::PathBuf};

use libfuzzer_sys::fuzz_target;
use postly_core::import_postman_collection;

fuzz_target!(|data: &[u8]| {
    let Ok(directory) = tempfile::tempdir() else {
        return;
    };
    let input = directory.path().join("input.json");
    let output = directory.path().join("workspace");
    if fs::write(&input, data).is_ok() {
        let _ = import_postman_collection(PathBuf::from(&input), PathBuf::from(&output));
    }
});
