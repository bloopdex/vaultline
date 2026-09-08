#![no_main]
use libfuzzer_sys::fuzz_target;

// The configuration parser is the largest untrusted-input surface (a
// vaultline.toml may be shared, generated, or tampered): parsing and
// validating arbitrary bytes must never panic. The stable-rust mutation
// harness (config::tests::mutated_configurations_never_panic) proves the
// same property deterministically; libFuzzer explores it deeper on
// nightly CI (docs/testing.md).
fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(parsed) = vaultline_core::config::parse_str(text) {
        // Validation is the second half of the surface — aggregate
        // errors, never panic.
        let _ = vaultline_core::config::validate(parsed);
    }
});
