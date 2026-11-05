//! `checkout_template::css::sanitize` reads merchants' CSS: any bytes must give
//! CSS or problems, never a panic or a hang, and what it writes must stay inside
//! the page and load nothing, as a browser would read it.
#![no_main]

use libfuzzer_sys::fuzz_target;
use std::collections::BTreeSet;

fuzz_target!(|data: &[u8]| {
    let Ok(css) = std::str::from_utf8(data) else {
        return;
    };
    let classes: BTreeSet<String> = ["pay".to_string()].into();
    if let Ok(written) = checkout_template::css::sanitize(css, &classes) {
        assert!(checkout_template::css::written_is_safe(&written), "{written}");
    }
});
