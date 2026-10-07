//! The owner's file writes: the disk cache and the device state, one at a
//! time in the order asked, so a later write or removal of a file lands last.
use super::owner::Event;
use tokio::{sync::mpsc, task::JoinHandle};

type Job = Box<dyn FnOnce() -> Option<Event> + Send>;

pub(super) struct Disk {
    jobs: mpsc::UnboundedSender<Job>,
    worker: JoinHandle<()>,
}
impl Disk {
    /// Each job's event goes to the owner once the job ran.
    pub fn new(events: mpsc::Sender<Event>) -> Self {
        let (jobs, mut queue) = mpsc::unbounded_channel::<Job>();
        let worker = tokio::spawn(async move {
            while let Some(job) = queue.recv().await {
                if let Ok(Some(event)) = tokio::task::spawn_blocking(job).await {
                    let _ = events.send(event).await;
                }
            }
        });
        Self { jobs, worker }
    }

    pub fn run(&self, job: impl FnOnce() -> Option<Event> + Send + 'static) {
        let _ = self.jobs.send(Box::new(job));
    }

    /// Waits until every job asked so far ran.
    pub async fn finish(self) {
        drop(self.jobs);
        let _ = self.worker.await;
    }
}
