//! The native viewport's delivered snapshot owns incremental bridge updates.
use super::hosted::Hosted;
use nsh_core::JsonValue;
use nsh_platform_macos::{ComponentContext, Components, NativeComponent};
use objc2::rc::Retained;
use objc2_app_kit::NSView;
use std::{collections::BTreeMap, rc::Rc};

struct Conversation {
    hosted: Hosted,
    scope: String,
    order: Vec<String>,
    delivered: BTreeMap<String, JsonValue>,
}

impl NativeComponent for Conversation {
    fn view(&self) -> Retained<NSView> {
        self.hosted.view()
    }

    fn set_prop(&mut self, name: &str, value: Option<&JsonValue>) {
        let Some(value) = value.filter(|_| name == "data") else {
            return;
        };
        let update =
            crate::conversation::native::changes(&self.scope, &self.order, &self.delivered, value);
        if update["reset"] == true {
            self.delivered.clear();
        }
        if let Some(order) = update["order"].as_array() {
            self.order = order
                .iter()
                .filter_map(|id| id.as_str().map(str::to_owned))
                .collect();
        }
        if let Some(removed) = update["removed"].as_array() {
            for id in removed.iter().filter_map(JsonValue::as_str) {
                self.delivered.remove(id);
            }
        }
        if let Some(changed) = update["changed"].as_array() {
            for row in changed {
                if let Some(id) = row["id"].as_str() {
                    self.delivered.insert(id.into(), row.clone());
                }
            }
        }
        self.scope = value["scope"].as_str().unwrap_or_default().into();
        self.hosted.set_prop("data", Some(&update));
    }

    fn command(&mut self, name: &str, args: &JsonValue) {
        self.hosted.command(name, args);
    }
}

pub(super) fn register(components: &Components) {
    components.register(
        "bex.conversation",
        Rc::new(|context: ComponentContext| {
            Box::new(Conversation {
                hosted: Hosted::new(context, "bex_nsh_conversation"),
                scope: String::new(),
                order: Vec::new(),
                delivered: BTreeMap::new(),
            })
        }),
    );
}
