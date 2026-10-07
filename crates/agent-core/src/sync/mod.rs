//! Pure reducers for the shell and thread subscriptions, history pages and the
//! disk cache. The connection owner feeds them stream items and RPC results.
pub mod cache;
pub mod history;
pub mod shell;
pub mod thread;

#[cfg(test)]
pub(crate) mod fixtures;

pub use cache::{DiskCache, ShellCacheEntry, ThreadCacheEntry};
pub use history::HistoryMeta;
pub use shell::{ShellCache, ShellStatus};
pub use thread::{CachedThread, Detail, ThreadStatus, ThreadSync};

/// How long a thread's folded state stays in memory after its last view.
pub const THREAD_SNAPSHOT_IDLE_TTL_MS: u64 = 5 * 60_000;
/// First retry of a subscription the Host refused; doubled per consecutive
/// failure up to the cap and reset by the first item of a healthy stream.
pub const RESUBSCRIBE_DELAY_MS: u64 = 250;
pub const MAX_RESUBSCRIBE_DELAY_MS: u64 = 30_000;

pub fn resubscribe_delay_ms(consecutive_failures: u32) -> u64 {
    RESUBSCRIBE_DELAY_MS
        .saturating_mul(1u64 << consecutive_failures.min(16))
        .min(MAX_RESUBSCRIBE_DELAY_MS)
}

#[cfg(test)]
mod tests {
    #[test]
    fn subscription_retries_double_from_250_ms_up_to_30_seconds() {
        let delays: Vec<_> = (0..9).map(super::resubscribe_delay_ms).collect();
        assert_eq!(
            delays,
            [250, 500, 1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000]
        );
    }
}
