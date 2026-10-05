use crate::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UsageStatus {
    Complete,
    Partial,
    Unavailable,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnTokenUsage {
    pub status: UsageStatus,
    pub has_subagents: bool,
    pub input: Option<u64>,
    pub cached_input: Option<u64>,
    pub cache_creation: Option<u64>,
    pub output: Option<u64>,
    pub reasoning: Option<u64>,
}
impl TurnTokenUsage {
    pub fn unavailable(has_subagents: bool) -> Self {
        Self {
            status: UsageStatus::Unavailable,
            has_subagents,
            input: None,
            cached_input: None,
            cache_creation: None,
            output: None,
            reasoning: None,
        }
    }
}
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageCounters {
    pub input: u64,
    pub cached_input: u64,
    pub cache_creation: Option<u64>,
    pub output: u64,
    pub reasoning: u64,
}
impl UsageCounters {
    pub fn observed(&self) -> bool {
        self.input > 0 || self.cached_input > 0 || self.output > 0 || self.reasoning > 0
    }
}
pub fn add_usage(previous: Option<&UsageCounters>, delta: &UsageCounters) -> UsageCounters {
    let empty = UsageCounters {
        cache_creation: Some(0),
        ..UsageCounters::default()
    };
    let previous = previous.unwrap_or(&empty);
    UsageCounters {
        input: previous.input + delta.input,
        cached_input: previous.cached_input + delta.cached_input,
        cache_creation: previous
            .cache_creation
            .zip(delta.cache_creation)
            .map(|(total, delta)| total + delta),
        output: previous.output + delta.output,
        reasoning: previous.reasoning + delta.reasoning,
    }
}
pub fn codex_usage_delta(
    previous: Option<&UsageCounters>,
    current: &UsageCounters,
    last: &UsageCounters,
) -> UsageCounters {
    let Some(previous) = previous.filter(|previous| {
        current.input >= previous.input
            && current.cached_input >= previous.cached_input
            && current.output >= previous.output
            && current.reasoning >= previous.reasoning
    }) else {
        return last.clone();
    };
    UsageCounters {
        input: current.input - previous.input,
        cached_input: current.cached_input - previous.cached_input,
        cache_creation: current
            .cache_creation
            .zip(previous.cache_creation)
            .filter(|(current, previous)| current >= previous)
            .map(|(current, previous)| current - previous),
        output: current.output - previous.output,
        reasoning: current.reasoning - previous.reasoning,
    }
}
pub fn complete_codex_usage(
    usage: Option<&UsageCounters>,
    observed: bool,
    completed: bool,
    has_subagents: bool,
) -> TurnTokenUsage {
    let Some(usage) = usage.filter(|_| observed) else {
        return TurnTokenUsage::unavailable(has_subagents);
    };
    TurnTokenUsage {
        status: if completed {
            UsageStatus::Complete
        } else {
            UsageStatus::Partial
        },
        has_subagents,
        input: Some(usage.input),
        cached_input: Some(usage.cached_input.min(usage.input)),
        cache_creation: usage
            .cache_creation
            .map(|creation| creation.min(usage.input)),
        output: Some(usage.output),
        reasoning: Some(usage.reasoning.min(usage.output)),
    }
}
pub fn normalize_claude_turn_usage(
    subtype: &str,
    usage: Option<&Value>,
    terminal: RunStatus,
) -> TurnTokenUsage {
    let Some(usage) = usage.filter(|usage| usage.is_object()) else {
        return TurnTokenUsage::unavailable(false);
    };
    let integer = |value: &Value| {
        value
            .as_f64()
            .filter(|value| value.is_finite() && *value >= 0.0)
            .map(|value| value.round() as u64)
    };
    let input = integer(&usage["input_tokens"]);
    let cached = integer(&usage["cache_read_input_tokens"]);
    let creation = integer(&usage["cache_creation_input_tokens"]);
    let output = integer(&usage["output_tokens"]);
    let thinking = integer(&usage["output_tokens_details"]["thinking_tokens"]);
    let cached_contribution = if usage["cache_read_input_tokens"].is_null() {
        Some(0)
    } else {
        cached
    };
    let creation_contribution = if usage["cache_creation_input_tokens"].is_null() {
        Some(0)
    } else {
        creation
    };
    let total_input = input
        .zip(cached_contribution)
        .zip(creation_contribution)
        .map(|((input, cached), creation)| input + cached + creation);
    if [input, cached, creation, output]
        .iter()
        .all(Option::is_none)
        || subtype != "success"
            && [input, cached, creation, output]
                .into_iter()
                .flatten()
                .sum::<u64>()
                == 0
    {
        return TurnTokenUsage::unavailable(false);
    }
    TurnTokenUsage {
        status: if terminal == RunStatus::Completed
            && subtype == "success"
            && total_input.is_some()
            && output.is_some()
        {
            UsageStatus::Complete
        } else {
            UsageStatus::Partial
        },
        has_subagents: false,
        input: total_input,
        cached_input: cached,
        cache_creation: creation,
        output,
        reasoning: thinking
            .zip(output)
            .map(|(thinking, output)| thinking.min(output)),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn counters(input: u64, cached: u64, output: u64) -> UsageCounters {
        UsageCounters {
            input,
            cached_input: cached,
            cache_creation: None,
            output,
            reasoning: 0,
        }
    }
    fn add(accumulated: &mut UsageCounters, delta: &UsageCounters) {
        accumulated.input += delta.input;
        accumulated.cached_input += delta.cached_input;
        accumulated.output += delta.output;
        accumulated.reasoning += delta.reasoning;
        accumulated.cache_creation = accumulated
            .cache_creation
            .zip(delta.cache_creation)
            .map(|(total, delta)| total + delta);
    }
    #[test]
    fn codex_counts_each_response_once_and_excludes_resumed_history() {
        let first = counters(1010, 505, 102);
        let last = counters(10, 5, 2);
        let mut accumulator = counters(0, 0, 0);
        add(&mut accumulator, &codex_usage_delta(None, &first, &last));
        add(
            &mut accumulator,
            &codex_usage_delta(Some(&first), &first, &last),
        );
        add(
            &mut accumulator,
            &codex_usage_delta(
                Some(&first),
                &counters(1030, 515, 108),
                &counters(20, 10, 6),
            ),
        );
        assert_eq!(
            complete_codex_usage(Some(&accumulator), true, true, false),
            TurnTokenUsage {
                status: UsageStatus::Complete,
                has_subagents: false,
                input: Some(30),
                cached_input: Some(15),
                cache_creation: None,
                output: Some(8),
                reasoning: Some(0)
            }
        );
    }
    #[test]
    fn codex_keeps_usage_when_compaction_resets_cumulative_counters() {
        let previous = counters(100, 40, 20);
        let current = counters(20, 5, 3);
        let mut accumulator = previous.clone();
        add(
            &mut accumulator,
            &codex_usage_delta(Some(&previous), &current, &current),
        );
        let usage = complete_codex_usage(Some(&accumulator), true, false, true);
        assert_eq!(usage.status, UsageStatus::Partial);
        assert!(usage.has_subagents);
        assert_eq!(usage.input, Some(120));
        assert_eq!(usage.cached_input, Some(45));
        assert_eq!(usage.output, Some(23));
        assert_eq!(
            complete_codex_usage(None, false, false, false).status,
            UsageStatus::Unavailable
        );
    }
    #[test]
    fn claude_cached_input_and_thinking_match_reference_billing() {
        let usage = normalize_claude_turn_usage(
            "success",
            Some(
                &json!({"input_tokens":100,"cache_read_input_tokens":40,"cache_creation_input_tokens":10,"output_tokens":20,"output_tokens_details":{"thinking_tokens":25}}),
            ),
            RunStatus::Completed,
        );
        assert_eq!(
            usage,
            TurnTokenUsage {
                status: UsageStatus::Complete,
                has_subagents: false,
                input: Some(150),
                cached_input: Some(40),
                cache_creation: Some(10),
                output: Some(20),
                reasoning: Some(20)
            }
        );
        assert_eq!(
            normalize_claude_turn_usage(
                "error_during_execution",
                Some(&json!({"input_tokens":0,"output_tokens":0})),
                RunStatus::Failed
            )
            .status,
            UsageStatus::Unavailable
        );
        let usage = normalize_claude_turn_usage(
            "success",
            Some(&json!({"input_tokens":4,"output_tokens":2})),
            RunStatus::Interrupted,
        );
        assert_eq!(usage.status, UsageStatus::Partial);
        assert_eq!(usage.input, Some(4));
        assert_eq!(usage.output, Some(2));
    }
}
