use super::Receipt;
use agent_process::PtyCommand;
use std::collections::VecDeque;

#[derive(Default)]
pub(super) struct InputQueue {
    pending: Option<(u64, Option<Receipt>)>,
    replies: VecDeque<Vec<u8>>,
    next_id: u64,
}

impl InputQueue {
    pub(super) fn is_idle(&self) -> bool {
        self.pending.is_none() && self.replies.is_empty()
    }

    pub(super) fn extend_replies(&mut self, replies: Vec<Vec<u8>>) {
        self.replies.extend(replies);
    }

    pub(super) fn begin(&mut self, complete: Option<Receipt>) -> Result<u64, String> {
        if self.pending.is_some() {
            return Err("terminal operation is already pending".into());
        }
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or("terminal operation ID exhausted")?;
        self.pending = Some((self.next_id, complete));
        Ok(self.next_id)
    }

    pub(super) fn next_reply(&mut self) -> Result<Option<PtyCommand>, String> {
        if self.pending.is_some() {
            return Ok(None);
        }
        let Some(data) = self.replies.pop_front() else {
            return Ok(None);
        };
        Ok(Some(PtyCommand::Write {
            id: self.begin(None)?,
            data,
        }))
    }

    pub(super) fn acknowledge(&mut self, id: u64, error: Option<String>) -> Result<(), String> {
        if id == 0 {
            return error.map_or(Ok(()), Err);
        }
        let (expected, complete) = self
            .pending
            .take()
            .ok_or("unexpected terminal acknowledgement")?;
        if expected != id {
            if let Some(complete) = complete {
                let _ = complete.send(Err("terminal acknowledgement ID changed".into()));
            }
            return Err("terminal acknowledgement ID changed".into());
        }
        let result = error.map_or(Ok(()), Err);
        if let Some(complete) = complete {
            let _ = complete.send(result);
            Ok(())
        } else {
            result
        }
    }

    pub(super) fn finish(&mut self) {
        if let Some((_, Some(complete))) = self.pending.take() {
            let _ = complete.send(Err("terminal has exited".into()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminals::screen::TerminalScreen;
    use agent_protocol::operations::TerminalSize;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn query_replies_wait_for_input_ack_and_are_sent_once_in_order() {
        let mut input = InputQueue::default();
        let mut screen = TerminalScreen::new(TerminalSize { rows: 24, cols: 80 });
        let (complete, completed) = oneshot::channel();
        let user_id = input.begin(Some(complete)).unwrap();
        input.extend_replies(screen.feed(b"\x1b[2;3H\x1b[6n\x1b[4;5H\x1b[6n\x1b[5n"));
        assert!(input.next_reply().unwrap().is_none());
        assert!(!input.is_idle());
        input.acknowledge(user_id, None).unwrap();
        assert_eq!(completed.await.unwrap(), Ok(()));
        for (offset, expected) in [b"\x1b[2;3R".as_slice(), b"\x1b[4;5R", b"\x1b[0n"]
            .into_iter()
            .enumerate()
        {
            let Some(PtyCommand::Write { id, data }) = input.next_reply().unwrap() else {
                panic!("query reply missing");
            };
            assert_eq!(id, user_id + offset as u64 + 1);
            assert_eq!(data, expected);
            assert!(input.next_reply().unwrap().is_none());
            input.acknowledge(id, None).unwrap();
        }
        assert!(input.is_idle());
        assert!(input.next_reply().unwrap().is_none());
        input.extend_replies(screen.feed(b"ordinary output"));
        assert!(input.is_idle());
    }

    #[tokio::test]
    async fn user_input_errors_preserve_replies_but_query_errors_fail_the_terminal() {
        let mut input = InputQueue::default();
        let (complete, completed) = oneshot::channel();
        let user_id = input.begin(Some(complete)).unwrap();
        input.extend_replies(vec![b"reply".to_vec()]);
        input
            .acknowledge(user_id, Some("write failed".into()))
            .unwrap();
        assert_eq!(completed.await.unwrap(), Err("write failed".into()));
        let Some(PtyCommand::Write { id, data }) = input.next_reply().unwrap() else {
            panic!("query reply missing");
        };
        assert_eq!(data, b"reply");
        assert_eq!(
            input.acknowledge(id, Some("closed".into())),
            Err("closed".into())
        );
        assert_eq!(
            input.acknowledge(id, None),
            Err("unexpected terminal acknowledgement".into())
        );
    }

    #[tokio::test]
    async fn attachment_ack_does_not_release_input_and_exit_completes_the_receipt() {
        let mut input = InputQueue::default();
        let (complete, completed) = oneshot::channel();
        input.begin(Some(complete)).unwrap();
        input.extend_replies(vec![b"reply".to_vec()]);
        input.acknowledge(0, None).unwrap();
        assert!(input.next_reply().unwrap().is_none());
        input.finish();
        assert_eq!(completed.await.unwrap(), Err("terminal has exited".into()));
    }
}
