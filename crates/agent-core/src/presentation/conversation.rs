//! Immutable conversation projection shared by desktop, Swift and Kotlin.
use super::{ItemMetadata, Role, body, item_presentation, project_items, source_order};
use crate::{
    localization::{UiLanguage, localized_ui_format},
    models,
    state::{PendingSubmission, Snapshot},
};
use agent_protocol::requests::Request as WireRequest;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
mod follow;
pub use follow::{ScrollPosition, away_from_end, follows_end};
mod activity;
mod announcement;
pub use announcement::{
    ConversationAnnouncementPhase, ConversationAnnouncementState, conversation_announcement,
};
pub mod attempts;
mod images;
mod layout;
mod proposed_plan;
pub mod rows;
pub use layout::ConversationWorkGroupRange;
mod timeline;
pub use activity::ActivityDecoration;
pub use proposed_plan::{PlanDisplay, plan_display, plan_export, plan_write_enabled};
pub use timeline::{PlanPresentation, SubagentMember, TimelineStyle, TurnRow, WorkDisclosure};
use timeline::{turn_message_search_rows, turn_rows};
mod work;
use work::LiveActivity;
pub use work::{WorkRowLabel, item_detail_key};
mod row_binding;
pub use row_binding::retained_row_index;
#[cfg(test)]
mod approval_tests;
#[cfg(test)]
mod progress_contract_tests;

impl Snapshot {
    /// Display pending input before a new conversation has a server ID.
    pub fn conversation_thread(
        &self,
        pane: &crate::state::PaneState,
    ) -> Option<Arc<models::Thread>> {
        if let Some(source) = pane
            .navigation
            .thread_id
            .as_ref()
            .and_then(|id| self.sessions.conversations.get(id))
        {
            return Some(source.clone());
        }
        self.sessions
            .pending_submissions
            .values()
            .any(|pending| pending.draft_key == pane.navigation.draft_key)
            .then(|| {
                Arc::new(models::Thread {
                    ..Default::default()
                })
            })
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct RenderedConversation {
    language: UiLanguage,
    history_notice: Option<String>,
    pub source: Arc<models::Thread>,
    pending: PendingItems,
    pub turns: Vec<Arc<RenderedTurn>>,
    pub queued: Vec<Arc<RenderedItem>>,
    pub session_rows: Vec<ConversationRow>,
    pub source_paths: Arc<Vec<String>>,
}

pub struct RenderedTurn {
    supports_fork: bool,
    language: UiLanguage,
    pub source: Arc<models::Turn>,
    // Native turn IDs may repeat; the occurrence also belongs to this cache entry.
    occurrence: usize,
    pending: PendingItems,
    requests: Vec<Arc<WireRequest>>,
    pub rows: Vec<ConversationRow>,
}
/// Immutable turn layout; pane projection filters activity before native bridging.
#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ConversationRow {
    pub id: String,
    pub content: ConversationRowContent,
    pub timestamp_ms: Option<u64>,
    /// A user message sent while its turn was working, which steered it.
    pub steer: bool,
    /// A user message that waited in the queue and then started its turn.
    pub queued: bool,
    /// The initiating user input starts a new provider session in this conversation.
    pub fresh_context: bool,
    /// The turn "Edit from here" returns to just before: set on the message
    /// that opened a settled turn whose provider can continue from an earlier point.
    pub edit_turn_id: Option<agent_protocol::ids::TurnId>,
}

fn conversation_row_id(
    turn: &str,
    turn_occurrence: usize,
    base: &str,
    row_occurrence: usize,
) -> String {
    // Scope every key, including activity expansion, to one turn occurrence.
    // Escaping the delimiter keeps provider IDs from aliasing another scope.
    format!(
        "{base}:occurrence:{row_occurrence}:{}",
        turn_key(turn, turn_occurrence)
    )
}

fn turn_key(turn: &str, occurrence: usize) -> String {
    let scope = percent_encoding::utf8_percent_encode(turn, percent_encoding::NON_ALPHANUMERIC);
    format!("turn:{scope}:{occurrence}")
}
#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ConversationRowContent {
    Queue {
        queue: Arc<super::queue::QueuePresentation>,
    },
    /// Current work belongs to the scrollable end of the conversation, rather
    /// than the input. The existing background owner supplies all of its copy.
    Background {
        title: String,
        description: Option<String>,
        waiting: bool,
        turn_id: Option<agent_protocol::ids::TurnId>,
    },
    Preparation {
        preparation: Box<PreparationPresentation>,
    },
    ProviderHandoff {
        text: String,
    },
    /// What the background work its agent woke for did, starting the turn
    /// without a user message, as T3 Code's notification row. Clients name
    /// it in their language with `notification_text`.
    Notification {
        reports: Vec<agent_protocol::execution::BackgroundReport>,
        icon: super::background_work::NotificationIcon,
    },
    User {
        item: Arc<RenderedItem>,
    },
    ActivityHeader {
        activity: ActivityPresentation,
    },
    Activity {
        item: Arc<RenderedItem>,
        turn_id: agent_protocol::ids::TurnId,
    },
    PendingRequest {
        request: Box<Request>,
    },
    Error {
        error: TurnErrorPresentation,
    },
    Response {
        item: Arc<RenderedItem>,
        show_actions: bool,
        fork_turn_id: Option<agent_protocol::ids::TurnId>,
    },
    InProgress {
        turn_id: agent_protocol::ids::TurnId,
    },
}

#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PreparationPresentation {
    pub text: String,
    /// Rendered like a running turn's working row, as T3 Code shows startup.
    pub in_progress: bool,
    /// Setting up a worktree, which shimmers as T3 Code's "Setting up worktree…".
    pub shimmer: bool,
    pub retry: Option<agent_protocol::operations::RetryPreparation>,
    pub steps: Vec<PreparationStep>,
    pub show_steps: bool,
    pub details: Vec<PreparationDetail>,
    pub started_at_ms: Option<u64>,
    pub ended_at_ms: Option<u64>,
    pub cancel: Option<agent_protocol::operations::Interrupt>,
    pub terminal: Option<SetupTerminal>,
    pub work_locally: Option<agent_protocol::operations::RetryPreparation>,
    pub error: Option<String>,
}
#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PreparationStep {
    pub id: String,
    pub label: String,
    pub state: String,
    pub detail: Option<String>,
    pub started_at_ms: Option<u64>,
    pub ended_at_ms: Option<u64>,
    pub tail: Vec<String>,
}
#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PreparationDetail {
    pub label: String,
    pub value: String,
}
#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SetupTerminal {
    pub handle: String,
    pub cwd: String,
}
mod preparation;
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn preparation_elapsed(language: UiLanguage, start: u64, end: Option<u64>, now: u64) -> String {
    let ms = end.unwrap_or(now).saturating_sub(start);
    if ms < 1000 {
        String::new()
    } else {
        super::duration_text(language, ms)
    }
}

/// Pending and skipped stages have no elapsed clock. Other stages retain the
/// distinction between an absent start and a duration below the display unit.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn preparation_step_elapsed(
    language: UiLanguage,
    state: String,
    start: Option<u64>,
    end: Option<u64>,
    now: u64,
) -> Option<String> {
    if matches!(state.as_str(), "pending" | "skipped") {
        None
    } else {
        start.map(|start| preparation_elapsed(language, start, end, now))
    }
}
#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ActivityPresentation {
    pub id: String,
    pub status: String,
    pub activity_summary: String,
    pub activity_can_collapse: bool,
    pub is_in_progress: bool,
}
mod user_message;
pub use user_message::*;

/// A settled response with no body says so; an unfinished body stays empty.
fn assistant_response_text(text: &str, streaming: bool) -> &str {
    if text.is_empty() && !streaming {
        "(empty response)"
    } else {
        text
    }
}

mod minimap;
pub use minimap::*;

mod search;
pub use search::{
    TextMatch, adjacent_match_index, find_matches, format_find_count, step_find_index,
};

/// Navigation stops at the first/last exchange and waits for a known viewport.
pub fn minimap_adjacent_index(count: u32, current: Option<u32>, next: bool) -> Option<u32> {
    let current = current.filter(|index| *index < count)?;
    if next {
        current.checked_add(1).filter(|index| *index < count)
    } else {
        current.checked_sub(1)
    }
}

/// A message's time of day as T3 Code shows it in each language: `2:17 AM`,
/// `14:17`.
fn time_of_day(language: UiLanguage, hour: u32, minute: u32) -> String {
    match language {
        UiLanguage::English => format!(
            "{}:{minute:02} {}",
            (hour + 11) % 12 + 1,
            if hour < 12 { "AM" } else { "PM" }
        ),
        UiLanguage::Japanese => format!("{hour:02}:{minute:02}"),
    }
}

/// A date on the viewer's calendar.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CalendarDay {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

/// When a message was sent, as T3 Code's `formatDayAwareTimestamp`: the time
/// today, "yesterday" the day before, and the date before that, its year only
/// when it is not this one.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn day_aware_time(
    language: UiLanguage,
    hour: u32,
    minute: u32,
    date: CalendarDay,
    days_ago: i64,
    this_year: i32,
) -> String {
    let time = time_of_day(language, hour, minute);
    if days_ago <= 0 {
        return time;
    }
    if days_ago == 1 {
        return match language {
            UiLanguage::English => format!("yesterday at {time}"),
            UiLanguage::Japanese => format!("昨日 {time}"),
        };
    }
    let CalendarDay { year, month, day } = date;
    let date = match (language, year == this_year) {
        (_, true) => format!("{month}/{day}"),
        (UiLanguage::English, false) => format!("{month}/{day}/{year}"),
        (UiLanguage::Japanese, false) => format!("{year}/{month}/{day}"),
    };
    format!("{date} {time}")
}

/// The widest the column of messages and of the composer grows, as T3 Code's
/// `--chat-content-max-width` (46rem).
pub const LANE_WIDTH: f32 = 736.;

/// The space the timeline and the composer keep beside their lane, as T3 Code's
/// workspace gutter (`0.75rem`, `1.25rem` from a 640 pt wide window).
pub fn lane_padding(window_width: f32) -> f32 {
    if window_width >= 640. { 20. } else { 12. }
}

/// T3 Code's thread details card, beside the chat lane when it fits and
/// anchored to its header button as a popover otherwise.
pub const DETAILS_CARD_WIDTH: f32 = 280.;
pub const DETAILS_CARD_GAP: f32 = 12.;

/// Keep a 640 pt conversation, its gutter and 32 pt clearance beside the
/// card. `canvas_height` excludes the conversation's header.
pub fn details_card_inline(canvas_width: f32, canvas_height: f32) -> bool {
    canvas_width - DETAILS_CARD_WIDTH - DETAILS_CARD_GAP - 32. - lane_padding(canvas_width) >= 640.
        && canvas_height - DETAILS_CARD_GAP * 2. >= 160.
}

/// Work starts collapsed. A manual choice belongs to the current status;
/// a transition closes it again, including after deferred-detail hydration.
pub fn activity_is_expanded(status: &str, expanded_status: Option<String>) -> bool {
    expanded_status.as_deref() == Some(status)
}

/// Fill the viewport on opening and continue at the oldest visible boundary.
/// Wait for latest-message positioning before paging a scrollable initial page.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn should_load_history(
    has_more: bool,
    loading: bool,
    oldest_visible: bool,
    latest_visible: bool,
    following_latest: bool,
) -> bool {
    has_more && !loading && oldest_visible && (!following_latest || latest_visible)
}

impl RenderedTurn {
    pub fn key(&self) -> String {
        turn_key(self.source.id.as_str(), self.occurrence)
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl RenderedConversation {
    pub fn language(&self) -> UiLanguage {
        self.language
    }
    pub fn history_notice(&self) -> Option<String> {
        self.history_notice.clone()
    }
}

impl RenderedConversation {
    fn pending_input(&self, id: &str) -> Option<&Arc<RenderedItem>> {
        self.queued.iter().find(|item| item.data.id == id)
    }
}

/// The running turn's elapsed time and phase. Tool descriptions belong to
/// the live work row; only compaction changes or animates the working header.
#[derive(Clone, PartialEq)]
pub struct WorkProgress {
    started_at: Option<f64>,
    compacting: bool,
}

impl WorkProgress {
    pub fn label(&self, language: UiLanguage, now_seconds: f64) -> String {
        // As T3 Code's working row: the time so far, in the fold's units.
        let elapsed = self.started_at.map(|started| {
            super::elapsed_text(language, ((now_seconds - started).max(0.) * 1000.) as u64)
        });
        match elapsed {
            Some(elapsed) => localized_ui_format(
                language,
                if self.compacting {
                    "work.compaction.progress.duration"
                } else {
                    "work.progress.duration"
                }
                .into(),
                vec![elapsed],
            ),
            None => language
                .text(if self.compacting {
                    "work.compaction.progress"
                } else {
                    "作業中…"
                })
                .into(),
        }
    }

    pub fn is_compacting(&self) -> bool {
        self.compacting
    }
}

impl RenderedTurn {
    pub fn work_progress(&self) -> WorkProgress {
        WorkProgress {
            started_at: self.source.started_at.or_else(|| {
                self.source
                    .started_at_ms
                    .map(|milliseconds| milliseconds as f64 / 1000.)
            }),
            compacting: self.source.items.iter().flatten().any(|item| {
                item.status == models::ItemStatus::Running
                    && matches!(item.body(), models::ItemBody::Compaction { .. })
            }),
        }
    }
    fn items(&self) -> impl Iterator<Item = &Arc<RenderedItem>> {
        self.rows.iter().filter_map(|row| match &row.content {
            ConversationRowContent::User { item }
            | ConversationRowContent::Activity { item, .. }
            | ConversationRowContent::Response { item, .. } => Some(item),
            _ => None,
        })
    }
}

#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TurnErrorPresentation {
    pub title: String,
    pub message: String,
    pub details: Option<String>,
    pub is_reconnecting: bool,
    pub is_usage_limit: bool,
}

pub enum ItemSource {
    Native(Arc<models::Item>),
    Pending(String, Arc<PendingSubmission>),
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct RenderedItem {
    language: Option<UiLanguage>,
    pub source: ItemSource,
    pub data: ItemPresentation,
    delivery_unknown: bool,
}

/// Specialized clients render typed context references. Classic text/export
/// consumers retain `data.body`, whose readable fallback includes the details.
#[derive(serde::Serialize)]
pub struct UserMessageBody {
    pub text: String,
    pub images: Vec<String>,
    pub contexts: Vec<super::message_context::ContextPresentation>,
    pub attachments: Vec<super::message_attachment::MessageAttachment>,
    pub attachments_only: Option<super::message_attachment::AttachmentKind>,
}
#[cfg(test)]
#[path = "conversation/context_body_tests.rs"]
mod context_body_tests;
// Prepared bodies are immutable render resources. Reusing their identity avoids
// comparing or preparing long Markdown bodies again in each native surface.
impl PartialEq for RenderedItem {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}
impl RenderedItem {
    pub fn user_message_body(
        &self,
        metadata: &super::files::FileMetadataCache,
    ) -> Option<UserMessageBody> {
        let present = |index, name: &str, path: &str| {
            let entry = metadata.get(path).and_then(|entry| entry.metadata.as_ref());
            super::message_attachment::attachment(
                index,
                name,
                path,
                entry.and_then(|entry| entry.byte_length),
                entry.and_then(|entry| entry.mime_type.as_deref()),
            )
        };
        let language = self.language.unwrap_or_default();
        let (text, original, images, attachments) = match &self.source {
            ItemSource::Native(item) => {
                let models::ItemBody::UserMessage { text, content } = item.body() else {
                    return None;
                };
                let original = body::message_text(text.as_deref(), content);
                let text = body::message(
                    language,
                    text.as_deref(),
                    content,
                    body::ContextFormat::References,
                );
                let images: Vec<_> = content
                    .iter()
                    .filter_map(|part| match part {
                        models::MessagePart::Image { source } => Some(source.clone()),
                        _ => None,
                    })
                    .collect();
                let attachments: Vec<_> = content
                    .iter()
                    .enumerate()
                    .filter_map(|(index, part)| match part {
                        models::MessagePart::Attachment { name, path } => {
                            Some(present(index, name, path))
                        }
                        _ => None,
                    })
                    .collect();
                (text, original, images, attachments)
            }
            ItemSource::Pending(_, pending) => {
                let text = body::draft_with_contexts(
                    language,
                    &pending.draft,
                    body::ContextFormat::References,
                )
                .text
                .unwrap_or_default();
                let images = pending
                    .draft
                    .attachments
                    .iter()
                    .filter(|file| file.is_image)
                    .map(|file| file.path.clone())
                    .collect();
                let attachments = pending
                    .draft
                    .attachments
                    .iter()
                    .enumerate()
                    .filter(|(_, file)| !file.is_image)
                    .map(|(index, file)| present(index, &file.name, &file.path))
                    .collect();
                (text, pending.draft.text.clone(), images, attachments)
            }
        };
        if self.data.contexts.is_empty() && attachments.is_empty() {
            return None;
        }
        let attachments_only = super::message_attachment::attachments_only_kind(
            self.data.author.as_deref(),
            &original,
            !images.is_empty(),
            !self.data.contexts.is_empty(),
            &attachments,
        );
        let text = if attachments_only.is_some() {
            crate::text::trim(&text).to_owned()
        } else {
            text
        };
        Some(UserMessageBody {
            text,
            images,
            contexts: super::message_context::context_chips(self.data.contexts.clone()),
            attachments,
            attachments_only,
        })
    }

    pub fn is_streaming(&self) -> bool {
        matches!(&self.source, ItemSource::Native(item) if item.status == models::ItemStatus::Running)
    }

    pub fn response_text(&self) -> Option<&str> {
        let text = self.data.body.as_deref()?;
        Some(
            if matches!(self.data.kind.as_str(), "agent" | "commentary") {
                assistant_response_text(text, self.is_streaming())
            } else {
                text
            },
        )
    }
}
#[cfg_attr(feature = "bindings", uniffi::export)]
impl RenderedItem {
    /// Native chips render structured contexts separately; copy/export retain the complete body.
    pub fn message_body(&self) -> Option<String> {
        if self.data.contexts.is_empty() {
            return self.data.body.clone();
        }
        let language = self.language.unwrap_or_default();
        match &self.source {
            ItemSource::Native(item) => match item.body() {
                models::ItemBody::UserMessage { text, content } => Some(body::message(
                    language,
                    text.as_deref(),
                    content,
                    body::ContextFormat::Separate,
                )),
                _ => self.data.body.clone(),
            },
            ItemSource::Pending(_, pending) => {
                body::draft_with_contexts(language, &pending.draft, body::ContextFormat::Separate)
                    .text
            }
        }
    }

    pub fn collapses_user_text(&self) -> bool {
        self.data.kind == "user"
            && self
                .message_body()
                .as_deref()
                .is_some_and(should_collapse_user_message)
    }

