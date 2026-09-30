//! Selective decoding of `ExportMetricsServiceRequest`.
//!
//! Every data point becomes one or more [`MetricPoint`]s attributed to the
//! resource's entity:
//!
//! | OTLP type | points produced |
//! |---|---|
//! | Gauge | `name` (gauge) |
//! | Sum, monotonic | `name` (delta or cumulative) |
//! | Sum, non-monotonic | `name` (gauge: an up-down counter is a level) |
//! | Histogram, exponential histogram | `name.count`, `name.sum` (delta or cumulative) |
//! | Summary | `name.count`, `name.sum` (cumulative), `name.pNN` (gauge) per quantile |
//!
//! Data points of the same metric with different attributes are different
//! *streams* (tracked separately for cumulative counters) but land in the
//! same series: root-cause analysis works at the level of the entity.

use etio_core::{Interner, Sym};
use etio_engine::{MetricKind, MetricPoint};

use super::{AttrHash, DecodeError, DecodeStats, find_resource};
use crate::wire::{Reader, WireError, WireType};

/// Metric decoding options.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct MetricOptions {
    /// Prefix service names with `service.namespace` when present.
    pub service_namespace: bool,
}

/// Decodes a protobuf `ExportMetricsServiceRequest`, appending points to `out`.
///
/// # Errors
/// Returns [`DecodeError::Wire`] if the encoding is malformed.
pub fn decode(
    buf: &[u8],
    interner: &Interner,
    opts: MetricOptions,
    out: &mut Vec<MetricPoint>,
) -> Result<DecodeStats, DecodeError> {
    let mut stats = DecodeStats::default();
    let mut r = Reader::new(buf);
    while !r.is_empty() {
        let (field, wt) = r.key()?;
        if field == 1 {
            resource_metrics(r.expect_bytes(field, wt)?, interner, opts, out, &mut stats)?;
        } else {
            r.skip(wt)?;
        }
    }
    Ok(stats)
}

fn resource_metrics(
    buf: &[u8],
    interner: &Interner,
    opts: MetricOptions,
    out: &mut Vec<MetricPoint>,
    stats: &mut DecodeStats,
) -> Result<(), DecodeError> {
    let service = find_resource(buf)?.entity(interner, opts.service_namespace);
    let service_name = interner.resolve(service);
    let mut r = Reader::new(buf);
    while !r.is_empty() {
        let (field, wt) = r.key()?;
        if field != 2 {
            r.skip(wt)?;
            continue;
        }
        let mut s = Reader::new(r.expect_bytes(field, wt)?);
        while !s.is_empty() {
            let (f, w) = s.key()?;
            if f == 2 {
                let ctx = Ctx { service, service_name: &service_name, interner };
                metric(s.expect_bytes(f, w)?, &ctx, out, stats)?;
            } else {
                s.skip(w)?;
            }
        }
    }
    Ok(())
}

struct Ctx<'a> {
    service: Sym,
    service_name: &'a str,
    interner: &'a Interner,
}

impl Ctx<'_> {
    fn stream(&self, attrs: AttrHash, name: &str) -> u64 {
        attrs.finish(&[self.service_name.as_bytes(), name.as_bytes()])
    }
}

/// Temporality of a sum or histogram.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Temporality {
    Unspecified,
    Delta,
    Cumulative,
}

fn metric(buf: &[u8], ctx: &Ctx<'_>, out: &mut Vec<MetricPoint>, stats: &mut DecodeStats) -> Result<(), DecodeError> {
    // First pass: the name and the data (a oneof: the last one wins).
    let mut name = "";
    let mut data: Option<(u32, &[u8])> = None;
    let mut r = Reader::new(buf);
    while !r.is_empty() {
        let (field, wt) = r.key()?;
        match field {
            1 => name = std::str::from_utf8(r.expect_bytes(field, wt)?).map_err(|_| WireError::Utf8)?,
            5 | 7 | 9 | 10 | 11 => data = Some((field, r.expect_bytes(field, wt)?)),
            _ => r.skip(wt)?,
        }
    }
    let Some((kind, body)) = data else { return Ok(()) };
    if name.is_empty() {
        stats.rejected += 1;
        return Ok(());
    }
    match kind {
        5 => number_points(body, name, ctx, |_| Some(MetricKind::Gauge), out, stats),
        7 => {
            let (temporality, monotonic) = sum_header(body)?;
            number_points(
                body,
                name,
                ctx,
                move |start| match (monotonic, temporality) {
                    (false, _) => Some(MetricKind::Gauge),
                    (true, Temporality::Delta) => Some(MetricKind::Delta),
                    (true, Temporality::Cumulative | Temporality::Unspecified) => {
                        Some(MetricKind::Cumulative { start })
                    }
                },
                out,
                stats,
            )
        }
        9 | 10 => histogram_points(body, kind == 10, name, ctx, out, stats),
        11 => summary_points(body, name, ctx, out, stats),
        _ => Ok(()),
    }
}

