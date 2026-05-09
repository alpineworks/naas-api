use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use async_channel::{Receiver, Sender};
use bytes::{Bytes, BytesMut};

use crate::error::ServiceError;

#[derive(Clone, Debug)]
pub struct Pool {
    tx: Sender<Bytes>,
    rx: Receiver<Bytes>,
    last_push_ms: Arc<AtomicU64>,
}

impl Pool {
    pub fn new(capacity_chunks: usize) -> Self {
        let (tx, rx) = async_channel::bounded(capacity_chunks);
        Self {
            tx,
            rx,
            last_push_ms: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn sender(&self) -> PoolSender {
        PoolSender {
            tx: self.tx.clone(),
            last_push_ms: self.last_push_ms.clone(),
        }
    }

    pub async fn pull(&self, n: usize) -> Result<Bytes, ServiceError> {
        let mut out = BytesMut::with_capacity(n);
        while out.len() < n {
            let chunk = self.rx.recv().await.map_err(|_| ServiceError::PoolClosed)?;
            let need = n - out.len();
            if chunk.len() <= need {
                out.extend_from_slice(&chunk);
            } else {
                out.extend_from_slice(&chunk[..need]);
            }
        }
        Ok(out.freeze())
    }

    pub async fn pull_chunk(&self) -> Result<Bytes, ServiceError> {
        self.rx.recv().await.map_err(|_| ServiceError::PoolClosed)
    }

    pub fn pending_chunks(&self) -> usize {
        self.rx.len()
    }

    pub fn capacity_chunks(&self) -> usize {
        self.rx.capacity().unwrap_or(0)
    }

    pub fn last_push_ms(&self) -> u64 {
        self.last_push_ms.load(Ordering::Relaxed)
    }

    pub fn close(&self) {
        self.tx.close();
        self.rx.close();
    }
}

#[derive(Clone, Debug)]
pub struct PoolSender {
    tx: Sender<Bytes>,
    last_push_ms: Arc<AtomicU64>,
}

impl PoolSender {
    pub fn push_blocking(&self, bytes: Bytes) -> Result<(), ()> {
        if self.tx.send_blocking(bytes).is_err() {
            return Err(());
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.last_push_ms.store(now, Ordering::Relaxed);
        Ok(())
    }

    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}
