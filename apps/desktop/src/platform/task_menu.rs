//! macOS owns a system menu independently of any application window.
use crate::{Runtime, store_session::StoreSession};
use agent_core::state::Snapshot;
use agent_protocol::live_activity::{TaskActivityIconKind, TaskActivityView};
use gpui_kit::{App, AppContext, Context, Entity, Global, Task};
use objc2::{
    DefinedClass, MainThreadOnly, define_class, msg_send, rc::Retained, runtime::AnyObject, sel,
};
use objc2_app_kit::{
    NSCellImagePosition, NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem,
    NSVariableStatusItemLength,
};
use objc2_foundation::{
    MainThreadMarker, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};
use std::sync::Arc;

#[derive(Clone, Copy)]
enum Action {
    Open,
    Quit,
}

// SAFETY: NSObject has no subclass requirements. Actions run on the AppKit main thread.
define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = async_channel::Sender<Action>]
    struct MenuActions;
    unsafe impl NSObjectProtocol for MenuActions {}
    impl MenuActions {
        #[unsafe(method(openBex:))]
        fn open(&self, _: Option<&AnyObject>) { let _ = self.ivars().try_send(Action::Open); }
        #[unsafe(method(quitBex:))]
        fn quit(&self, _: Option<&AnyObject>) { let _ = self.ivars().try_send(Action::Quit); }
    }
);

struct StatusMenu {
    item: Retained<NSStatusItem>,
    actions: Retained<MenuActions>,
}
impl StatusMenu {
    fn new(actions: async_channel::Sender<Action>) -> Self {
        let mtm = MainThreadMarker::new().expect("menu runs on main thread");
        let allocated = MenuActions::alloc(mtm).set_ivars(actions);
        // SAFETY: NSObject init has no additional requirements.
        let actions = unsafe { msg_send![super(allocated), init] };
        let result = Self {
            item: NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength),
            actions,
        };
        result.render(None);
        result
    }
    fn render(&self, view: Option<&TaskActivityView>) {
        let mtm = MainThreadMarker::new().expect("menu runs on main thread");
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("Bex"));
        menu.setAutoenablesItems(false);
        if let Some(view) = view {
            let header = menu_item(&view.label, None, mtm);
            header.setEnabled(false);
            menu.addItem(&header);
            for icon in &view.icons {
                let row = menu_item(&icon.label, None, mtm);
                row.setEnabled(false);
                row.setImage(symbol(icon.kind, &icon.label).as_deref());
                menu.addItem(&row);
            }
            if view.overflow > 0 {
                let row = menu_item(&format!("+{}", view.overflow), None, mtm);
                row.setEnabled(false);
                menu.addItem(&row);
            }
            menu.addItem(&NSMenuItem::separatorItem(mtm));
        }
        for (title, action) in [("Bexを開く", sel!(openBex:)), ("Bexを終了", sel!(quitBex:))]
        {
            let row = menu_item(title, Some(action), mtm);
            // SAFETY: The retained target implements these selectors with the NSMenuItem action signature.
            unsafe {
                row.setTarget(Some(&self.actions));
            }
            menu.addItem(&row);
        }
        self.item.setMenu(Some(&menu));
        if let Some(button) = self.item.button(mtm) {
            button.setImagePosition(NSCellImagePosition::ImageLeft);
            if let Some(view) = view {
                let symbols: Vec<_> = view
                    .icons
                    .iter()
                    .map(|icon| symbol(icon.kind, &icon.label))
                    .collect();
                let count = symbols.len();
                let draw = block2::RcBlock::new(move |_: NSRect| {
                    for (index, symbol) in symbols.iter().enumerate() {
                        if let Some(symbol) = symbol {
                            symbol.drawInRect(NSRect::new(
                                NSPoint::new(index as f64 * 18.0, 1.0),
                                NSSize::new(14.0, 14.0),
                            ));
                        }
                    }
                    objc2::runtime::Bool::YES
                });
                let image = NSImage::imageWithSize_flipped_drawingHandler(
                    NSSize::new((count as f64 * 18.0 - 4.0).max(14.0), 16.0),
                    false,
                    &draw,
                );
                image.setTemplate(true);
                image.setAccessibilityDescription(Some(&NSString::from_str(&format!(
                    "Bex: {}",
                    view.label
                ))));
                button.setImage(Some(&image));
                button.setTitle(&NSString::from_str(&if view.overflow > 0 {
                    format!("+{}", view.overflow)
                } else {
                    String::new()
                }));
                button.setToolTip(Some(&NSString::from_str(&view.label)));
            } else {
                button.setImage(symbol(TaskActivityIconKind::Unknown, "Bex").as_deref());
                button.setTitle(&NSString::from_str(""));
                button.setToolTip(Some(&NSString::from_str("Bex")));
            }
        }
    }
}
impl Drop for StatusMenu {
    fn drop(&mut self) {
        NSStatusBar::systemStatusBar().removeStatusItem(&self.item);
    }
}
fn menu_item(
    title: &str,
    action: Option<objc2::runtime::Sel>,
    mtm: MainThreadMarker,
) -> Retained<NSMenuItem> {
    // SAFETY: Actions are either absent or implemented by MenuActions above.
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            action,
            &NSString::from_str(""),
        )
    }
}
fn symbol(kind: TaskActivityIconKind, label: &str) -> Option<Retained<NSImage>> {
    let name = match kind {
        TaskActivityIconKind::Running => "circle.dotted",
        TaskActivityIconKind::Waiting => "person.crop.circle.badge.questionmark",
        TaskActivityIconKind::Unknown => "arrow.clockwise.circle",
        TaskActivityIconKind::Finished => "checkmark.circle.fill",
    };
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(name),
        Some(&NSString::from_str(label)),
    )?;
    image.setTemplate(true);
    Some(image)
}

