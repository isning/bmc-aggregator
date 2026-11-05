use std::sync::{Mutex, MutexGuard, OnceLock};

static GST_INIT: OnceLock<Result<(), String>> = OnceLock::new();
static GST_LOCK: Mutex<()> = Mutex::new(());

pub fn ensure_gst_init() -> Result<(), String> {
    GST_INIT
        .get_or_init(|| {
            gstreamer::init().map_err(|error| format!("GStreamer initialization failed: {error}"))
        })
        .clone()
}

/// Acquire the global GStreamer lock.
///
/// GStreamer's plugin registry is not fully thread-safe when pipelines are
/// created concurrently from multiple threads (e.g. parallel test execution).
/// Hold this lock during any operation that touches GStreamer elements.
pub fn gst_lock() -> Result<MutexGuard<'static, ()>, String> {
    ensure_gst_init()?;
    Ok(GST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner))
}