/// Reads the temporality (field 2) and monotonicity (field 3) of a sum; also
/// used for histograms, which have no monotonic flag (they are monotonic).
fn sum_header(buf: &[u8]) -> Result<(Temporality, bool), DecodeError> {
    let mut temporality = Temporality::Unspecified;
    let mut monotonic = false;
    let mut r = Reader::new(buf);
    while !r.is_empty() {
        let (field, wt) = r.key()?;
        match (field, wt) {
            (2, WireType::Varint) => {
                temporality = match r.varint()? {
                    1 => Temporality::Delta,
                    2 => Temporality::Cumulative,
                    _ => Temporality::Unspecified,
                };
            }
            (3, WireType::Varint) => monotonic = r.varint()? != 0,
            _ => r.skip(wt)?,
        }
    }
    Ok((temporality, monotonic))
}

const FLAG_NO_RECORDED_VALUE: u64 = 1;

fn number_points(
    buf: &[u8],
    name: &str,
    ctx: &Ctx<'_>,
    kind_of: impl Fn(i64) -> Option<MetricKind>,
    out: &mut Vec<MetricPoint>,
    stats: &mut DecodeStats,
) -> Result<(), DecodeError> {
    let name_sym = ctx.interner.intern(name);
    let mut r = Reader::new(buf);
    while !r.is_empty() {
        let (field, wt) = r.key()?;
        if field != 1 {
            r.skip(wt)?;
            continue;
        }
        let mut p = Reader::new(r.expect_bytes(field, wt)?);
        let (mut start, mut time, mut value, mut flags) = (0u64, 0u64, None::<f64>, 0u64);
        let mut attrs = AttrHash::default();
        while !p.is_empty() {
            let (f, w) = p.key()?;
            match (f, w) {
                (2, WireType::I64) => start = p.fixed64()?,
                (3, WireType::I64) => time = p.fixed64()?,
                (4, WireType::I64) => value = Some(f64::from_bits(p.fixed64()?)),
                #[allow(clippy::cast_possible_wrap, clippy::cast_precision_loss)]
                (6, WireType::I64) => value = Some(p.fixed64()? as i64 as f64),
                (7, WireType::Len) => attrs.add(p.bytes()?),
                (8, WireType::Varint) => flags = p.varint()?,
                _ => p.skip(w)?,
            }
        }
        #[allow(clippy::cast_possible_wrap)]
        let (start, time) = (start as i64, time as i64);
        match (value, kind_of(start)) {
            (Some(v), Some(kind)) if time > 0 && v.is_finite() && flags & FLAG_NO_RECORDED_VALUE == 0 => {
                out.push(MetricPoint {
                    service: ctx.service,
                    name: name_sym,
                    stream: ctx.stream(attrs, name),
                    ts: time,
                    value: v,
                    kind,
                });
                stats.accepted += 1;
            }
            _ => stats.rejected += 1,
        }
    }
    Ok(())
}

