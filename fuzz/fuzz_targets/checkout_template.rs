//! `checkout_template::check` reads merchants' templates: any bytes must give a
//! template or problems, never a panic or a hang, and a template with no regions
//! must read again from what it writes, with its features as tokens.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(source) = std::str::from_utf8(data) else {
        return;
    };
    let _ = checkout_template::check(source);
    if let Ok(template) = checkout_template::Template::load(source) {
        for state in checkout_template::all_states() {
            let written = template.render(&state, &mut |part| {
                format!("{{{{{}}}}}", part.feature.name())
            });
            if template.regions() == 0 {
                let again = checkout_template::Template::load(&written).expect("it reads again");
                let rewritten = again.render(&state, &mut |part| {
                    format!("{{{{{}}}}}", part.feature.name())
                });
                assert_eq!(written, rewritten);
            }
        }
    }
});
