//! Selective, zero-copy decoding of OTLP export requests.
//!
//! The decoders walk the protobuf encoding directly and extract only what the
//! engine uses, borrowing strings from the request buffer and interning the
//! few that are kept. Unknown fields, span events, links and all attributes
//! that are not listed in [`crate::semconv`] are skipped without allocating.
//! Field order is not assumed: protobuf allows fields in any order, and
//! messages whose semantics depend on a sibling field (the resource of a
//! batch, the temporality of a sum) are scanned twice.
//!
//! Correctness is established by differential tests against the reference
//! `prost` decoding of randomly generated requests (see the `tests` modules).

pub mod logs;
pub mod metrics;
pub mod traces;

use std::hash::BuildHasher;

use etio_core::{Interner, Sym};

use crate::semconv;
use crate::wire::{Reader, WireError, WireType};

/// Maximum nesting of `AnyValue` arrays and key-value lists that is walked.
/// Deeper structures are skipped as opaque bytes, which bounds recursion on
/// adversarial input.
pub const MAX_VALUE_DEPTH: usize = 8;

/// Counters describing one decoded request.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DecodeStats {
    /// Items accepted (spans, data points or log records).
    pub accepted: u64,
    /// Items rejected as invalid (missing identifiers, no value).
    pub rejected: u64,
}

/// A decoded attribute value (only the scalar kinds the engine reads).
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Value<'a> {
    /// A string.
    Str(&'a str),
    /// A signed integer.
    Int(i64),
    /// A float.
    Double(f64),
    /// A boolean.
    Bool(bool),
    /// Anything else (arrays, maps, bytes).
    Other,
}

impl<'a> Value<'a> {
    /// The value as a string, if it is one.
    #[must_use]
    pub const fn as_str(&self) -> Option<&'a str> {
        match *self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The value as an integer (strings holding integers are accepted, as
    /// some instrumentations record status codes as strings).
    #[must_use]
    pub fn as_int(&self) -> Option<i64> {
        match *self {
            Self::Int(i) => Some(i),
            Self::Str(s) => s.trim().parse().ok(),
            _ => None,
        }
    }
}

/// Decodes an `AnyValue` message.
///
/// # Errors
/// Fails on malformed encodings.
pub fn any_value(buf: &[u8]) -> Result<Value<'_>, WireError> {
    let mut r = Reader::new(buf);
    let mut v = Value::Other;
    while !r.is_empty() {
        let (field, wt) = r.key()?;
        match (field, wt) {
            (1, WireType::Len) => v = Value::Str(r.str()?),
            (2, WireType::Varint) => v = Value::Bool(r.varint()? != 0),
            #[allow(clippy::cast_possible_wrap)]
            (3, WireType::Varint) => v = Value::Int(r.varint()? as i64),
            (4, WireType::I64) => v = Value::Double(f64::from_bits(r.fixed64()?)),
            _ => {
                r.skip(wt)?;
                v = Value::Other;
            }
        }
    }
    Ok(v)
}

/// Iterates over a `KeyValue` message: returns the key and the raw value bytes.
///
/// # Errors
/// Fails on malformed encodings or invalid UTF-8 keys.
pub fn key_value(buf: &[u8]) -> Result<(&str, &[u8]), WireError> {
    let mut r = Reader::new(buf);
    let mut key = "";
    let mut value: &[u8] = &[];
    while !r.is_empty() {
        let (field, wt) = r.key()?;
        match (field, wt) {
            (1, WireType::Len) => key = r.str()?,
            (2, WireType::Len) => value = r.bytes()?,
            _ => r.skip(wt)?,
        }
    }
    Ok((key, value))
}

/// What the engine needs from a `Resource`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ResourceInfo<'a> {
    /// `service.name`.
    pub service_name: Option<&'a str>,
    /// `service.namespace`.
    pub service_namespace: Option<&'a str>,
    /// `k8s.deployment.name`, `k8s.statefulset.name`, ... (the workload).
    pub workload: Option<&'a str>,
    /// `host.name`.
    pub host: Option<&'a str>,
}

impl<'a> ResourceInfo<'a> {
    /// Parses a `Resource` message.
    ///
    /// # Errors
    /// Fails on malformed encodings.
    pub fn parse(buf: &'a [u8]) -> Result<Self, WireError> {
        let mut info = Self::default();
        let mut r = Reader::new(buf);
        while !r.is_empty() {
            let (field, wt) = r.key()?;
            if field != 1 || wt != WireType::Len {
                r.skip(wt)?;
                continue;
            }
            let (key, value) = key_value(r.bytes()?)?;
            let slot = match key {
                semconv::SERVICE_NAME => &mut info.service_name,
                semconv::SERVICE_NAMESPACE => &mut info.service_namespace,
                semconv::K8S_DEPLOYMENT_NAME | semconv::K8S_STATEFULSET_NAME | semconv::K8S_DAEMONSET_NAME => {
                    &mut info.workload
                }
                semconv::HOST_NAME => &mut info.host,
                _ => continue,
            };
            // The last occurrence of a key wins, as in a decoded message.
            *slot = any_value(value)?.as_str().filter(|s| !s.is_empty());
        }
        Ok(info)
    }

