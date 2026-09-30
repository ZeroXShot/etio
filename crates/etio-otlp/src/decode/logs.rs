//! Selective, zero-copy decoding of `ExportLogsServiceRequest`.
//!
//! Log bodies are the bulk of a logs request. They are not copied: a
//! [`LogBatch`] keeps the request buffer alive (a reference-counted
//! [`Bytes`]) and records each body as a range into it.

use std::ops::Range;

use bytes::Bytes;
use etio_core::{Interner, Sym};
use etio_engine::LogEntry;
use etio_pipeline::logs::Severity;

use super::{DecodeError, DecodeStats, any_value, find_resource, key_value};
use crate::wire::{Reader, WireType, offset_in};

/// Log decoding options.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct LogOptions {
    /// Prefix service names with `service.namespace` when present.
    pub service_namespace: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RawLog {
    ts: i64,
    service: Sym,
    body: Range<usize>,
    severity: Option<Severity>,
}

/// Decoded log records borrowing their bodies from the request buffer.
#[derive(Clone, Debug, Default)]
pub struct LogBatch {
    buf: Bytes,
    records: Vec<RawLog>,
}

impl LogBatch {
    /// Number of records.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether the batch is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// The records as engine log entries.
    pub fn entries(&self) -> impl Iterator<Item = LogEntry<'_>> {
        self.records.iter().map(|r| LogEntry {
            ts: r.ts,
            service: r.service,
            body: std::str::from_utf8(&self.buf[r.body.clone()]).unwrap_or(""),
            severity: r.severity,
        })
    }

    /// Builds a batch from owned records (used by the JSON decoder).
    #[must_use]
    pub fn from_owned(records: Vec<(i64, Sym, String, Option<Severity>)>) -> Self {
        let mut text = String::new();
        let mut raw = Vec::with_capacity(records.len());
        for (ts, service, body, severity) in records {
            let start = text.len();
            text.push_str(&body);
            raw.push(RawLog { ts, service, body: start..text.len(), severity });
        }
        Self { buf: Bytes::from(text), records: raw }
    }
}

/// Decodes a protobuf `ExportLogsServiceRequest`.
///
/// Records without a timestamp or with a body that is not text are rejected.
///
/// # Errors
/// Returns [`DecodeError::Wire`] if the encoding is malformed.
pub fn decode(buf: Bytes, interner: &Interner, opts: LogOptions) -> Result<(LogBatch, DecodeStats), DecodeError> {
    let mut stats = DecodeStats::default();
    let mut records = Vec::new();
    {
        let base: &[u8] = &buf;
        let mut r = Reader::new(base);
        while !r.is_empty() {
            let (field, wt) = r.key()?;
            if field != 1 {
                r.skip(wt)?;
                continue;
            }
            let rl = r.expect_bytes(field, wt)?;
            let service = find_resource(rl)?.entity(interner, opts.service_namespace);
            let mut rr = Reader::new(rl);
            while !rr.is_empty() {
                let (f, w) = rr.key()?;
                if f != 2 {
                    rr.skip(w)?;
                    continue;
                }
                let mut sl = Reader::new(rr.expect_bytes(f, w)?);
                while !sl.is_empty() {
                    let (lf, lw) = sl.key()?;
                    if lf == 2 {
                        match record(sl.expect_bytes(lf, lw)?, base, service)? {
                            Some(rec) => {
                                records.push(rec);
                                stats.accepted += 1;
                            }
                            None => stats.rejected += 1,
                        }
                    } else {
                        sl.skip(lw)?;
                    }
                }
            }
        }
    }
    Ok((LogBatch { buf, records }, stats))
}

fn record(buf: &[u8], base: &[u8], service: Sym) -> Result<Option<RawLog>, DecodeError> {
    let (mut time, mut observed, mut severity_number) = (0u64, 0u64, 0u64);
    let mut body: Option<&str> = None;
    let mut r = Reader::new(buf);
    while !r.is_empty() {
        let (field, wt) = r.key()?;
        match (field, wt) {
            (1, WireType::I64) => time = r.fixed64()?,
            (11, WireType::I64) => observed = r.fixed64()?,
            (2, WireType::Varint) => severity_number = r.varint()?,
            (5, WireType::Len) => body = body_text(r.bytes()?)?,
            _ => r.skip(wt)?,
        }
    }
    let ts = if time != 0 { time } else { observed };
    let (Some(body), true) = (body, ts != 0) else { return Ok(None) };
    let Some(range) = offset_in(base, body.as_bytes()) else { return Ok(None) };
    #[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
    Ok(Some(RawLog { ts: ts as i64, service, body: range, severity: Severity::from_otlp(severity_number as i32) }))
}