fn histogram_points(
    buf: &[u8],
    exponential: bool,
    name: &str,
    ctx: &Ctx<'_>,
    out: &mut Vec<MetricPoint>,
    stats: &mut DecodeStats,
) -> Result<(), DecodeError> {
    let (temporality, _) = sum_header(buf)?;
    let count_name = format!("{name}.count");
    let sum_name = format!("{name}.sum");
    let (count_sym, sum_sym) = (ctx.interner.intern(&count_name), ctx.interner.intern(&sum_name));
    let (attrs_field, flags_field) = if exponential { (1, 10) } else { (9, 10) };
    let mut r = Reader::new(buf);
    while !r.is_empty() {
        let (field, wt) = r.key()?;
        if field != 1 {
            r.skip(wt)?;
            continue;
        }
        let mut p = Reader::new(r.expect_bytes(field, wt)?);
        let (mut start, mut time, mut count, mut sum, mut flags) = (0u64, 0u64, None::<u64>, None::<f64>, 0u64);
        let mut attrs = AttrHash::default();
        while !p.is_empty() {
            let (f, w) = p.key()?;
            match (f, w) {
                (2, WireType::I64) => start = p.fixed64()?,
                (3, WireType::I64) => time = p.fixed64()?,
                (4, WireType::I64) => count = Some(p.fixed64()?),
                (5, WireType::I64) => sum = Some(f64::from_bits(p.fixed64()?)),
                (f, WireType::Len) if f == attrs_field => attrs.add(p.bytes()?),
                (f, WireType::Varint) if f == flags_field => flags = p.varint()?,
                _ => p.skip(w)?,
            }
        }
        #[allow(clippy::cast_possible_wrap)]
        let (start, time) = (start as i64, time as i64);
        let kind = match temporality {
            Temporality::Delta => MetricKind::Delta,
            Temporality::Cumulative | Temporality::Unspecified => MetricKind::Cumulative { start },
        };
        let Some(count) = count.filter(|_| time > 0 && flags & FLAG_NO_RECORDED_VALUE == 0) else {
            stats.rejected += 1;
            continue;
        };
        #[allow(clippy::cast_precision_loss)]
        out.push(MetricPoint {
            service: ctx.service,
            name: count_sym,
            stream: ctx.stream(attrs, &count_name),
            ts: time,
            value: count as f64,
            kind,
        });
        if let Some(sum) = sum.filter(|s| s.is_finite()) {
            out.push(MetricPoint {
                service: ctx.service,
                name: sum_sym,
                stream: ctx.stream(attrs, &sum_name),
                ts: time,
                value: sum,
                kind,
            });
        }
        stats.accepted += 1;
    }
    Ok(())
}

