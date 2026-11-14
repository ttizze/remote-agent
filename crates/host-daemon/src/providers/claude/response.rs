//! Response boundaries and input ownership are independent of timeline IO.
mod background;
mod roster;
use super::native;
use agent_protocol::{execution::BackgroundTask, ids::ItemId};
use background::Reports;
use roster::Roster;
pub(super) use roster::task_key;
use serde_json::Value;
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InputDelivery {
    NewTurn,
    CurrentTurn,
    Background,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MessageDisposition {
    Forward,
    Applied,
    Deferred,
    Failed,
    Interrupted,
}

pub(super) struct Observation<'a> {
    pub echoes: Vec<(&'a str, bool)>,
    pub disposition: MessageDisposition,
    /// Claude started a response on its own after its answer, as it does for
    /// background work: as T3 Code, that response is a turn of its own.
    pub woke: bool,
}

#[derive(Default)]
enum Interruption {
    #[default]
    None,
    Pending(String),
    Confirmed,
}

#[derive(Default)]
pub(super) struct ResponseState {
    interruption: Interruption,
    /// Input that joins the response at once ends what it was doing; that
    /// stop is not the response's outcome.
    steered: bool,
    pending_inputs: HashSet<ItemId>,
    result_received: bool,
    idle: bool,
    background: Reports,
    roster: Roster,
}

impl ResponseState {
    pub(super) fn interrupted(&self) -> bool {
        !matches!(self.interruption, Interruption::None)
    }
    pub(super) fn pending_interrupt(&self) -> Option<&str> {
        match &self.interruption {
            Interruption::Pending(id) => Some(id),
            _ => None,
        }
    }
    pub(super) fn interrupt(&mut self, id: &str) -> bool {
        if matches!(self.interruption, Interruption::Confirmed) {
            return false;
        }
        self.interruption = Interruption::Pending(id.into());
        true
    }
    /// A refused stop leaves the response and its background roster running.
    /// An acknowledgement from an older request cannot settle a later retry.
    pub(super) fn interrupt_result(
        &mut self,
        id: &str,
        error: Option<String>,
    ) -> Option<Result<(), String>> {
        if self.pending_interrupt() != Some(id) {
            return None;
        }
        self.interruption = if error.is_none() {
            Interruption::Confirmed
        } else {
            Interruption::None
        };
        Some(error.map_or(Ok(()), Err))
    }

    pub(super) fn answered(&self) -> bool {
        self.result_received && self.idle && self.pending_inputs.is_empty()
    }

    pub(super) fn finished(&self) -> bool {
        self.answered() && (!self.background.pending() || self.interrupted())
    }

    /// Whether Claude still runs work that a new process would end.
    pub(super) fn has_background(&self) -> bool {
        !self.roster.tasks().is_empty()
    }

    /// A task the roster names, before its notification removes it.
    pub(super) fn background_task(&self, id: &str) -> Option<&BackgroundTask> {
        self.roster.tasks().iter().find(|task| task.id == id)
    }

    /// Claude answered; input that awaits its turn, or a stop, owns the
    /// response instead.
    fn answered_input(&self) -> bool {
        self.result_received && self.pending_inputs.is_empty() && !self.interrupted()
    }

    pub(super) fn background_tasks(&self) -> &[BackgroundTask] {
        if matches!(self.interruption, Interruption::Confirmed) {
            &[]
        } else {
            self.roster.tasks()
        }
    }

    /// As T3 Code, Claude's answer ends its turn while background work it
    /// reports on later keeps the process running.
    pub(super) fn settles(&self) -> bool {
        self.answered_input() && (self.has_background() || self.background.pending())
    }

    pub(super) fn delivery(&self) -> InputDelivery {
        if self.background.pending() {
            InputDelivery::Background
        } else if self.answered() && !self.interrupted() {
            InputDelivery::NewTurn
        } else {
            InputDelivery::CurrentTurn
        }
    }

    pub(super) fn accept(&mut self, id: ItemId) {
        self.pending_inputs.insert(id);
    }

    pub(super) fn steer(&mut self) {
        self.steered = true;
    }

    pub(super) fn observe<'a>(&mut self, message: &'a Value) -> Result<Observation<'a>, String> {
        self.roster.observe(message);
        let background_level = self.observe_background(message)?;
        let answered = self.answered_input();
        let echoes = self.consume_echoes(message);
        let disposition = if background_level {
            MessageDisposition::Applied
        } else {
            self.transition(message, !echoes.is_empty())
        };
        Ok(Observation {
            woke: answered && echoes.is_empty() && !self.result_received,
            echoes,
            disposition,
        })
    }

    fn consume_echoes<'a>(&mut self, message: &'a Value) -> Vec<(&'a str, bool)> {
        if !message["parent_tool_use_id"].is_null() {
            return Vec::new();
        }
        // Reply frames can echo a batch before the individual user
        // replays. Consume every UUID in SDK consumption order.
        let echoes: Vec<_> = if message["type"] == "user" {
            message["uuid"].as_str().into_iter().collect()
        } else if let Some(ids) = message["user_message_uuids"].as_array() {
            ids.iter().filter_map(Value::as_str).collect()
        } else {
            message["user_message_uuid"].as_str().into_iter().collect()
        };
        let consumed: Vec<_> = echoes
            .into_iter()
            .filter(|id| self.pending_inputs.remove(*id))
            .collect();
        // Claude runs input it did not fold into a response as the next prompt,
        // right after the result and without an idle level. As in the
        // transcript, input consumed after a result starts its own turn.
        let opens_turn = self.result_received && !self.interrupted();
        consumed
            .into_iter()
            .enumerate()
            .map(|(index, id)| (id, opens_turn && index == 0))
            .collect()
    }

    fn observe_background(&mut self, message: &Value) -> Result<bool, String> {
        if message["type"] != "system" {
            return Ok(false);
        }
        match message["subtype"].as_str() {
            Some("task_notification") => {
                let id = task_key(message).ok_or("Claude task notification ID is missing")?;
                self.background.notified(
                    id,
                    message["ambient"] == true || message["skip_transcript"] == true,
                );
            }
            Some("task_started") => {
                let id = task_key(message).ok_or("Claude task start ID is missing")?;
                let report = native::needs_background_report(
                    message["task_type"].as_str(),
                    message["is_backgrounded"] != false,
                    message["ambient"] == true || message["skip_transcript"] == true,
                );
                self.background.observe([(id.to_owned(), report)]);
            }
            Some("background_tasks_changed") => {
                let tasks = message["tasks"]
                    .as_array()
                    .ok_or("Claude background task level is missing")?
                    .iter()
                    .map(|task| {
                        let id = task_key(task).ok_or("Claude background task ID is missing")?;
                        Ok::<_, String>((
                            id.to_owned(),
                            native::needs_background_report(
                                task["task_type"].as_str(),
                                true,
                                task["ambient"] == true,
                            ),
                        ))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                self.background.observe(tasks);
                return Ok(true);
            }
            _ => {}
        }
        Ok(false)
    }

    fn transition(&mut self, message: &Value, consumed: bool) -> MessageDisposition {
        let kind = message["type"].as_str().unwrap_or_default();
        match kind {
            "system" if message["subtype"] == "session_state_changed" => {
                if self.idle && message["state"] == "running" && self.result_received {
                    if !self.pending_inputs.is_empty() {
                        // A turn-start level can precede the input echo.
                        // Keep the prior terminal boundary until its owner is known.
                        return MessageDisposition::Deferred;
                    }
                    self.background.response_started(None);
                }
                self.idle = message["state"] == "idle";
            }
            "result" => {
                // A steer aborts the streaming or tools it interrupts and the
                // response continues with it, as T3 Code steers Claude.
                let aborted = matches!(
                    message["terminal_reason"].as_str(),
                    Some("aborted_streaming" | "aborted_tools")
                );
                if self.steered && aborted && !self.interrupted() {
                    return MessageDisposition::Applied;
                }
                self.steered = false;
                // Claude Code ends a stopped response, including one awaiting
                // an answer, with an execution error. The stop is its outcome,
                // as in T3 Code.
                if message["is_error"] == true && !self.interrupted() {
                    return MessageDisposition::Failed;
                }
                if message["origin"]["kind"] == "task-notification" {
                    self.background
                        .response_started(message["origin"]["runId"].as_str());
                }
                self.background.response_finished();
                self.result_received = true;
                if self.interrupted() {
                    return MessageDisposition::Interrupted;
                }
            }
            _ => {
                let notification =
                    kind == "user" && message["origin"]["kind"] == "task-notification";
                let started = message["parent_tool_use_id"].is_null()
                    && (kind == "assistant"
                        || (kind == "system"
                            && message["subtype"] == "init"
                            && self.pending_inputs.is_empty())
                        || (kind == "stream_event" && message["event"]["type"] == "message_start"));
                if notification {
                    self.background
                        .response_started(message["origin"]["runId"].as_str());
                } else if started && !consumed && self.result_received {
                    self.background.response_started(None);
                }
                if consumed || started || (notification && message["parent_tool_use_id"].is_null())
                {
                    self.result_received = false;
                    self.idle = false;
                }
                return MessageDisposition::Forward;
            }
        }
        MessageDisposition::Applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn observe(response: &mut ResponseState, message: Value) -> MessageDisposition {
        response.observe(&message).unwrap().disposition
    }

    fn settle(response: &mut ResponseState) {
        assert_eq!(
            observe(response, json!({"type":"result"})),
            MessageDisposition::Applied
        );
        assert_eq!(
            observe(
                response,
                json!({"type":"system","subtype":"session_state_changed","state":"idle"})
            ),
            MessageDisposition::Applied
        );
    }

    #[test]
    fn a_task_roster_survives_later_foreground_input_until_completion_or_stop() {
        let ids = |response: &ResponseState| {
            response
                .background_tasks()
                .iter()
                .map(|task| task.id.clone())
                .collect::<Vec<_>>()
        };
        let mut response = ResponseState::default();
        observe(
            &mut response,
            json!({"type":"system","subtype":"task_started","task_id":"agent","task_type":"local_agent"}),
        );
        assert!(response.has_background());
        assert_eq!(
            ids(&response),
            ["agent"],
            "the Host tracks ownership while Claude responds"
        );
        // A background subagent keeps the SDK from going idle after the result.
        observe(&mut response, json!({"type":"result"}));
        assert_eq!(ids(&response), ["agent"]);
        response.accept("queued".into());
        assert_eq!(ids(&response), ["agent"]);
        observe(&mut response, json!({"type":"user","uuid":"queued"}));
        assert_eq!(ids(&response), ["agent"]);
        observe(&mut response, json!({"type":"result"}));
        assert_eq!(ids(&response), ["agent"]);
        observe(
            &mut response,
            json!({"type":"system","subtype":"task_notification","task_id":"agent"}),
        );
        assert!(ids(&response).is_empty());
        assert!(!response.has_background());

        let mut stopped = ResponseState::default();
        observe(
            &mut stopped,
            json!({"type":"system","subtype":"task_started","task_id":"shell","task_type":"local_bash"}),
        );
        settle(&mut stopped);
        assert_eq!(ids(&stopped), ["shell"]);
        stopped.interrupt("first");
        assert_eq!(
            ids(&stopped),
            ["shell"],
            "a pending stop must keep the recoverable roster"
        );
        assert!(
            stopped
                .interrupt_result("first", Some("refused".into()))
                .unwrap()
                .is_err()
        );
        assert_eq!(ids(&stopped), ["shell"]);
        stopped.interrupt("retry");
        assert!(
            stopped.interrupt_result("first", None).is_none(),
            "an old acknowledgement cannot settle the retry"
        );
        assert_eq!(ids(&stopped), ["shell"]);
        stopped.interrupt_result("retry", None).unwrap().unwrap();
        stopped.interrupt("duplicate");
        assert!(
            stopped
                .interrupt_result("duplicate", Some("late refusal".into()))
                .is_none()
        );
        assert!(
            stopped.interrupted(),
            "a repeated stop cannot undo an acknowledged interruption"
        );
        assert!(
            ids(&stopped).is_empty(),
            "an acknowledged stop ends the wait"
        );
    }

    #[test]
    fn pending_input_keeps_the_prior_boundary_until_its_echo() {
        let mut response = ResponseState::default();
        observe(
            &mut response,
            json!({"type":"system","subtype":"background_tasks_changed","tasks":[{"run_id":"shell","task_type":"local_bash"}]}),
        );
        settle(&mut response);
        assert_eq!(response.delivery(), InputDelivery::Background);
        response.accept("queued".into());
        assert!(!response.answered());
        assert_eq!(
            observe(
                &mut response,
                json!({"type":"system","subtype":"session_state_changed","state":"running"})
            ),
            MessageDisposition::Deferred
        );
        let echo = json!({"type":"user","uuid":"queued"});
        let observation = response.observe(&echo).unwrap();
        assert_eq!(observation.echoes, [("queued", true)]);
        assert_eq!(observation.disposition, MessageDisposition::Forward);
        assert!(!response.answered());
        settle(&mut response);
        assert!(response.answered());
        assert!(!response.finished());
    }

    #[test]
    fn parent_responses_and_nested_activity_have_distinct_boundaries() {
        for (message, stays_settled) in [
            (json!({"type":"assistant"}), false),
            (
                json!({"type":"assistant","parent_tool_use_id":"child"}),
                true,
            ),
            (
                json!({"type":"stream_event","event":{"type":"message_start"}}),
                false,
            ),
            (
                json!({"type":"stream_event","event":{"type":"content_block_delta"}}),
                true,
            ),
            (
                json!({"type":"user","origin":{"kind":"task-notification"}}),
                false,
            ),
            (
                json!({"type":"user","origin":{"kind":"task-notification"},"parent_tool_use_id":"child"}),
                true,
            ),
            (json!({"type":"user"}), true),
        ] {
            let mut response = ResponseState::default();
            settle(&mut response);
            assert_eq!(response.delivery(), InputDelivery::NewTurn);
            assert_eq!(observe(&mut response, message), MessageDisposition::Forward);
            assert_eq!(response.answered(), stays_settled);
        }
    }

    #[test]
    fn background_work_needs_its_own_report_and_errors_do_not_finish_it() {
        let mut response = ResponseState::default();
        assert_eq!(response.delivery(), InputDelivery::CurrentTurn);
        observe(
            &mut response,
            json!({"type":"system","subtype":"background_tasks_changed","tasks":[{"run_id":"shell","task_type":"local_bash"}]}),
        );
        settle(&mut response);
        assert!(!response.finished());
        assert_eq!(
            observe(
                &mut response,
                json!({"type":"result","is_error":true,"origin":{"kind":"task-notification","runId":"shell"}})
            ),
            MessageDisposition::Failed
        );
        assert!(!response.finished());
        assert_eq!(
            observe(
                &mut response,
                json!({"type":"result","origin":{"kind":"task-notification","runId":"shell"}})
            ),
            MessageDisposition::Applied
        );
        assert!(response.finished());
        response.accept("pending".into());
        response.interrupt("interrupt");
        assert!(
            response
                .interrupt_result("interrupt", Some("not interrupted".into()))
                .unwrap()
                .is_err()
        );
        assert!(!response.interrupted());
        response.interrupt("interrupt");
        response
            .interrupt_result("interrupt", None)
            .unwrap()
            .unwrap();
        assert!(response.interrupted());
        assert_eq!(
            observe(&mut response, json!({"type":"result"})),
            MessageDisposition::Interrupted
        );
    }

    #[test]
    fn a_requested_stop_owns_the_error_result_unless_it_is_refused() {
        let mut stopped = ResponseState::default();
        stopped.interrupt("interrupt");
        assert_eq!(
            observe(&mut stopped, json!({"type":"result","is_error":true})),
            MessageDisposition::Interrupted
        );
        let mut refused = ResponseState::default();
        refused.interrupt("interrupt");
        assert!(
            refused
                .interrupt_result("interrupt", Some("refused".into()))
                .unwrap()
                .is_err()
        );
        assert!(!refused.interrupted());
        assert_eq!(
            observe(&mut refused, json!({"type":"result","is_error":true})),
            MessageDisposition::Failed
        );
    }

    #[test]
    fn background_notices_are_answered_by_the_next_foreground_response() {
        let mut response = ResponseState::default();
        observe(
            &mut response,
            json!({"type":"system","subtype":"task_started","task_id":"shell","task_type":"local_bash"}),
        );
        assert_eq!(response.delivery(), InputDelivery::Background);
        // The foreground result settles before Claude answers a task notice.
        settle(&mut response);
        observe(
            &mut response,
            json!({"type":"system","subtype":"task_notification","task_id":"shell"}),
        );
        observe(&mut response, json!({"type":"assistant"}));
        settle(&mut response);
        assert!(response.finished());
        assert!(
            response
                .observe(&json!({"type":"system","subtype":"task_started"}))
                .is_err()
        );
    }

    #[test]
    fn delivery_waits_for_echoes_and_interruption_keeps_input_in_the_current_turn() {
        let mut response = ResponseState::default();
        response.accept("input".into());
        settle(&mut response);
        assert_eq!(response.delivery(), InputDelivery::CurrentTurn);
        observe(&mut response, json!({"type":"user","uuid":"input"}));
        settle(&mut response);
        assert_eq!(response.delivery(), InputDelivery::NewTurn);
        response.interrupt("interrupt");
        assert_eq!(response.delivery(), InputDelivery::CurrentTurn);
    }

    #[test]
    fn input_sent_while_answering_opens_a_turn_only_when_run_after_the_result() {
        let mut response = ResponseState::default();
        assert_eq!(response.delivery(), InputDelivery::CurrentTurn);
        response.accept("folded".into());
        response.accept("next".into());
        let folded = json!({"type":"user","uuid":"folded"});
        assert_eq!(
            response.observe(&folded).unwrap().echoes,
            [("folded", false)]
        );
        // Claude Code starts the next prompt right after the result, without idle.
        observe(&mut response, json!({"type":"result"}));
        let next = json!({"type":"system","subtype":"init","user_message_uuids":["next"]});
        assert_eq!(response.observe(&next).unwrap().echoes, [("next", true)]);
        assert!(!response.answered());
    }

    #[test]
    fn a_batch_consumes_each_pending_echo_once_in_sdk_order_and_opens_one_turn() {
        let mut response = ResponseState::default();
        observe(
            &mut response,
            json!({"type":"system","subtype":"task_started","task_id":"shell","task_type":"local_bash"}),
        );
        settle(&mut response);
        response.accept("first".into());
        response.accept("second".into());
        let message =
            json!({"type":"assistant","user_message_uuids":["unknown","second","first","second"]});
        let observation = response.observe(&message).unwrap();
        assert_eq!(observation.echoes, [("second", true), ("first", false)]);
        assert_eq!(observation.disposition, MessageDisposition::Forward);
        assert!(!response.answered());
    }

    #[test]
    fn interrupted_background_input_never_opens_a_turn_and_does_not_owe_a_report() {
        let mut response = ResponseState::default();
        observe(
            &mut response,
            json!({"type":"system","subtype":"task_started","task_id":"shell","task_type":"local_bash"}),
        );
        settle(&mut response);
        response.accept("queued".into());
        response.interrupt("interrupt");
        let message = json!({"type":"user","uuid":"queued"});
        assert_eq!(
            response.observe(&message).unwrap().echoes,
            [("queued", false)]
        );
        assert_eq!(
            observe(&mut response, json!({"type":"result"})),
            MessageDisposition::Interrupted
        );
        assert!(!response.finished());
        observe(
            &mut response,
            json!({"type":"system","subtype":"session_state_changed","state":"idle"}),
        );
        assert!(response.finished());
    }

    #[test]
    fn running_without_pending_input_starts_the_response_to_a_task_notice() {
        let mut response = ResponseState::default();
        observe(
            &mut response,
            json!({"type":"system","subtype":"task_started","task_id":"shell","task_type":"local_bash"}),
        );
        settle(&mut response);
        observe(
            &mut response,
            json!({"type":"system","subtype":"task_notification","task_id":"shell"}),
        );
        assert_eq!(
            observe(
                &mut response,
                json!({"type":"system","subtype":"session_state_changed","state":"running"})
            ),
            MessageDisposition::Applied
        );
        assert!(!response.answered());
        settle(&mut response);
        assert!(response.finished());
    }

    #[test]
    fn silent_task_notices_finish_without_another_response() {
        for flag in ["ambient", "skip_transcript"] {
            let mut response = ResponseState::default();
            observe(
                &mut response,
                json!({"type":"system","subtype":"task_started","task_id":"shell","task_type":"local_bash"}),
            );
            settle(&mut response);
            assert!(!response.finished());
            let mut notice =
                json!({"type":"system","subtype":"task_notification","task_id":"shell"});
            notice[flag] = true.into();
            observe(&mut response, notice);
            assert!(response.finished());
        }
    }

    #[test]
    fn silent_and_foreground_tasks_do_not_require_a_background_report() {
        for message in [
            json!({"type":"system","subtype":"task_started","task_id":"shell","task_type":"local_bash","ambient":true}),
            json!({"type":"system","subtype":"task_started","task_id":"shell","task_type":"local_bash","skip_transcript":true}),
            json!({"type":"system","subtype":"task_started","task_id":"shell","task_type":"local_bash","is_backgrounded":false}),
            json!({"type":"system","subtype":"background_tasks_changed","tasks":[{"run_id":"shell","task_type":"local_bash","ambient":true}]}),
        ] {
            let mut response = ResponseState::default();
            observe(&mut response, message);
            settle(&mut response);
            assert!(response.finished());
            assert_eq!(response.delivery(), InputDelivery::NewTurn);
        }
    }

    #[test]
    fn a_queued_human_reply_does_not_answer_a_task_notice() {
        let mut response = ResponseState::default();
        observe(
            &mut response,
            json!({"type":"system","subtype":"task_started","task_id":"shell","task_type":"local_bash"}),
        );
        settle(&mut response);
        observe(
            &mut response,
            json!({"type":"system","subtype":"task_notification","task_id":"shell"}),
        );
        // Idle is a repeated level, not the start of the task's response.
        observe(
            &mut response,
            json!({"type":"system","subtype":"session_state_changed","state":"idle"}),
        );
        response.accept("human".into());
        let reply = json!({"type":"assistant","user_message_uuid":"human"});
        assert_eq!(response.observe(&reply).unwrap().echoes, [("human", true)]);
        settle(&mut response);
        assert!(!response.finished());
    }

    #[test]
    fn init_starts_a_response_when_no_input_is_waiting_for_its_echo() {
        let mut response = ResponseState::default();
        settle(&mut response);
        assert_eq!(
            observe(&mut response, json!({"type":"system","subtype":"init"})),
            MessageDisposition::Forward
        );
        assert!(!response.answered());
    }

    #[test]
    fn init_preserves_a_pending_inputs_terminal_boundary_until_its_echo() {
        let mut response = ResponseState::default();
        observe(
            &mut response,
            json!({"type":"system","subtype":"task_started","task_id":"shell","task_type":"local_bash"}),
        );
        settle(&mut response);
        response.accept("queued".into());
        observe(&mut response, json!({"type":"system","subtype":"init"}));
        let echo = json!({"type":"user","uuid":"queued"});
        assert_eq!(response.observe(&echo).unwrap().echoes, [("queued", true)]);
    }

    #[test]
    fn a_steer_continues_the_response_its_abort_stopped() {
        for (reason, continues) in [
            ("aborted_tools", true),
            ("aborted_streaming", true),
            ("completed", false),
        ] {
            let mut response = ResponseState::default();
            observe(&mut response, json!({"type":"assistant"}));
            response.accept("steer".into());
            response.steer();
            let aborted = json!({"type":"result","is_error":true,"terminal_reason":reason});
            let disposition = observe(&mut response, aborted);
            if !continues {
                assert_eq!(disposition, MessageDisposition::Failed);
                continue;
            }
            assert_eq!(disposition, MessageDisposition::Applied);
            // The steer joins the response it stopped instead of opening a turn.
            let echo = json!({"type":"user","uuid":"steer"});
            assert_eq!(response.observe(&echo).unwrap().echoes, [("steer", false)]);
            settle(&mut response);
            assert!(response.finished());
            // Only the first abort after a steer belongs to it.
            observe(&mut response, json!({"type":"assistant"}));
            let again = json!({"type":"result","is_error":true,"terminal_reason":reason});
            assert_eq!(observe(&mut response, again), MessageDisposition::Failed);
        }
    }

    #[test]
    fn a_stop_after_a_steer_still_ends_the_response() {
        let mut response = ResponseState::default();
        observe(&mut response, json!({"type":"assistant"}));
        response.steer();
        response.interrupt("interrupt");
        let aborted = json!({"type":"result","is_error":true,"terminal_reason":"aborted_tools"});
        assert_eq!(
            observe(&mut response, aborted),
            MessageDisposition::Interrupted
        );
    }
}
