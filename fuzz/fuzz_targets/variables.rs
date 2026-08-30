#![no_main]

use libfuzzer_sys::fuzz_target;
use postly_core::VariableContext;

fuzz_target!(|data: &[u8]| {
    let input = String::from_utf8_lossy(data);
    let context = VariableContext::default();
    let _ = context.resolve(&input);
});