/// The text of a log body: a string, or the `message`/`msg` entry of a map.
fn body_text(buf: &[u8]) -> Result<Option<&str>, DecodeError> {
    let mut r = Reader::new(buf);
    let mut text = None;
    while !r.is_empty() {
        let (field, wt) = r.key()?;
        match (field, wt) {
            (1, WireType::Len) => text = Some(r.str()?),
            (6, WireType::Len) => {
                let mut kvl = Reader::new(r.bytes()?);
                while !kvl.is_empty() {
                    let (f, w) = kvl.key()?;
                    if f == 1 && w == WireType::Len {
                        let (key, value) = key_value(kvl.bytes()?)?;
                        if matches!(key, "message" | "msg" | "log") {
                            text = any_value(value)?.as_str().or(text);
                        }
                    } else {
                        kvl.skip(w)?;
                    }
                }
            }
            _ => r.skip(wt)?,
        }
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::collector::logs::v1::ExportLogsServiceRequest;
    use crate::proto::common::v1::{AnyValue, KeyValue, KeyValueList, any_value::Value as PValue};
    use crate::proto::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
    use crate::proto::resource::v1::Resource;
    use proptest::prelude::*;
    use prost::Message;

    fn text(s: &str) -> Option<AnyValue> {
        Some(AnyValue { value: Some(PValue::StringValue(s.into())) })
    }

    fn request(records: Vec<LogRecord>) -> Bytes {
        let res = Resource {
            attributes: vec![KeyValue { key: "service.name".into(), value: text("cart"), ..Default::default() }],
            ..Default::default()
        };
        Bytes::from(
            ExportLogsServiceRequest {
                resource_logs: vec![ResourceLogs {
                    resource: Some(res),
                    scope_logs: vec![ScopeLogs { log_records: records, ..Default::default() }],
                    ..Default::default()
                }],
            }
            .encode_to_vec(),
        )
    }

    #[test]
    fn decodes_bodies_without_copying() {
        let i = Interner::default();
        let map = AnyValue {
            value: Some(PValue::KvlistValue(KeyValueList {
                values: vec![
                    KeyValue { key: "level".into(), value: text("info"), ..Default::default() },
                    KeyValue { key: "msg".into(), value: text("structured hello"), ..Default::default() },
                ],
            })),
        };
        let buf = request(vec![
            LogRecord { time_unix_nano: 5, severity_number: 17, body: text("boom"), ..Default::default() },
            LogRecord { observed_time_unix_nano: 6, body: Some(map), ..Default::default() },
            LogRecord { time_unix_nano: 7, body: None, ..Default::default() },
            LogRecord { body: text("no time"), ..Default::default() },
        ]);
        let (batch, stats) = decode(buf, &i, LogOptions::default()).unwrap();
        assert_eq!((stats.accepted, stats.rejected), (2, 2));
        let entries: Vec<_> = batch.entries().collect();
        assert_eq!(entries[0].body, "boom");
        assert_eq!(entries[0].severity, Some(Severity::Error));
        assert_eq!(entries[1].body, "structured hello");
        assert_eq!(entries[1].ts, 6);
        assert_eq!(&*i.resolve(entries[1].service), "cart");
    }

    #[test]
    fn owned_batches_behave_like_decoded_ones() {
        let b =
            LogBatch::from_owned(vec![(1, Sym(3), "a".into(), None), (2, Sym(3), "bc".into(), Some(Severity::Warn))]);
        let e: Vec<_> = b.entries().collect();
        assert_eq!((e[0].body, e[1].body), ("a", "bc"));
    }

    proptest! {
        #[test]
        fn never_panics_on_arbitrary_bytes(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
            let i = Interner::default();
            let _ = decode(Bytes::from(bytes), &i, LogOptions::default());
        }

        #[test]
        fn bodies_round_trip(bodies in prop::collection::vec("\\PC{0,40}", 0..20)) {
            let i = Interner::default();
            let recs = bodies.iter().enumerate().map(|(k, b)| LogRecord { time_unix_nano: k as u64 + 1, body: text(b), ..Default::default() }).collect();
            let (batch, _) = decode(request(recs), &i, LogOptions::default()).unwrap();
            let got: Vec<&str> = batch.entries().map(|e| e.body).collect();
            prop_assert_eq!(got, bodies.iter().map(String::as_str).collect::<Vec<_>>());
        }
    }
}
