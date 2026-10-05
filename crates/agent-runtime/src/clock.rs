use agent_domain::Timestamp;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;
}

pub struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis() as i64);
        Timestamp::from_millis(millis).expect("system time is within years 0..=9999")
    }
}

/// A clock that only moves when told to.
pub struct ManualClock(AtomicI64);
impl ManualClock {
    pub fn new(at: &Timestamp) -> Self {
        Self(AtomicI64::new(at.millis()))
    }
    pub fn set(&self, at: &Timestamp) {
        self.0.store(at.millis(), Ordering::SeqCst);
    }
    pub fn advance(&self, millis: i64) {
        self.0.fetch_add(millis, Ordering::SeqCst);
    }
}
impl Clock for ManualClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0.load(Ordering::SeqCst)).expect("manual clock is in range")
    }
}
