//! `checkout_template::check` reads merchants' templates: any bytes must give a
//! template or problems, never a panic or a hang. What a template with no
//! regions writes must read as a template again and write the same, so the
//! writer escapes everything the reader is strict about. Features stand in as
//! `[name]` text, as the crate's tests write them: what the engine writes for
//! one is its own markup, which no template may hold.
//!
//! The one thing written that a template can't read back is `{{`, where two
//! `{` of text meet (`{<!---->{`): a browser reads that as text, and the writer
//! writes it that way on purpose rather than as `&#123;`, since a page must come
//! back from a browser byte for byte (the corpus test).
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(source) = std::str::from_utf8(data) else {
        return;
    };
    let _ = checkout_template::check(source);
    if let Ok(template) = checkout_template::Template::load(source) {
        let mut stub = |part: &checkout_template::Part| format!("[{}]", part.feature.name());
        for state in checkout_template::all_states() {
            let written = template.render(&state, &mut stub);
            if template.regions() == 0 && !written.contains("{{") {
                let again = checkout_template::Template::load(&written).expect("it reads again");
                assert_eq!(again.render(&state, &mut stub), written);
            }
        }
    }
});
