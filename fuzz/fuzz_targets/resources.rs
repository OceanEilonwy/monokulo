#![no_main]
libfuzzer_sys::fuzz_target!(|values: [u64; 8]| {
    engine::exploration::resources(values);
});
