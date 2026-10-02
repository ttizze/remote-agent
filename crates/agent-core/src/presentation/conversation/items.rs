//! Materialize native and pending items without losing their cached render identity.
use super::super::{body, item_presentation};
use super::{ItemPresentation, ItemSource, RenderedItem};
use crate::{models, state::PendingSubmission};
use std::sync::Arc;

impl RenderedItem {
    pub(super) fn key(&self) -> (Option<&str>, *const ()) {
        match &self.source {
            ItemSource::Native(item) => (None, Arc::as_ptr(item).cast()),
            ItemSource::Pending(id, pending) => (Some(id), Arc::as_ptr(pending).cast()),
        }
    }
    pub(super) fn native(
        item: &Arc<models::Item>,
        provider: Option<crate::session::ProviderKind>,
        deferred: bool,
        previous: Option<&Arc<Self>>,
    ) -> Arc<Self> {
        if let Some(previous) = previous
            && previous.data.deferred == deferred
        {
            return previous.clone();
        }
        let presentation = item_presentation(item, provider);
        let body = body::item_body(item, &presentation);
        let image_placeholder = presentation.kind == "imageGeneration"
            && item.status == models::ItemStatus::Running
            && body.images.is_empty();
        Arc::new(Self {
            source: ItemSource::Native(item.clone()),
            data: ItemPresentation {
                id: item
                    .client_input_id
                    .as_deref()
                    .unwrap_or(item.id.as_str())
                    .to_owned(),
                native_id: Some(item.id.clone()),
                kind: presentation.kind.into(),
                title: presentation.title,
                collapsible: presentation.collapsible,
                visible: presentation.visible,
                body: body.text,
                image_sources: body.images,
                image_placeholder,
                deferred,
            },
        })
    }
    pub(super) fn pending(
        id: &str,
        pending: &Arc<PendingSubmission>,
        previous: Option<&Arc<Self>>,
    ) -> Arc<Self> {
        if let Some(previous) = previous {
            return previous.clone();
        }
        let body = body::draft_body(&pending.draft);
        Arc::new(Self {
            source: ItemSource::Pending(id.into(), pending.clone()),
            data: ItemPresentation {
                id: id.into(),
                native_id: None,
                kind: "user".into(),
                title: pending.delivery_label().into(),
                collapsible: false,
                visible: true,
                body: body.text,
                image_sources: body.images,
                image_placeholder: false,
                deferred: false,
            },
        })
    }
}
