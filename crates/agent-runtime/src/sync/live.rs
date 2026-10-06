//! Live delivery with T3 `LiveStreamBudget` limits: a subscriber that falls behind
//! by more than its item or serialized-byte budget is closed, never waited for, and
//! resumes from the last sequence it applied.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::sync::mpsc;

use tokio::sync::mpsc::error::TryRecvError;

/// Serialized bytes a subscription may hold undelivered.
pub const LIVE_STREAM_MAX_BYTES: u64 = 8 * 1024 * 1024;
/// Updates a subscription may hold undelivered.
pub const LIVE_STREAM_MAX_ITEMS: usize = 1_000;
/// T3 `LiveStreamBufferError`.
pub const LIVE_BUFFER_FULL: &str =
    "The live event buffer is full. Resume from the last received sequence.";

pub(crate) fn live_channel<T>(
    max_items: usize,
    max_bytes: u64,
) -> (LiveSender<T>, LiveReceiver<T>) {
    let (items, receiver) = mpsc::channel(max_items.max(1));
    let retained = Arc::new(AtomicU64::new(0));
    let overflowed = Arc::new(AtomicBool::new(false));
    (
        LiveSender {
            items,
            retained: retained.clone(),
            max_bytes,
            overflowed: overflowed.clone(),
        },
        LiveReceiver {
            items: receiver,
            retained,
            overflowed,
        },
    )
}

pub(crate) struct LiveSender<T> {
    items: mpsc::Sender<(T, u64)>,
    retained: Arc<AtomicU64>,
    max_bytes: u64,
    overflowed: Arc<AtomicBool>,
}

impl<T> LiveSender<T> {
    /// Queues `item`, charged `bytes` until received. False when the subscriber is
    /// gone or over its budget; dropping the sender then closes the stream.
    pub(crate) fn offer(&self, item: T, bytes: u64) -> bool {
        let retained = self.retained.fetch_add(bytes, Ordering::SeqCst) + bytes;
        let sent = match retained > self.max_bytes {
            true => Err(None),
            false => self
                .items
                .try_send((item, bytes))
                .map_err(|error| match error {
                    mpsc::error::TrySendError::Full(_) => None,
                    mpsc::error::TrySendError::Closed(_) => Some(()),
                }),
        };
        if let Err(closed) = sent {
            self.retained.fetch_sub(bytes, Ordering::SeqCst);
            if closed.is_none() {
                self.fail();
            }
            return false;
        }
        true
    }

    /// The subscriber fell behind; its stream ends with [`LIVE_BUFFER_FULL`].
    pub(crate) fn fail(&self) {
        self.overflowed.store(true, Ordering::SeqCst);
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.items.is_closed()
    }

    /// Resolves once the subscriber dropped its receiver.
    pub(crate) async fn closed(&self) {
        self.items.closed().await;
    }
}

/// The receiving end of a subscription. `None` means the stream closed.
pub struct LiveReceiver<T> {
    items: mpsc::Receiver<(T, u64)>,
    retained: Arc<AtomicU64>,
    overflowed: Arc<AtomicBool>,
}

impl<T> LiveReceiver<T> {
    /// After the stream closed: whether it closed because the subscriber fell
    /// behind, rather than because the Host stopped publishing.
    pub fn overflowed(&self) -> bool {
        self.overflowed.load(Ordering::SeqCst)
    }

    pub async fn recv(&mut self) -> Option<T> {
        let (item, bytes) = self.items.recv().await?;
        self.retained.fetch_sub(bytes, Ordering::SeqCst);
        Some(item)
    }

    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        let (item, bytes) = self.items.try_recv()?;
        self.retained.fetch_sub(bytes, Ordering::SeqCst);
        Ok(item)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closes_a_subscriber_over_its_item_or_byte_budget_and_releases_received_bytes() {
        let (sender, mut receiver) = live_channel::<u32>(4, 100);
        assert!(sender.offer(1, 60));
        assert!(!receiver.overflowed());
        assert!(!sender.offer(2, 41));
        assert!(receiver.overflowed());
        assert!(sender.offer(3, 40));
        assert_eq!(receiver.try_recv(), Ok(1));
        assert!(sender.offer(4, 60));
        assert_eq!(receiver.try_recv(), Ok(3));
        assert_eq!(receiver.try_recv(), Ok(4));

        let (sender, _receiver) = live_channel::<u32>(2, u64::MAX);
        assert!(sender.offer(1, 0) && sender.offer(2, 0));
        assert!(!sender.offer(3, 0));

        let (sender, receiver) = live_channel::<u32>(2, u64::MAX);
        let overflowed = receiver.overflowed.clone();
        drop(receiver);
        assert!(sender.is_closed());
        assert!(!sender.offer(1, 0));
        assert!(!overflowed.load(Ordering::SeqCst));
    }
}
