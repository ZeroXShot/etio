//! A gRPC codec that hands request messages over as raw bytes.
//!
//! tonic normally decodes each request into a full `prost` message tree:
//! every attribute key and value becomes an owned `String`. The ingestion
//! path needs a handful of fields per span, so the OTLP servers use this
//! codec instead and decode the bytes selectively (see [`crate::decode`]).
//! Responses are ordinary `prost` messages.

use std::marker::PhantomData;

use bytes::{Buf, Bytes};
use tonic::Status;
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};

/// Codec with raw-bytes requests and `prost` responses.
#[derive(Debug, Clone, Copy)]
pub struct RawCodec<E>(PhantomData<E>);

impl<E> Default for RawCodec<E> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<E: prost::Message + Send + 'static> Codec for RawCodec<E> {
    type Encode = E;
    type Decode = Bytes;
    type Encoder = MessageEncoder<E>;
    type Decoder = BytesDecoder;

    fn encoder(&mut self) -> Self::Encoder {
        MessageEncoder(PhantomData)
    }

    fn decoder(&mut self) -> Self::Decoder {
        BytesDecoder
    }
}

/// Encodes `prost` messages.
#[derive(Debug, Clone, Copy)]
pub struct MessageEncoder<E>(PhantomData<E>);

impl<E: prost::Message> Encoder for MessageEncoder<E> {
    type Item = E;
    type Error = Status;

    fn encode(&mut self, item: E, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
        item.encode(dst).map_err(|e| Status::internal(format!("encoding response: {e}")))
    }
}

/// Yields the message bytes untouched.
#[derive(Debug, Clone, Copy)]
pub struct BytesDecoder;

impl Decoder for BytesDecoder {
    type Item = Bytes;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<Bytes>, Status> {
        Ok(Some(src.copy_to_bytes(src.remaining())))
    }
}
