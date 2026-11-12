//! Rust-owned roots for a desktop window or an embedded native conversation.
use crate::{Area, application::Application, shared::Shared};
use nsh_core::{
    element::{AnyValue, PropError, PropSpec, TsTypes},
    prelude::*,
};
#[cfg(target_os = "ios")]
use nsh_platform_ios::Root;
#[cfg(target_os = "macos")]
use nsh_platform_macos::Root;
#[cfg(windows)]
use nsh_platform_windows::Root;
use std::sync::Arc;
#[cfg(target_os = "ios")]
use tokio::sync::watch;

type Actions = Arc<dyn Fn(serde_json::Value) + Send + Sync>;

#[derive(Clone)]
struct ViewProps {
    application: Option<Shared<Application>>,
    area: Area,
    actions: Option<Actions>,
    #[cfg(target_os = "ios")]
    composer_inset: watch::Receiver<f32>,
}

impl PartialEq for ViewProps {
    fn eq(&self, other: &Self) -> bool {
        #[cfg(target_os = "ios")]
        if !self.composer_inset.same_channel(&other.composer_inset) {
            return false;
        }
        self.application == other.application
            && self.area == other.area
            && match (&self.actions, &other.actions) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
    }
}

impl Props for ViewProps {
    fn spec(_: &mut TsTypes) -> Vec<PropSpec> {
        Vec::new()
    }
    fn initial() -> Self {
        Self {
            application: None,
            area: Area::Mobile,
            actions: None,
            #[cfg(target_os = "ios")]
            composer_inset: watch::channel(0.0).1,
        }
    }
    fn set(&mut self, name: &str, value: Option<AnyValue>) -> Result<bool, PropError> {
        ().set(name, value)
    }
    fn set_slot(&mut self, _: &str, _: Option<Slot>) -> bool {
        false
    }
    fn set_children(&mut self, _: Children) -> bool {
        false
    }
}

struct ViewRoot;

#[cfg(target_os = "ios")]
#[derive(Clone, Copy)]
pub(crate) struct ComposerInset(pub ReadSignal<f32>);

impl Component for ViewRoot {
    type Props = ViewProps;
    type Event = NoEvent;
    fn view(props: ViewProps, _: Scope<NoEvent>) -> impl IntoElement {
        #[cfg(target_os = "ios")]
        {
            let mut updates = props.composer_inset;
            let inset = signal(*updates.borrow_and_update());
            spawn(async move {
                while updates.changed().await.is_ok() {
                    inset.set(*updates.borrow_and_update());
                }
            });
            provide_context(ComposerInset(inset.read_only()));
        }
        if let Some(actions) = props.actions {
            provide_context(crate::native::ExternalActions(stored(actions)));
        }
        crate::app_view(
            props.application.expect("native root has an application").0,
            props.area,
        )
    }
}

pub(crate) fn root(
    application: Arc<Application>,
    area: Area,
    actions: Option<Actions>,
    #[cfg(target_os = "ios")] composer_inset: watch::Receiver<f32>,
) -> Root {
    Root::new::<ViewRoot>(move || ViewProps {
        application: Some(Shared(application.clone())),
        area,
        actions: actions.clone(),
        #[cfg(target_os = "ios")]
        composer_inset: composer_inset.clone(),
    })
}
