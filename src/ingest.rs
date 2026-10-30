//! Detection and telemetry ingestion boundaries.
mod detection;
mod telemetry;

pub use detection::ingest_detection;
pub use telemetry::ingest_telemetry;
