//! Rust-owned roots for a desktop window or an embedded native conversation.
use crate::{Area, application::Application, shared::Shared};
use nsh_core::{
    element::{AnyValue, PropError, PropSpec, TsTypes},
    prelude::*,
};
use nsh_engine::Root;
use std::sync::Arc;

type Actions = Arc<dyn Fn(serde_json::Value) + Send + Sync>;

#[derive(Clone)]
struct ViewProps {
    application: Option<Shared<Application>>,
    area: Area,
    actions: Option<Actions>,
}

impl PartialEq for ViewProps {
    fn eq(&self, other: &Self) -> bool {
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
impl Component for ViewRoot {
    type Props = ViewProps;
    type Event = NoEvent;
    fn view(props: ViewProps, _: Scope<NoEvent>) -> impl IntoElement {
        if let Some(actions) = props.actions {
            let send: Box<dyn Fn(serde_json::Value)> = Box::new(move |event| actions(event));
            provide_context(crate::native::ExternalActions(stored(send)));
        }
        crate::app_view(
            props.application.expect("native root has an application").0,
            props.area,
        )
    }
}

pub(crate) fn root(application: Arc<Application>, area: Area, actions: Option<Actions>) -> Root {
    Root::new::<ViewRoot>(move || ViewProps {
        application: Some(Shared(application.clone())),
        area,
        actions: actions.clone(),
    })
}
