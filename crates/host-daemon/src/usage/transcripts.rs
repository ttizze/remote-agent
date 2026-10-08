//! Pure, line-oriented readers for the two supported native transcript types.
use agent_protocol::usage::{Provider, TokenTotals};
use chrono::DateTime;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum Speed {
    Standard,
    Fast,
    Ultrafast,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Record {
    pub provider: Provider,
    pub timestamp_ms: i64,
    pub model: String,
    pub session_id: String,
    pub totals: TokenTotals,
    pub reported_cost_usd: Option<f64>,
    pub speed: Speed,
    pub dedupe_key: Option<String>,
}

pub(crate) fn total_tokens(totals: &TokenTotals) -> u64 {
    totals.total()
}

pub(crate) fn might_carry_usage(line: &str, provider: Provider) -> bool {
    match provider {
        Provider::Claude => line.contains("\"usage\""),
        Provider::Codex => {
            line.contains("\"token_count\"")
                || line.contains("\"turn_context\"")
                || line.contains("\"thread_settings_applied\"")
                || line.contains("\"session_meta\"")
        }
    }
}

fn positive_int(value: Option<&Value>) -> u64 {
    value
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value > 0.)
        .map(|value| value.trunc() as u64)
        .unwrap_or_default()
}

fn timestamp_ms(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(Value::String(value)) => DateTime::parse_from_rfc3339(value)
            .ok()
            .map(|value| value.timestamp_millis()),
        Some(Value::Number(value)) => {
            let value = value.as_f64()?;
            if !value.is_finite() {
                return None;
            }
            Some(if value.abs() < 1.0e12 {
                (value * 1000.).round() as i64
            } else {
                value.round() as i64
            })
        }
        _ => None,
    }
}

pub(crate) fn parse_claude_line(line: &str) -> Option<Record> {
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|value| parse_claude_value(&value))
}

