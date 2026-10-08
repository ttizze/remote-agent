//! Read-only local Host background and resource diagnostics tools.
use super::{AgentTools, Outcome, Scope, bounded, failure, invalid};
use agent_protocol::background::{ReadProcessResourceHistory, ReadTraceDiagnostics};
use serde_json::{Value, to_value};

fn encoded<T: serde::Serialize>(value: T) -> Outcome {
    to_value(value).map_err(|error| failure("diagnostics_unavailable", error.to_string()))
}

impl AgentTools {
    pub(crate) async fn background_status(&self, _scope: Scope<'_>) -> Outcome {
        encoded(
            self.backend
                .background_policy()
                .await
                .map_err(|error| failure("diagnostics_unavailable", error))?,
        )
    }

    pub(crate) async fn host_resources(&self, _scope: Scope<'_>) -> Outcome {
        encoded(
            self.backend
                .host_resources()
                .await
                .map_err(|error| failure("diagnostics_unavailable", error))?,
        )
    }

    pub(crate) async fn process_diagnostics(&self, _scope: Scope<'_>) -> Outcome {
        encoded(
            self.backend
                .process_diagnostics()
                .await
                .map_err(|error| failure("diagnostics_unavailable", error))?,
        )
    }

    pub(crate) async fn process_resource_history(
        &self,
        _scope: Scope<'_>,
        input: &Value,
    ) -> Outcome {
        let window_ms = input
            .get("windowMs")
            .and_then(Value::as_u64)
            .unwrap_or(60 * 60_000);
        let bucket_ms = input
            .get("bucketMs")
            .and_then(Value::as_u64)
            .unwrap_or(60_000);
        bounded("windowMs", window_ms, 1_000, Some(60 * 60_000))?;
        bounded("bucketMs", bucket_ms, 1_000, Some(window_ms))?;
        encoded(
            self.backend
                .process_history(ReadProcessResourceHistory {
                    window_ms,
                    bucket_ms,
                })
                .await
                .map_err(|error| failure("diagnostics_unavailable", error))?,
        )
    }

    pub(crate) async fn trace_diagnostics(&self, _scope: Scope<'_>, input: &Value) -> Outcome {
        let trace_file_path = input
            .get("traceFilePath")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let max_files = input.get("maxFiles").and_then(Value::as_u64).unwrap_or(16);
        bounded("maxFiles", max_files, 0, Some(16))?;
        let slow_span_threshold_ms = input
            .get("slowSpanThresholdMs")
            .and_then(Value::as_f64)
            .map(|value| {
                if value.is_finite() && value >= 0.0 {
                    Ok(value)
                } else {
                    Err(invalid(
                        "slowSpanThresholdMs must be finite and non-negative",
                    ))
                }
            })
            .transpose()?;
        encoded(
            self.backend
                .trace_diagnostics(ReadTraceDiagnostics {
                    trace_file_path,
                    max_files: u32::try_from(max_files).unwrap_or(16),
                    slow_span_threshold_ms,
                })
                .await
                .map_err(|error| failure("diagnostics_unavailable", error))?,
        )
    }
}
