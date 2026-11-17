//! Each workspace preparation owns cancellation and serialized resource I/O.
use super::*;

pub(super) struct Preparation {
    pub io: AsyncMutex<()>,
    pub active: Mutex<Option<(Uuid, tokio_util::sync::CancellationToken)>>,
    /// The I/O owner clears this immediately before the user write. Stop can
    /// certify non-delivery while allocation or context setup is still pending,
    /// retaining late resources without cancelling an in-flight user write.
    pub preparing_input: Mutex<HashMap<String, String>>,
}

impl Preparation {
    pub(super) fn begin_input_write(
        &self,
        effect_id: &str,
        ready: impl FnOnce() -> Result<bool, Failure>,
    ) -> Result<bool, Failure> {
        let mut preparing_input = self.preparing_input.lock().unwrap();
        if !ready()? {
            return Ok(false);
        }
        preparing_input.remove(effect_id);
        Ok(true)
    }

    pub(super) fn cancel_generation(&self, generation: Uuid) {
        if let Some((current, cancel)) = &*self.active.lock().unwrap()
            && *current == generation
        {
            cancel.cancel();
        }
    }
}

#[cfg(test)]
mod tests;

impl Runtime {
    pub(super) fn preparation(&self, target: &SessionRef) -> Arc<Preparation> {
        self.preparations
            .lock()
            .unwrap()
            .entry(target.clone())
            .or_insert_with(|| {
                Arc::new(Preparation {
                    io: Default::default(),
                    active: Default::default(),
                    preparing_input: Default::default(),
                })
            })
            .clone()
    }

    pub(super) fn retire_preparation(&self, target: &SessionRef) {
        let mut preparations = self.preparations.lock().unwrap();
        if preparations.get(target).is_some_and(|preparation| {
            Arc::strong_count(preparation) == 1 && preparation.active.lock().unwrap().is_none()
        }) {
            preparations.remove(target);
        }
    }
}
