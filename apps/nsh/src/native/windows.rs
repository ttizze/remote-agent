//! Win32 input NSH's views do not report: the wheel and touchpad scrolls over
//! a view whose content is not an NSH scroll view (a Host's page image, the
//! terminal's grid), which NSH would otherwise only use to scroll, and the
//! image viewer's Ctrl + wheel, which zooms it.
use super::Scroll;
use agent_core::presentation::media::ZoomGesture;
use nsh_platform_windows::{ComponentContext, Components, NativeComponent, Wheel};
use std::rc::Rc;

mod conversation;

pub(crate) fn register(components: &Components) {
    conversation::register(components);
    components.register("bex.wheel", Rc::new(|context| Box::new(WheelArea(context))));
    components.register("bex.zoom", Rc::new(|context| Box::new(ZoomArea(context))));
}

/// Draws nothing; it takes Ctrl + wheel, which a precision touchpad's pinch
/// also is, and leaves other wheels to the scroll views around it.
struct ZoomArea(ComponentContext);

impl NativeComponent for ZoomArea {
    fn set_prop(&mut self, _: &str, _: Option<&serde_json::Value>) {}

    fn wheel(&mut self, event: &Wheel) -> bool {
        if !event.ctrl || event.notches_y == 0.0 {
            return false;
        }
        let gesture = ZoomGesture::Wheel {
            notches: event.notches_y,
        };
        self.0.emit(
            "zoom",
            serde_json::to_value(gesture).expect("a gesture is plain numbers"),
        );
        true
    }
}

/// Draws nothing; its view keeps presses and moves, it takes the wheel.
struct WheelArea(ComponentContext);

impl NativeComponent for WheelArea {
    fn set_prop(&mut self, _: &str, _: Option<&serde_json::Value>) {}

    fn wheel(&mut self, event: &Wheel) -> bool {
        let scroll = Scroll {
            x: event.x,
            y: event.y,
            dx: event.dx,
            dy: event.dy,
            shift: event.shift,
            ctrl: event.ctrl,
            alt: event.alt,
        };
        self.0.emit(
            "wheel",
            serde_json::to_value(scroll).expect("a scroll is plain numbers"),
        );
        true
    }
}
