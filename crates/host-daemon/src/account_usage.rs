use agent_protocol::operations::{AccountUsage, UsageWindow};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub(crate) type UsageCache = HashMap<String, Arc<UsageEntry>>;

#[derive(Default)]
pub(crate) struct UsageEntry(tokio::sync::Mutex<Option<(Instant, AccountUsage)>>);

impl UsageEntry {
    pub(crate) async fn read(
        &self,
        fetch: impl std::future::Future<Output = Result<Vec<UsageWindow>, String>>,
    ) -> AccountUsage {
        // Serialize only requests for this account's usage, never account selection.
        let mut cached = self.0.lock().await;
        if let Some((at, usage)) = cached.as_ref()
            && at.elapsed() < Duration::from_secs(60)
        {
            return usage.clone();
        }
        let result = tokio::time::timeout(Duration::from_secs(8), fetch).await;
        let (windows, error) = match result {
            Ok(Ok(windows)) if !windows.is_empty() => (windows, None),
            _ => (
                Vec::new(),
                Some(
                    "使用量を取得できませんでした。しばらくしてから再読み込みしてください。".into(),
                ),
            ),
        };
        let usage = AccountUsage {
            windows,
            fetched_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
            error,
        };
        *cached = Some((Instant::now(), usage.clone()));
        usage
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cache_is_scoped_to_account_and_expired_failures_replace_old_usage() {
        let mut cache = UsageCache::default();
        let a = cache.entry("a".into()).or_default().clone();
        let usage = a
            .read(async {
                Ok(vec![
                    UsageWindow::from_used("5時間枠".into(), 20., None).unwrap(),
                ])
            })
            .await;
        let cached = a
            .read(async { panic!("fresh usage must not be fetched again") })
            .await;
        assert_eq!(cached.windows, usage.windows);
        let failed = cache
            .entry("b".into())
            .or_default()
            .read(async { Err("private upstream error".into()) })
            .await;
        assert!(failed.windows.is_empty());
        assert!(!failed.error.unwrap().contains("private"));
        a.0.lock().await.as_mut().unwrap().0 = Instant::now() - Duration::from_secs(61);
        let expired = a.read(async { Err("private upstream error".into()) }).await;
        assert!(expired.windows.is_empty());
        assert!(expired.error.is_some());
        cache.remove("a");
        assert!(!Arc::ptr_eq(&a, cache.entry("a".into()).or_default()));
    }
}