fn summary_points(
    buf: &[u8],
    name: &str,
    ctx: &Ctx<'_>,
    out: &mut Vec<MetricPoint>,
    stats: &mut DecodeStats,
) -> Result<(), DecodeError> {
    let count_name = format!("{name}.count");
    let sum_name = format!("{name}.sum");
    let (count_sym, sum_sym) = (ctx.interner.intern(&count_name), ctx.interner.intern(&sum_name));
    let mut r = Reader::new(buf);
    while !r.is_empty() {
        let (field, wt) = r.key()?;
        if field != 1 {
            r.skip(wt)?;
            continue;
        }
        let mut p = Reader::new(r.expect_bytes(field, wt)?);
        let (mut start, mut time, mut count, mut sum, mut flags) = (0u64, 0u64, 0u64, 0f64, 0u64);
        let mut attrs = AttrHash::default();
        let mut quantiles: Vec<(f64, f64)> = Vec::new();
        while !p.is_empty() {
            let (f, w) = p.key()?;
            match (f, w) {
                (2, WireType::I64) => start = p.fixed64()?,
                (3, WireType::I64) => time = p.fixed64()?,
                (4, WireType::I64) => count = p.fixed64()?,
                (5, WireType::I64) => sum = f64::from_bits(p.fixed64()?),
                (6, WireType::Len) => {
                    let mut q = Reader::new(p.bytes()?);
                    let (mut quantile, mut value) = (0f64, f64::NAN);
                    while !q.is_empty() {
                        let (qf, qw) = q.key()?;
                        match (qf, qw) {
                            (1, WireType::I64) => quantile = f64::from_bits(q.fixed64()?),
                            (2, WireType::I64) => value = f64::from_bits(q.fixed64()?),
                            _ => q.skip(qw)?,
                        }
                    }
                    quantiles.push((quantile, value));
                }
                (7, WireType::Len) => attrs.add(p.bytes()?),
                (8, WireType::Varint) => flags = p.varint()?,
                _ => p.skip(w)?,
            }
        }
        #[allow(clippy::cast_possible_wrap)]
        let (start, time) = (start as i64, time as i64);
        if time <= 0 || flags & FLAG_NO_RECORDED_VALUE != 0 {
            stats.rejected += 1;
            continue;
        }
        let cumulative = MetricKind::Cumulative { start };
        #[allow(clippy::cast_precision_loss)]
        out.push(MetricPoint {
            service: ctx.service,
            name: count_sym,
            stream: ctx.stream(attrs, &count_name),
            ts: time,
            value: count as f64,
            kind: cumulative,
        });
        if sum.is_finite() {
            out.push(MetricPoint {
                service: ctx.service,
                name: sum_sym,
                stream: ctx.stream(attrs, &sum_name),
                ts: time,
                value: sum,
                kind: cumulative,
            });
        }
        for (q, v) in quantiles {
            if (0.0..=1.0).contains(&q) && v.is_finite() {
                let qname = format!("{name}.p{}", (q * 100.0).round());
                out.push(MetricPoint {
                    service: ctx.service,
                    name: ctx.interner.intern(&qname),
                    stream: ctx.stream(attrs, &qname),
                    ts: time,
                    value: v,
                    kind: MetricKind::Gauge,
                });
            }
        }
        stats.accepted += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::collector::metrics::v1::ExportMetricsServiceRequest;
    use crate::proto::common::v1::{AnyValue, KeyValue, any_value::Value as PValue};
    use crate::proto::metrics::v1::{
        Gauge, Histogram, HistogramDataPoint, Metric, NumberDataPoint, ResourceMetrics, ScopeMetrics, Sum, Summary,
        SummaryDataPoint, metric::Data, number_data_point, summary_data_point::ValueAtQuantile,
    };
    use crate::proto::resource::v1::Resource;
    use proptest::prelude::*;
    use prost::Message;

    fn kv(k: &str, v: &str) -> KeyValue {
        KeyValue {
            key: k.into(),
            value: Some(AnyValue { value: Some(PValue::StringValue(v.into())) }),
            ..Default::default()
        }
    }

    fn request(metrics: Vec<Metric>) -> Vec<u8> {
        ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                resource: Some(Resource { attributes: vec![kv("service.name", "cart")], ..Default::default() }),
                scope_metrics: vec![ScopeMetrics { metrics, ..Default::default() }],
                ..Default::default()
            }],
        }
        .encode_to_vec()
    }

    fn ndp(t: u64, v: f64, attrs: Vec<KeyValue>) -> NumberDataPoint {
        NumberDataPoint {
            time_unix_nano: t,
            start_time_unix_nano: 5,
            value: Some(number_data_point::Value::AsDouble(v)),
            attributes: attrs,
            ..Default::default()
        }
    }

    fn decode_all(buf: &[u8], i: &Interner) -> (Vec<MetricPoint>, DecodeStats) {
        let mut out = Vec::new();
        let stats = decode(buf, i, MetricOptions::default(), &mut out).unwrap();
        (out, stats)
    }

    #[test]
    fn gauges_sums_and_up_down_counters() {
        let i = Interner::default();
        let buf = request(vec![
            Metric {
                name: "mem".into(),
                data: Some(Data::Gauge(Gauge { data_points: vec![ndp(10, 1.5, vec![])] })),
                ..Default::default()
            },
            Metric {
                name: "requests".into(),
                data: Some(Data::Sum(Sum {
                    data_points: vec![ndp(10, 7.0, vec![])],
                    aggregation_temporality: 2,
                    is_monotonic: true,
                })),
                ..Default::default()
            },
            Metric {
                name: "inflight".into(),
                data: Some(Data::Sum(Sum {
                    data_points: vec![ndp(10, 3.0, vec![])],
                    aggregation_temporality: 2,
                    is_monotonic: false,
                })),
                ..Default::default()
            },
            Metric {
                name: "bytes".into(),
                data: Some(Data::Sum(Sum {
                    data_points: vec![ndp(10, 3.0, vec![])],
                    aggregation_temporality: 1,
                    is_monotonic: true,
                })),
                ..Default::default()
            },
        ]);
        let (out, stats) = decode_all(&buf, &i);
        assert_eq!(stats.accepted, 4);
        let kinds: Vec<MetricKind> = out.iter().map(|p| p.kind).collect();
        assert_eq!(
            kinds,
            vec![MetricKind::Gauge, MetricKind::Cumulative { start: 5 }, MetricKind::Gauge, MetricKind::Delta]
        );
        assert_eq!(&*i.resolve(out[0].service), "cart");
        assert!((out[0].value - 1.5).abs() < 1e-12);
    }

    #[test]
    fn histograms_and_summaries_expand() {
        let i = Interner::default();
        let buf = request(vec![
            Metric {
                name: "http.server.request.duration".into(),
                data: Some(Data::Histogram(Histogram {
                    data_points: vec![HistogramDataPoint {
                        time_unix_nano: 9,
                        count: 40,
                        sum: Some(2.5),
                        ..Default::default()
                    }],
                    aggregation_temporality: 1,
                })),
                ..Default::default()
            },
            Metric {
                name: "rpc".into(),
                data: Some(Data::Summary(Summary {
                    data_points: vec![SummaryDataPoint {
                        time_unix_nano: 9,
                        count: 3,
                        sum: 1.0,
                        quantile_values: vec![ValueAtQuantile { quantile: 0.99, value: 0.8 }],
                        ..Default::default()
                    }],
                })),
                ..Default::default()
            },
        ]);
        let (out, _) = decode_all(&buf, &i);
        let names: Vec<String> = out.iter().map(|p| i.resolve(p.name).to_string()).collect();
        assert_eq!(
            names,
            vec![
                "http.server.request.duration.count",
                "http.server.request.duration.sum",
                "rpc.count",
                "rpc.sum",
                "rpc.p99"
            ]
        );
        assert_eq!(out[0].kind, MetricKind::Delta);
        assert!((out[0].value - 40.0).abs() < 1e-12);
        assert_eq!(out[4].kind, MetricKind::Gauge);
    }

    #[test]
    fn streams_distinguish_attributes_but_not_their_order() {
        let i = Interner::default();
        let a = vec![kv("route", "/a"), kv("code", "200")];
        let b = vec![kv("code", "200"), kv("route", "/a")];
        let c = vec![kv("route", "/b")];
        let buf = request(vec![Metric {
            name: "req".into(),
            data: Some(Data::Gauge(Gauge { data_points: vec![ndp(1, 1.0, a), ndp(1, 1.0, b), ndp(1, 1.0, c)] })),
            ..Default::default()
        }]);
        let (out, _) = decode_all(&buf, &i);
        assert_eq!(out[0].stream, out[1].stream);
        assert_ne!(out[0].stream, out[2].stream);
    }

    #[test]
    fn rejects_points_without_value_or_time() {
        let i = Interner::default();
        let no_time = NumberDataPoint { value: Some(number_data_point::Value::AsInt(3)), ..Default::default() };
        let no_value = NumberDataPoint { time_unix_nano: 4, ..Default::default() };
        let flagged = NumberDataPoint {
            time_unix_nano: 4,
            value: Some(number_data_point::Value::AsInt(3)),
            flags: 1,
            ..Default::default()
        };
        let ok = NumberDataPoint {
            time_unix_nano: 4,
            value: Some(number_data_point::Value::AsInt(-3)),
            ..Default::default()
        };
        let buf = request(vec![Metric {
            name: "x".into(),
            data: Some(Data::Gauge(Gauge { data_points: vec![no_time, no_value, flagged, ok] })),
            ..Default::default()
        }]);
        let (out, stats) = decode_all(&buf, &i);
        assert_eq!((stats.accepted, stats.rejected), (1, 3));
        assert!((out[0].value + 3.0).abs() < 1e-12, "sfixed64 decoded as signed");
    }

    proptest! {
        #[test]
        fn never_panics_on_arbitrary_bytes(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
            let i = Interner::default();
            let mut out = Vec::new();
            let _ = decode(&bytes, &i, MetricOptions::default(), &mut out);
        }

        #[test]
        fn gauge_values_round_trip(values in prop::collection::vec((1u64..u64::MAX / 2, -1e12f64..1e12), 0..30)) {
            let i = Interner::default();
            let points = values.iter().map(|&(t, v)| ndp(t, v, vec![])).collect();
            let buf = request(vec![Metric { name: "g".into(), data: Some(Data::Gauge(Gauge { data_points: points })), ..Default::default() }]);
            let (out, stats) = decode_all(&buf, &i);
            prop_assert_eq!(stats.accepted as usize, values.len());
            for (p, &(t, v)) in out.iter().zip(&values) {
                prop_assert_eq!(p.ts, i64::try_from(t).unwrap());
                prop_assert_eq!(p.value.to_bits(), v.to_bits());
            }
        }
    }
}
