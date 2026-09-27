use super::*;
/// Bounded, connection-local item work. A continuation keeps its original
/// receipt and slot identity, without blocking unrelated work.
#[derive(Default)]
pub(super) struct ItemReads {
    receipts: BTreeMap<op::ReadItem, Receipt>,
    running: BTreeSet<op::ReadItem>,
    pending: VecDeque<Scheduled>,
}
impl ItemReads {
    pub(super) fn enqueue(&mut self, mut scheduled: Scheduled) -> Result<(), PeerError> {
        let key = scheduled.effect.0.item_read().unwrap().clone();
        if let Some(receipt) = self.receipts.get(&key) {
            if let Some(complete) = scheduled.complete.take() {
                receipt.join(complete);
            }
            if !scheduled.effect.1 {
                return Ok(());
            }
            scheduled.complete = Some(receipt.clone());
        } else {
            if self.receipts.len() >= 132 {
                let error = PeerError::InvalidMessage(format!(
                    "too many pending item reads: {}",
                    key.item_id
                ));
                if let Some(complete) = scheduled.complete {
                    complete.send(Err(error.clone()));
                }
                return Err(error);
            }
            let receipt = scheduled.complete.get_or_insert_default().clone();
            self.receipts.insert(key, receipt);
        }
        if scheduled.effect.1 {
            // Continue the same item before issuing new grants. Its slot covers
            // the control response, body transfer, and final application.
            self.pending.push_front(scheduled);
        } else {
            self.pending.push_back(scheduled);
        }
        Ok(())
    }
    pub(super) fn next(&mut self) -> Option<Scheduled> {
        let next = self.pending.front()?;
        if self.running.len() >= 4 && !self.running.contains(next.effect.0.item_read().unwrap()) {
            return None;
        }
        let scheduled = self.pending.pop_front().unwrap();
        self.running
            .insert(scheduled.effect.0.item_read().unwrap().clone());
        Some(scheduled)
    }
    pub(super) fn finish(
        &mut self,
        updates: &watch::Sender<Arc<Snapshot>>,
        completed: Completed,
        catalog_generation: u64,
    ) -> Vec<Scheduled> {
        let key = completed.item_read.clone();
        let effects = finish(updates, completed, catalog_generation);
        if let Some(key) = key
            && !effects
                .iter()
                .any(|s| s.effect.1 && s.effect.0.item_read() == Some(&key))
        {
            self.running.remove(&key);
            self.receipts.remove(&key);
        }
        effects
    }
}