    /// The root-cause candidate this resource belongs to.
    ///
    /// `service.name` when present (the SDK default `unknown_service:*` is
    /// replaced by the workload or host when those are known), else the
    /// Kubernetes workload, else `host:<name>`, else `unknown_service`.
    pub fn entity(&self, interner: &Interner, with_namespace: bool) -> Sym {
        let service = self.service_name.filter(|s| !s.starts_with("unknown_service"));
        match (service, self.workload, self.host) {
            (Some(s), ..) => match self.service_namespace.filter(|_| with_namespace) {
                Some(ns) => interner.intern(&format!("{ns}/{s}")),
                None => interner.intern(s),
            },
            (None, Some(w), _) => interner.intern(w),
            (None, None, Some(h)) => interner.intern(&format!("host:{h}")),
            (None, None, None) => interner.intern(self.service_name.unwrap_or("unknown_service")),
        }
    }
}

/// Scans a message for its resource (field 1), wherever it appears.
///
/// # Errors
/// Fails on malformed encodings.
pub fn find_resource(buf: &[u8]) -> Result<ResourceInfo<'_>, WireError> {
    let mut r = Reader::new(buf);
    let mut info = ResourceInfo::default();
    while !r.is_empty() {
        let (field, wt) = r.key()?;
        if field == 1 && wt == WireType::Len {
            info = ResourceInfo::parse(r.bytes()?)?;
        } else {
            r.skip(wt)?;
        }
    }
    Ok(info)
}

/// Order-independent hash of a set of attributes: the sum of the hashes of
/// the encoded key-value pairs. SDKs may serialise the same attribute set in
/// different orders between exports; the identity of a stream must not change.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct AttrHash(u64);

impl AttrHash {
    /// Adds one encoded `KeyValue`.
    pub fn add(&mut self, encoded: &[u8]) {
        let h = foldhash::fast::FixedState::with_seed(0x6174_7472).hash_one(encoded);
        self.0 = self.0.wrapping_add(h);
    }

    /// Mixes the attribute hash with other identity components.
    #[must_use]
    pub fn finish(self, parts: &[&[u8]]) -> u64 {
        let base = foldhash::fast::FixedState::with_seed(0x7374_7265_616d).hash_one(parts);
        base ^ self.0.rotate_left(17)
    }
}

/// Decoding errors.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DecodeError {
    /// The protobuf encoding is malformed.
    #[error("malformed OTLP protobuf: {0}")]
    Wire(#[from] WireError),
    /// The JSON encoding is malformed.
    #[error("malformed OTLP JSON: {0}")]
    Json(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::common::v1::{AnyValue, KeyValue, any_value::Value as PValue};
    use crate::proto::resource::v1::Resource;
    use prost::Message;

    fn kv(k: &str, v: PValue) -> KeyValue {
        KeyValue { key: k.into(), value: Some(AnyValue { value: Some(v) }), ..Default::default() }
    }

    #[test]
    fn parses_resources_and_names_entities() {
        let i = Interner::default();
        let res = Resource {
            attributes: vec![
                kv("service.name", PValue::StringValue("cart".into())),
                kv("service.namespace", PValue::StringValue("shop".into())),
                kv("host.name", PValue::StringValue("node-1".into())),
                kv("other", PValue::IntValue(3)),
            ],
            ..Default::default()
        };
        let buf = res.encode_to_vec();
        let info = ResourceInfo::parse(&buf).unwrap();
        assert_eq!(info.service_name, Some("cart"));
        assert_eq!(&*i.resolve(info.entity(&i, false)), "cart");
        assert_eq!(&*i.resolve(info.entity(&i, true)), "shop/cart");

        let sdk_default =
            ResourceInfo { service_name: Some("unknown_service:java"), host: Some("n1"), ..Default::default() };
        assert_eq!(&*i.resolve(sdk_default.entity(&i, false)), "host:n1");
        assert_eq!(&*i.resolve(ResourceInfo::default().entity(&i, false)), "unknown_service");
    }

    #[test]
    fn decodes_scalar_values() {
        let enc = |v| AnyValue { value: Some(v) }.encode_to_vec();
        assert_eq!(any_value(&enc(PValue::StringValue("x".into()))).unwrap(), Value::Str("x"));
        assert_eq!(any_value(&enc(PValue::IntValue(-5))).unwrap(), Value::Int(-5));
        assert_eq!(any_value(&enc(PValue::DoubleValue(2.5))).unwrap(), Value::Double(2.5));
        assert_eq!(any_value(&enc(PValue::BoolValue(true))).unwrap(), Value::Bool(true));
        assert_eq!(any_value(&enc(PValue::BytesValue(vec![1]))).unwrap(), Value::Other);
        assert_eq!(Value::Str(" 503 ").as_int(), Some(503));
    }

    #[test]
    fn attribute_hash_ignores_order() {
        let a = kv("a", PValue::IntValue(1)).encode_to_vec();
        let b = kv("b", PValue::IntValue(2)).encode_to_vec();
        let (mut x, mut y) = (AttrHash::default(), AttrHash::default());
        x.add(&a);
        x.add(&b);
        y.add(&b);
        y.add(&a);
        assert_eq!(x.finish(&[b"m"]), y.finish(&[b"m"]));
        assert_ne!(x.finish(&[b"m"]), x.finish(&[b"n"]));
    }
}
