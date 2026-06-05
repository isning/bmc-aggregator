use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

use crate::bmc::encoder::Codec;
use super::encoder::VideoStreamEncoder;

#[derive(Clone, Debug)]
pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub codec: Codec,
    pub pts: i64,
}

pub struct SharedVideoPipeline {
    encoder: Mutex<Box<dyn VideoStreamEncoder>>,
    tx: broadcast::Sender<EncodedFrame>,
}

impl SharedVideoPipeline {
    pub fn new(encoder: Box<dyn VideoStreamEncoder>) -> Arc<Self> {
        let (tx, _) = broadcast::channel(64);
        Arc::new(Self {
            encoder: Mutex::new(encoder),
            tx,
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EncodedFrame> {
        self.tx.subscribe()
    }

    pub fn tx(&self) -> broadcast::Sender<EncodedFrame> {
        self.tx.clone()
    }

    pub fn set_encoder(&self, encoder: Box<dyn VideoStreamEncoder>) {
        *self.encoder.lock().unwrap() = encoder;
    }

    pub fn push_frame(&self, rgba: &[u8], width: u32, height: u32) {
        let Ok(mut encoder) = self.encoder.try_lock() else {
            return;
        };
        match encoder.encode(rgba, width, height) {
            Ok(packets) => {
                let codec = encoder.codec();
                for data in packets {
                    let frame = EncodedFrame { data, codec, pts: 0 };
                    let _ = self.tx.send(frame);
                }
            }
            Err(e) => {
                tracing::warn!("encode error: {e}");
            }
        }
    }

    pub fn request_keyframe(&self) {
        if let Ok(encoder) = self.encoder.lock() {
            encoder.request_keyframe();
        }
    }

    pub fn codec(&self) -> Codec {
        self.encoder.lock().unwrap().codec()
    }

    pub fn perf_snapshot(&self) -> serde_json::Value {
        let result = self.encoder.lock().map(|e| {
            let v = e.perf_snapshot();
            tracing::debug!(target: "perf", ?v, "pipeline encoder perf_snapshot");
            v
        }).unwrap_or_default();
        tracing::debug!(target: "perf", ?result, "pipeline perf_snapshot result");
        result
    }
}
