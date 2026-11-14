use super::*;

const STREAM_QUEUED_BYTES: usize = 8 * 1024 * 1024;

/// A frame retains both charges while queued and while its write is in flight.
pub(in crate::host_rpc) struct Credit {
    cost: usize,
    connection: Arc<AtomicUsize>,
    stream: Arc<AtomicUsize>,
}

impl Credit {
    pub(in crate::host_rpc) fn reserve(
        connection: &Arc<AtomicUsize>,
        stream: &Arc<AtomicUsize>,
        capacity: usize,
    ) -> Result<Self, String> {
        let cost = frame_cost(capacity);
        stream
            .fetch_update(Relaxed, Relaxed, |bytes| {
                bytes
                    .checked_add(cost)
                    .filter(|bytes| *bytes <= STREAM_QUEUED_BYTES)
            })
            .map_err(|_| "conversation stream byte budget exceeded".to_string())?;
        if connection
            .fetch_update(Relaxed, Relaxed, |bytes| {
                bytes
                    .checked_add(cost)
                    .filter(|bytes| *bytes <= MAX_QUEUED_BYTES)
            })
            .is_err()
        {
            stream.fetch_sub(cost, Relaxed);
            return Err("conversation subscription byte budget exceeded".into());
        }
        Ok(Self {
            cost,
            connection: connection.clone(),
            stream: stream.clone(),
        })
    }
}

impl Drop for Credit {
    fn drop(&mut self) {
        self.connection.fetch_sub(self.cost, Relaxed);
        self.stream.fetch_sub(self.cost, Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streams_share_connection_credit_and_release_it_only_when_the_frame_is_dropped() {
        let connection = Arc::new(AtomicUsize::new(0));
        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new(AtomicUsize::new(0));
        let capacity = MAX_QUEUED_BYTES / 2 - std::mem::size_of::<Vec<u8>>();
        let one = Credit::reserve(&connection, &first, capacity).unwrap();
        let two = Credit::reserve(&connection, &second, capacity).unwrap();
        assert_eq!(connection.load(Relaxed), MAX_QUEUED_BYTES);
        assert!(Credit::reserve(&connection, &Arc::default(), 1).is_err());
        assert!(Credit::reserve(&Arc::default(), &first, 1).is_err());
        let independent = Arc::new(AtomicUsize::new(0));
        let other = Credit::reserve(&independent, &Arc::default(), capacity).unwrap();
        drop(one);
        assert_eq!(connection.load(Relaxed), MAX_QUEUED_BYTES / 2);
        assert_eq!(first.load(Relaxed), 0);
        drop(two);
        assert_eq!(connection.load(Relaxed), 0);
        assert_eq!(independent.load(Relaxed), MAX_QUEUED_BYTES / 2);
        drop(other);
        assert_eq!(independent.load(Relaxed), 0);
    }
}
