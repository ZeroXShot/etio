//! OTLP/JSON documents of all three signals: decoding must never panic.
#![no_main]

use etio_core::Interner;
use etio_otlp::decode::logs::LogOptions;
use etio_otlp::decode::metrics::MetricOptions;
use etio_otlp::decode::traces::TraceOptions;
use etio_otlp::json;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let interner = Interner::with_capacity(4096);
    let _ = json::decode_traces(data, &interner, TraceOptions::default(), &mut Vec::new());
    let _ = json::decode_metrics(data, &interner, MetricOptions::default(), &mut Vec::new());
    let _ = json::decode_logs(data, &interner, LogOptions::default());
});
