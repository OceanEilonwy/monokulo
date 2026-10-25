//! `dns_client::parse` reads answers from other people's name servers: any
//! bytes must give a message or an error, never a panic or a hang.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = dns_client::parse(data);
});
