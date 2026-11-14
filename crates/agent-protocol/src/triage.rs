//! A diagnostic draft is prepared without starting a provider or uploading logs.
use crate::session::SessionRef;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prepare {
    pub source: Option<SessionRef>,
    pub description: String,
}

impl Prepare {
    pub fn validate(&self) -> Result<(), &'static str> {
        if let Some(source) = &self.source {
            source.validate()?;
        }
        if self.description.len() > 32 * 1024 {
            return Err("problem description is too long");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Context {
    pub machine_name: String,
    pub cli_command: String,
    pub id: String,
    pub directory: String,
    pub facts_path: String,
    pub playbook_path: String,
    pub source: Option<SessionRef>,
    pub version: String,
    pub revision: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prepared {
    pub context: Context,
    pub prompt: String,
}

pub fn prompt(context: &Context, description: &str) -> String {
    format!(
        "Bexの不具合を調査してください。\n\n報告された症状:\n{}\n\n機械情報: {}\n調査手順: {}\n\nまずこの2ファイルを読んで、事実と推測を区別して調査してください。",
        description.trim(),
        context.facts_path,
        context.playbook_path
    )
}
