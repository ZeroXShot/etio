//! Protobuf metric export requests: decoding must never panic.
#![no_main]

use etio_core::Interner;
use etio_otlp::decode::metrics::{MetricOptions, decode};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let interner = Interner::with_capacity(4096);
    let mut out = Vec::new();
    let _ = decode(data, &interner, MetricOptions::default(), &mut out);
});