pub(crate) fn parse_claude_value(value: &Value) -> Option<Record> {
    if value["type"] != "assistant" {
        return None;
    }
    let message = value.get("message")?.as_object()?;
    let usage = message.get("usage")?.as_object()?;
    let timestamp_ms = timestamp_ms(value.get("timestamp"))?;
    let model = message.get("model")?.as_str()?.to_owned();
    if model.is_empty() {
        return None;
    }
    let message_id = message
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let request_id = value
        .get("requestId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let dedupe_key = if message_id.is_empty() && request_id.is_empty() {
        None
    } else {
        Some(format!("{message_id}:{request_id}"))
    };
    let cost = value
        .get("costUSD")
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite());
    Some(Record {
        provider: Provider::Claude,
        timestamp_ms,
        model,
        session_id: value
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        totals: TokenTotals {
            uncached_input_tokens: positive_int(usage.get("input_tokens")),
            cached_input_tokens: positive_int(usage.get("cache_read_input_tokens")),
            cache_creation_tokens: positive_int(usage.get("cache_creation_input_tokens")),
            output_tokens: positive_int(usage.get("output_tokens")),
            reasoning_tokens: 0,
        },
        reported_cost_usd: cost,
        speed: (usage.get("speed").and_then(Value::as_str) == Some("fast"))
            .then_some(Speed::Fast)
            .unwrap_or(Speed::Standard),
        dedupe_key,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct CodexState {
    pub model: String,
    pub speed: SpeedValue,
    pub session_id: String,
    pub last_usage_signature: Option<String>,
    pub saw_session_meta: bool,
    pub suppressing_fork_copies: bool,
    pub fork_copy_anchor_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum SpeedValue {
    Standard,
    Fast,
    Ultrafast,
}

impl Default for CodexState {
    fn default() -> Self {
        Self {
            model: String::new(),
            speed: SpeedValue::Standard,
            session_id: String::new(),
            last_usage_signature: None,
            saw_session_meta: false,
            suppressing_fork_copies: false,
            fork_copy_anchor_ms: 0,
        }
    }
}

impl From<SpeedValue> for Speed {
    fn from(value: SpeedValue) -> Self {
        match value {
            SpeedValue::Standard => Self::Standard,
            SpeedValue::Fast => Self::Fast,
            SpeedValue::Ultrafast => Self::Ultrafast,
        }
    }
}

fn codex_speed(value: Option<&Value>) -> SpeedValue {
    match value.and_then(Value::as_str) {
        Some("priority" | "fast") => SpeedValue::Fast,
        Some("ultrafast") => SpeedValue::Ultrafast,
        _ => SpeedValue::Standard,
    }
}

fn forked_session(payload: &serde_json::Map<String, Value>) -> bool {
    if payload
        .get("forked_from_id")
        .and_then(Value::as_str)
        .is_some()
    {
        return true;
    }
    payload
        .get("source")
        .and_then(Value::as_object)
        .and_then(|source| source.get("subagent"))
        .and_then(Value::as_object)
        .and_then(|subagent| subagent.get("thread_spawn"))
        .and_then(Value::as_object)
        .and_then(|spawn| spawn.get("parent_thread_id"))
        .and_then(Value::as_str)
        .is_some()
}

pub(crate) fn parse_codex_line(line: &str, state: &mut CodexState) -> Option<Record> {
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|value| parse_codex_value(&value, state))
}

pub(crate) fn parse_codex_value(value: &Value, state: &mut CodexState) -> Option<Record> {
    let payload = value.get("payload")?.as_object()?;
    if value.get("type").and_then(Value::as_str) == Some("session_meta") {
        if state.saw_session_meta {
            return None;
        }
        state.saw_session_meta = true;
        if let Some(id) = payload
            .get("id")
            .and_then(Value::as_str)
            .or_else(|| payload.get("session_id").and_then(Value::as_str))
        {
            state.session_id = id.into();
        }
        if forked_session(payload) {
            if let Some(at) = timestamp_ms(value.get("timestamp")) {
                state.suppressing_fork_copies = true;
                state.fork_copy_anchor_ms = at;
            }
        }
        return None;
    }
    if value.get("type").and_then(Value::as_str) == Some("turn_context") {
        if let Some(model) = payload.get("model").and_then(Value::as_str) {
            state.model = model.into();
        }
        return None;
    }
    if payload.get("type").and_then(Value::as_str) == Some("thread_settings_applied") {
        state.speed = codex_speed(
            payload
                .get("thread_settings")
                .and_then(Value::as_object)
                .and_then(|settings| settings.get("service_tier")),
        );
        return None;
    }
    if payload.get("type").and_then(Value::as_str) != Some("token_count") {
        return None;
    }
    let info = payload.get("info")?.as_object()?;
    let usage = info.get("last_token_usage")?.as_object()?;
    let timestamp_ms = timestamp_ms(value.get("timestamp"))?;
    if state.model.is_empty() {
        return None;
    }
    let signature = serde_json::to_string(usage).ok()?;
    if state.last_usage_signature.as_deref() == Some(signature.as_str()) {
        return None;
    }
    state.last_usage_signature = Some(signature);
    if state.suppressing_fork_copies {
        if timestamp_ms.saturating_sub(state.fork_copy_anchor_ms) < 1000 {
            state.fork_copy_anchor_ms = timestamp_ms;
            return None;
        }
        state.suppressing_fork_copies = false;
    }
    let input = positive_int(usage.get("input_tokens"));
    let cached = positive_int(usage.get("cached_input_tokens"));
    let created = positive_int(usage.get("cache_write_input_tokens"));
    let output = positive_int(usage.get("output_tokens"));
    let totals = TokenTotals {
        uncached_input_tokens: input.saturating_sub(cached).saturating_sub(created),
        cached_input_tokens: cached,
        cache_creation_tokens: created,
        output_tokens: output,
        reasoning_tokens: positive_int(usage.get("reasoning_output_tokens")).min(output),
    };
    (total_tokens(&totals) > 0).then(|| Record {
        provider: Provider::Codex,
        timestamp_ms,
        model: state.model.clone(),
        session_id: state.session_id.clone(),
        totals,
        reported_cost_usd: None,
        speed: state.speed.into(),
        dedupe_key: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claude_repeated_content_blocks_share_a_dedupe_key() {
        let value = json!({
            "type":"assistant", "timestamp":"2030-01-01T00:00:00Z", "sessionId":"s",
            "requestId":"r", "message":{"id":"m","model":"claude-test","usage":{"input_tokens":4,"output_tokens":2}}
        });
        let first = parse_claude_value(&value).unwrap();
        let second = parse_claude_value(&value).unwrap();
        assert_eq!(first.dedupe_key, second.dedupe_key);
        assert_eq!(first.totals.total(), 6);
    }

    #[test]
    fn codex_carries_model_and_drops_consecutive_duplicate_usage() {
        let mut state = CodexState::default();
        assert!(
            parse_codex_value(
                &json!({"type":"turn_context","payload":{"model":"gpt-test"}}),
                &mut state
            )
            .is_none()
        );
        let line = json!({"timestamp":"2030-01-01T00:00:01Z","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":10,"cached_input_tokens":4,"output_tokens":3}}}});
        assert!(parse_codex_value(&line, &mut state).is_some());
        assert!(parse_codex_value(&line, &mut state).is_none());
    }
}