enum Update {
    Connected(Result<StoreSession, String>),
    Snapshot(Arc<Snapshot>),
}
struct TaskMenu {
    menu: StatusMenu,
    session: Option<StoreSession>,
    _updates: Task<()>,
    _actions: Task<()>,
}
struct TaskMenuRoot {
    _menu: Entity<TaskMenu>,
}
impl Global for TaskMenuRoot {}

pub(crate) fn init(cx: &mut App) {
    let menu = cx.new(|cx: &mut Context<TaskMenu>| {
        StoreSession::on_app_quit(cx, |menu| &mut menu.session);
        let (actions, incoming_actions) = async_channel::unbounded();
        let (updates, incoming) = async_channel::unbounded();
        let runtime = cx.global::<Runtime>().clone();
        let connections = runtime.connections.clone();
        runtime.handle.clone().spawn(async move {
            let mut delay = std::time::Duration::from_millis(250);
            let store = loop {
                if updates.is_closed() {
                    return;
                }
                match connections.connect(None, Snapshot::default()).await {
                    Ok(store) => break store,
                    Err(error) => {
                        if updates
                            .send(Update::Connected(Err(format!("{error:#}"))))
                            .await
                            .is_err()
                        {
                            return;
                        }
                        tokio::time::sleep(delay).await;
                        delay = (delay * 2).min(std::time::Duration::from_secs(5));
                    }
                }
            };
            StoreSession::publish(
                Ok(store),
                runtime,
                updates,
                Update::Connected,
                Update::Snapshot,
            )
            .await;
        });
        let updates = cx.spawn(async move |menu, cx| {
            while let Ok(update) = incoming.recv().await {
                if menu
                    .update(cx, |menu, _| match update {
                        Update::Connected(Ok(session)) => menu.session = Some(session),
                        Update::Connected(Err(error)) => {
                            tracing::error!(target: "bex", operation = "task_menu.connect", %error)
                        }
                        Update::Snapshot(snapshot) => {
                            if let Some(display) = snapshot.task_activity_display() {
                                menu.menu.render(Some(&display.current));
                            }
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let actions_task = cx.spawn(async move |_, cx| {
            while let Ok(action) = incoming_actions.recv().await {
                cx.update(|cx| match action {
                    Action::Open => crate::open_main_window(cx),
                    Action::Quit => cx.quit(),
                });
            }
        });
        TaskMenu {
            menu: StatusMenu::new(actions),
            session: None,
            _updates: updates,
            _actions: actions_task,
        }
    });
    cx.set_global(TaskMenuRoot { _menu: menu });
}
