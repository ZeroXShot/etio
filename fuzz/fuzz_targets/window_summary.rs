//! Window summaries as received by a core from an edge (postcard): decoding
//! must never panic, and merging a decoded summary must not either.
#![no_main]

use etio_engine::WindowSummary;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = postcard::from_bytes::<WindowSummary>(data) {
        let mut acc = WindowSummary::empty(s.window);
        acc.merge(&s);
        acc.merge(&s);
    }
});
