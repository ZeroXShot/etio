//! Protobuf log export requests: decoding must never panic.
#![no_main]

use etio_core::Interner;
use etio_otlp::decode::logs::{LogOptions, decode};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let interner = Interner::with_capacity(4096);
    if let Ok((batch, _)) = decode(bytes::Bytes::copy_from_slice(data), &interner, LogOptions::default()) {
        for entry in batch.entries() {
            let _ = entry.body.len();
        }
    }
});
