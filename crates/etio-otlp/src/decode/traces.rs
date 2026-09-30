//! Selective decoding of `ExportTraceServiceRequest`.

use etio_core::{Interner, Sym};
use etio_pipeline::{Span, SpanKind, SpanStatus};

use super::{DecodeError, DecodeStats, any_value, find_resource, key_value};
use crate::semconv::{self, PEER_KEYS};
use crate::wire::{Reader, WireType};

/// Trace decoding options.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct TraceOptions {
    /// Prefix service names with `service.namespace` when present.
    pub service_namespace: bool,
}

/// Decodes a protobuf `ExportTraceServiceRequest`, appending spans to `out`.
///
/// Spans without a valid 16-byte trace id or 8-byte span id are rejected
/// and counted; malformed encodings fail the whole request.
///
/// # Errors
/// Returns [`DecodeError::Wire`] if the encoding is malformed.
pub fn decode(
    buf: &[u8],
    interner: &Interner,
    opts: TraceOptions,
    out: &mut Vec<Span>,
) -> Result<DecodeStats, DecodeError> {
    let mut stats = DecodeStats::default();
    let mut r = Reader::new(buf);
    while !r.is_empty() {
        let (field, wt) = r.key()?;
        if field == 1 {
            resource_spans(r.expect_bytes(field, wt)?, interner, opts, out, &mut stats)?;
        } else {
            r.skip(wt)?;
        }
    }
    Ok(stats)
}

fn resource_spans(
    buf: &[u8],
    interner: &Interner,
    opts: TraceOptions,
    out: &mut Vec<Span>,
    stats: &mut DecodeStats,
) -> Result<(), DecodeError> {
    let service = find_resource(buf)?.entity(interner, opts.service_namespace);
    let mut r = Reader::new(buf);
    while !r.is_empty() {
        let (field, wt) = r.key()?;
        if field == 2 {
            let scope = r.expect_bytes(field, wt)?;
            let mut s = Reader::new(scope);
            while !s.is_empty() {
                let (f, w) = s.key()?;
                if f == 2 {
                    span(s.expect_bytes(f, w)?, service, interner, out, stats)?;
                } else {
                    s.skip(w)?;
                }
            }
        } else {
            r.skip(wt)?;
        }
    }
    Ok(())
}

fn id_bytes<const N: usize>(b: &[u8]) -> Option<[u8; N]> {
    <[u8; N]>::try_from(b).ok()
}

/// Whether a gRPC status code on a server span indicates a server-side fault
/// (OpenTelemetry RPC conventions).
const fn grpc_server_error(code: i64) -> bool {
    matches!(code, 2 | 4 | 12 | 13 | 14 | 15)
}

