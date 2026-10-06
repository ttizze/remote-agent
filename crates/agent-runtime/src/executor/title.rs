use super::ExecutorContext;
use crate::{
    Durability, EffectError, EffectHandler, EffectJob, TextGenerationRequest, ThreadTitlePrompt,
    TitleMessage, TitleRole, format_thread_title_context, sanitize_thread_title,
    thread_title_output_schema, thread_title_prompt, title_link_candidates,
};
use agent_domain::{EffectBody, EffectResult, Role, State};
use futures_util::future::BoxFuture;

/// Attempts of an initial title (T3 retries it twice); a regeneration runs once.
const INITIAL_TITLE_ATTEMPTS: u32 = 3;
const FALLBACK_TITLE: &str = "New thread";

/// T3 `ThreadTitleRegenerationService`: titles the thread from its first message,
/// or regenerates the title from the conversation. Any failure keeps the title.
pub(crate) struct GenerateTitle(pub(crate) ExecutorContext);

enum Outcome {
    Stale,
    Complete(Option<String>),
}

impl EffectHandler for GenerateTitle {
    fn durability(&self) -> Durability {
        Durability::ReplaySafe
    }

    fn run(&self, job: EffectJob) -> BoxFuture<'_, Result<Option<EffectResult>, EffectError>> {
        Box::pin(async move {
            let EffectBody::GenerateTitle { request, message } = &job.effect.body else {
                return Err(EffectError::Permanent("not a title generation".into()));
            };
            let initial = message.is_some();
            let outcome = match self.generate(&job, message.as_ref()).await {
                Ok(outcome) => outcome,
                Err(error) if initial && job.will_retry && job.attempt < INITIAL_TITLE_ATTEMPTS => {
                    return Err(EffectError::Retryable(error));
                }
                Err(error) => {
                    tracing::warn!(thread = %job.thread, %request, %error, "thread title generation failed");
                    Outcome::Complete(None)
                }
            };
            Ok(match outcome {
                Outcome::Stale => None,
                Outcome::Complete(title) => Some(EffectResult::TitleGenerated {
                    request: request.clone(),
                    title,
                }),
            })
        })
    }
}

impl GenerateTitle {
    async fn generate(
        &self,
        job: &EffectJob,
        message: Option<&agent_domain::MessageId>,
    ) -> Result<Outcome, String> {
        let EffectBody::GenerateTitle { request, .. } = &job.effect.body else {
            return Err("not a title generation".into());
        };
        let context = &self.0;
        let state = context
            .registry
            .state(&job.thread)
            .await
            .map_err(|error| error.to_string())?;
        let Some(thread) = state
            .thread
            .as_ref()
            .filter(|thread| thread.title_request.as_ref().map(|r| &r.id) == Some(request))
        else {
            return Ok(Outcome::Stale);
        };
        let Some(project) = context.ops.project(&thread.project) else {
            return Ok(Outcome::Complete(None));
        };
        let (text, attachments) = match message {
            Some(id) => state
                .messages
                .iter()
                .find(|candidate| &candidate.id == id && !candidate.streaming)
                .map(|message| (message.text.clone(), message.attachments.clone()))
                .unwrap_or_default(),
            None => {
                let context = format_thread_title_context(&title_messages(&state));
                (context.message, context.attachments)
            }
        };
        if text.is_empty() && attachments.is_empty() {
            return Ok(Outcome::Complete(None));
        }
        let cwd = context.cwd(&state).unwrap_or(project.root);
        let links = title_link_candidates(&text);
        let linked = if links.is_empty() {
            None
        } else {
            context.ops.title_link_context(cwd.clone(), links).await
        };
        let instructions = context.ops.title_instructions(&thread.project);
        let previous = message.is_none().then_some(thread.title.as_str());
        let prompt = thread_title_prompt(&ThreadTitlePrompt {
            message: &text,
            previous_title: previous,
            attachments: &attachments,
            linked_context: linked.as_deref(),
            instructions: instructions.as_deref(),
        });
        let raw = context
            .ops
            .generate_text(TextGenerationRequest {
                operation: "generateThreadTitle",
                project: thread.project.clone(),
                cwd,
                prompt,
                attachments,
                output_schema: thread_title_output_schema(),
            })
            .await?;
        let generated = sanitize_thread_title(&raw);
        let generated = generated.trim();
        Ok(Outcome::Complete(
            (generated != FALLBACK_TITLE
                && previous.is_none_or(|previous| generated != previous.trim()))
            .then(|| generated.to_owned()),
        ))
    }
}

fn title_messages(state: &State) -> Vec<TitleMessage> {
    state
        .messages
        .iter()
        .filter(|message| !message.streaming)
        .map(|message| TitleMessage {
            role: match message.role {
                Role::User => TitleRole::User,
                Role::Assistant => TitleRole::Assistant,
                Role::System => TitleRole::System,
            },
            text: message.text.clone(),
            attachments: message.attachments.clone(),
        })
        .collect::<Vec<_>>()
}