    pub fn unknown_submission_id(&self) -> Option<String> {
        self.delivery_unknown.then(|| self.data.id.clone())
    }
    pub fn detail_blocks(&self) -> Vec<super::markdown::MarkdownBlock> {
        match &self.source {
            ItemSource::Native(item) => {
                body::detail_blocks(self.language.unwrap_or_default(), item)
            }
            ItemSource::Pending(..) => {
                super::markdown::markdown_blocks(self.data.body.clone().unwrap_or_default())
            }
        }
    }
    pub fn expanded_body(&self) -> String {
        match &self.source {
            ItemSource::Native(item) => {
                body::expanded_body(self.language.unwrap_or_default(), item)
            }
            ItemSource::Pending(..) => self.data.body.clone().expect("pending input has text"),
        }
    }
}
impl RenderedItem {
    fn key(&self) -> (Option<&str>, *const ()) {
        match &self.source {
            ItemSource::Native(item) => (None, Arc::as_ptr(item).cast()),
            ItemSource::Pending(id, pending) => (Some(id), Arc::as_ptr(pending).cast()),
        }
    }
    fn native(
        language: UiLanguage,
        item: &Arc<models::Item>,
        provider: Option<crate::session::ProviderKind>,
        turn_status: models::TurnStatus,
        preparation: Option<&agent_protocol::session::PreparationStatus>,
        timestamp_ms: Option<u64>,
        previous: Option<&Arc<Self>>,
    ) -> Arc<Self> {
        let deferred = item.is_deferred();
        let empty_response = !deferred
            && matches!(item.body(), models::ItemBody::AssistantText { text, .. } if text.is_empty())
            && work::item_is_settled(item.status, turn_status);
        let delivery_unknown =
            preparation == Some(&agent_protocol::session::PreparationStatus::Unknown);
        let native_id = preparation.is_none().then(|| item.id.clone());
        // Plain messages have no translated copy, so retain their native Markdown
        // caches across a language change. Activity details and attachments do.
        let localized = match item.body() {
            models::ItemBody::AssistantText { .. } | models::ItemBody::Plan { .. } => false,
            models::ItemBody::UserMessage { content, .. } => content
                .iter()
                .any(|part| matches!(part, models::MessagePart::Attachment { .. })),
            _ => true,
        }
        .then_some(language);
        if let Some(previous) = previous
            && previous.language == localized
            && previous.data.native_id == native_id
            && previous.data.deferred == deferred
            && previous.data.empty_response == empty_response
            && previous.delivery_unknown == delivery_unknown
            && previous.data.timestamp_ms == timestamp_ms
        {
            return previous.clone();
        }
        let presentation = item_presentation(language, item, provider);
        let body = body::item_body(language, item);
        let image_placeholder = presentation.kind == "imageGeneration"
            && item.status == models::ItemStatus::Running
            && body.images.is_empty();
        Arc::new(Self {
            language: localized,
            source: ItemSource::Native(item.clone()),
            delivery_unknown,
            data: ItemPresentation {
                id: item
                    .client_input_id
                    .as_deref()
                    .unwrap_or(item.id.as_str())
                    .to_owned(),
                native_id,
                author: item.author.clone(),
                timestamp_ms,
                kind: presentation.kind.into(),
                title: presentation.title,
                collapsible: presentation.collapsible,
                body: body.text,
                image_sources: body.images,
                contexts: body.contexts,
                image_placeholder,
                empty_response,
                deferred,
                preview: body::reasoning_preview(item),
            },
        })
    }
    fn pending(
        language: UiLanguage,
        id: &str,
        pending: &Arc<PendingSubmission>,
        previous: Option<&Arc<Self>>,
    ) -> Arc<Self> {
        let localized = (pending.delivery_unknown
            || pending.draft.attachments.iter().any(|file| !file.is_image))
        .then_some(language);
        if let Some(previous) = previous
            && previous.language == localized
        {
            return previous.clone();
        }
        let body = body::draft_body(language, &pending.draft);
        Arc::new(Self {
            language: localized,
            source: ItemSource::Pending(id.into(), pending.clone()),
            delivery_unknown: pending.delivery_unknown,
            data: ItemPresentation {
                id: id.into(),
                native_id: None,
                author: None,
                timestamp_ms: pending.submitted_at_ms,
                kind: "user".into(),
                title: pending
                    .delivery_unknown
                    .then(|| language.text("送信結果不明（自動再送しません）").into()),
                collapsible: false,
                body: body.text,
                image_sources: body.images,
                contexts: body.contexts,
                image_placeholder: false,
                empty_response: false,
                deferred: false,
                preview: None,
            },
        })
    }
}

/// Pass the previous projection to retain native render identities across deltas.
pub fn project_conversation(
    language: UiLanguage,
    draft_key: &crate::state::DraftKey,
    pending_submissions: &std::collections::BTreeMap<
        agent_protocol::ids::ClientInputId,
        Arc<PendingSubmission>,
    >,
    source: Arc<models::Thread>,
    previous: &Option<Arc<RenderedConversation>>,
) -> Arc<RenderedConversation> {
    let draft_key = source
        .id
        .as_ref()
        .map(crate::state::DraftKey::from)
        .unwrap_or_else(|| draft_key.clone());
    let preparations = &source.preparations;
    let mut candidates = pending_submissions
        .iter()
        .filter(|(_, pending)| pending.draft_key == draft_key)
        .peekable();
    let delivered: HashSet<_> = if candidates.peek().is_none() {
        HashSet::new()
    } else {
        source
            .turns
            .iter()
            .flatten()
            .flat_map(|turn| turn.items.iter().flatten())
            .filter_map(|item| item.client_input_id.as_ref())
            .collect()
    };
    let mut pending: PendingItems = candidates
        .filter(|(id, _)| !delivered.contains(id))
        .filter(|(id, _)| {
            preparations
                .iter()
                .all(|preparation| &preparation.submission.client_user_message_id != *id)
        })
        // The queue above the composer shows input that waits for a turn, and
        // the turn it starts shows it as the Host has it, edits included. Only
        // a send whose delivery is unknown stays here for its recovery.
        .filter(|(id, pending)| {
            (!pending.queued || pending.delivery_unknown)
                && !source
                    .queue
                    .inputs
                    .iter()
                    .any(|input| &input.client_user_message_id == *id)
        })
        .map(|(id, pending)| (id.clone(), pending.clone()))
        .collect();
    pending.sort_by_key(|(_, pending)| pending.sequence);
    let requests = &source.requests;
    if let Some(previous) = previous
        && language == previous.language
        && Arc::ptr_eq(&source, &previous.source)
        && preparations.iter().all(|preparation| {
            let id = &preparation.submission.client_user_message_id;
            previous.pending_input(id.as_str()).is_none_or(|item| {
                item.data.timestamp_ms
                    == pending_submissions
                        .get(id)
                        .and_then(|pending| pending.submitted_at_ms)
            })
        })
        && pending
            .iter()
            .map(|(id, pending)| (id, Arc::as_ptr(pending)))
            .eq(previous
                .pending
                .iter()
                .map(|(id, pending)| (id, Arc::as_ptr(pending))))
    {
        return previous.clone();
    }
    let previous = previous.as_ref().filter(|old| source.id == old.source.id);
    let supports_fork = source.capabilities.unwrap_or_default().fork;
    let supports_rewind = source.capabilities.unwrap_or_default().rollback;
    let previous_supports_rewind =
        previous.is_some_and(|old| old.source.capabilities.unwrap_or_default().rollback);
    let previous_supports_fork =
        previous.is_some_and(|old| old.source.capabilities.unwrap_or_default().fork);
    let cached: HashMap<_, _> = previous
        .into_iter()
        .flat_map(|old| &old.turns)
        .map(|turn| ((turn.source.id.as_str(), turn.occurrence), turn))
        .collect();
    let native = source.turns.as_deref().unwrap_or_default();
    let unconfirmed: Vec<_> = preparations
        .iter()
        .map(|preparation| {
            let id = &preparation.submission.client_user_message_id;
            let item = Arc::new(models::Item::new(
                id.to_string().into(),
                models::ItemStatus::Completed,
                models::ItemBody::UserMessage {
                    text: None,
                    content: agent_protocol::operations::Input::message_parts(
                        &preparation.submission.input,
                    ),
                },
            ));
            let old = previous
                .and_then(|old| old.pending_input(id.as_str()))
                .filter(|old| matches!(&old.source, ItemSource::Native(native) if native == &item));
            RenderedItem::native(
                language,
                &item,
                source.active_provider,
                models::TurnStatus::Unknown,
                Some(&preparation.status),
                pending_submissions
                    .get(id)
                    .and_then(|pending| pending.submitted_at_ms),
                old,
            )
        })
        .collect();
    let positions: HashMap<_, _> = if pending.is_empty() && requests.is_empty() {
        HashMap::new()
    } else {
        native
            .iter()
            .enumerate()
            .map(|(index, turn)| (turn.id.as_str(), index))
            .collect()
    };
    // The local outbox keeps its captured placement until the canonical input
    // arrives. A send made before any history precedes the first incoming turn.
    let mut pending_by_turn: HashMap<_, Vec<_>> = HashMap::new();
    for entry @ (_, submission) in &pending {
        let position = match submission.turn_id.as_deref() {
            Some(id) => positions.get(id).copied(),
            None if !submission.accepted && !native.is_empty() => Some(0),
            None => None,
        };
        pending_by_turn.entry(position).or_default().push(entry);
    }
    let mut requests_by_turn: HashMap<_, Vec<_>> = HashMap::new();
    let mut session_requests = Vec::new();
    for request in requests.values() {
        if let agent_protocol::requests::RequestTarget::Turn { turn_id, .. } = &request.target
            && positions.contains_key(turn_id.as_str())
        {
            requests_by_turn
                .entry(turn_id.as_str())
                .or_default()
                .push(request);
        } else {
            session_requests.push(request);
        }
    }
    let mut occurrences = HashMap::new();
    let turns = native
        .iter()
        .enumerate()
        .map(|(index, turn)| {
            let next = occurrences.entry(turn.id.as_str()).or_insert(0usize);
            let occurrence = *next;
            *next += 1;
            let handoff = index
                .checked_sub(1)
                .and_then(|previous| native[previous].provider)
                .zip(turn.provider)
                .filter(|(from, to)| from != to)
                .map(|(from, to)| {
                    localized_ui_format(
                        language,
                        "{0} → {1} に切り替えました".into(),
                        vec![format!("{from:?}"), format!("{to:?}")],
                    )
                });
            let pending = pending_by_turn
                .get(&Some(index))
                .into_iter()
                .flatten()
                .copied();
            let requests = requests_by_turn
                .get(turn.id.as_str())
                .into_iter()
                .flatten()
                .copied();
            let old = cached.get(&(turn.id.as_str(), occurrence)).copied();
            if let Some(old) = old
                && language == old.language
                && Arc::ptr_eq(turn, &old.source)
                && (turn.supports_fork.is_some() || supports_fork == previous_supports_fork)
                && supports_rewind == previous_supports_rewind
                && old.rows.iter().find_map(|row| match &row.content {
                    ConversationRowContent::ProviderHandoff { text } => Some(text),
                    _ => None,
                }) == handoff.as_ref()
                && pending
                    .clone()
                    .map(|(id, p)| (id, Arc::as_ptr(p)))
                    .eq(old.pending.iter().map(|(id, p)| (id, Arc::as_ptr(p))))
                && requests
                    .clone()
                    .map(Arc::as_ptr)
                    .eq(old.requests.iter().map(Arc::as_ptr))
            {
                return old.clone();
            }
            render_turn(
                source.id.as_ref(),
                (supports_fork, supports_rewind),
                handoff,
                (turn.clone(), occurrence, language),
                pending.cloned().collect(),
                requests.cloned().collect(),
                old,
            )
        })
        .collect();
    let queued: HashMap<_, _> = previous
        .into_iter()
        .flat_map(|old| &old.queued)
        .map(|item| (item.key(), item))
        .collect();
    let mut queued: Vec<_> = pending_by_turn
        .get(&None)
        .into_iter()
        .flatten()
        .copied()
        .map(|(id, p)| {
            RenderedItem::pending(
                language,
                id,
                p,
                queued
                    .get(&(Some(id.as_str()), Arc::as_ptr(p).cast()))
                    .copied(),
            )
        })
        .collect();
    queued.splice(0..0, unconfirmed);
    let mut session_rows: Vec<_> = session_requests
        .into_iter()
        .map(|source| ConversationRow {
            id: format!("request:{}", source.id),
            timestamp_ms: None,
            steer: false,
            queued: false,
            fresh_context: false,
            edit_turn_id: None,
            content: ConversationRowContent::PendingRequest {
                request: Box::new(request(language, source)),
            },
        })
        .collect();
    if let Some(setup) = &source.workspace_setup {
        session_rows.push(ConversationRow {
            id: "workspace-setup".into(),
            timestamp_ms: Some(setup.started_at_ms),
            steer: false,
            queued: false,
            fresh_context: false,
            edit_turn_id: None,
            content: ConversationRowContent::Preparation {
                preparation: Box::new(preparation::project(language, &source, setup)),
            },
        });
    }
    let running = native
        .iter()
        .any(|turn| turn.status == models::TurnStatus::Running);
    for preparation in preparations.iter().filter(|input| {
        source.workspace_setup.is_none()
            && (!running
                || !matches!(
                    input.status,
                    agent_protocol::session::PreparationStatus::Preparing
                        | agent_protocol::session::PreparationStatus::Sending
                ))
    }) {
        session_rows.push(ConversationRow {
            id: format!(
                "preparation:{}",
                preparation.submission.client_user_message_id
            ),
            timestamp_ms: None,
            steer: false,
            queued: false,
            fresh_context: false,
            edit_turn_id: None,
            content: ConversationRowContent::Preparation {
                preparation: Box::new(preparation_presentation(language, preparation)),
            },
        });
    }
    if let Some(work) = super::background_work::BackgroundWork::new(
        &source.background,
        crate::state::follow_up::working(&source),
    ) {
        session_rows.push(ConversationRow {
            id: "background-work".into(),
            timestamp_ms: None,
            steer: false,
            queued: false,
            fresh_context: false,
            edit_turn_id: None,
            content: ConversationRowContent::Background {
                title: work.title(language),
                description: work.description(language),
                waiting: work.waiting,
                turn_id: source.active_turn_id(),
            },
        });
    }
    let paths: std::collections::BTreeSet<_> = source
        .turns
        .iter()
        .flatten()
        .flat_map(|turn| turn.items.iter().flatten())
        .filter_map(|item| match item.body() {
            agent_protocol::items::ItemBody::UserMessage { content, .. } => Some(content),
            _ => None,
        })
        .flatten()
        .filter_map(|part| match part {
            agent_protocol::items::MessagePart::Image { source }
                if !source.starts_with("data:") =>
            {
                Some(source.to_owned())
            }
            agent_protocol::items::MessagePart::Attachment { path, .. }
            | agent_protocol::items::MessagePart::Invocation { path, .. } => Some(path.to_owned()),
            _ => None,
        })
        .collect();
    let source_paths: Vec<_> = paths.into_iter().collect();
    let source_paths = previous
        .filter(|old| old.source_paths.as_ref() == &source_paths)
        .map_or_else(|| Arc::new(source_paths), |old| old.source_paths.clone());
    let history_notice = history_notice(language, &source);
    Arc::new(RenderedConversation {
        language,
        history_notice,
        source,
        pending,
        turns,
        queued,
        session_rows,
        source_paths,
    })
}

fn preparation_presentation(
    language: UiLanguage,
    preparation: &agent_protocol::session::SessionPreparation,
) -> PreparationPresentation {
    use agent_protocol::{
        operations::RetryPreparation,
        session::{PreparationStage, PreparationStatus},
    };
    // Startup reads as ordinary work; the composer's stop control interrupts it.
    let (text, in_progress, retry) = match &preparation.status {
        PreparationStatus::Preparing | PreparationStatus::Sending => {
            let text = match preparation.stage {
                PreparationStage::Worktree | PreparationStage::SetupScript => {
                    "ワークツリーを準備中…"
                }
                PreparationStage::Workspace | PreparationStage::Provider => "開始中…",
            };
            (language.text(text).into(), true, false)
        }
        PreparationStatus::Failed { error } => (
            localized_ui_format(
                language,
                "準備に失敗しました: {0}".into(),
                vec![error.message.clone()],
            ),
            false,
            true,
        ),
        PreparationStatus::Interrupted => {
            (language.text("準備を停止しました。").into(), false, true)
        }
        PreparationStatus::Unknown => (
            language
                .text("送信結果を確認できません。二重実行を避けるため再送していません。")
                .into(),
            false,
            false,
        ),
    };
    PreparationPresentation {
        text,
        in_progress,
        shimmer: in_progress && preparation.stage == PreparationStage::Worktree,
        retry: retry.then(|| RetryPreparation {
            input_id: preparation.submission.client_user_message_id.clone(),
            thread_id: preparation.submission.thread_id.clone(),
            work_locally: false,
        }),
        steps: Vec::new(),
        show_steps: false,
        details: Vec::new(),
        started_at_ms: None,
        ended_at_ms: None,
        cancel: None,
        terminal: None,
        work_locally: None,
        error: None,
    }
}

#[cfg(test)]
mod source_tests {
    use super::*;

    #[test]
    fn empty_response_display_waits_for_settlement_and_keeps_the_actual_body_empty() {
        let item = Arc::new(models::Item::new(
            "answer".into(),
            models::ItemStatus::Unknown,
            models::ItemBody::AssistantText {
                text: String::new(),
                phase: Default::default(),
            },
        ));
        let source = Arc::new(models::Thread {
            turns: Some(vec![Arc::new(models::Turn {
                id: "turn".into(),
                status: models::TurnStatus::Running,
                items: Some(vec![item]),
                ..Default::default()
            })]),
            ..Default::default()
        });
        let render = |source, previous| {
            project_conversation(
                UiLanguage::English,
                &Default::default(),
                &Default::default(),
                source,
                &previous,
            )
        };
        let first = render(source.clone(), None);
        let before = first.turns[0].items().next().unwrap();
        assert!(
            !before.data.empty_response,
            "A streaming empty body must remain blank"
        );
        let mut settled = source.as_ref().clone();
        Arc::make_mut(&mut settled.turns.as_mut().unwrap()[0]).status =
            models::TurnStatus::Completed;
        let second = render(Arc::new(settled), Some(first.clone()));
        let after = second.turns[0].items().next().unwrap();
        assert!(
            after.data.empty_response,
            "Settlement must invalidate the cached display decision"
        );
        assert_eq!(
            after.message_body().as_deref(),
            Some(""),
            "The placeholder is not provider text to copy or cite"
        );
        assert!(!before.data.empty_response);
        let mut source = source.as_ref().clone();
        let turn = Arc::make_mut(&mut source.turns.as_mut().unwrap()[0]);
        Arc::make_mut(&mut turn.items.as_mut().unwrap()[0]).status = models::ItemStatus::Completed;
        assert!(
            render(Arc::new(source), None).turns[0]
                .items()
                .next()
                .unwrap()
                .data
                .empty_response
        );
        let mut deferred = models::Item::new(
            "deferred".into(),
            models::ItemStatus::Completed,
            models::ItemBody::AssistantText {
                text: String::new(),
                phase: Default::default(),
            },
        );
        deferred.defer();
        let deferred = RenderedItem::native(
            UiLanguage::English,
            &Arc::new(deferred),
            None,
            models::TurnStatus::Completed,
            None,
            None,
            None,
        );
        assert!(
            !deferred.data.empty_response,
            "Unloaded content is not an empty answer"
        );
        let text = Arc::new(models::Item::new(
            "text".into(),
            models::ItemStatus::Completed,
            models::ItemBody::AssistantText {
                text: "Answer".into(),
                phase: Default::default(),
            },
        ));
        let text = RenderedItem::native(
            UiLanguage::English,
            &text,
            None,
            models::TurnStatus::Completed,
            None,
            None,
            None,
        );
        assert!(!text.data.empty_response);
    }

    #[test]
    fn user_file_sources_are_deduplicated_and_reused_when_other_content_changes() {
        use agent_protocol::{
            execution::ItemStatus,
            items::{ItemBody, MessagePart},
        };
        let item = Arc::new(models::Item::new(
            "input".into(),
            ItemStatus::Completed,
            ItemBody::UserMessage {
                text: None,
                content: vec![
                    MessagePart::Attachment {
                        name: "a".into(),
                        path: "/project/a".into(),
                    },
                    MessagePart::Image {
                        source: "/project/a".into(),
                    },
                    MessagePart::Image {
                        source: "data:image/png;base64,a".into(),
                    },
                    MessagePart::Invocation {
                        name: "skill".into(),
                        path: "/project/skill".into(),
                    },
                ],
            },
        ));
        let source = Arc::new(models::Thread {
            turns: Some(vec![Arc::new(models::Turn {
                items: Some(vec![item]),
                ..Default::default()
            })]),
            ..Default::default()
        });
        let graph = project_conversation(
            UiLanguage::Japanese,
            &Default::default(),
            &Default::default(),
            source.clone(),
            &None,
        );
        assert_eq!(
            graph.source_paths.as_slice(),
            ["/project/a", "/project/skill"]
        );
        let mut changed = source.as_ref().clone();
        changed.name = Some("A new title".into());
        let next = project_conversation(
            UiLanguage::Japanese,
            &Default::default(),
            &Default::default(),
            Arc::new(changed),
            &Some(graph.clone()),
        );
        assert!(Arc::ptr_eq(&graph.source_paths, &next.source_paths));
    }
}

#[cfg(test)]
mod preparation_tests {
    use super::*;
    use agent_protocol::{
        operations::{Input, Submission},
        session::{
            PreparationStage, PreparationStatus, ProviderKind, SessionPreparation, SessionRef,
        },
    };

    #[test]
    fn startup_labels_follow_host_stages_in_both_languages() {
        use crate::localization::UiLanguage;
        let id = SessionRef::new(ProviderKind::Codex, "startup".into()).unwrap();
        let mut preparation = SessionPreparation {
            submission: Submission {
                fresh_context: false,
                thread_id: id,
                client_user_message_id: "input".into(),
                input: Vec::new(),
                model: None,
                effort: None,
                service_tier: None,
                follow_up: Default::default(),
            },
            status: PreparationStatus::Preparing,
            stage: PreparationStage::Workspace,
        };
        for (stage, japanese, english) in [
            (PreparationStage::Workspace, "開始中…", "Starting…"),
            (
                PreparationStage::Worktree,
                "ワークツリーを準備中…",
                "Setting up worktree…",
            ),
            (PreparationStage::Provider, "開始中…", "Starting…"),
        ] {
            preparation.stage = stage;
            for (language, expected) in [
                (UiLanguage::Japanese, japanese),
                (UiLanguage::English, english),
            ] {
                let presentation = preparation_presentation(language, &preparation);
                assert!(presentation.in_progress);
                assert!(presentation.retry.is_none());
                assert_eq!(presentation.text, expected);
            }
        }
        preparation.status = PreparationStatus::Sending;
        assert_eq!(
            preparation_presentation(UiLanguage::Japanese, &preparation).text,
            "開始中…"
        );
        preparation.status = PreparationStatus::Failed {
            error: models::ExecutionError {
                message: "設定\n{0}".into(),
                ..Default::default()
            },
        };
        assert_eq!(
            preparation_presentation(UiLanguage::English, &preparation).text,
            "Preparation failed: 設定\n{0}"
        );
    }