fn span(
    buf: &[u8],
    service: Sym,
    interner: &Interner,
    out: &mut Vec<Span>,
    stats: &mut DecodeStats,
) -> Result<(), DecodeError> {
    let mut trace_id: Option<u128> = None;
    let mut span_id: Option<u64> = None;
    let mut parent_id = 0u64;
    let mut name = "";
    let mut kind = 0i32;
    let (mut start, mut end) = (0u64, 0u64);
    let mut status_code = 0u64;
    let mut peers: [Option<&str>; PEER_KEYS.len()] = [None; PEER_KEYS.len()];
    // The current and the older spelling of the HTTP status; the current one wins.
    let mut http_status: Option<i64> = None;
    let mut http_status_old: Option<i64> = None;
    let mut grpc_status: Option<i64> = None;

    let mut r = Reader::new(buf);
    while !r.is_empty() {
        let (field, wt) = r.key()?;
        match field {
            1 => trace_id = id_bytes::<16>(r.expect_bytes(field, wt)?).map(u128::from_be_bytes),
            2 => span_id = id_bytes::<8>(r.expect_bytes(field, wt)?).map(u64::from_be_bytes),
            4 => parent_id = id_bytes::<8>(r.expect_bytes(field, wt)?).map_or(0, u64::from_be_bytes),
            5 => name = std::str::from_utf8(r.expect_bytes(field, wt)?).unwrap_or(""),
            #[allow(clippy::cast_possible_truncation)]
            6 => kind = r.expect_varint(field, wt)? as i32,
            7 => start = r.expect_fixed64(field, wt)?,
            8 => end = r.expect_fixed64(field, wt)?,
            9 => {
                let (key, value) = key_value(r.expect_bytes(field, wt)?)?;
                if let Some(i) = PEER_KEYS.iter().position(|k| *k == key) {
                    peers[i] = any_value(value)?.as_str().filter(|s| !s.is_empty());
                } else if key == semconv::HTTP_RESPONSE_STATUS_CODE {
                    http_status = any_value(value)?.as_int();
                } else if key == semconv::HTTP_STATUS_CODE {
                    http_status_old = any_value(value)?.as_int();
                } else if key == semconv::RPC_GRPC_STATUS_CODE {
                    grpc_status = any_value(value)?.as_int();
                }
            }
            15 => {
                let mut s = Reader::new(r.expect_bytes(field, wt)?);
                while !s.is_empty() {
                    let (f, w) = s.key()?;
                    if f == 3 && w == WireType::Varint {
                        status_code = s.varint()?;
                    } else {
                        s.skip(w)?;
                    }
                }
            }
            _ => r.skip(wt)?,
        }
    }

    let (Some(trace_id), Some(span_id)) = (trace_id.filter(|t| *t != 0), span_id.filter(|s| *s != 0)) else {
        stats.rejected += 1;
        return Ok(());
    };
    let kind = SpanKind::from_otlp(kind);
    let http_status = http_status.or(http_status_old);
    let status = match status_code {
        2 => SpanStatus::Error,
        1 => SpanStatus::Ok,
        _ => {
            // Instrumentations do not always set the status; derive it from
            // protocol codes as the semantic conventions prescribe.
            let http_error = http_status.is_some_and(|c| match kind {
                SpanKind::Client => c >= 400,
                _ => c >= 500,
            });
            let grpc_error = grpc_status.is_some_and(|c| match kind {
                SpanKind::Client => c != 0,
                _ => grpc_server_error(c),
            });
            if http_error || grpc_error { SpanStatus::Error } else { SpanStatus::Unset }
        }
    };
    let peer = if kind.is_outbound() {
        peers.iter().flatten().next().map_or(Sym::EMPTY, |p| interner.intern(p))
    } else {
        Sym::EMPTY
    };
    #[allow(clippy::cast_possible_wrap)]
    out.push(Span {
        trace_id,
        span_id,
        parent_id,
        service,
        operation: interner.intern(name),
        kind,
        start: start as i64,
        end: end as i64,
        status,
        peer,
    });
    stats.accepted += 1;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::collector::trace::v1::ExportTraceServiceRequest;
    use crate::proto::common::v1::{AnyValue, KeyValue, any_value::Value as PValue};
    use crate::proto::resource::v1::Resource;
    use crate::proto::trace::v1::{ResourceSpans, ScopeSpans, Span as PSpan, Status, span};
    use proptest::prelude::*;
    use prost::Message;

    fn kv(k: &str, v: PValue) -> KeyValue {
        KeyValue { key: k.into(), value: Some(AnyValue { value: Some(v) }), ..Default::default() }
    }

    /// Reference semantics, written directly against the prost types.
    fn reference(req: &ExportTraceServiceRequest, interner: &Interner) -> (Vec<Span>, DecodeStats) {
        let mut out = Vec::new();
        let mut stats = DecodeStats::default();
        for rs in &req.resource_spans {
            let attr =
                |k: &str| {
                    rs.resource.as_ref().and_then(|r| r.attributes.iter().rev().find(|a| a.key == k)).and_then(|a| {
                        match a.value.as_ref()?.value.as_ref()? {
                            PValue::StringValue(s) if !s.is_empty() => Some(s.as_str()),
                            _ => None,
                        }
                    })
                };
            let service = super::super::ResourceInfo {
                service_name: attr("service.name"),
                service_namespace: attr("service.namespace"),
                workload: attr("k8s.deployment.name"),
                host: attr("host.name"),
            }
            .entity(interner, false);
            for ss in &rs.scope_spans {
                for s in &ss.spans {
                    let tid =
                        <[u8; 16]>::try_from(s.trace_id.as_slice()).ok().map(u128::from_be_bytes).filter(|t| *t != 0);
                    let sid =
                        <[u8; 8]>::try_from(s.span_id.as_slice()).ok().map(u64::from_be_bytes).filter(|t| *t != 0);
                    let (Some(tid), Some(sid)) = (tid, sid) else {
                        stats.rejected += 1;
                        continue;
                    };
                    let kind = SpanKind::from_otlp(s.kind);
                    let last =
                        |k: &str| s.attributes.iter().rev().find(|a| a.key == k).and_then(|a| a.value.clone()?.value);
                    let as_int = |v: Option<PValue>| match v {
                        Some(PValue::IntValue(i)) => Some(i),
                        Some(PValue::StringValue(s)) => s.trim().parse().ok(),
                        _ => None,
                    };
                    let http = as_int(last("http.response.status_code")).or(as_int(last("http.status_code")));
                    let grpc = as_int(last("rpc.grpc.status_code"));
                    let code = s.status.as_ref().map_or(0, |st| st.code);
                    let status = match code {
                        2 => SpanStatus::Error,
                        1 => SpanStatus::Ok,
                        _ if http.is_some_and(|c| if kind == SpanKind::Client { c >= 400 } else { c >= 500 })
                            || grpc.is_some_and(|c| {
                                if kind == SpanKind::Client { c != 0 } else { grpc_server_error(c) }
                            }) =>
                        {
                            SpanStatus::Error
                        }
                        _ => SpanStatus::Unset,
                    };
                    let peer = if kind.is_outbound() {
                        PEER_KEYS
                            .iter()
                            .find_map(|k| match last(k) {
                                Some(PValue::StringValue(v)) if !v.is_empty() => Some(v),
                                _ => None,
                            })
                            .map_or(Sym::EMPTY, |p| interner.intern(&p))
                    } else {
                        Sym::EMPTY
                    };
                    let parent = <[u8; 8]>::try_from(s.parent_span_id.as_slice()).map_or(0, u64::from_be_bytes);
                    #[allow(clippy::cast_possible_wrap)]
                    out.push(Span {
                        trace_id: tid,
                        span_id: sid,
                        parent_id: parent,
                        service,
                        operation: interner.intern(&s.name),
                        kind,
                        start: s.start_time_unix_nano as i64,
                        end: s.end_time_unix_nano as i64,
                        status,
                        peer,
                    });
                    stats.accepted += 1;
                }
            }
        }
        (out, stats)
    }

    fn arb_value() -> impl Strategy<Value = PValue> {
        prop_oneof![
            "[a-z0-9.]{0,8}".prop_map(PValue::StringValue),
            (-600i64..600).prop_map(PValue::IntValue),
            any::<bool>().prop_map(PValue::BoolValue),
            (-1e3f64..1e3).prop_map(PValue::DoubleValue),
            prop::collection::vec(any::<u8>(), 0..4).prop_map(PValue::BytesValue),
        ]
    }

    fn arb_attr() -> impl Strategy<Value = KeyValue> {
        let keys = prop_oneof![
            Just("peer.service".to_owned()),
            Just("db.system".to_owned()),
            Just("server.address".to_owned()),
            Just("http.response.status_code".to_owned()),
            Just("http.status_code".to_owned()),
            Just("rpc.grpc.status_code".to_owned()),
            "[a-z.]{1,10}",
        ];
        (keys, arb_value()).prop_map(|(k, v)| kv(&k, v))
    }

    fn arb_span() -> impl Strategy<Value = PSpan> {
        (
            prop_oneof![prop::collection::vec(any::<u8>(), 16), prop::collection::vec(any::<u8>(), 0..20)],
            prop_oneof![
                prop::collection::vec(any::<u8>(), 8),
                prop::collection::vec(any::<u8>(), 0..10),
                Just(vec![0u8; 8])
            ],
            prop_oneof![Just(vec![]), prop::collection::vec(any::<u8>(), 8)],
            "[a-zA-Z /]{0,12}",
            0i32..7,
            any::<u64>(),
            any::<u64>(),
            prop::collection::vec(arb_attr(), 0..6),
            prop::option::of(0i32..4),
        )
            .prop_map(|(tid, sid, parent, name, kind, start, end, attributes, code)| PSpan {
                trace_id: tid,
                span_id: sid,
                parent_span_id: parent,
                name,
                kind,
                start_time_unix_nano: start,
                end_time_unix_nano: end,
                attributes,
                status: code.map(|c| Status { code: c, ..Default::default() }),
                events: vec![span::Event { name: "exception".into(), ..Default::default() }],
                ..Default::default()
            })
    }

    fn arb_request() -> impl Strategy<Value = ExportTraceServiceRequest> {
        let resource = (
            prop::option::of(prop_oneof![Just("cart".to_owned()), Just("unknown_service:x".to_owned()), "[a-z]{1,6}"]),
            prop::option::of("[a-z]{1,6}"),
        )
            .prop_map(|(svc, host)| {
                let mut attributes = Vec::new();
                if let Some(s) = svc {
                    attributes.push(kv("service.name", PValue::StringValue(s)));
                }
                if let Some(h) = host {
                    attributes.push(kv("host.name", PValue::StringValue(h)));
                }
                Resource { attributes, ..Default::default() }
            });
        prop::collection::vec(
            (prop::option::of(resource), prop::collection::vec(prop::collection::vec(arb_span(), 0..8), 0..3)),
            0..4,
        )
        .prop_map(|rss| ExportTraceServiceRequest {
            resource_spans: rss
                .into_iter()
                .map(|(resource, scopes)| ResourceSpans {
                    resource,
                    scope_spans: scopes.into_iter().map(|spans| ScopeSpans { spans, ..Default::default() }).collect(),
                    ..Default::default()
                })
                .collect(),
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]
        #[test]
        fn matches_reference_decoding(req in arb_request()) {
            let interner = Interner::default();
            let buf = req.encode_to_vec();
            let mut got = Vec::new();
            let stats = decode(&buf, &interner, TraceOptions::default(), &mut got).unwrap();
            let (want, want_stats) = reference(&req, &interner);
            prop_assert_eq!(stats, want_stats);
            prop_assert_eq!(got, want);
        }

        #[test]
        fn never_panics_on_arbitrary_bytes(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
            let interner = Interner::default();
            let mut out = Vec::new();
            let _ = decode(&bytes, &interner, TraceOptions::default(), &mut out);
        }
    }

    #[test]
    fn resource_may_follow_its_spans() {
        let interner = Interner::default();
        let span = PSpan { trace_id: vec![1; 16], span_id: vec![2; 8], name: "op".into(), ..Default::default() };
        let scope = ScopeSpans { spans: vec![span], ..Default::default() };
        let resource =
            Resource { attributes: vec![kv("service.name", PValue::StringValue("cart".into()))], ..Default::default() };
        // Hand-encode ResourceSpans with field 2 before field 1.
        let mut rs = Vec::new();
        prost::encoding::message::encode(2, &scope, &mut rs);
        prost::encoding::message::encode(1, &resource, &mut rs);
        let mut req = Vec::new();
        prost::encoding::bytes::encode(1, &rs, &mut req);
        let mut out = Vec::new();
        decode(&req, &interner, TraceOptions::default(), &mut out).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(&*interner.resolve(out[0].service), "cart");
    }

    #[test]
    fn derives_errors_from_protocol_codes() {
        let interner = Interner::default();
        let mk = |kind: i32, attrs: Vec<KeyValue>| PSpan {
            trace_id: vec![1; 16],
            span_id: vec![2; 8],
            kind,
            attributes: attrs,
            ..Default::default()
        };
        let req = ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                scope_spans: vec![ScopeSpans {
                    spans: vec![
                        mk(2, vec![kv("http.response.status_code", PValue::IntValue(503))]),
                        mk(2, vec![kv("http.response.status_code", PValue::IntValue(404))]),
                        mk(3, vec![kv("http.response.status_code", PValue::IntValue(404))]),
                        mk(3, vec![kv("peer.service", PValue::StringValue("redis".into()))]),
                        mk(2, vec![kv("rpc.grpc.status_code", PValue::IntValue(14))]),
                    ],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };
        let mut out = Vec::new();
        decode(&req.encode_to_vec(), &interner, TraceOptions::default(), &mut out).unwrap();
        let statuses: Vec<SpanStatus> = out.iter().map(|s| s.status).collect();
        assert_eq!(
            statuses,
            vec![SpanStatus::Error, SpanStatus::Unset, SpanStatus::Error, SpanStatus::Unset, SpanStatus::Error]
        );
        assert_eq!(&*interner.resolve(out[3].peer), "redis");
    }
}
