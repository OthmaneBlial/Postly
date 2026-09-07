#![no_main]

use libfuzzer_sys::fuzz_target;
use postly_core::VariableContext;

fuzz_target!(|data: &[u8]| {
    let input = String::from_utf8_lossy(data);
    let mut context = VariableContext::default();
    context.set_runtime("baseUrl", "http://127.0.0.1:3979");
    context.set_runtime("version", "v1");
    context.set_runtime("nested", "{{baseUrl}}/{{version}}");
    context.set_runtime("cycle", "{{cycle}}");
    let _ = context.resolve(&input);
    context.set_runtime("input", input.into_owned());
    let _ = context.resolve("{{input}}");
});
