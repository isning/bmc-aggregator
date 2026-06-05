use std::sync::atomic::{AtomicU64, Ordering};

/// Performance counters aggregated over a sampling window (reset after each snapshot).
pub struct PerfCounters {
    pub encode_count: AtomicU64,
    pub encode_time_total_us: AtomicU64,
    pub encode_time_max_us: AtomicU64,
    pub tile_count: AtomicU64,
    pub pipeline_total_us: AtomicU64,
    pub pipeline_count: AtomicU64,
    pub pipeline_max_us: AtomicU64,
}

impl PerfCounters {
    pub fn new() -> Self {
        Self {
            encode_count: AtomicU64::new(0),
            encode_time_total_us: AtomicU64::new(0),
            encode_time_max_us: AtomicU64::new(0),
            tile_count: AtomicU64::new(0),
            pipeline_total_us: AtomicU64::new(0),
            pipeline_count: AtomicU64::new(0),
            pipeline_max_us: AtomicU64::new(0),
        }
    }

    /// Take a snapshot and reset all counters.
    pub fn snapshot(&self) -> serde_json::Value {
        let encode_count = self.encode_count.swap(0, Ordering::Relaxed);
        let encode_total = self.encode_time_total_us.swap(0, Ordering::Relaxed);
        let encode_max = self.encode_time_max_us.swap(0, Ordering::Relaxed);
        let tile_count = self.tile_count.swap(0, Ordering::Relaxed);
        let pipe_total = self.pipeline_total_us.swap(0, Ordering::Relaxed);
        let pipe_count = self.pipeline_count.swap(0, Ordering::Relaxed);
        let pipe_max = self.pipeline_max_us.swap(0, Ordering::Relaxed);
        serde_json::json!({
            "encode_time_avg_ms": if encode_count > 0 { encode_total as f64 / encode_count as f64 / 1000.0 } else { 0.0 },
            "encode_time_max_ms": encode_max as f64 / 1000.0,
            "tile_rate": tile_count,
            "pipeline_latency_avg_ms": if pipe_count > 0 { pipe_total as f64 / pipe_count as f64 / 1000.0 } else { 0.0 },
            "pipeline_latency_max_ms": pipe_max as f64 / 1000.0,
            "encode_count": encode_count,
        })
    }
}