    fn preparing_thread() -> models::Thread {
        let id = SessionRef::new(ProviderKind::Codex, "admitted".into()).unwrap();
        models::Thread {
            id: Some(id.clone()),
            preparations: vec![SessionPreparation {
                submission: Submission {
                    fresh_context: false,
                    thread_id: id.clone(),
                    client_user_message_id: "input".into(),
                    input: vec![Input::Text {
                        text: "accepted text".into(),
                    }],
                    model: None,
                    effort: None,
                    service_tier: None,
                    follow_up: Default::default(),
                },
                status: PreparationStatus::Preparing,
                stage: PreparationStage::Workspace,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn admitted_input_is_visible_once_through_failure_retry_and_native_echo() {
        let mut thread = preparing_thread();
        let id = thread.id.clone().unwrap();
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.sessions.pending_submissions).insert(
            "input".into(),
            Arc::new(PendingSubmission {
                new_draft_slot: None,
                cursor: None,
                sequence: 0,
                submitted_at_ms: Some(12_000),
                draft_key: id.clone().into(),
                draft: Arc::new(crate::state::Draft {
                    text: "accepted text".into(),
                    ..Default::default()
                }),
                turn_id: None,
                after_item_id: None,
                accepted: false,
                delivery_unknown: false,
                queued: false,
            }),
        );
        let prepared = project_conversation(
            UiLanguage::Japanese,
            &snapshot.panes.primary().navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            Arc::new(thread.clone()),
            &None,
        );
        let mut timed = snapshot.clone();
        Arc::make_mut(
            Arc::make_mut(&mut timed.sessions.pending_submissions)
                .get_mut(&agent_protocol::ids::ClientInputId::from("input"))
                .unwrap(),
        )
        .submitted_at_ms = Some(13_000);
        let retimed = project_conversation(
            UiLanguage::Japanese,
            &timed.panes.primary().navigation.draft_key,
            &timed.sessions.pending_submissions,
            prepared.source.clone(),
            &Some(prepared.clone()),
        );
        assert_eq!(retimed.queued[0].data.timestamp_ms, Some(13_000));
        assert_eq!(prepared.queued[0].data.timestamp_ms, Some(12_000));
        assert_eq!(prepared.queued.len(), 1);
        assert_eq!(prepared.queued[0].data.timestamp_ms, Some(12_000));
        assert_eq!(
            prepared.queued[0].data.body.as_deref(),
            Some("accepted text")
        );
        let ConversationRowContent::Preparation { preparation } = &prepared.session_rows[0].content
        else {
            panic!("missing preparation")
        };
        assert!(preparation.in_progress && preparation.retry.is_none());
        thread.preparations.first_mut().unwrap().status = PreparationStatus::Failed {
            error: Default::default(),
        };
        let failed = project_conversation(
            UiLanguage::Japanese,
            &snapshot.panes.primary().navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            Arc::new(thread.clone()),
            &Some(prepared.clone()),
        );
        assert!(Arc::ptr_eq(&prepared.queued[0], &failed.queued[0]));
        let ConversationRowContent::Preparation { preparation } = &failed.session_rows[0].content
        else {
            panic!("missing failure")
        };
        assert!(preparation.retry.is_some() && !preparation.in_progress);
        let mut user = models::Item::new(
            "native-input".into(),
            models::ItemStatus::Completed,
            models::ItemBody::UserMessage {
                text: Some("accepted text".into()),
                content: Vec::new(),
            },
        );
        user.client_input_id = Some("input".into());
        thread.preparations.clear();
        thread.turns = Some(vec![Arc::new(models::Turn {
            id: "native-turn".into(),
            items: Some(vec![Arc::new(user)]),
            ..Default::default()
        })]);
        let echoed = project_conversation(
            UiLanguage::Japanese,
            &snapshot.panes.primary().navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            Arc::new(thread.clone()),
            &Some(failed),
        );
        assert!(echoed.queued.is_empty());
        assert!(echoed.session_rows.is_empty());
        assert_eq!(
            echoed.turns[0]
                .rows
                .iter()
                .filter(|row| matches!(row.content, ConversationRowContent::User { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn every_failed_input_has_its_own_recovery_after_the_saved_history() {
        let mut thread = preparing_thread();
        thread.turns = Some(vec![Arc::new(models::Turn {
            id: "previous-turn".into(),
            status: models::TurnStatus::Completed,
            items: Some(vec![Arc::new(models::Item::new(
                "previous-reply".into(),
                models::ItemStatus::Completed,
                models::ItemBody::AssistantText {
                    text: "previous answer".into(),
                    phase: Default::default(),
                },
            ))]),
            ..Default::default()
        })]);
        thread.preparations[0].status = PreparationStatus::Failed {
            error: Default::default(),
        };
        let mut second = thread.preparations[0].clone();
        second.submission.client_user_message_id = "second-input".into();
        second.submission.input = vec![Input::Text {
            text: "second text".into(),
        }];
        second.status = PreparationStatus::Unknown;
        thread.preparations.push(second);
        let project = |thread: models::Thread, previous| {
            project_conversation(
                UiLanguage::English,
                &Default::default(),
                &Default::default(),
                Arc::new(thread),
                &previous,
            )
        };
        let failed = project(thread.clone(), None);
        assert_eq!(
            failed
                .queued
                .iter()
                .map(|item| item.data.body.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("accepted text"), Some("second text")]
        );
        assert!(failed.turns[0].items().all(|item| item.data.kind != "user"));
        assert_eq!(failed.session_rows.len(), 2);
        assert_ne!(failed.session_rows[0].id, failed.session_rows[1].id);
        let ConversationRowContent::Preparation { preparation } = &failed.session_rows[0].content
        else {
            panic!("missing retry")
        };
        assert_eq!(
            preparation.retry.as_ref().unwrap().input_id.as_str(),
            "input"
        );
        assert_eq!(failed.queued[0].unknown_submission_id(), None);
        assert!(failed.queued[1].data.native_id.is_none());
        assert_eq!(
            failed.queued[1].unknown_submission_id().as_deref(),
            Some("second-input")
        );
        thread.preparations.remove(0);
        let remaining = project(thread, Some(failed.clone()));
        assert_eq!(remaining.session_rows.len(), 1);
        assert!(Arc::ptr_eq(&remaining.queued[0], &failed.queued[1]));
        assert!(Arc::ptr_eq(&remaining.turns[0], &failed.turns[0]));
    }

    #[test]
    fn a_running_turn_hides_only_startup_rows_and_keeps_recovery_rows() {
        for (status, recovery) in [
            (PreparationStatus::Preparing, false),
            (PreparationStatus::Sending, false),
            (
                PreparationStatus::Failed {
                    error: Default::default(),
                },
                true,
            ),
            (PreparationStatus::Unknown, true),
        ] {
            let mut thread = preparing_thread();
            thread.preparations[0].status = status;
            thread.turns = Some(vec![Arc::new(models::Turn {
                id: "running".into(),
                status: models::TurnStatus::Running,
                ..Default::default()
            })]);
            let projected = project_conversation(
                UiLanguage::Japanese,
                &Default::default(),
                &Default::default(),
                Arc::new(thread),
                &None,
            );
            assert_eq!(
                projected
                    .session_rows
                    .iter()
                    .any(|row| row.id == "preparation:input"),
                recovery
            );
            assert!(
                projected
                    .turns
                    .iter()
                    .flat_map(|turn| turn.rows.iter())
                    .any(|row| matches!(row.content, ConversationRowContent::InProgress { .. }))
            );
        }
    }

    #[test]
    fn pending_input_edits_do_not_change_saved_turn_rows() {
        let mut thread = preparing_thread();
        thread.turns = Some(vec![Arc::new(models::Turn {
            id: "starting".into(),
            status: models::TurnStatus::Running,
            ..Default::default()
        })]);
        let mut previous = None;
        for expected in [Some("accepted text"), Some("corrected text"), None] {
            if let Some(text) = expected {
                thread.preparations.first_mut().unwrap().submission.input =
                    vec![Input::Text { text: text.into() }];
            } else {
                thread.preparations.clear();
            }
            let projected = project_conversation(
                UiLanguage::Japanese,
                &Default::default(),
                &Default::default(),
                Arc::new(thread.clone()),
                &previous,
            );
            let users: Vec<_> = projected
                .queued
                .iter()
                .filter(|item| item.data.kind == "user")
                .map(|item| item.data.body.as_deref())
                .collect();
            assert_eq!(users, expected.map(Some).into_iter().collect::<Vec<_>>());
            previous = Some(projected);
        }
    }
}

fn render_turn(
    session: Option<&crate::session::SessionRef>,
    (supports_fork, supports_rewind): (bool, bool),
    handoff: Option<String>,
    (source, turn_occurrence, language): (Arc<models::Turn>, usize, UiLanguage),
    pending: PendingItems,
    requests: Vec<Arc<WireRequest>>,
    previous: Option<&Arc<RenderedTurn>>,
) -> Arc<RenderedTurn> {
    let session_provider = session.and_then(|session| session.provider);
    let provider = source.provider.or(session_provider);
    let supports_fork = source.supports_fork.unwrap_or(supports_fork);
    let supports_rewind = supports_rewind && source.supports_rewind.unwrap_or(true);
    let cached: HashMap<_, _> = previous
        .into_iter()
        .filter(|turn| turn.source.provider.or(session_provider) == provider)
        .flat_map(|turn| turn.items())
        .map(|item| (item.key(), item))
        .collect();
    let native = source.items.as_deref().unwrap_or_default();
    let retained: Vec<_> = pending.iter().collect();
    let order = source_order(
        native.len(),
        |index| native[index].id.as_str(),
        retained
            .iter()
            .map(|(_, pending)| pending.after_item_id.as_deref()),
    );
    let metadata = |index: usize| {
        let index = order[index];
        if let Some(item) = native.get(index) {
            ItemMetadata::from(item.as_ref())
        } else {
            let id = retained[index - native.len()].0.as_str();
            ItemMetadata {
                id,
                kind: super::GroupKind::User,
                ..Default::default()
            }
        }
    };
    let render_native = |item: &Arc<models::Item>| {
        RenderedItem::native(
            language,
            item,
            provider,
            source.status,
            None,
            None,
            cached.get(&(None, Arc::as_ptr(item).cast())).copied(),
        )
    };
    let render = |index: usize| {
        let index = order[index];
        if let Some(item) = native.get(index) {
            render_native(item)
        } else {
            let (id, submission) = retained[index - native.len()];
            RenderedItem::pending(
                language,
                id,
                submission,
                cached
                    .get(&(Some(id.as_str()), Arc::as_ptr(submission).cast()))
                    .copied(),
            )
        }
    };
    use ConversationRowContent::*;
    let steering = std::cell::Cell::new(false);
    let queued_shown = std::cell::Cell::new(false);
    let edit_shown = std::cell::Cell::new(false);
    let fresh_context_shown = std::cell::Cell::new(false);
    let mut rows = Vec::new();
    let mut occurrences = HashMap::new();
    let mut push = |mut content: ConversationRowContent| {
        let id = match &content {
            ProviderHandoff { .. } => format!("history-handoff:{}", source.id),
            Notification { .. } => format!("history-notification:{}", source.id),
            User { item } | Activity { item, .. } | Response { item, .. } => {
                format!("history-item:{}:{}", source.id, item.data.id)
            }
            ActivityHeader { activity } => activity.id.clone(),
            PendingRequest { request } => format!("history-request:{}", request.id),
            Error { .. } => format!("history-error:{}", source.id),
            InProgress { turn_id } => format!("in-progress:{turn_id}"),
            Preparation { .. } => "preparation".into(),
            Background { .. } => "background-work".into(),
            Queue { .. } => "queued-messages".into(),
        };
        let occurrence = occurrences.entry(id.clone()).or_insert(0usize);
        let id = conversation_row_id(source.id.as_str(), turn_occurrence, &id, *occurrence);
        *occurrence += 1;
        if let ActivityHeader { activity } = &mut content {
            activity.id = id.clone();
        }
        let started = source.started_at_ms.or_else(|| {
            source
                .started_at
                .filter(|seconds| {
                    seconds.is_finite() && *seconds >= 0. && *seconds < u64::MAX as f64 / 1000.
                })
                .map(|seconds| (seconds * 1000.) as u64)
        });
        let timestamp_ms = match &content {
            User { item } => item
                .data
                .timestamp_ms
                .or(item.data.native_id.as_ref().and(started)),
            Response {
                show_actions: true, ..
            } => source.completed_at_ms.or_else(|| {
                started
                    .zip(source.duration_ms)
                    .and_then(|(start, duration)| start.checked_add(duration))
                    .or(started)
            }),
            _ => None,
        };
        let user = matches!(content, User { .. });
        let steer = user && steering.get();
        // The message that started a turn after waiting in the queue.
        let queued = user && !steer && source.queued && !queued_shown.replace(true);
        let fresh_context = user && source.fresh_context && !fresh_context_shown.replace(true);
        let edit_turn_id = (user
            && !steer
            && supports_rewind
            && source.status != models::TurnStatus::Running
            && !edit_shown.replace(true))
        .then(|| source.id.clone());
        rows.push(ConversationRow {
            id,
            content,
            timestamp_ms,
            steer,
            queued,
            fresh_context,
            edit_turn_id,
        });
    };
    if let Some(text) = handoff {
        push(ProviderHandoff { text });
    }
    let segments: Vec<_> = project_items(language, &source, order.len(), metadata).collect();
    // The last assistant message owns a settled turn's actions. Image rows
    // do not take copy or fork actions away from its text.
    let responses = |segment: &super::Segment| -> Vec<usize> {
        (segment.start..segment.end)
            .filter(|&index| segment.role(index, metadata(index)) == Role::Response)
            .collect()
    };
    let terminal = segments.iter().rev().find_map(|segment| {
        responses(segment)
            .into_iter()
            .rfind(|&index| metadata(index).kind == super::GroupKind::Assistant)
    });
    for segment in &segments {
        // An earlier input or wake message makes the next input a steer.
        // Informational notices do not create such a preceding message.
        let group = |role| {
            (segment.start..segment.end)
                .filter(move |&index| segment.role(index, metadata(index)) == role)
                .map(&render)
        };
        let in_progress = segment.last && source.status == models::TurnStatus::Running;
        for item in group(Role::Notice) {
            push(Activity {
                item,
                turn_id: source.id.clone(),
            });
        }
        for item in group(Role::User) {
            if let ItemSource::Native(native) = &item.source
                && let models::ItemBody::Notification { reports } = native.body()
            {
                push(Notification {
                    reports: reports.clone(),
                    icon: super::background_work::notification_icon(reports.clone()),
                });
            } else if item.data.deferred {
                push(Activity {
                    item,
                    turn_id: source.id.clone(),
                });
            } else {
                push(User { item });
            }
            steering.set(true);
        }
        if let Some(summary) = &segment.label {
            push(ActivityHeader {
                activity: ActivityPresentation {
                    id: segment.id.clone(),
                    status: source.status.label().into(),
                    activity_summary: summary.clone(),
                    activity_can_collapse: segment.collapsible,
                    is_in_progress: in_progress,
                },
            });
            for item in group(Role::Activity) {
                push(Activity {
                    item,
                    turn_id: source.id.clone(),
                });
            }
        }
        if segment.last {
            for pending in &requests {
                push(PendingRequest {
                    request: Box::new(request(language, pending)),
                });
            }
            if let Some(error) = &source.error {
                push(Error {
                    error: turn_error(language, error),
                });
            }
        }
        for index in responses(segment) {
            let item = render(index);
            let terminal = Some(index) == terminal;
            let show_actions = shows_actions(source.status, &item.data.kind, terminal);
            let can_fork = supports_fork && show_actions && terminal;
            if item.data.deferred {
                push(Activity {
                    item,
                    turn_id: source.id.clone(),
                });
            } else {
                push(Response {
                    item,
                    show_actions,
                    fork_turn_id: can_fork.then(|| source.id.clone()),
                });
            }
        }
        if in_progress {
            push(InProgress {
                turn_id: source.id.clone(),
            });
        }
    }
    Arc::new(RenderedTurn {
        supports_fork,
        language,
        source,
        occurrence: turn_occurrence,
        pending,
        requests,
        rows: images::without_repeated_images(rows),
    })
}

fn request_delivery_label(
    delivery: agent_protocol::session::RequestDelivery,
) -> Option<&'static str> {
    use agent_protocol::session::RequestDelivery;
    match delivery {
        RequestDelivery::Awaiting => None,
        RequestDelivery::Sending => Some("回答を送信中"),
        RequestDelivery::Sent => Some("回答を送信しました"),
        RequestDelivery::Unknown => Some("回答の配送結果が不明です"),
    }
}

pub fn request(language: UiLanguage, source: &WireRequest) -> Request {
    use agent_protocol::requests::{ApprovalKind, RequestBody};
    let (title, body, details) = match &source.body {
        RequestBody::Approval {
            kind,
            description,
            details,
            ..
        } => {
            // T3 Code's approval labels.
            let title = match kind {
                ApprovalKind::Command => "コマンドの承認",
                ApprovalKind::FileChange => "ファイル変更の承認",
                ApprovalKind::Tool => "ツールの承認",
            };
            (title, description.clone(), details.clone())
        }
        RequestBody::Permission {
            description,
            details,
            ..
        } => ("権限の承認", description.clone(), details.clone()),
        RequestBody::Question { questions } => (
            "回答待ち",
            questions
                .first()
                .map(|question| question.prompt.clone())
                .unwrap_or_default(),
            String::new(),
        ),
        RequestBody::Elicitation {
            server, message, ..
        } => (
            "MCPからの入力待ち",
            format!("{server}\n{message}"),
            String::new(),
        ),
        RequestBody::ToolExecution {
            tool, arguments, ..
        } => (
            "ツールの入力待ち",
            tool.clone(),
            serde_json::to_string_pretty(arguments).expect("arguments serialize"),
        ),
    };
    Request {
        approval: inline_approval(
            language,
            source.delivery,
            &source.body,
            language.text(title),
        ),
        id: source.id.clone(),
        title: language
            .text(request_delivery_label(source.delivery).unwrap_or(title))
            .into(),
        body,
        details,
        can_respond: source.delivery == crate::session::RequestDelivery::Awaiting,
        request_body: source.body.clone(),
    }
}

/// How prominently a request action is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ActionTone {
    Primary,
    Secondary,
    Danger,
}

impl Request {
    /// The input request's body, then its details, then its label as a fallback.
    pub fn summary(&self) -> &str {
        [&self.body, &self.details, &self.title]
            .into_iter()
            .find(|text| !text.is_empty())
            .map_or("", String::as_str)
    }
}

/// A decision an approval offers, labelled in the UI language.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ApprovalAction {
    pub choice_id: String,
    pub label: String,
}

/// An approval's decisions as T3 Code lays them out: declining (outlined)
/// and approving once (filled) as buttons, every other decision (a lasting
/// grant, a rule) in a menu after them, in the provider's order.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ApprovalActions {
    pub decline: Option<ApprovalAction>,
    pub approve: Option<ApprovalAction>,
    pub more: Vec<ApprovalAction>,
}

pub fn approval_actions(
    language: UiLanguage,
    choices: &[agent_protocol::requests::Choice],
) -> ApprovalActions {
    use agent_protocol::requests::ChoiceEffect;
    let action = |choice: &agent_protocol::requests::Choice| ApprovalAction {
        choice_id: choice.id.clone(),
        label: language.text(&choice.label).into(),
    };
    let first = |effect| choices.iter().position(|choice| choice.effect == effect);
    let decline = first(ChoiceEffect::Deny);
    let approve = first(ChoiceEffect::Allow);
    ApprovalActions {
        decline: decline.map(|index| action(&choices[index])),
        approve: approve.map(|index| action(&choices[index])),
        more: choices
            .iter()
            .enumerate()
            .filter(|(index, _)| Some(*index) != decline && Some(*index) != approve)
            .map(|(_, choice)| action(choice))
            .collect(),
    }
}

/// Approvals are actions on timeline work; questions and other input use the composer.
struct ApprovalInput<'a> {
    kind: &'static str,
    detail: &'a str,
    reason: &'a str,
    choices: &'a [agent_protocol::requests::Choice],
    permission: bool,
}

fn approval_input(body: &agent_protocol::requests::RequestBody) -> Option<ApprovalInput<'_>> {
    use agent_protocol::requests::{ApprovalKind, RequestBody};
    let (kind, detail, reason, choices, permission) = match body {
        RequestBody::Approval {
            kind,
            details,
            description,
            choices,
        } => (
            match kind {
                ApprovalKind::Command => "command",
                ApprovalKind::FileChange => "fileChange",
                ApprovalKind::Tool => "toolCall",
            },
            details,
            description,
            choices,
            false,
        ),
        RequestBody::Permission {
            details,
            description,
            choices,
        } => ("toolCall", details, description, choices, true),
        _ => return None,
    };
    Some(ApprovalInput {
        kind,
        detail,
        reason,
        choices,
        permission,
    })
}

#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct InlineApproval {
    pub kind: String,
    pub detail: String,
    pub details: String,
    pub status: String,
    pub notes: Vec<String>,
    pub actions: ApprovalActions,
}

fn inline_approval(
    language: UiLanguage,
    delivery: agent_protocol::session::RequestDelivery,
    body: &agent_protocol::requests::RequestBody,
    title: &str,
) -> Option<InlineApproval> {
    let input = approval_input(body)?;
    let detail = [input.reason, input.detail, title]
        .into_iter()
        .find(|value| !value.trim().is_empty())
        .unwrap_or("");
    Some(InlineApproval {
        kind: input.kind.into(),
        detail: detail.into(),
        details: if input.detail == detail {
            String::new()
        } else {
            input.detail.into()
        },
        status: language
            .text(request_delivery_label(delivery).unwrap_or("承認待ち"))
            .into(),
        notes: input
            .choices
            .iter()
            .map(|choice| &choice.description)
            .filter(|note| !note.trim().is_empty())
            .fold(Vec::new(), |mut notes, note| {
                if !notes.contains(note) {
                    notes.push(note.clone());
                }
                notes
            }),
        actions: approval_actions(language, input.choices),
    })
}

/// Resolve only an offered approval decision. Both native clients route the same answer.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn request_choice_answer(
    body: agent_protocol::requests::RequestBody,
    choice_id: String,
) -> Option<agent_protocol::requests::Answer> {
    use agent_protocol::requests::Answer;
    let input = approval_input(&body)?;
    if !input.choices.iter().any(|choice| choice.id == choice_id) {
        return None;
    }
    Some(if input.permission {
        Answer::Permission { choice_id }
    } else {
        Answer::Approval { choice_id }
    })
}

/// The composer answers one question and one non-approval input request at a time.
pub(crate) fn docked_request(thread: &models::Thread, question: bool) -> Option<&WireRequest> {
    thread.requests.values().map(Arc::as_ref).find(|request| {
        matches!(
            request.body,
            agent_protocol::requests::RequestBody::Question { .. }
        ) == question
            && (question || approval_input(&request.body).is_none())
    })
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    /// The requests shown above the composer: the other request, then the question.
    pub fn docked_requests(&self, session: crate::session::SessionRef) -> Vec<Request> {
        let Some(thread) = self.sessions.conversations.get(&session) else {
            return Vec::new();
        };
        [false, true]
            .into_iter()
            .filter_map(|question| docked_request(thread, question))
            .map(|source| request(self.catalog.ui_language, source))
            .collect()
    }
}

/// Native editors keep typed choice IDs separate from free text.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn build_question_answer(
    multiple: bool,
    text: String,
    choice_ids: Vec<String>,
) -> agent_protocol::requests::QuestionAnswer {
    use agent_protocol::requests::QuestionAnswer;
    if !text.is_empty() {
        QuestionAnswer::FreeText { text }
    } else if multiple {
        QuestionAnswer::MultipleChoices { choice_ids }
    } else {
        QuestionAnswer::SingleChoice {
            choice_id: choice_ids.into_iter().next().unwrap_or_default(),
        }
    }
}

pub fn answer_from_json(
    body: &agent_protocol::requests::RequestBody,
    text: &str,
) -> Result<agent_protocol::requests::Answer, String> {
    use agent_protocol::requests::{Answer, ElicitationAnswer, RequestBody, ToolContent};
    let answer = match body {
        RequestBody::Elicitation { .. } => Answer::Elicitation {
            action: ElicitationAnswer::Accept {
                values: serde_json::from_str(text).map_err(|error| error.to_string())?,
            },
        },
        RequestBody::ToolExecution { .. } => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct ResultInput {
                success: bool,
                content: Vec<ToolContent>,
            }
            let value: ResultInput =
                serde_json::from_str(text).map_err(|error| error.to_string())?;
            Answer::ToolExecution {
                success: value.success,
                content: value.content,
            }
        }
        _ => return Err("this request requires a typed choice or question answer".into()),
    };
    agent_protocol::requests::validate_answer(body, &answer)?;
    Ok(answer)
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn request_input_default(body: agent_protocol::requests::RequestBody) -> String {
    use agent_protocol::requests::{ElicitationInput, FormInput, RequestBody};
    let value = match body {
        RequestBody::Elicitation {
            input: ElicitationInput::Form { fields },
            ..
        } => Value::Object(
            fields
                .into_iter()
                .filter_map(|field| {
                    let default = match field.input {
                        FormInput::String { default, .. } | FormInput::Choice { default, .. } => {
                            default.map(Value::from)
                        }
                        FormInput::Number { default, .. } => default.map(Value::from),
                        FormInput::Boolean { default } => default.map(Value::from),
                        FormInput::Multiple { default, .. } if !default.is_empty() => {
                            Some(serde_json::json!(default))
                        }
                        _ => None,
                    };
                    default.map(|value| (field.name, value))
                })
                .collect(),
        ),
        RequestBody::Elicitation {
            input: ElicitationInput::Url { .. },
            ..
        } => Value::Null,
        RequestBody::ToolExecution { .. } => serde_json::json!({"success":true,"content":[]}),
        _ => Value::Null,
    };
    serde_json::to_string_pretty(&value).expect("request defaults serialize")
}
/// Whether a turn's message of `kind` shows its actions: an answer once its
/// turn settles (a provider message can end while the response still runs),
/// and commentary that is the `terminal` message of a turn stopped before
/// its answer.
fn shows_actions(status: models::TurnStatus, kind: &str, terminal: bool) -> bool {
    status != models::TurnStatus::Running && (kind == "agent" || terminal && kind == "commentary")
}

/// A turn's error, titled as T3 Code's work log titles a provider's error:
/// its retry, a usage limit, or a provider error, which the message explains.
fn turn_error(language: UiLanguage, error: &models::ExecutionError) -> TurnErrorPresentation {
    let retry = error.retry.as_ref().filter(|retry| retry.retrying);
    let retrying = retry.is_some();
    let is_usage_limit = !retrying && error.category == models::ErrorCategory::UsageLimit;
    // T3 Code's progress: the attempt, of how many when the provider says.
    let progress = retry.and_then(|retry| {
        let attempt = retry.attempt?;
        Some(
            retry
                .max_attempts
                .map_or_else(|| attempt.to_string(), |max| format!("{attempt}/{max}")),
        )
    });
    let title = match progress {
        Some(progress) => {
            localized_ui_format(language, "error.retrying.progress".into(), vec![progress])
        }
        None if retrying => language.text("プロバイダーを再試行しています").into(),
        None if is_usage_limit => language.text("利用上限に達しました").into(),
        None => language.text("プロバイダーエラー").into(),
    };
    TurnErrorPresentation {
        title,
        message: language.text(&error.message).into(),
        details: error.details.clone(),
        is_reconnecting: retrying,
        is_usage_limit,
    }
}

pub type PendingItems = Vec<(agent_protocol::ids::ClientInputId, Arc<PendingSubmission>)>;
#[derive(Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ItemPresentation {
    pub author: Option<Box<agent_protocol::attribution::Author>>,
    pub id: String,
    pub native_id: Option<agent_protocol::ids::ItemId>,
    pub timestamp_ms: Option<u64>,
    pub body: Option<String>,
    pub image_sources: Vec<String>,
    pub contexts: Vec<agent_protocol::context::MessageContext>,
    pub image_placeholder: bool,
    pub empty_response: bool,
    pub deferred: bool,
    pub kind: String,
    pub title: Option<String>,
    pub collapsible: bool,
    /// Reasoning's trace on one line, provider text shown verbatim.
    pub preview: Option<String>,
}
#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Request {
    pub approval: Option<InlineApproval>,
    pub id: agent_protocol::ids::RequestId,
    pub title: String,
    pub body: String,
    pub can_respond: bool,
    pub details: String,
    pub request_body: agent_protocol::requests::RequestBody,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Draft, Snapshot};
    use serde_json::json;
    use std::collections::HashSet;

    #[test]
    fn only_settled_missing_assistant_bodies_show_the_empty_response_copy() {
        for (text, streaming, expected) in [
            ("", true, ""),
            ("", false, "(empty response)"),
            (" ", false, " "),
            ("Answer 日本語", true, "Answer 日本語"),
            ("Answer 日本語", false, "Answer 日本語"),
        ] {
            assert_eq!(assistant_response_text(text, streaming), expected);
        }
    }

    #[test]
    fn rendered_response_placeholder_does_not_change_copy_text_or_empty_user_messages() {
        use models::{AssistantPhase, Item, ItemBody, ItemStatus};
        for status in [
            ItemStatus::Running,
            ItemStatus::Completed,
            ItemStatus::Failed,
            ItemStatus::Interrupted,
            ItemStatus::Unknown,
        ] {
            for (body, expected) in [
                (
                    ItemBody::AssistantText {
                        text: String::new(),
                        phase: AssistantPhase::Final,
                    },
                    if status == ItemStatus::Running {
                        ""
                    } else {
                        "(empty response)"
                    },
                ),
                (
                    ItemBody::UserMessage {
                        text: Some(String::new()),
                        content: vec![],
                    },
                    "",
                ),
                (
                    ItemBody::AssistantText {
                        text: "**Answer** 日本語".into(),
                        phase: AssistantPhase::Commentary,
                    },
                    "**Answer** 日本語",
                ),
            ] {
                let source = Arc::new(Item::new("response".into(), status, body));
                let item = RenderedItem::native(
                    UiLanguage::English,
                    &source,
                    Some(crate::session::ProviderKind::Codex),
                    crate::models::TurnStatus::Completed,
                    None,
                    None,
                    None,
                );
                assert_eq!(item.is_streaming(), status == ItemStatus::Running);
                assert_eq!(item.response_text(), Some(expected));
                if expected == "(empty response)" {
                    assert_eq!(item.data.body.as_deref(), Some(""));
                }
            }
        }
    }

    #[test]
    fn context_only_messages_render_chips_without_a_second_plain_text_body() {
        let context = agent_protocol::context::MessageContext {
            id: "build".into(),
            label: "Build".into(),
            content: agent_protocol::context::ContextContent::Terminal {
                terminal_id: "terminal".into(),
                terminal_label: "Build".into(),
                line_start: 1,
                line_end: 1,
                text: "literal **output**".into(),
            },
        };
        for text in [None, Some("explain")] {
            let item = Arc::new(models::Item::new(
                "user".into(),
                models::ItemStatus::Completed,
                models::ItemBody::UserMessage {
                    text: text.map(str::to_owned),
                    content: vec![models::MessagePart::Context {
                        context: Box::new(context.clone()),
                    }],
                },
            ));
            let shown = RenderedItem::native(
                UiLanguage::English,
                &item,
                None,
                models::TurnStatus::Completed,
                None,
                None,
                None,
            );
            assert_eq!(
                shown.message_body().as_deref(),
                Some(text.unwrap_or_default())
            );
            assert_eq!(
                shown.data.contexts.as_slice(),
                std::slice::from_ref(&context)
            );
            assert!(
                shown
                    .data
                    .body
                    .as_deref()
                    .unwrap()
                    .contains("literal **output**")
            );
            assert!(!shown.collapses_user_text());
        }
        let pending = Arc::new(PendingSubmission {
            new_draft_slot: None,
            cursor: None,
            sequence: 0,
            submitted_at_ms: None,
            draft_key: Default::default(),
            draft: Arc::new(Draft {
                text: "explain".into(),
                contexts: vec![context],
                ..Default::default()
            }),
            turn_id: None,
            after_item_id: None,
            accepted: false,
            delivery_unknown: false,
            queued: false,
        });
        let shown = RenderedItem::pending(UiLanguage::English, "pending", &pending, None);
        assert_eq!(shown.message_body().as_deref(), Some("explain"));
        assert!(
            shown
                .data
                .body
                .as_deref()
                .unwrap()
                .contains("literal **output**")
        );
    }

    proptest::proptest! {
        #[test]
        fn row_identity_distinguishes_every_source_component(
            source in (proptest::prelude::any::<String>(), proptest::prelude::any::<usize>(), proptest::prelude::any::<String>(), proptest::prelude::any::<usize>()),
            other in (proptest::prelude::any::<String>(), proptest::prelude::any::<usize>(), proptest::prelude::any::<String>(), proptest::prelude::any::<usize>()),
        ) {
            let source = (source.0.as_str(), source.1, source.2.as_str(), source.3);
            let other = (other.0.as_str(), other.1, other.2.as_str(), other.3);
            let original = conversation_row_id(source.0, source.1, source.2, source.3);
            for candidate in [
                (other.0, source.1, source.2, source.3),
                (source.0, other.1, source.2, source.3),
                (source.0, source.1, other.2, source.3),
                (source.0, source.1, source.2, other.3),
                other,
            ] {
                if candidate != source {
                    let changed = conversation_row_id(candidate.0, candidate.1, candidate.2, candidate.3);
                    proptest::prop_assert_ne!(&original, &changed);
                }
            }
        }
    }

    #[test]
    fn a_notification_starts_the_turn_its_agent_woke_for_as_its_own_row() {
        use agent_protocol::execution::{BackgroundOutcome, BackgroundReport, BackgroundTaskKind};
        let source = Arc::new(models::Thread {
            turns: Some(vec![Arc::new(models::Turn {
                id: "wake".into(),
                status: models::TurnStatus::Completed,
                items: Some(vec![
                    Arc::new(models::Item::new(
                        "wake:notification".into(),
                        models::ItemStatus::Completed,
                        models::ItemBody::Notification {
                            reports: vec![BackgroundReport {
                                kind: BackgroundTaskKind::Command,
                                label: Some("Start the dev server".into()),
                                outcome: BackgroundOutcome::Completed,
                                exit_code: None,
                            }],
                        },
                    )),
                    Arc::new(models::Item::new(
                        "report".into(),
                        models::ItemStatus::Completed,
                        models::ItemBody::AssistantText {
                            text: "The server is up.".into(),
                            phase: models::AssistantPhase::Final,
                        },
                    )),
                ]),
                ..Default::default()
            })]),
            ..Default::default()
        });
        let snapshot = Snapshot::default();
        let rows = project_conversation(
            crate::localization::UiLanguage::Japanese,
            &snapshot.panes.primary().navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            source,
            &None,
        )
        .turns[0]
            .rows
            .clone();
        let ConversationRowContent::Notification { reports, icon } = &rows[0].content else {
            panic!("the notification is the turn's first row");
        };
        assert_eq!(
            super::super::background_work::notification_text(
                reports.clone(),
                crate::localization::UiLanguage::English
            ),
            "Command \"Start the dev server\" finished"
        );
        assert_eq!(
            *icon,
            super::super::background_work::NotificationIcon::Terminal
        );
        assert!(
            !rows
                .iter()
                .any(|row| matches!(row.content, ConversationRowContent::User { .. })),
            "the agent woke without a user message"
        );
        assert!(rows.iter().any(|row| matches!(
            row.content,
            ConversationRowContent::Response {
                show_actions: true,
                ..
            }
        )));
    }

    #[test]
    fn row_identity_keeps_provider_delimiters_opaque() {
        assert_ne!(
            conversation_row_id("a:occurrence:0:turn:b", 0, "item", 0),
            conversation_row_id("b", 0, "item:occurrence:0:turn:a", 0),
        );
    }

    #[rstest::rstest]
    #[case::plan(json!({"plan":{"text":"Plan"}}), true)]
    #[case::mcp(json!({"toolCall":{"kind":"mcp","tool":"read","arguments":{},"content":[]}}), true)]
    #[case::dynamic(json!({"toolCall":{"kind":"dynamic","tool":"search","arguments":{},"content":[]}}), true)]
    #[case::subagent(json!({"subagent":{"tool":"spawn_agent","receivers":[],"states":[]}}), true)]
    #[case::web(json!({"webSearch":{"query":"Query"}}), true)]
    #[case::image(json!({"imageGeneration":{"savedPath":"/fixture/image.png"}}), true)]
    #[case::compaction(json!({"compaction":{}}), true)]
    #[case::sleep(json!({"sleep":{}}), false)]
    // As T3 Code, reasoning shows when it has text to read.
    #[case::reasoning(json!({"reasoning":{"content":["private"],"summary":[]}}), true)]
    #[case::blank_reasoning(json!({"reasoning":{"content":[""],"summary":[" "]}}), false)]
    #[case::review_entered(json!({"review":{"entering":true,"text":"Review"}}), false)]
    #[case::review_left(json!({"review":{"entering":false,"text":"Review"}}), false)]
    fn item_families_project_visible_content_without_exposing_state_only_events(
        #[case] body: serde_json::Value,
        #[case] visible: bool,
        #[values(models::TurnStatus::Running, models::TurnStatus::Completed)]
        status: models::TurnStatus,
    ) {
        let source = Arc::new(models::Thread {
            turns: Some(vec![Arc::new(models::Turn {
                id: "work".into(),
                status,
                items: Some(vec![
                    Arc::new(models::Item::new(
                        "family".into(),
                        Default::default(),
                        serde_json::from_value(body).unwrap(),
                    )),
                    Arc::new(models::Item::new(
                        "answer".into(),
                        Default::default(),
                        models::ItemBody::AssistantText {
                            text: "Final answer".into(),
                            phase: models::AssistantPhase::Final,
                        },
                    )),
                ]),
                ..Default::default()
            })]),
            ..Default::default()
        });
        let snapshot = Snapshot::default();
        let rendered = project_conversation(
            UiLanguage::Japanese,
            &snapshot.panes.primary().navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            source,
            &None,
        );
        let ids = rendered.turns[0]
            .items()
            .map(|item| item.data.native_id.as_deref().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            if visible {
                vec!["family", "answer"]
            } else {
                vec!["answer"]
            }
        );
    }

    #[rstest::rstest]
    #[case::distinct_turns(false)]
    #[case::repeated_turn_ids(true)]
    fn reopening_history_retains_each_answer_and_its_native_row_identity(#[case] repeated: bool) {
        let user = |text: &str| models::ItemBody::UserMessage {
            text: Some(text.into()),
            content: Vec::new(),
        };
        let reply = |text: &str, phase| models::ItemBody::AssistantText {
            text: text.into(),
            phase,
        };
        let source = Arc::new(models::Thread {
            turns: Some(
                [("old", "Earlier answer"), ("latest", "Latest answer")]
                    .into_iter()
                    .map(|(id, text)| {
                        Arc::new(models::Turn {
                            id: if repeated { "repeated" } else { id }.into(),
                            status: models::TurnStatus::Completed,
                            items: Some(
                                [
                                    ("question", user("First question")),
                                    (
                                        "work",
                                        models::ItemBody::CommandExecution {
                                            output_omitted: false,
                                            command: "pwd".into(),
                                            cwd: None,
                                            output: "/fixture".into(),
                                            exit_code: Some(0),
                                        },
                                    ),
                                    (
                                        "early",
                                        reply("Earlier reply", models::AssistantPhase::Unknown),
                                    ),
                                    ("followup", user("Next question")),
                                    (
                                        "commentary",
                                        reply(
                                            "Reply before the next instruction",
                                            models::AssistantPhase::Commentary,
                                        ),
                                    ),
                                    ("last-question", user("One more question")),
                                    ("answer", reply(text, models::AssistantPhase::Final)),
                                ]
                                .into_iter()
                                .map(|(item_id, body)| {
                                    Arc::new(models::Item::new(
                                        format!("{id}-{item_id}").into(),
                                        Default::default(),
                                        body,
                                    ))
                                })
                                .collect(),
                            ),
                            ..Default::default()
                        })
                    })
                    .collect(),
            ),
            ..Default::default()
        });
        let snapshot = Snapshot::default();
        let live = project_conversation(
            UiLanguage::Japanese,
            &snapshot.panes.primary().navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            source.clone(),
            &None,
        );
        let restored: Arc<models::Thread> =
            Arc::new(serde_json::from_slice(&serde_json::to_vec(&source).unwrap()).unwrap());
        let live_ids: Vec<_> = live
            .turns
            .iter()
            .flat_map(|turn| &turn.rows)
            .map(|row| row.id.clone())
            .collect();
        for previous in [None, Some(live.clone())] {
            let reopened = project_conversation(
                UiLanguage::Japanese,
                &snapshot.panes.primary().navigation.draft_key,
                &snapshot.sessions.pending_submissions,
                restored.clone(),
                &previous,
            );
            let rows = reopened
                .turns
                .iter()
                .flat_map(|turn| &turn.rows)
                .collect::<Vec<_>>();
            let answers = rows
                .iter()
                .filter_map(|row| match &row.content {
                    ConversationRowContent::Response { item, .. } => item.data.body.as_deref(),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                answers,
                [
                    "Earlier reply",
                    "Reply before the next instruction",
                    "Earlier answer",
                    "Earlier reply",
                    "Reply before the next instruction",
                    "Latest answer",
                ]
            );
            assert_eq!(
                rows.iter().map(|row| &row.id).collect::<HashSet<_>>().len(),
                rows.len()
            );
            assert_eq!(
                rows.iter().map(|row| &row.id).collect::<Vec<_>>(),
                live_ids.iter().collect::<Vec<_>>()
            );
            let activities: Vec<_> = rows
                .iter()
                .filter_map(|row| match &row.content {
                    ConversationRowContent::ActivityHeader { activity } => Some(activity),
                    _ => None,
                })
                .collect();
            assert_eq!(activities.len(), 2);
            assert_eq!(
                activities
                    .iter()
                    .map(|activity| &activity.id)
                    .collect::<HashSet<_>>()
                    .len(),
                activities.len()
            );
        }
        let mut changed = source.clone();
        Arc::make_mut(&mut Arc::make_mut(&mut changed).turns.as_mut().unwrap()[1]).status =
            models::TurnStatus::Interrupted;
        let updated = project_conversation(
            UiLanguage::Japanese,
            &snapshot.panes.primary().navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            changed,
            &Some(live.clone()),
        );
        assert!(Arc::ptr_eq(&live.turns[0], &updated.turns[0]));
        assert!(!Arc::ptr_eq(&live.turns[1], &updated.turns[1]));
    }

    #[test]
    fn work_expansion_requires_an_explicit_choice_for_the_current_status() {
        let statuses = ["running", "completed", "failed", "interrupted", "unknown"];
        for status in statuses {
            assert!(!activity_is_expanded(status, None));
            for previous in statuses {
                assert_eq!(
                    activity_is_expanded(status, Some(previous.into())),
                    status == previous,
                );
            }
        }
    }

    #[rstest::rstest]
    #[case::initial_viewport_needs_more(true, false, true, true, true, true)]
    #[case::wait_for_initial_latest_position(true, false, true, false, true, false)]
    #[case::older_boundary_is_visible(true, false, true, false, false, true)]
    #[case::viewport_is_filled(true, false, false, true, true, false)]
    #[case::reading_the_middle(true, false, false, false, false, false)]
    #[case::request_in_flight(true, true, true, true, true, false)]
    #[case::all_history_loaded(false, false, true, true, true, false)]
    fn history_pages_follow_the_viewport(
        #[case] has_more: bool,
        #[case] loading: bool,
        #[case] oldest_visible: bool,
        #[case] latest_visible: bool,
        #[case] following_latest: bool,
        #[case] expected: bool,
    ) {
        assert_eq!(
            should_load_history(
                has_more,
                loading,
                oldest_visible,
                latest_visible,
                following_latest
            ),
            expected
        );
    }

    #[test]
    fn language_changes_reproject_copy_and_retain_plain_message_caches() {
        let snapshot = fixture();
        let source = snapshot
            .sessions
            .conversations
            .values()
            .next()
            .unwrap()
            .clone();
        let project = |language, previous: Option<Arc<RenderedConversation>>| {
            project_conversation(
                language,
                &snapshot.panes.primary().navigation.draft_key,
                &snapshot.sessions.pending_submissions,
                source.clone(),
                &previous,
            )
        };
        let japanese = project(UiLanguage::Japanese, None);
        assert!(Arc::ptr_eq(
            &japanese,
            &project(UiLanguage::Japanese, Some(japanese.clone()))
        ));
        let english = project(UiLanguage::English, Some(japanese.clone()));
        assert!(!Arc::ptr_eq(&japanese, &english));
        assert!(Arc::ptr_eq(
            &english,
            &project(UiLanguage::English, Some(english.clone()))
        ));
        for (old, new) in japanese.turns.iter().zip(&english.turns) {
            assert!(!Arc::ptr_eq(old, new));
            for (old, new) in old.items().zip(new.items()) {
                if matches!(old.data.kind.as_str(), "user" | "agent" | "commentary") {
                    assert!(
                        Arc::ptr_eq(old, new),
                        "plain messages must retain native Markdown caches"
                    );
                }
                if new.data.kind == "command" {
                    let blocks = new.detail_blocks();
                    let super::super::markdown::MarkdownBlock::Paragraph { style, .. } = &blocks[0]
                    else {
                        panic!()
                    };
                    assert_eq!(style.filename.as_deref(), Some("Command"));
                }
            }
        }
        let summary = |conversation: &RenderedConversation| {
            conversation.turns[1]
                .rows
                .iter()
                .find_map(|row| match &row.content {
                    ConversationRowContent::ActivityHeader { activity } => {
                        Some(activity.activity_summary.clone())
                    }
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(summary(&japanese), "コマンドを実行しました");
        assert_eq!(summary(&english), "Ran a command");
        assert!(Arc::ptr_eq(&japanese.source_paths, &english.source_paths));
    }

    #[test]
    fn language_changes_refresh_pending_copy_without_translating_its_input() {
        let snapshot = Snapshot::default();
        let pending = [(
            "pending".into(),
            Arc::new(PendingSubmission {
                new_draft_slot: None,
                cursor: None,
                sequence: 0,
                submitted_at_ms: None,
                draft_key: snapshot.panes.primary().navigation.draft_key.clone(),
                draft: Arc::new(crate::state::Draft {
                    text: "設定\n{0}".into(),
                    attachments: vec![crate::state::Attachment {
                        name: "設定".into(),
                        path: "/設定/{0}".into(),
                        is_image: false,
                    }],
                    ..Default::default()
                }),
                turn_id: None,
                after_item_id: None,
                accepted: false,
                delivery_unknown: true,
                queued: false,
            }),
        )]
        .into();
        let source = Arc::new(models::Thread::default());
        let japanese = project_conversation(
            UiLanguage::Japanese,
            &snapshot.panes.primary().navigation.draft_key,
            &pending,
            source.clone(),
            &None,
        );
        let english = project_conversation(
            UiLanguage::English,
            &snapshot.panes.primary().navigation.draft_key,
            &pending,
            source,
            &Some(japanese.clone()),
        );
        assert_eq!(
            japanese.queued[0].data.title.as_deref(),
            Some("送信結果不明（自動再送しません）")
        );
        assert_eq!(
            english.queued[0].data.title.as_deref(),
            Some("Delivery unknown (will not resend automatically)")
        );
        assert_eq!(
            japanese.queued[0].data.body.as_deref(),
            Some("設定\n{0}\n添付: 設定 (/設定/{0})")
        );
        assert_eq!(
            english.queued[0].data.body.as_deref(),
            Some("設定\n{0}\nAttachment: 設定 (/設定/{0})")
        );
    }

    #[test]
    fn host_authored_error_copy_is_localized_as_a_whole() {
        let error = models::ExecutionError {
            message:
                "エージェントとの接続が終了しました。ホストを再起動してから再送信してください。"
                    .into(),
            ..Default::default()
        };
        assert_eq!(
            turn_error(UiLanguage::Japanese, &error).message,
            error.message
        );
        assert_eq!(
            turn_error(UiLanguage::English, &error).message,
            "The connection to the agent ended. Restart the Host, then send again."
        );
        let external = models::ExecutionError {
            message: "Provider said 設定\n{0}".into(),
            ..Default::default()
        };
        assert_eq!(
            turn_error(UiLanguage::English, &external).message,
            external.message
        );
    }

    #[test]
    fn progress_formats_elapsed_time_in_the_ui_language() {
        let progress = WorkProgress {
            started_at: Some(100.),
            compacting: false,
        };
        assert_eq!(progress.label(UiLanguage::Japanese, 108.), "8秒 作業中…");
        assert_eq!(progress.label(UiLanguage::English, 108.), "Working for 8s");
        let unknown_start = WorkProgress {
            started_at: None,
            compacting: false,
        };
        assert_eq!(unknown_start.label(UiLanguage::English, 108.), "Working…");
    }

    fn fixture() -> Snapshot {
        let thread = serde_json::from_value(json!({"id":{"provider":"codex","id":"thread"},"turns":[{"id":"done","status":"completed","items":[{"id":"answer","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"earlier","phase":"unknown"}}}}}]},{"id":"live","status":"running","items":[{"id":"user","status":"unknown","clientInputId":"accepted","body":{"inline":{"body":{"userMessage":{"text":"question","content":[]}}}}},{"id":"command","status":"completed","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"pwd","cwd":null,"output":"/fixture","exitCode":null}}}}},{"id":"stream","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"hello","phase":"unknown"}}}}}]}]})).unwrap();
        Snapshot {
            sessions: crate::state::Sessions {
                conversations: Arc::new(
                    [(
                        agent_protocol::session::SessionRef {
                            provider: Some(agent_protocol::session::ProviderKind::Codex),
                            id: "thread".into(),
                        },
                        Arc::new(thread),
                    )]
                    .into(),
                ),
                ..Default::default()
            },
            ..Default::default()
        }
    }
    #[test]
    fn older_history_reveals_a_provider_boundary_without_stale_cached_rows() {
        let mut snapshot = fixture();
        let session = snapshot
            .sessions
            .conversations
            .keys()
            .next()
            .unwrap()
            .clone();
        let thread = Arc::make_mut(
            Arc::make_mut(&mut snapshot.sessions.conversations)
                .get_mut(&session)
                .unwrap(),
        );
        let turns = thread.turns.as_mut().unwrap();
        Arc::make_mut(&mut turns[0]).provider = Some(crate::session::ProviderKind::Codex);
        Arc::make_mut(&mut turns[1]).provider = Some(crate::session::ProviderKind::Claude);
        let old = turns.remove(0);
        let initial = project_snapshot(snapshot.clone(), None);
        assert!(
            !initial.turns[0]
                .rows
                .iter()
                .any(|row| matches!(row.content, ConversationRowContent::ProviderHandoff { .. }))
        );
        let thread = Arc::make_mut(
            Arc::make_mut(&mut snapshot.sessions.conversations)
                .get_mut(&session)
                .unwrap(),
        );
        thread.turns.as_mut().unwrap().insert(0, old);
        let loaded = project_snapshot(snapshot.clone(), Some(&initial));
        assert!(
            matches!(&loaded.turns[1].rows[0].content, ConversationRowContent::ProviderHandoff { text } if text == "Codex → Claude に切り替えました")
        );
        assert!(!Arc::ptr_eq(&initial.turns[0], &loaded.turns[1]));
        let thread = Arc::make_mut(
            Arc::make_mut(&mut snapshot.sessions.conversations)
                .get_mut(&session)
                .unwrap(),
        );
        Arc::make_mut(&mut thread.turns.as_mut().unwrap()[0]).duration_ms = Some(500);
        let updated = project_snapshot(snapshot, Some(&loaded));
        assert!(
            Arc::ptr_eq(&loaded.turns[1], &updated.turns[1]),
            "unchanged provider boundaries retain their native render identity"
        );
    }

    #[test]
    fn completed_edits_stay_in_collapsed_activity_when_details_load() {
        let mut snapshot = fixture();
        let session = agent_protocol::session::SessionRef {
            provider: Some(agent_protocol::session::ProviderKind::Codex),
            id: "thread".into(),
        };
        let thread = Arc::make_mut(
            Arc::make_mut(&mut snapshot.sessions.conversations)
                .get_mut(&session)
                .unwrap(),
        );
        let turn = Arc::make_mut(&mut thread.turns.as_mut().unwrap()[1]);
        turn.started_at_ms = Some(1_000);
        turn.duration_ms = Some(5_000);
        turn.status = models::TurnStatus::Completed;
        let edit: models::Item = serde_json::from_value(json!({"id":"edit","status":"completed","clientInputId":null,"body":{"inline":{"body":{"fileChange":{"changes":[{"path":"src/a.rs","kind":"add","diff":"@@ -0,0 +1 @@\n+new","proposal":null}],"output":""}}}}})).unwrap();
        let mut deferred = edit.clone();
        deferred.defer();
        let items = turn.items.as_mut().unwrap();
        let index = items.len() - 1;
        items.insert(index, Arc::new(deferred));
        let projection = project_snapshot(snapshot.clone(), None);
        let thread = Arc::make_mut(
            Arc::make_mut(&mut snapshot.sessions.conversations)
                .get_mut(&session)
                .unwrap(),
        );
        let turn = Arc::make_mut(&mut thread.turns.as_mut().unwrap()[1]);
        turn.items.as_mut().unwrap()[index] = Arc::new(edit);
        let loaded = project_snapshot(snapshot, Some(&projection));
        for (rendered, deferred) in [(&projection, true), (&loaded, false)] {
            let rows = &rendered.turns[1].rows;
            assert_eq!(rows[0].timestamp_ms, Some(1_000));
            let response = rows.last().unwrap();
            assert_eq!(response.timestamp_ms, Some(6_000));
            assert!(matches!(
                &response.content,
                ConversationRowContent::Response { item, .. }
                    if item.data.native_id.as_deref() == Some("stream")
            ));
            let activity = rows
                .iter()
                .find_map(|row| match &row.content {
                    ConversationRowContent::ActivityHeader { activity } => Some(activity),
                    _ => None,
                })
                .unwrap();
            assert!(activity.activity_can_collapse);
            assert!(!activity_is_expanded(&activity.status, None));
            let edit = rows
                .iter()
                .find_map(|row| match &row.content {
                    ConversationRowContent::Activity { item, .. }
                        if item.data.native_id.as_deref() == Some("edit") =>
                    {
                        Some(item)
                    }
                    _ => None,
                })
                .unwrap();
            assert_eq!(edit.data.deferred, deferred);
            assert!(edit.data.collapsible);
            if !deferred {
                assert_eq!(edit.expanded_body(), "add: src/a.rs\n@@ -0,0 +1 @@\n+new");
            }
        }
    }
    fn project_snapshot(
        snapshot: Snapshot,
        previous: Option<&Arc<RenderedConversation>>,
    ) -> Arc<RenderedConversation> {
        project_conversation(
            UiLanguage::Japanese,
            &snapshot.panes.primary().navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            snapshot.sessions.conversations[&agent_protocol::session::SessionRef {
                provider: Some(agent_protocol::session::ProviderKind::Codex),
                id: "thread".into(),
            }]
                .clone(),
            &previous.cloned(),
        )
    }

    #[test]
    fn a_turn_queued_input_started_labels_its_message_and_not_its_steers() {
        let user = |id: &str| {
            json!({"id":id,"status":"completed","clientInputId":id,
            "body":{"inline":{"body":{"userMessage":{"text":null,"content":[]}}}}})
        };
        let answer = |id: &str| {
            json!({"id":id,"status":"completed","clientInputId":null,
            "body":{"inline":{"body":{"assistantText":{"text":id,"phase":"final"}}}}})
        };
        for queued in [false, true] {
            let mut snapshot = Snapshot::default();
            Arc::make_mut(&mut snapshot.sessions.conversations).insert(
                agent_protocol::session::SessionRef {
                    provider: Some(agent_protocol::session::ProviderKind::Codex),
                    id: "thread".into(),
                },
                Arc::new(
                    serde_json::from_value(json!({"id":{"provider":"codex","id":"thread"},
                        "turns":[{"id":"turn","status":"completed","queued":queued,
                            "items":[user("waited"),answer("first"),user("steered"),answer("second")]}]}))
                    .unwrap(),
                ),
            );
            let rendered = project_snapshot(snapshot, None);
            let marks: Vec<_> = rendered.turns[0]
                .rows
                .iter()
                .filter(|row| matches!(row.content, ConversationRowContent::User { .. }))
                .map(|row| (row.queued, row.steer))
                .collect();
            assert_eq!(marks, [(queued, false), (false, true)]);
        }
    }

    #[test]
    fn fresh_context_marks_only_the_initiating_user_message_on_running_and_settled_turns() {
        let user = |id: &str| {
            json!({"id": id, "status": "completed", "clientInputId": id,
            "body": {"inline": {"body": {"userMessage": {"text": id, "content": []}}}}})
        };
        let answer = json!({"id": "answer", "status": "completed", "body": {"inline": {"body": {
            "assistantText": {"text": "answer", "phase": "final"}}}}});
        for status in ["running", "completed"] {
            for fresh in [false, true] {
                let thread =
                    serde_json::from_value(json!({"id": {"provider": "codex", "id": "thread"},
                    "turns": [{"id": "turn", "status": status, "freshContext": fresh,
                        "items": [user("opened"), answer.clone(), user("steered")]}]}))
                    .unwrap();
                let rendered = project_conversation(
                    UiLanguage::Japanese,
                    &Default::default(),
                    &Default::default(),
                    Arc::new(thread),
                    &None,
                );
                let marks: Vec<_> = rendered.turns[0]
                    .rows
                    .iter()
                    .filter_map(|row| {
                        matches!(row.content, ConversationRowContent::User { .. })
                            .then_some(row.fresh_context)
                    })
                    .collect();
                assert_eq!(marks, [fresh, false]);
            }
        }
    }

    #[test]
    fn only_the_message_opening_a_settled_forkable_turn_can_be_edited_from() {
        let user = |id: &str| {
            json!({"id":id,"status":"completed","clientInputId":id,
            "body":{"inline":{"body":{"userMessage":{"text":null,"content":[]}}}}})
        };
        let answer = |id: &str| {
            json!({"id":id,"status":"completed","clientInputId":null,
            "body":{"inline":{"body":{"assistantText":{"text":id,"phase":"final"}}}}})
        };
        for (status, rollback, expected) in [
            ("completed", true, [Some("turn"), None]),
            ("failed", true, [Some("turn"), None]),
            ("running", true, [None, None]),
            ("completed", false, [None, None]),
        ] {
            let mut snapshot = Snapshot::default();
            Arc::make_mut(&mut snapshot.sessions.conversations).insert(
                agent_protocol::session::SessionRef {
                    provider: Some(agent_protocol::session::ProviderKind::Codex),
                    id: "thread".into(),
                },
                Arc::new(
                    serde_json::from_value(json!({"id":{"provider":"codex","id":"thread"},
                        "capabilities":{"fork":true,"rollback":rollback},
                        "turns":[{"id":"turn","status":status,
                            "items":[user("opened"),answer("first"),user("steered"),answer("second")]}]}))
                    .unwrap(),
                ),
            );
            let rendered = project_snapshot(snapshot, None);
            let edits: Vec<_> = rendered.turns[0]
                .rows
                .iter()
                .filter(|row| matches!(row.content, ConversationRowContent::User { .. }))
                .map(|row| row.edit_turn_id.as_ref().map(|id| id.as_str()))
                .collect();
            assert_eq!(edits, expected);
        }
    }

    #[test]
    fn approvals_offer_declining_and_approving_once_with_the_rest_in_a_menu_as_t3() {
        use agent_protocol::requests::{Choice, ChoiceEffect};
        let choice = |id: &str, label: &str, effect| Choice {
            id: id.into(),
            label: label.into(),
            description: String::new(),
            effect,
        };
        let action = |id: &str, label: &str| ApprovalAction {
            choice_id: id.into(),
            label: label.into(),
        };
        // Codex's order: approving, then the lasting grants, then declining.
        let choices = [
            choice("allow", "承認", ChoiceEffect::Allow),
            choice(
                "session",
                "このセッションで承認",
                ChoiceEffect::AllowLasting,
            ),
            choice("rule", "ネットワーク拒否ルールを追加", ChoiceEffect::Deny),
            choice("deny", "拒否", ChoiceEffect::Deny),
        ];
        assert_eq!(
            approval_actions(UiLanguage::English, &choices),
            ApprovalActions {
                // The first denial is the one declining.
                decline: Some(action("rule", "Add a network deny rule")),
                approve: Some(action("allow", "Approve")),
                more: vec![
                    action("session", "Always allow this session"),
                    action("deny", "Decline"),
                ],
            }
        );
        // A request without a one-time grant has no approve button.
        assert_eq!(
            approval_actions(UiLanguage::Japanese, &choices[1..2]),
            ApprovalActions {
                decline: None,
                approve: None,
                more: vec![action("session", "このセッションで承認")],
            }
        );
    }

    #[test]
    fn input_summary_uses_its_body_then_details_then_label() {
        let request = |body: &str, details: &str| Request {
            approval: None,
            id: "1".into(),
            title: "Command approval".into(),
            body: body.into(),
            can_respond: true,
            details: details.into(),
            request_body: agent_protocol::requests::RequestBody::Question {
                questions: Vec::new(),
            },
        };
        assert_eq!(request("why", "echo").summary(), "why");
        assert_eq!(request("", "echo").summary(), "echo");
        assert_eq!(request("", "").summary(), "Command approval");
    }

    #[test]
    fn composer_docks_one_other_request_before_one_question_in_request_order() {
        use agent_protocol::{
            requests::{Question, RequestBody, RequestTarget},
            session::{ProviderKind, RequestDelivery, SessionRef},
        };
        let question = || RequestBody::Question {
            questions: vec![Question {
                id: "q".into(),
                header: String::new(),
                prompt: "which?".into(),
                secret: false,
                allow_free_text: true,
                multiple: false,
                choices: vec![],
            }],
        };
        let tool = || RequestBody::ToolExecution {
            tool: "tool".into(),
            namespace: None,
            arguments: json!({}),
        };
        let session = SessionRef::new(ProviderKind::Codex, "thread".into()).unwrap();
        let mut snapshot = Snapshot::default();
        assert!(snapshot.docked_requests(session.clone()).is_empty());
        let thread = models::Thread {
            requests: [
                ("1-question", question()),
                ("2-tool", tool()),
                ("3-question", question()),
                ("4-tool", tool()),
            ]
            .into_iter()
            .map(|(id, body)| {
                let request = WireRequest {
                    id: id.into(),
                    target: RequestTarget::Session,
                    delivery: RequestDelivery::Awaiting,
                    body,
                };
                (request.id.clone(), Arc::new(request))
            })
            .collect(),
            ..Default::default()
        };
        Arc::make_mut(&mut snapshot.sessions.conversations)
            .insert(session.clone(), Arc::new(thread));
        let docked: Vec<_> = snapshot
            .docked_requests(session.clone())
            .into_iter()
            .map(|request| request.id.as_str().to_owned())
            .collect();
        assert_eq!(docked, ["2-tool", "1-question"]);
        assert_eq!(
            snapshot
                .question_prompt(session)
                .unwrap()
                .request_id
                .as_str(),
            "1-question"
        );
    }

    #[test]
    fn repeated_turns_keep_pending_input_on_the_latest_occurrence_and_route_requests_in_order() {
        use agent_protocol::{
            requests::{RequestBody, RequestTarget},
            session::{ProviderKind, RequestDelivery, SessionRef},
        };
        let session = SessionRef::new(ProviderKind::Codex, "thread".into()).unwrap();
        let mut source = models::Thread {
            id: Some(session.clone()),
            turns: Some(
                ["first", "last"]
                    .into_iter()
                    .map(|text| {
                        Arc::new(models::Turn {
                            id: "repeat".into(),
                            status: models::TurnStatus::Completed,
                            items: Some(vec![Arc::new(models::Item::new(
                                text.into(),
                                Default::default(),
                                models::ItemBody::AssistantText {
                                    text: text.into(),
                                    phase: models::AssistantPhase::Final,
                                },
                            ))]),
                            ..Default::default()
                        })
                    })
                    .collect(),
            ),
            ..Default::default()
        };
        let mut snapshot = Snapshot::default();
        for (sequence, (id, turn, accepted)) in [
            ("assigned", Some("repeat"), true),
            ("missing", Some("absent"), true),
            ("queued", None, true),
            ("unassigned", None, false),
        ]
        .into_iter()
        .enumerate()
        {
            Arc::make_mut(&mut snapshot.sessions.pending_submissions).insert(
                id.into(),
                Arc::new(PendingSubmission {
                    new_draft_slot: None,
                    cursor: None,
                    sequence: sequence as u64,
                    submitted_at_ms: None,
                    draft_key: session.clone().into(),
                    draft: Arc::new(Draft {
                        text: id.into(),
                        ..Default::default()
                    }),
                    turn_id: turn.map(Into::into),
                    after_item_id: None,
                    accepted,
                    delivery_unknown: false,
                    queued: false,
                }),
            );
        }
        for (id, turn) in [
            ("a-session", None),
            ("b-turn", Some("repeat")),
            ("c-missing", Some("absent")),
            ("d-turn", Some("repeat")),
        ] {
            let request = WireRequest {
                id: id.into(),
                target: turn.map_or(RequestTarget::Session, |turn| RequestTarget::Turn {
                    turn_id: turn.into(),
                    item_id: None,
                }),
                delivery: RequestDelivery::Awaiting,
                body: RequestBody::ToolExecution {
                    tool: id.into(),
                    namespace: None,
                    arguments: Value::Null,
                },
            };
            source
                .requests
                .insert(request.id.clone(), Arc::new(request));
        }
        let projected = project_conversation(
            UiLanguage::Japanese,
            &snapshot.panes.primary().navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            Arc::new(source),
            &None,
        );
        assert_eq!(
            projected.turns[0]
                .items()
                .filter_map(|item| item.data.body.as_deref())
                .collect::<Vec<_>>(),
            ["unassigned", "first"]
        );
        assert_eq!(
            projected.turns[1]
                .items()
                .filter_map(|item| item.data.body.as_deref())
                .collect::<Vec<_>>(),
            ["assigned", "last"]
        );
        assert_eq!(
            projected
                .queued
                .iter()
                .map(|item| item.data.id.as_str())
                .collect::<Vec<_>>(),
            ["missing", "queued"]
        );
        let requests = |rows: &[ConversationRow]| {
            rows.iter()
                .filter_map(|row| match &row.content {
                    ConversationRowContent::PendingRequest { request } => {
                        Some(request.id.to_string())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            requests(&projected.session_rows),
            ["a-session", "c-missing"]
        );
        for turn in &projected.turns {
            assert_eq!(requests(&turn.rows), ["b-turn", "d-turn"]);
        }
    }

    #[test]
    fn session_requests_render_without_history_and_sent_requests_disable_answers() {
        use agent_protocol::{
            requests::{ElicitationInput, RequestBody, RequestTarget},
            session::RequestDelivery,
        };
        let make = |delivery| {
            let request = WireRequest {
                id: "request".into(),
                target: RequestTarget::Session,
                delivery,
                body: RequestBody::Elicitation {
                    server: "mcp".into(),
                    message: "confirm".into(),
                    input: ElicitationInput::Url {
                        url: "https://example.com/".into(),
                    },
                },
            };
            Arc::new(models::Thread {
                requests: [(request.id.clone(), Arc::new(request))].into(),
                ..Default::default()
            })
        };
        let awaiting = project_conversation(
            UiLanguage::Japanese,
            &Snapshot::default().panes.primary().navigation.draft_key,
            &Snapshot::default().sessions.pending_submissions,
            make(RequestDelivery::Awaiting),
            &None,
        );
        assert!(awaiting.turns.is_empty());
        assert_eq!(awaiting.session_rows.len(), 1);
        let ConversationRowContent::PendingRequest { request } = &awaiting.session_rows[0].content
        else {
            panic!("request row")
        };
        assert!(request.can_respond);
        let sent = project_conversation(
            UiLanguage::Japanese,
            &Snapshot::default().panes.primary().navigation.draft_key,
            &Snapshot::default().sessions.pending_submissions,
            make(RequestDelivery::Sent),
            &Some(awaiting.clone()),
        );
        let ConversationRowContent::PendingRequest { request } = &sent.session_rows[0].content
        else {
            panic!("request row")
        };
        assert!(!request.can_respond);
        assert_ne!(
            &request.title,
            match &awaiting.session_rows[0].content {
                ConversationRowContent::PendingRequest { request } => &request.title,
                _ => unreachable!(),
            }
        );
    }

    #[test]
    fn generated_image_placeholder_yields_to_result_and_stops_on_failure() {
        for (status, saved_path, result, placeholder, sources, error) in [
            ("running", "", "", true, vec![], None),
            (
                "running",
                "/preview.png",
                "",
                false,
                vec!["/preview.png"],
                None,
            ),
            (
                "completed",
                "/generated.png",
                "",
                false,
                vec!["/generated.png"],
                None,
            ),
            (
                "completed",
                "",
                "png-data",
                false,
                vec!["data:image/png;base64,png-data"],
                None,
            ),
            ("completed", "", "", false, vec![], None),
            (
                "failed",
                "",
                "",
                false,
                vec![],
                Some("画像を生成できませんでした"),
            ),
        ] {
            let item = Arc::new(
                serde_json::from_value(json!({"id":"image","status":status,"clientInputId":null,"body":{"inline":{"body":{"imageGeneration":{"savedPath":saved_path,"data":result,"revisedPrompt":null}}}}}))
                .unwrap(),
            );
            let projected = render_turn(
                Some(&crate::session::SessionRef::new(crate::session::ProviderKind::Codex, "session".into()).unwrap()),
                (false, false),
                None,
                (Arc::new(models::Turn {
                    id: "turn".into(),
                    status: models::TurnStatus::Completed,
                    items: Some(vec![item, Arc::new(serde_json::from_value(json!({"id":"answer","status":"completed","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"Here is the image","phase":"final"}}}}})).unwrap())]),
                    ..Default::default()
                }), 0, UiLanguage::Japanese),
                vec![],
                vec![],
                None,
            );
            let items: Vec<_> = projected.items().collect();
            assert_eq!(items.len(), 2);
            assert_eq!(items[0].data.image_placeholder, placeholder);
            assert_eq!(items[0].data.image_sources, sources);
            assert_eq!(items[0].data.title, None);
            assert_eq!(items[0].data.body.as_deref(), error);
            assert_eq!(items[1].data.body.as_deref(), Some("Here is the image"));
        }
    }

    #[test]
    fn flat_rows_preserve_history_order_and_only_offer_fork_on_last_completed_response() {
        let mut snapshot = fixture();
        let thread = Arc::make_mut(
            Arc::make_mut(&mut snapshot.sessions.conversations)
                .get_mut(&agent_protocol::session::SessionRef {
                    provider: Some(agent_protocol::session::ProviderKind::Codex),
                    id: "thread".into(),
                })
                .unwrap(),
        );
        thread.capabilities = Some(crate::session::Capabilities {
            fork: true,
            ..Default::default()
        });
        let rendered = project_snapshot(snapshot.clone(), None);
        let rows = &rendered.turns[1].rows;
        let ids: Vec<_> = rows.iter().map(|row| row.id.as_str()).collect();
        assert!(
            matches!(&rows[0].content, ConversationRowContent::User { item } if item.data.native_id.as_deref() == Some("user"))
        );
        let accepted_id = ids[0].to_owned();
        assert_eq!(ids.iter().copied().collect::<HashSet<_>>().len(), ids.len());
        assert!(rows.iter().any(|row| matches!(&row.content, ConversationRowContent::Activity { item, .. } if item.data.native_id.as_deref() == Some("command"))));
        assert!(rows.iter().all(|row| !matches!(
            &row.content,
            ConversationRowContent::Response {
                fork_turn_id: Some(_),
                ..
            }
        )));
        assert!(matches!(
            rows.last().unwrap().content,
            ConversationRowContent::InProgress { .. }
        ));
        let thread = Arc::make_mut(
            Arc::make_mut(&mut snapshot.sessions.conversations)
                .get_mut(&agent_protocol::session::SessionRef {
                    provider: Some(agent_protocol::session::ProviderKind::Codex),
                    id: "thread".into(),
                })
                .unwrap(),
        );
        Arc::make_mut(&mut thread.turns.as_mut().unwrap()[1]).status =
            models::TurnStatus::Completed;
        let completed = project_snapshot(snapshot, Some(&rendered));
        let rows = &completed.turns[1].rows;
        assert!(
            matches!(&rows.last().unwrap().content, ConversationRowContent::Response { item, fork_turn_id: Some(id), .. } if id.as_str() == "live" && item.data.native_id.as_deref() == Some("stream"))
        );
        assert!(rows.iter().any(|row| row.id == accepted_id));
        for row in rows {
            if let ConversationRowContent::ActivityHeader { activity } = &row.content {
                assert!(!activity_is_expanded(&activity.status, None));
                assert!(activity_is_expanded(
                    &activity.status,
                    Some(activity.status.clone())
                ));
                assert!(!activity_is_expanded(
                    &activity.status,
                    Some("running".into())
                ));
            }
        }
    }

    #[rstest::rstest]
    #[case::codex("codex", "commentary", "final")]
    #[case::claude("claude", "commentary", "final")]
    #[case::deepseek("deepseek", "unknown", "unknown")]
    fn response_actions_wait_for_turn_completion_and_only_belong_to_the_answer(
        #[case] provider: &str,
        #[case] progress_phase: &str,
        #[case] answer_phase: &str,
        #[values(false, true)] supports_fork: bool,
    ) {
        let snapshot = Snapshot::default();
        let mut thread: Arc<models::Thread> = Arc::new(serde_json::from_value(json!({
            "id": {"provider": provider, "id": "thread"},
            "capabilities": {"fork": supports_fork},
            "turns": [{"id": "turn", "status": "running", "items": [
                {"id": "progress", "status": "completed", "body": {"inline": {"body": {"assistantText": {"text": "Working", "phase": progress_phase}}}}},
                {"id": "tool", "status": "completed", "body": {"inline": {"body": {"commandExecution": {"command": "pwd", "output": "/work"}}}}},
                {"id": "update", "status": "completed", "body": {"inline": {"body": {"assistantText": {"text": "Waiting", "phase": progress_phase}}}}},
                {"id": "answer", "status": "completed", "body": {"inline": {"body": {"assistantText": {"text": "Done", "phase": answer_phase}}}}}
            ]}]
        })).unwrap());
        let live = project_conversation(
            UiLanguage::Japanese,
            &snapshot.panes.primary().navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            thread.clone(),
            &None,
        );
        let responses: Vec<_> = live.turns[0]
            .rows
            .iter()
            .filter_map(|row| match &row.content {
                ConversationRowContent::Response {
                    item,
                    show_actions,
                    fork_turn_id,
                } => {
                    assert!(
                        !show_actions,
                        "live messages must not expose answer actions"
                    );
                    assert!(fork_turn_id.is_none());
                    Some(item)
                }
                _ => None,
            })
            .collect();
        assert_eq!(responses.len(), 3);
        let live_answer = responses.last().unwrap();

        Arc::make_mut(&mut Arc::make_mut(&mut thread).turns.as_mut().unwrap()[0]).status =
            models::TurnStatus::Completed;
        let completed = project_conversation(
            UiLanguage::Japanese,
            &snapshot.panes.primary().navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            thread,
            &Some(live.clone()),
        );
        let actions: Vec<_> = completed.turns[0]
            .rows
            .iter()
            .filter_map(|row| match &row.content {
                ConversationRowContent::Response {
                    item,
                    show_actions: true,
                    fork_turn_id,
                } => {
                    assert_eq!(fork_turn_id.is_some(), supports_fork);
                    Some(item)
                }
                _ => None,
            })
            .collect();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].data.native_id.as_deref(), Some("answer"));
        assert!(Arc::ptr_eq(live_answer, actions[0]));
        let reopened = project_conversation(
            UiLanguage::Japanese,
            &snapshot.panes.primary().navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            completed.source.clone(),
            &None,
        );
        assert_eq!(
            reopened.turns[0]
                .rows
                .iter()
                .filter(|row| matches!(
                    row.content,
                    ConversationRowContent::Response {
                        show_actions: true,
                        ..
                    }
                ))
                .count(),
            1
        );
    }

    #[test]
    fn cached_assistant_labels_follow_turn_provider_attribution() {
        let snapshot = Snapshot::default();
        let mut thread: Arc<models::Thread> = Arc::new(
            serde_json::from_value(json!({
                "id": {"provider": "codex", "id": "thread"},
                "turns": [{"id": "turn", "status": "completed", "items": [
                    {"id": "answer", "status": "completed", "body": {"inline": {
                        "body": {"assistantText": {"text": "Done", "phase": "final"}}
                    }}}
                ]}]
            }))
            .unwrap(),
        );
        let initial = project_conversation(
            UiLanguage::Japanese,
            &snapshot.panes.primary().navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            thread.clone(),
            &None,
        );
        let before = initial.turns[0].items().next().unwrap();
        assert_eq!(before.data.title.as_deref(), Some("Codex"));
        Arc::make_mut(&mut Arc::make_mut(&mut thread).turns.as_mut().unwrap()[0]).provider =
            Some(crate::session::ProviderKind::Codex);
        let attributed = project_conversation(
            UiLanguage::Japanese,
            &snapshot.panes.primary().navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            thread.clone(),
            &Some(initial.clone()),
        );
        assert!(Arc::ptr_eq(
            before,
            attributed.turns[0].items().next().unwrap()
        ));
        Arc::make_mut(&mut Arc::make_mut(&mut thread).turns.as_mut().unwrap()[0]).provider =
            Some(crate::session::ProviderKind::Claude);
        let updated = project_conversation(
            UiLanguage::Japanese,
            &snapshot.panes.primary().navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            thread,
            &Some(attributed),
        );
        let after = updated.turns[0].items().next().unwrap();
        assert_eq!(after.data.title.as_deref(), Some("Claude"));
        assert_eq!(after.data.body.as_deref(), Some("Done"));
        assert!(matches!((&before.source, &after.source),
            (ItemSource::Native(before), ItemSource::Native(after)) if Arc::ptr_eq(before, after)));
        let unchanged = project_conversation(
            UiLanguage::Japanese,
            &snapshot.panes.primary().navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            updated.source.clone(),
            &Some(updated.clone()),
        );
        assert!(Arc::ptr_eq(&updated.turns[0], &unchanged.turns[0]));
    }

    #[rstest::rstest]
    fn cached_response_actions_follow_capability_changes(
        #[values(None, Some(false), Some(true))] turn_supports_fork: Option<bool>,
    ) {
        let snapshot = Snapshot::default();
        let mut thread: Arc<models::Thread> = Arc::new(
            serde_json::from_value(json!({
                "id": {"provider": "codex", "id": "thread"},
                "turns": [{"id": "turn", "status": "completed", "supportsFork": turn_supports_fork,
                    "items": [{"id": "answer", "status": "completed", "body": {"inline": {
                        "body": {"assistantText": {"text": "Done", "phase": "final"}}
                    }}}]
                }]
            }))
            .unwrap(),
        );
        let mut previous = None;
        for enabled in [false, true, false] {
            Arc::make_mut(&mut thread).capabilities = Some(crate::session::Capabilities {
                fork: enabled,
                ..Default::default()
            });
            let projected = project_conversation(
                UiLanguage::Japanese,
                &snapshot.panes.primary().navigation.draft_key,
                &snapshot.sessions.pending_submissions,
                thread.clone(),
                &previous,
            );
            let actions: Vec<_> = projected.turns[0]
                .rows
                .iter()
                .filter_map(|row| match &row.content {
                    ConversationRowContent::Response { fork_turn_id, .. } => Some(fork_turn_id),
                    _ => None,
                })
                .collect();
            assert_eq!(actions.len(), 1);
            assert_eq!(actions[0].is_some(), turn_supports_fork.unwrap_or(enabled));
            if let Some(previous) = &previous {
                assert!(Arc::ptr_eq(
                    &previous.turns[0].source,
                    &projected.turns[0].source,
                ));
                assert_eq!(
                    Arc::ptr_eq(&previous.turns[0], &projected.turns[0]),
                    turn_supports_fork.is_some(),
                    "only turns that inherit the changed capability need new rows",
                );
                assert!(Arc::ptr_eq(
                    previous.turns[0].items().next().unwrap(),
                    projected.turns[0].items().next().unwrap(),
                ));
            }
            previous = Some(projected);
        }
    }

    #[test]
    fn flat_row_ids_distinguish_repeated_items_and_turns() {
        let source = Arc::new(
            serde_json::from_value(json!({"id":{"provider":"codex","id":"thread"},"turns":[{"id":"first","status":"completed","items":[{"id":"same","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"old","phase":"unknown"}}}}},{"id":"same","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"new","phase":"final"}}}}}]},{"id":"second","status":"completed","items":[{"id":"same","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"another turn","phase":"unknown"}}}}}]}]}))
            .unwrap(),
        );
        let rendered = project_conversation(
            UiLanguage::Japanese,
            &Snapshot::default().panes.primary().navigation.draft_key,
            &Snapshot::default().sessions.pending_submissions,
            source,
            &None,
        );
        let rows: Vec<_> = rendered.turns.iter().flat_map(|turn| &turn.rows).collect();
        assert_eq!(
            rows.iter().map(|row| &row.id).collect::<HashSet<_>>().len(),
            rows.len()
        );
        let texts: Vec<_> = rows
            .iter()
            .filter_map(|row| match &row.content {
                ConversationRowContent::Activity { item, .. }
                | ConversationRowContent::Response { item, .. } => item.data.body.as_deref(),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["old", "new", "another turn"]);
        let again: Vec<_> = rendered
            .turns
            .iter()
            .flat_map(|turn| &turn.rows)
            .map(|row| row.id.clone())
            .collect();
        assert_eq!(
            again,
            rows.iter().map(|row| row.id.clone()).collect::<Vec<_>>()
        );
        let mut changed = rendered.source.clone();
        Arc::make_mut(&mut Arc::make_mut(&mut changed).turns.as_mut().unwrap()[0]).status =
            models::TurnStatus::Interrupted;
        let updated = project_conversation(
            UiLanguage::Japanese,
            &Snapshot::default().panes.primary().navigation.draft_key,
            &Snapshot::default().sessions.pending_submissions,
            changed,
            &Some(rendered.clone()),
        );
        assert_eq!(updated.turns[0].items().count(), 2);
        for (before, after) in rendered.turns[0].items().zip(updated.turns[0].items()) {
            assert!(
                Arc::ptr_eq(before, after),
                "unchanged items must retain their cache even when IDs repeat"
            );
        }
    }

    #[test]
    fn deferred_work_keeps_input_and_answer_visible_with_item_details_on_demand() {
        let source = Arc::new(serde_json::from_value(json!({
            "id":{"provider":"codex","id":"thread"},"turns":[{
                "id":"turn","status":"completed","items":[
                    {"id":"user","body":{"inline":{"body":{"userMessage":{"text":"question","content":[]}}}}},
                    {"id":"work","status":"completed","body":{"deferred":{"summary":{"commandExecution":{"command":"inspect","cwd":null,"output":"","exitCode":0}}}}},
                    {"id":"answer","body":{"inline":{"body":{"assistantText":{"text":"answer","phase":"final"}}}}}
                ]
            }]
        })).unwrap());
        let rendered = project_conversation(
            UiLanguage::Japanese,
            &Snapshot::default().panes.primary().navigation.draft_key,
            &Snapshot::default().sessions.pending_submissions,
            source,
            &None,
        );
        let rows = &rendered.turns[0].rows;
        assert!(matches!(
            &rows[0].content,
            ConversationRowContent::User { .. }
        ));
        let ConversationRowContent::ActivityHeader { activity } = &rows[1].content else {
            panic!("summary activity is inaccessible")
        };
        assert!(!activity_is_expanded(&activity.status, None));
        assert!(activity.activity_can_collapse);
        assert!(
            matches!(&rows[2].content, ConversationRowContent::Activity { item, .. } if item.data.deferred)
        );
        assert!(matches!(
            &rows[3].content,
            ConversationRowContent::Response { .. }
        ));
    }

    #[test]
    fn unknown_submissions_keep_send_order_and_position_after_reopening() {
        use crate::state::{Event, Intent, reduce};
        for status in ["completed", "running"] {
            let mut snapshot = Snapshot {
                sessions: crate::state::Sessions {
                    conversations: Arc::new(
                        [(
                            agent_protocol::session::SessionRef { provider: Some(agent_protocol::session::ProviderKind::Codex), id: "thread".into() },
                            Arc::new(
                                serde_json::from_value(json!({"id":{"provider":"codex","id":"thread"},"capabilities":{"steer":true,"fork":false,"rename":false,"modelChange":false},"turns":[{"id":"before","status":status,"items":[{"id":"answer","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"before","phase":"unknown"}}}}}]}]}))
                                .unwrap(),
                            ),
                        )]
                        .into(),
                    ),
                    ..Default::default()
                },
                ..Default::default()
            };
            // IDs deliberately disagree with submission order.
            let mut activity_ids = HashSet::new();
            for id in ["z-first", "a-second", "m-third"] {
                Arc::make_mut(&mut snapshot.input.drafts).insert(
                    agent_protocol::session::SessionRef {
                        provider: Some(agent_protocol::session::ProviderKind::Codex),
                        id: "thread".into(),
                    }
                    .into(),
                    Arc::new(Draft {
                        text: id.into(),
                        ..Default::default()
                    }),
                );
                snapshot = reduce(
                    &snapshot,
                    "",
                    Event::from(Intent::Submit {
                        thread_id: Some(agent_protocol::session::SessionRef {
                            provider: Some(agent_protocol::session::ProviderKind::Codex),
                            id: "thread".into(),
                        }),
                        client_user_message_id: id.into(),
                        // Input joins a running turn as a steer.
                        alternate: status == "running",
                    }),
                )
                .0;
                snapshot = reduce(&snapshot, "", Event::SubmissionUnknown(id.into())).0;
                if status == "running" {
                    let rendered = project_snapshot(snapshot.clone(), None);
                    let activity = rendered.turns[0]
                        .rows
                        .iter()
                        .find_map(|row| match &row.content {
                            ConversationRowContent::ActivityHeader { activity } => Some(activity),
                            _ => None,
                        })
                        .unwrap();
                    assert!(
                        activity_ids.insert(activity.id.clone()),
                        "a new input must not inherit another group's expansion"
                    );
                }
            }
            let first = project_snapshot(snapshot.clone(), None);
            let texts = |rendered: &RenderedConversation| {
                rendered
                    .turns
                    .iter()
                    .flat_map(|turn| turn.items())
                    .chain(rendered.queued.iter())
                    .filter_map(|item| item.data.body.clone())
                    .collect::<Vec<_>>()
            };
            assert_eq!(texts(&first), ["before", "z-first", "a-second", "m-third"]);
            assert!(Arc::ptr_eq(
                &first,
                &project_snapshot(snapshot.clone(), Some(&first))
            ));
            let thread = Arc::make_mut(
                Arc::make_mut(&mut snapshot.sessions.conversations)
                    .get_mut(&agent_protocol::session::SessionRef {
                        provider: Some(agent_protocol::session::ProviderKind::Codex),
                        id: "thread".into(),
                    })
                    .unwrap(),
            );
            thread.turns.as_mut().unwrap().push(Arc::new(
                serde_json::from_value(json!({"id":"later","status":"completed","items":[{"id":"later-user","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"userMessage":{"text":"later","content":[]}}}}}]}))
                .unwrap(),
            ));
            let reopened: Snapshot =
                serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
            let rendered = project_snapshot(reopened, Some(&first));
            assert_eq!(
                texts(&rendered),
                ["before", "z-first", "a-second", "m-third", "later"]
            );
            assert!(rendered.queued.is_empty());
        }
    }

    #[test]
    fn queued_submissions_keep_send_order_without_history() {
        use crate::state::{Event, Intent, reduce};
        let mut snapshot = Snapshot {
            sessions: crate::state::Sessions {
                conversations: Arc::new(
                    [(
                        agent_protocol::session::SessionRef {
                            provider: Some(agent_protocol::session::ProviderKind::Codex),
                            id: "thread".into(),
                        },
                        Arc::new(models::Thread {
                            id: Some(agent_protocol::session::SessionRef {
                                provider: Some(agent_protocol::session::ProviderKind::Codex),
                                id: "thread".into(),
                            }),
                            ..Default::default()
                        }),
                    )]
                    .into(),
                ),
                ..Default::default()
            },
            ..Default::default()
        };
        for id in ["z-first", "a-second", "m-third"] {
            snapshot = reduce(
                &snapshot,
                "",
                Event::from(Intent::Submit {
                    thread_id: Some(agent_protocol::session::SessionRef {
                        provider: Some(agent_protocol::session::ProviderKind::Codex),
                        id: "thread".into(),
                    }),
                    client_user_message_id: id.into(),
                    alternate: false,
                }),
            )
            .0;
            snapshot = reduce(&snapshot, "", Event::SubmissionUnknown(id.into())).0;
        }
        let rendered = project_snapshot(snapshot.clone(), None);
        assert_eq!(
            rendered
                .queued
                .iter()
                .map(|item| item.data.id.as_str())
                .collect::<Vec<_>>(),
            ["z-first", "a-second", "m-third"]
        );
        Arc::make_mut(
            Arc::make_mut(&mut snapshot.sessions.conversations)
                .get_mut(&agent_protocol::session::SessionRef {
                    provider: Some(agent_protocol::session::ProviderKind::Codex),
                    id: "thread".into(),
                })
                .unwrap(),
        )
        .turns = Some(vec![Arc::new(
            serde_json::from_value(json!({"id":"later","items":[{"id":"answer","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"later","phase":"unknown"}}}}}],"status":"unknown"}))
            .unwrap(),
        )]);
        let updated = project_snapshot(snapshot, Some(&rendered));
        assert!(updated.queued.is_empty());
        assert_eq!(
            updated.turns[0]
                .items()
                .map(|item| item.data.id.as_str())
                .collect::<Vec<_>>(),
            ["z-first", "a-second", "m-third", "answer"]
        );
    }

    #[test]
    fn pending_echoes_render_once_and_keep_remaining_input_order() {
        let mut snapshot = fixture();
        for (sequence, id) in ["a", "b", "a", "c"].into_iter().enumerate() {
            Arc::make_mut(&mut snapshot.sessions.pending_submissions).insert(
                id.into(),
                Arc::new(PendingSubmission {
                    new_draft_slot: None,
                    cursor: None,
                    sequence: sequence as u64,
                    submitted_at_ms: None,
                    draft_key: agent_protocol::session::SessionRef {
                        provider: Some(agent_protocol::session::ProviderKind::Codex),
                        id: "thread".into(),
                    }
                    .into(),
                    draft: Arc::new(Draft {
                        text: format!("pending {id} {sequence}"),
                        ..Default::default()
                    }),
                    turn_id: Some("echo-turn".into()),
                    after_item_id: Some("echo".into()),
                    accepted: false,
                    delivery_unknown: true,
                    queued: false,
                }),
            );
        }
        Arc::make_mut(Arc::make_mut(&mut snapshot.sessions.conversations).get_mut(&agent_protocol::session::SessionRef { provider: Some(agent_protocol::session::ProviderKind::Codex), id: "thread".into() }).unwrap())
            .turns.as_mut().unwrap().push(Arc::new(serde_json::from_value(json!({"id":"echo-turn","items":[{"id":"echo","status":"unknown","clientInputId":"b","body":{"inline":{"body":{"userMessage":{"text":"native b","content":[]}}}}}],"status":"unknown"})).unwrap()));
        let rendered = project_snapshot(snapshot.clone(), None);
        let items: Vec<_> = rendered.turns.last().unwrap().items().collect();
        assert_eq!(
            items
                .iter()
                .filter_map(|item| item.data.body.as_deref())
                .collect::<Vec<_>>(),
            ["native b", "pending a 2", "pending c 3"]
        );
        assert!(matches!(&items[0].source, ItemSource::Native(_)));
        assert!(rendered.queued.is_empty());
        assert_eq!(snapshot.sessions.pending_submissions.len(), 3);
        assert!(Arc::ptr_eq(
            &rendered,
            &project_snapshot(snapshot, Some(&rendered))
        ));
    }

    #[rstest::rstest]
    #[case::sending(false, false, None)]
    #[case::accepted(true, false, None)]
    #[case::unknown(false, true, Some("送信結果不明（自動再送しません）"))]
    #[case::accepted_then_unknown(true, true, Some("送信結果不明（自動再送しません）"))]
    fn pending_input_delivery_status_is_shared_in_history_and_queue(
        #[case] accepted: bool,
        #[case] delivery_unknown: bool,
        #[case] title: Option<&str>,
        #[values(false, true)] history_loaded: bool,
    ) {
        let mut snapshot = fixture();
        let session = snapshot
            .sessions
            .conversations
            .keys()
            .next()
            .unwrap()
            .clone();
        if !history_loaded {
            Arc::make_mut(
                Arc::make_mut(&mut snapshot.sessions.conversations)
                    .get_mut(&session)
                    .unwrap(),
            )
            .turns = None;
        }
        Arc::make_mut(&mut snapshot.sessions.pending_submissions).insert(
            "pending".into(),
            Arc::new(PendingSubmission {
                new_draft_slot: None,
                cursor: None,
                sequence: 0,
                submitted_at_ms: Some(12_000),
                draft_key: session.into(),
                draft: Arc::new(Draft {
                    text: "waiting for echo".into(),
                    ..Default::default()
                }),
                turn_id: Some("live".into()),
                after_item_id: None,
                accepted,
                delivery_unknown,
                queued: false,
            }),
        );
        let rendered = project_snapshot(snapshot, None);
        let item = rendered
            .queued
            .iter()
            .chain(rendered.turns.iter().flat_map(|turn| turn.items()))
            .find(|item| item.data.id == "pending")
            .unwrap();
        assert_eq!(item.data.title.as_deref(), title);
        assert_eq!(item.data.body.as_deref(), Some("waiting for echo"));
        assert_eq!(item.data.timestamp_ms, Some(12_000));
        assert!(item.data.native_id.is_none());
        assert_eq!(
            item.unknown_submission_id().as_deref(),
            delivery_unknown.then_some("pending")
        );
        assert!(
            rendered
                .turns
                .iter()
                .flat_map(|turn| turn.items())
                .all(|item| {
                    item.data.native_id.is_none() || item.unknown_submission_id().is_none()
                })
        );
        if history_loaded {
            let row = rendered.turns.iter().flat_map(|turn| &turn.rows)
                .find(|row| matches!(&row.content, ConversationRowContent::User { item } if item.data.id == "pending"))
                .unwrap();
            assert_eq!(row.timestamp_ms, Some(12_000));
        }
    }

    #[test]
    fn local_input_keeps_its_footer_while_acceptance_and_history_are_pending() {
        let mut snapshot = fixture();
        Arc::make_mut(&mut snapshot.sessions.pending_submissions).insert(
            "pending".into(),
            Arc::new(PendingSubmission {
                new_draft_slot: None,
                cursor: None,
                sequence: 0,
                submitted_at_ms: Some(12_000),
                draft_key: agent_protocol::session::SessionRef {
                    provider: Some(agent_protocol::session::ProviderKind::Codex),
                    id: "thread".into(),
                }
                .into(),
                draft: Arc::new(Draft {
                    text: "waiting for history".into(),
                    ..Default::default()
                }),
                turn_id: Some("unloaded".into()),
                after_item_id: None,
                accepted: false,
                delivery_unknown: false,
                queued: false,
            }),
        );
        let sending = project_snapshot(snapshot.clone(), None);
        assert!(sending.queued[0].data.title.is_none());
        assert_eq!(sending.queued[0].data.timestamp_ms, Some(12_000));
        Arc::make_mut(
            Arc::make_mut(&mut snapshot.sessions.pending_submissions)
                .get_mut("pending")
                .unwrap(),
        )
        .accepted = true;
        let first = project_snapshot(snapshot.clone(), Some(&sending));
        assert_eq!(first.queued.len(), 1);
        assert!(first.queued[0].data.title.is_none());
        assert_eq!(first.queued[0].data.timestamp_ms, Some(12_000));
        assert_eq!(
            first.queued[0].data.body.as_deref(),
            Some("waiting for history")
        );
        let thread = Arc::make_mut(
            Arc::make_mut(&mut snapshot.sessions.conversations)
                .get_mut(&agent_protocol::session::SessionRef {
                    provider: Some(agent_protocol::session::ProviderKind::Codex),
                    id: "thread".into(),
                })
                .unwrap(),
        );
        thread.turns.as_mut().unwrap().push(Arc::new(serde_json::from_value(json!({"id":"unloaded","status":"running","items":[{"id":"echo","status":"unknown","clientInputId":"pending","body":{"inline":{"body":{"userMessage":{"text":"waiting for history","content":[]}}}}}]})).unwrap()));
        let loaded = project_snapshot(snapshot, Some(&first));
        assert!(loaded.queued.is_empty());
        assert_eq!(loaded.turns.last().unwrap().items().count(), 1);
        let echoed = loaded.turns.last().unwrap().items().next().unwrap();
        assert_eq!(echoed.data.id, "pending");
        assert!(matches!(&echoed.source, ItemSource::Native(item) if item.id == "echo".into()));
    }

    #[test]
    fn delta_reuses_untouched_turns_and_items_but_invalidates_deferred_details() {
        let snapshot = fixture();
        let first = project_snapshot(snapshot.clone(), None);
        let same = project_snapshot(snapshot.clone(), Some(&first));
        assert!(Arc::ptr_eq(&first, &same));
        let mut updated = snapshot.clone();
        let thread = crate::session::SessionChange::Text {
            turn_id: "live".into(),
            item_id: "stream".into(),
            field: crate::session::TextField::AssistantText,
            delta: " world".into(),
        }
        .apply(
            &snapshot.sessions.conversations[&agent_protocol::session::SessionRef {
                provider: Some(agent_protocol::session::ProviderKind::Codex),
                id: "thread".into(),
            }],
        )
        .unwrap();
        Arc::make_mut(&mut updated.sessions.conversations).insert(
            agent_protocol::session::SessionRef {
                provider: Some(agent_protocol::session::ProviderKind::Codex),
                id: "thread".into(),
            },
            Arc::new(thread),
        );
        let second = project_snapshot(updated.clone(), Some(&first));
        assert!(Arc::ptr_eq(&first.turns[0], &second.turns[0]));
        assert!(!Arc::ptr_eq(&first.turns[1], &second.turns[1]));
        for index in [0, 1] {
            assert!(Arc::ptr_eq(
                first.turns[1].items().nth(index).unwrap(),
                second.turns[1].items().nth(index).unwrap()
            ));
        }
        assert_eq!(
            first.turns[1].items().nth(2).unwrap().data.body.as_deref(),
            Some("hello")
        );
        assert_eq!(
            second.turns[1].items().nth(2).unwrap().data.body.as_deref(),
            Some("hello world")
        );
        let mut deferred = updated;
        let thread = Arc::make_mut(
            Arc::make_mut(&mut deferred.sessions.conversations)
                .get_mut(&agent_protocol::session::SessionRef {
                    provider: Some(agent_protocol::session::ProviderKind::Codex),
                    id: "thread".into(),
                })
                .unwrap(),
        );
        Arc::make_mut(
            &mut Arc::make_mut(&mut thread.turns.as_mut().unwrap()[1])
                .items
                .as_mut()
                .unwrap()[1],
        )
        .defer();
        let third = project_snapshot(deferred, Some(&second));
        assert!(!Arc::ptr_eq(
            second.turns[1].items().nth(1).unwrap(),
            third.turns[1].items().nth(1).unwrap()
        ));
        assert!(third.turns[1].items().nth(1).unwrap().data.deferred);
        assert!(Arc::ptr_eq(
            second.turns[1].items().nth(2).unwrap(),
            third.turns[1].items().nth(2).unwrap()
        ));
    }
    #[test]
    fn requests_and_pending_submissions_have_one_shared_native_projection() {
        let mut snapshot = fixture();
        for value in [
            json!({"id": "1", "target": {"turn": {"turnId": "done", "itemId": null}}, "delivery": "awaiting", "body": {"approval": {"kind": "command", "description": "run command", "details": "", "choices": [{"id": "choice-0", "label": "承認", "description": "", "effect": "allow"}, {"id": "choice-1", "label": "このセッションで承認", "description": "", "effect": "allowLasting"}, {"id": "choice-2", "label": "拒否", "description": "", "effect": "deny"}]}}}),
            json!({"id": "question", "target": {"turn":{"turnId":"live","itemId":null}}, "delivery": "awaiting", "body": {"question": {"questions": [{"id": "question-0", "header": "", "prompt": "which?", "secret": false, "allowFreeText": true, "multiple": false, "choices": []}]}}}),
            json!({"id": "3", "target": "session", "delivery": "awaiting", "body": {"approval": {"kind": "fileChange", "description": "", "details": "", "choices": [{"id": "choice-0", "label": "承認", "description": "", "effect": "allow"}, {"id": "choice-1", "label": "このセッションで承認", "description": "", "effect": "allowLasting"}, {"id": "choice-2", "label": "拒否", "description": "", "effect": "deny"}]}}}),
        ] {
            let request: WireRequest = serde_json::from_value(value).unwrap();
            let request = Arc::new(request);
            let session = agent_protocol::session::SessionRef {
                provider: Some(agent_protocol::session::ProviderKind::Codex),
                id: if request.id.as_str() == "3" {
                    "other"
                } else {
                    "thread"
                }
                .into(),
            };
            let thread = Arc::make_mut(
                Arc::make_mut(&mut snapshot.sessions.conversations)
                    .entry(session.clone())
                    .or_insert_with(|| {
                        Arc::new(models::Thread {
                            id: Some(session),
                            ..Default::default()
                        })
                    }),
            );
            thread.requests.insert(request.id.clone(), request);
        }
        let pending = |turn_id| {
            Arc::new(PendingSubmission {
                new_draft_slot: None,
                cursor: None,
                sequence: 0,
                submitted_at_ms: None,
                draft_key: agent_protocol::session::SessionRef {
                    provider: Some(agent_protocol::session::ProviderKind::Codex),
                    id: "thread".into(),
                }
                .into(),
                draft: Arc::new(Draft {
                    text: "queued text".into(),
                    ..Default::default()
                }),
                turn_id,
                after_item_id: None,
                accepted: true,
                delivery_unknown: false,
                queued: false,
            })
        };
        Arc::make_mut(&mut snapshot.sessions.pending_submissions)
            .insert("accepted".into(), pending(Some("live".into())));
        Arc::make_mut(&mut snapshot.sessions.pending_submissions)
            .insert("queued".into(), pending(None));
        let rendered = project_snapshot(snapshot, None);
        let done: Vec<_> = rendered.turns[0]
            .rows
            .iter()
            .filter_map(|row| match &row.content {
                ConversationRowContent::PendingRequest { request } => Some(request),
                _ => None,
            })
            .collect();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].id.as_str(), "1");
        assert_eq!(done[0].title, "コマンドの承認");
        assert_eq!(done[0].body, "run command");
        assert_eq!(
            done[0]
                .request_body
                .choices()
                .iter()
                .map(|choice| choice.label.as_str())
                .collect::<Vec<_>>(),
            ["承認", "このセッションで承認", "拒否"]
        );
        let live: Vec<_> = rendered.turns[1]
            .rows
            .iter()
            .filter_map(|row| match &row.content {
                ConversationRowContent::PendingRequest { request } => Some(request),
                _ => None,
            })
            .collect();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].body, "which?");
        assert_eq!(
            rendered.turns[1]
                .items()
                .filter(|item| item.data.id == "accepted")
                .count(),
            1
        );
        assert_eq!(rendered.queued.len(), 1);
        assert_eq!(rendered.queued[0].data.body.as_deref(), Some("queued text"));
    }
    #[test]
    fn errors_are_titled_as_t3_codes_provider_errors() {
        let error = |category, retry: Option<models::RetryEvidence>| {
            turn_error(
                UiLanguage::English,
                &models::ExecutionError {
                    category,
                    message: "retry".into(),
                    retry,
                    ..Default::default()
                },
            )
        };
        let retrying = |attempt, max| Some(models::RetryEvidence::new(attempt, max));
        let retry = error(models::ErrorCategory::UsageLimit, retrying(None, None));
        assert_eq!(retry.title, "Retrying provider");
        assert!(retry.is_reconnecting);
        assert_eq!(retry.message, "retry");
        assert_eq!(
            error(models::ErrorCategory::Other, retrying(Some(1), None)).title,
            "Retrying provider (1)"
        );
        assert_eq!(
            error(models::ErrorCategory::Other, retrying(Some(2), Some(5))).title,
            "Retrying provider (2/5)"
        );
        assert_eq!(
            error(models::ErrorCategory::UsageLimit, None).title,
            "Usage limit reached"
        );
        assert_eq!(
            error(models::ErrorCategory::ContextLimit, None).title,
            "Provider error"
        );
    }

    #[test]
    fn usage_limit_actions_require_provider_evidence_and_a_finished_retry() {
        for category in [
            models::ErrorCategory::UsageLimit,
            models::ErrorCategory::RateLimited,
            models::ErrorCategory::Other,
        ] {
            let error = turn_error(
                UiLanguage::Japanese,
                &models::ExecutionError {
                    category: category.clone(),
                    message: "usage limit".into(),
                    ..Default::default()
                },
            );
            assert_eq!(
                error.is_usage_limit,
                category == models::ErrorCategory::UsageLimit
            );
        }
        let error = turn_error(
            UiLanguage::Japanese,
            &models::ExecutionError {
                category: models::ErrorCategory::UsageLimit,
                retry: Some(models::RetryEvidence::new(None, None)),
                ..Default::default()
            },
        );
        assert!(!error.is_usage_limit);
    }
}

#[cfg(test)]
mod minimap_tests {
    use super::*;

    #[test]
    fn previews_use_the_last_assistant_message_in_each_exchange_without_borrowing_another_answer() {
        use MinimapRow::{Assistant, Other, User};
        let rows = [
            Assistant, Other, User, Assistant, Other, Assistant, User, Other, User, Assistant,
            Assistant, Other, User,
        ];
        for (user, answer) in [
            (2, Some(5)),
            (6, None),
            (8, Some(10)),
            (12, None),
            (0, None),
            (usize::MAX, None),
        ] {
            assert_eq!(minimap_answer_row(user, rows), answer);
        }
    }

    #[test]
    fn navigation_uses_the_first_visible_prompt_or_the_preceding_exchange_and_never_wraps() {
        let current = |range| {
            minimap_markers(&[2, 8, 14], range, 100.)
                .iter()
                .position(|marker| marker.current)
        };
        assert_eq!(current(Some((0, 1))), None);
        assert_eq!(current(Some((2, 9))), Some(0));
        assert_eq!(current(Some((3, 7))), Some(0));
        assert_eq!(current(Some((3, 8))), Some(1));
        assert_eq!(current(Some((9, 13))), Some(1));
        assert_eq!(current(Some((15, 20))), Some(2));
        assert_eq!(current(None), None);
        assert_eq!(minimap_adjacent_index(3, Some(0), false), None);
        assert_eq!(minimap_adjacent_index(3, Some(0), true), Some(1));
        assert_eq!(minimap_adjacent_index(3, Some(1), false), Some(0));
        assert_eq!(minimap_adjacent_index(3, Some(1), true), Some(2));
        assert_eq!(minimap_adjacent_index(3, Some(2), true), None);
        assert_eq!(minimap_adjacent_index(3, Some(2), false), Some(1));
        assert_eq!(minimap_adjacent_index(3, None, true), None);
        assert_eq!(minimap_adjacent_index(0, Some(0), true), None);
        assert_eq!(minimap_adjacent_index(3, Some(3), false), None);
    }

    #[test]
    fn every_message_has_a_strip_lit_while_it_is_in_view() {
        let markers = minimap_markers(&[0, 4, 9], Some((3, 6)), 500.);
        assert_eq!(markers.iter().map(|m| m.row).collect::<Vec<_>>(), [0, 4, 9]);
        assert_eq!(
            markers.iter().map(|m| m.in_view).collect::<Vec<_>>(),
            [false, true, false]
        );
        assert_eq!(
            markers.iter().map(|m| m.top).collect::<Vec<_>>(),
            [0., 8., 16.]
        );
        assert!(
            minimap_markers(&[0, 4], None, 500.)
                .iter()
                .all(|m| !m.in_view)
        );
    }

    #[test]
    fn a_long_history_spreads_its_strips_over_the_tallest_rail() {
        let rows = (0..101).collect::<Vec<_>>();
        assert_eq!(minimap_rail_height(rows.len(), 400.), 400.);
        let markers = minimap_markers(&rows, None, 400.);
        assert_eq!(markers.len(), 101);
        assert_eq!(markers[50].top, 200.);
        assert_eq!(markers[100].top, 400.);
    }

    #[test]
    fn a_single_message_needs_no_map() {
        assert!(minimap_markers(&[], None, 400.).is_empty());
        assert!(minimap_markers(&[2], Some((0, 9)), 400.).is_empty());
    }

    #[test]
    fn a_message_time_names_its_day_as_t3_code_does() {
        let date = CalendarDay {
            year: 2026,
            month: 11,
            day: 7,
        };
        let cases = [
            (date, 0, 2026, "2:05 PM", "14:05"),
            (date, -1, 2026, "2:05 PM", "14:05"),
            (date, 1, 2026, "yesterday at 2:05 PM", "昨日 14:05"),
            (date, 2, 2026, "11/7 2:05 PM", "11/7 14:05"),
            (date, 370, 2027, "11/7/2026 2:05 PM", "2026/11/7 14:05"),
        ];
        for (date, days_ago, this_year, english, japanese) in cases {
            assert_eq!(
                day_aware_time(UiLanguage::English, 14, 5, date, days_ago, this_year),
                english
            );
            assert_eq!(
                day_aware_time(UiLanguage::Japanese, 14, 5, date, days_ago, this_year),
                japanese
            );
        }
    }

    #[test]
    fn times_read_as_t3_codes_in_each_language() {
        let time = |hour, minute| {
            (
                time_of_day(UiLanguage::English, hour, minute),
                time_of_day(UiLanguage::Japanese, hour, minute),
            )
        };
        assert_eq!(time(0, 5), ("12:05 AM".into(), "00:05".into()));
        assert_eq!(time(2, 17), ("2:17 AM".into(), "02:17".into()));
        assert_eq!(time(12, 0), ("12:00 PM".into(), "12:00".into()));
        assert_eq!(time(23, 59), ("11:59 PM".into(), "23:59".into()));
    }

    #[test]
    fn lanes_keep_t3s_workspace_gutter() {
        assert_eq!(lane_padding(639.9), 12.);
        assert_eq!(lane_padding(640.), 20.);
        assert_eq!(lane_padding(1440.), 20.);
        assert_eq!(lane_padding(0.), 12.);
    }

    #[rstest::rstest]
    #[case(984., 184., true)]
    #[case(983.9, 800., false)]
    #[case(1280., 183.9, false)]
    #[case(1280., 800., true)]
    #[case(700., 800., false)]
    #[case(0., 0., false)]
    fn details_keep_a_readable_conversation_or_use_the_header_popover(
        #[case] width: f32,
        #[case] height: f32,
        #[case] inline: bool,
    ) {
        assert_eq!(details_card_inline(width, height), inline);
    }

    #[test]
    fn the_map_stays_beside_a_48_point_gutter_of_the_canvas() {
        assert!(minimap_persistent(1008., 668.));
        assert!(minimap_persistent(832., 736.));
        assert!(!minimap_persistent(831., 736.));
        assert!(!minimap_persistent(500., 900.));
        assert!(!minimap_persistent(0., 0.));
    }

    #[test]
    fn the_pointer_area_never_reaches_the_lane() {
        assert_eq!(minimap_hit_width(20.), 8.);
        assert_eq!(minimap_hit_width(12.5), 0.);
        assert_eq!(minimap_hit_width(5.), 0.);
        assert_eq!(minimap_hit_width(200.), 40.);
    }

    #[test]
    fn strips_widen_toward_the_hovered_one() {
        assert_eq!(minimap_marker_width(Some(0)), 24.);
        assert!(minimap_marker_width(Some(1)) > minimap_marker_width(Some(2)));
        assert!(minimap_marker_width(Some(2)) > minimap_marker_width(Some(3)));
        assert_eq!(minimap_marker_width(Some(9)), minimap_marker_width(None));
    }
}
#[cfg(test)]
mod progress_tests {
    use super::*;
    #[test]
    fn elapsed_work_uses_provider_time_and_clamps_clock_skew() {
        for value in [
            serde_json::json!({"id":"t","startedAt":100.5}),
            serde_json::json!({"id":"t","startedAtMs":100500}),
            serde_json::json!({"id":"t","startedAt":100.5,"startedAtMs":0}),
        ] {
            let source = Arc::new(serde_json::from_value(value).unwrap());
            let turn = RenderedTurn {
                supports_fork: false,
                language: UiLanguage::Japanese,
                source,
                occurrence: 0,
                pending: Default::default(),
                requests: vec![],
                rows: vec![],
            };
            let progress = turn.work_progress();
            assert_eq!(progress.label(UiLanguage::Japanese, 108.5), "8秒 作業中…");
            assert_eq!(progress.label(UiLanguage::Japanese, 110.5), "10秒 作業中…");
            assert_eq!(progress.label(UiLanguage::Japanese, 99.), "0秒 作業中…");
            assert_eq!(
                progress.label(UiLanguage::Japanese, 285.5),
                "3分 5秒 作業中…"
            );
        }
        assert_eq!(
            WorkProgress {
                started_at: None,
                compacting: false
            }
            .label(UiLanguage::Japanese, 100.),
            "作業中…"
        );
    }

    #[test]
    fn compaction_phase_uses_active_items_including_deferred_summaries() {
        for delivery in ["inline", "deferred"] {
            for (status, compacting) in
                [("running", true), ("completed", false), ("unknown", false)]
            {
                let body = if delivery == "inline" {
                    serde_json::json!({"inline":{"body":{"compaction":{}}}})
                } else {
                    serde_json::json!({"deferred":{"summary":{"compaction":{}}}})
                };
                let source = Arc::new(
                    serde_json::from_value(serde_json::json!({
                        "id":{"provider":"codex", "id":"fixture"},
                        "turns":[{"id":"turn", "status":"running", "startedAt":100.5,
                                  "items":[{"id":"compact", "status":status, "body":body}]}]
                    }))
                    .unwrap(),
                );
                let conversation = project_conversation(
                    UiLanguage::Japanese,
                    &"draft".into(),
                    &Default::default(),
                    source,
                    &None,
                );
                let progress = conversation.turns[0].work_progress();
                assert_eq!(progress.is_compacting(), compacting);
                assert_eq!(
                    progress.label(UiLanguage::Japanese, 108.5),
                    if compacting {
                        "8秒 圧縮中…"
                    } else {
                        "8秒 作業中…"
                    }
                );
            }
        }
    }

    #[test]
    fn progress_keeps_phase_and_start_time_without_retaining_the_conversation() {
        let source = Arc::new(serde_json::from_value(serde_json::json!({
            "id":{"provider":"codex", "id":"fixture"},
            "turns":[{"id":"turn", "status":"running", "startedAt":100.5,
                      "items":[{"id":"compact", "status":"running", "body":{"inline":{"body":{"compaction":{}}}}}]}]
        })).unwrap());
        let conversation = project_conversation(
            UiLanguage::Japanese,
            &"draft".into(),
            &Default::default(),
            source,
            &None,
        );
        let turn = Arc::downgrade(&conversation.turns[0]);
        let progress = conversation.turns[0].work_progress();
        drop(conversation);
        assert!(turn.upgrade().is_none());
        assert_eq!(progress.label(UiLanguage::Japanese, 110.5), "10秒 圧縮中…");
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct HistoryControl {
    pub has_more: bool,
    pub loading: bool,
    pub load_label: String,
    pub error: Option<super::error::ErrorText>,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn history_control(&self, session: crate::session::SessionRef) -> Option<HistoryControl> {
        let has_more = self
            .sessions
            .conversations
            .get(&session)
            .is_some_and(|thread| thread.history_has_more == Some(true));
        let key = crate::state::operations::OperationKey::History { session };
        let error = self.operation_error(key.clone());
        if !has_more && error.is_none() {
            return None;
        }
        let loading = self.operation_running(key);
        Some(HistoryControl {
            has_more,
            loading,
            load_label: self
                .catalog
                .ui_language
                .text(if loading {
                    "以前のターンを読み込み中…"
                } else {
                    "以前のターンを読み込む"
                })
                .into(),
            error,
        })
    }
}

#[cfg(test)]
mod history_control_tests {
    use super::*;
    use crate::{
        session::{ProviderKind, SessionRef},
        state::operations::{OperationKey, OperationPhase, OperationState},
    };

    #[test]
    fn history_control_tracks_its_own_page_request_and_retry() {
        let session = SessionRef::new(ProviderKind::Codex, "history".into()).unwrap();
        let other = SessionRef::new(ProviderKind::Codex, "other".into()).unwrap();
        let mut snapshot = Snapshot::default();
        assert_eq!(snapshot.history_control(session.clone()), None);
        Arc::make_mut(&mut snapshot.sessions.conversations).insert(
            session.clone(),
            Arc::new(models::Thread {
                history_has_more: Some(true),
                ..Default::default()
            }),
        );
        let idle = HistoryControl {
            has_more: true,
            loading: false,
            load_label: "以前のターンを読み込む".into(),
            error: None,
        };
        assert_eq!(
            snapshot.history_control(session.clone()),
            Some(idle.clone())
        );
        let key = OperationKey::History {
            session: session.clone(),
        };
        for (phase, expected) in [
            (OperationPhase::Running, HistoryControl {
                loading: true,
                load_label: "以前のターンを読み込み中…".into(),
                ..idle.clone()
            }),
            (OperationPhase::Failed {
                message: r#"remote RPC error: {"code":"provider_failed","message":"Older page unavailable"}"#.into(),
            }, HistoryControl {
                error: Some(crate::presentation::error::ErrorText {
                    message: "ホストで操作に失敗しました。".into(),
                    details: Some("Older page unavailable".into()),
                }),
                ..idle.clone()
            }),
            (OperationPhase::Running, HistoryControl {
                loading: true,
                load_label: "以前のターンを読み込み中…".into(),
                ..idle.clone()
            }),
        ] {
            Arc::make_mut(&mut snapshot.operations).insert(key.clone(),
            OperationState {
                generation: 1,
                phase,
            },
            );
            assert_eq!(snapshot.history_control(session.clone()), Some(expected));
            assert_eq!(snapshot.history_control(other.clone()), None);
        }
        snapshot.operations = Default::default();
        assert_eq!(snapshot.history_control(session), Some(idle));
    }

    #[rstest::rstest]
    #[case(None)]
    #[case(Some(false))]
    fn no_continuation_hides_the_button_but_preserves_a_read_failure(#[case] more: Option<bool>) {
        let session = SessionRef::new(ProviderKind::Codex, "history".into()).unwrap();
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.sessions.conversations).insert(
            session.clone(),
            Arc::new(models::Thread {
                history_has_more: more,
                ..Default::default()
            }),
        );
        assert_eq!(snapshot.history_control(session.clone()), None);
        Arc::make_mut(&mut snapshot.operations).insert(
            OperationKey::History {
                session: session.clone(),
            },
            OperationState {
                generation: 1,
                phase: OperationPhase::Failed {
                    message: "History unavailable".into(),
                },
            },
        );
        assert_eq!(
            snapshot.history_control(session),
            Some(HistoryControl {
                has_more: false,
                loading: false,
                load_label: "以前のターンを読み込む".into(),
                error: Some(crate::presentation::error::ErrorText {
                    message: "エラーが発生しました。".into(),
                    details: Some("History unavailable".into()),
                }),
            })
        );
    }
}

/// Shared read-state wording; native views only render this projection.
pub fn history_notice(language: UiLanguage, thread: &models::Thread) -> Option<String> {
    use crate::session::HistoryReadKind;
    let state = thread.history_read_state.as_ref()?;
    let heading = match state.kind {
        // Older pages are loaded on demand. A usable continuation is normal,
        // while missing history or reported read issues still need a notice.
        HistoryReadKind::Partial
            if state.issues.is_empty()
                && thread.history_has_more == Some(true)
                && thread
                    .history_cursor
                    .as_ref()
                    .is_some_and(|cursor| !cursor.is_empty()) =>
        {
            return None;
        }
        HistoryReadKind::Loading => "履歴を読み込んでいます。",
        HistoryReadKind::Partial => "履歴の一部を表示しています。",
        HistoryReadKind::Incomplete => "履歴の一部を読み取れませんでした。",
        HistoryReadKind::Unavailable => {
            "履歴を取得できません。保存済みの表示は最新とは限りません。"
        }
        HistoryReadKind::Complete => return None,
    };
    let issues = state
        .issues
        .iter()
        .take(8)
        .map(|id| id.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    Some(if issues.is_empty() {
        language.text(heading).into()
    } else {
        format!("{}\n{issues}", language.text(heading))
    })
}

#[cfg(test)]
mod history_notice_tests {
    use super::*;
    use crate::session::{HistoryReadKind, HistoryReadState};

    #[test]
    fn history_headings_translate_without_changing_issue_identifiers() {
        for (kind, expected) in [
            (HistoryReadKind::Loading, "Loading history."),
            (HistoryReadKind::Partial, "Some history is shown."),
            (
                HistoryReadKind::Incomplete,
                "Some history could not be read.",
            ),
            (
                HistoryReadKind::Unavailable,
                "History is unavailable. Saved content may be out of date.",
            ),
        ] {
            let thread = models::Thread {
                history_read_state: Some(HistoryReadState {
                    kind,
                    issues: vec!["設定/{0}".into()],
                }),
                ..Default::default()
            };
            let source = Arc::new(thread);
            let japanese = project_conversation(
                UiLanguage::Japanese,
                &Default::default(),
                &Default::default(),
                source.clone(),
                &None,
            );
            let english = project_conversation(
                UiLanguage::English,
                &Default::default(),
                &Default::default(),
                source,
                &Some(japanese),
            );
            assert_eq!(
                english.history_notice(),
                Some(format!("{expected}\n設定/{{0}}"))
            );
        }
    }

    #[test]
    fn history_pages_keep_source_errors_visible_and_can_clear_them_after_recovery() {
        let mut thread = models::Thread {
            history_read_state: Some(HistoryReadState::new(
                HistoryReadKind::Incomplete,
                vec!["claude: Claude failed to start".into()],
            )),
            ..Default::default()
        };
        assert_eq!(
            history_notice(UiLanguage::Japanese, &thread).as_deref(),
            Some("履歴の一部を読み取れませんでした。\nclaude: Claude failed to start")
        );
        thread.history_read_state =
            Some(HistoryReadState::new(HistoryReadKind::Complete, Vec::new()));
        assert_eq!(history_notice(UiLanguage::Japanese, &thread), None);
    }

    #[test]
    fn normal_pagination_has_no_warning_before_or_after_loading_the_remaining_history() {
        let mut thread = models::Thread {
            history_cursor: Some("older-page".into()),
            history_has_more: Some(true),
            ..Default::default()
        };
        assert_eq!(history_notice(UiLanguage::Japanese, &thread), None);
        thread.history_cursor = None;
        thread.history_has_more = Some(false);
        assert_eq!(history_notice(UiLanguage::Japanese, &thread), None);
    }

    #[rstest::rstest]
    #[case(None, None)]
    #[case(None, Some(""))]
    #[case(None, Some("older-page"))]
    #[case(Some(false), None)]
    #[case(Some(false), Some(""))]
    #[case(Some(false), Some("older-page"))]
    #[case(Some(true), None)]
    #[case(Some(true), Some(""))]
    fn a_partial_read_without_a_usable_continuation_keeps_its_notice(
        #[case] more: Option<bool>,
        #[case] cursor: Option<&str>,
    ) {
        let thread = models::Thread {
            history_has_more: more,
            history_cursor: cursor.map(str::to_owned),
            history_read_state: Some(HistoryReadState::new(HistoryReadKind::Partial, Vec::new())),
            ..Default::default()
        };
        assert_eq!(
            history_notice(UiLanguage::Japanese, &thread).as_deref(),
            Some("履歴の一部を表示しています。")
        );
    }

    #[rstest::rstest]
    #[case(HistoryReadKind::Partial, "履歴の一部を表示しています。")]
    #[case(HistoryReadKind::Incomplete, "履歴の一部を読み取れませんでした。")]
    #[case(
        HistoryReadKind::Unavailable,
        "履歴を取得できません。保存済みの表示は最新とは限りません。"
    )]
    fn read_failures_remain_visible_even_when_older_pages_exist(
        #[case] kind: HistoryReadKind,
        #[case] heading: &str,
    ) {
        let thread = models::Thread {
            history_has_more: Some(true),
            history_cursor: Some("older-page".into()),
            history_read_state: Some(HistoryReadState::new(kind, vec!["read issue".into()])),
            ..Default::default()
        };
        assert_eq!(
            history_notice(UiLanguage::Japanese, &thread),
            Some(format!("{heading}\nread issue"))
        );
    }
}

#[cfg(test)]
mod system_notice_tests {
    use super::*;
    use crate::models::{Item, ItemBody, ItemStatus, Thread, Turn, TurnStatus};

    #[test]
    fn notices_stay_in_order_outside_work_and_do_not_make_the_first_input_a_steer() {
        let item = |id: &str, body| Arc::new(Item::new(id.into(), ItemStatus::Completed, body));
        let thread = Arc::new(Thread {
            turns: Some(vec![Arc::new(Turn {
                id: "notice-turn".into(),
                status: TurnStatus::Completed,
                items: Some(vec![
                    item(
                        "before",
                        ItemBody::SystemNotice {
                            message: "Earlier history omitted".into(),
                        },
                    ),
                    item(
                        "user",
                        ItemBody::UserMessage {
                            text: Some("First real question".into()),
                            content: vec![],
                        },
                    ),
                    item(
                        "middle",
                        ItemBody::SystemNotice {
                            message: "Provider information".into(),
                        },
                    ),
                    item(
                        "assistant",
                        ItemBody::AssistantText {
                            text: "Answer".into(),
                            phase: models::AssistantPhase::Final,
                        },
                    ),
                    item(
                        "after",
                        ItemBody::SystemNotice {
                            message: "Later information".into(),
                        },
                    ),
                ]),
                ..Default::default()
            })]),
            ..Default::default()
        });
        let view = project_conversation(
            UiLanguage::English,
            &Default::default(),
            &Default::default(),
            thread,
            &None,
        );
        let rows = turn_rows(
            crate::presentation::conversation::TimelineStyle::T3,
            &view.turns[0],
            &Default::default(),
            &Default::default(),
        );
        let visible: Vec<_> = rows
            .iter()
            .filter_map(|row| match row {
                TurnRow::Divider { label, .. } => Some(label.as_str()),
                TurnRow::Row(ConversationRow {
                    content: ConversationRowContent::User { item },
                    steer,
                    ..
                }) => {
                    assert!(!steer, "a preceding notice is not a preceding user input");
                    item.data.body.as_deref()
                }
                TurnRow::Row(ConversationRow {
                    content: ConversationRowContent::Response { item, .. },
                    ..
                }) => item.data.body.as_deref(),
                _ => None,
            })
            .collect();
        assert_eq!(
            visible,
            [
                "Earlier history omitted",
                "First real question",
                "Provider information",
                "Answer",
                "Later information"
            ]
        );
        assert!(
            !rows.iter().any(|row| matches!(row, TurnRow::Fold { .. })),
            "notices do not create empty work disclosures"
        );
    }
}
