//! OTLP message types generated from the vendored protocol definitions
//! (OTLP v1.11.1, Apache-2.0, see `proto/opentelemetry/LICENSE`).
//!
//! These full message types are used to *produce* OTLP (simulator, load
//! generator) and as the reference in differential tests. The ingestion path
//! does not use them: see [`crate::decode`].
#![allow(missing_docs, clippy::all, clippy::pedantic, unused_qualifications, rust_2018_idioms)]

pub mod common {
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/opentelemetry.proto.common.v1.rs"));
    }
}

pub mod resource {
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/opentelemetry.proto.resource.v1.rs"));
    }
}

pub mod trace {
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/opentelemetry.proto.trace.v1.rs"));
    }
}

pub mod metrics {
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/opentelemetry.proto.metrics.v1.rs"));
    }
}

pub mod logs {
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/opentelemetry.proto.logs.v1.rs"));
    }
}

pub mod collector {
    pub mod trace {
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/opentelemetry.proto.collector.trace.v1.rs"));
            /// Server receiving raw request bytes.
            pub mod raw {
                include!(concat!(env!("OUT_DIR"), "/raw/opentelemetry.proto.collector.trace.v1.TraceService.rs"));
            }
        }
    }
    pub mod metrics {
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/opentelemetry.proto.collector.metrics.v1.rs"));
            /// Server receiving raw request bytes.
            pub mod raw {
                include!(concat!(env!("OUT_DIR"), "/raw/opentelemetry.proto.collector.metrics.v1.MetricsService.rs"));
            }
        }
    }
    pub mod logs {
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/opentelemetry.proto.collector.logs.v1.rs"));
            /// Server receiving raw request bytes.
            pub mod raw {
                include!(concat!(env!("OUT_DIR"), "/raw/opentelemetry.proto.collector.logs.v1.LogsService.rs"));
            }
        }
    }
}
