//! The pen as the page should see it (Linux only; docs/ui-design.md §7.2.19).
//!
//! WebKitGTK's GTK 3 port builds every pointer event from the `GdkEvent` as a
//! mouse event (`WebEventFactory::createWebMouseEvent` reads neither the
//! source device nor the axes), so a stylus reaches the page as
//! `pointerType: "mouse"` with the mouse's default pressure (0.5 while a button
//! is down), its eraser touching as the left button, and its barrel buttons as
//! GDK's middle (BTN_STYLUS) and right (BTN_STYLUS2) buttons. This handler
//! reads the tool, the pressure and the barrel buttons from the same
//! `GdkEvent` and sends `app-pen` window events
//! `{ pen: true, eraser, eraserButton, barrel, barrel2, pressure }` (pressure
//! 0–1, `null` when the tablet has none) or `{ pen: false }` when they change;
//! ui/pen.js folds them into the page's pointer events. GTK 3 emits a widget's
//! `event` signal before WebKit's own handlers, and `eval` on the main thread
//! runs at once (Tauri skips its event queue there), so the page hears about
//! the pen before WebKit passes the same event on.
//!
//! GTK reports two different things as the eraser tool: a flip-over eraser
//! end, and the eraser button many laptop pens have instead (libwacom's
//! `EraserType=Button`: a barrel button that switches the pen to erasing while
//! held). `eraserButton` is the guess between them: a button when the eraser
//! tool has the same hardware id and serial as the pen tool last seen (an AES
//! pen's are both 0x8113 / 0x806680668e4f), an end when they differ, `null`
//! when GTK gives no ids or no pen was seen yet. Settings → Pen can override it.

use std::cell::RefCell;

use gtk::gdk::{self, AxisUse, EventType, InputSource};
use gtk::glib::Propagation;
use gtk::prelude::*;
use tauri::{Manager, Runtime, WebviewWindow};

/// The stylus behind a pointer event, as the page gets it.
#[derive(Clone, Copy, PartialEq)]
struct Pen {
    /// The eraser tool, an end or a button (see `eraser_button`).
    eraser: bool,
    /// Whether that eraser is a held button; `None` when unknown.
    eraser_button: Option<bool>,
    barrel: bool,
    barrel2: bool,
    pressure: Option<f64>,
}

#[derive(Default)]
struct Tracker {
    last: Option<Pen>,
    barrel: bool,
    barrel2: bool,
    /// Hardware id and serial of the pen tool last seen.
    pen_tool: Option<(u64, u64)>,
}

/// Tells the main window's page which pointer events come from a pen.
pub fn route_to_page<R: Runtime>(app: &tauri::App<R>) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let target = window.clone();
    if let Err(e) = window.with_webview(move |platform| install(&platform.inner(), target)) {
        eprintln!("pen: could not reach the webview ({e}); a pen draws like a mouse");
    }
}

fn install<V: IsA<gtk::Widget>, R: Runtime>(view: &V, window: WebviewWindow<R>) {
    let state = RefCell::new(Tracker::default());
    view.connect_event(move |_, event| {
        let pressed = match event.event_type() {
            EventType::MotionNotify => None,
            EventType::ButtonPress
            | EventType::DoubleButtonPress
            | EventType::TripleButtonPress => Some(true),
            EventType::ButtonRelease => Some(false),
            _ => return Propagation::Proceed,
        };
        let Some(source) = event.source_device().map(|d| d.source()) else {
            return Propagation::Proceed;
        };
        let mut s = state.borrow_mut();
        let now = match source {
            InputSource::Pen | InputSource::Eraser => {
                // GTK on Wayland leaves held barrel buttons out of the
                // event's button state (only the tip sets one), so WebKit's
                // `buttons` lose a barrel held while drawing; follow their own
                // presses. GDK numbers BTN_STYLUS 2 and BTN_STYLUS2 3.
                match (pressed, event.button()) {
                    (Some(down), Some(2)) => s.barrel = down,
                    (Some(down), Some(3)) => s.barrel2 = down,
                    _ => {}
                }
                let eraser = source == InputSource::Eraser;
                let eraser_button = if eraser {
                    s.eraser_button(event)
                } else {
                    s.saw_pen(event);
                    None
                };
                Some(Pen {
                    eraser,
                    eraser_button,
                    barrel: s.barrel,
                    barrel2: s.barrel2,
                    pressure: event.axis(AxisUse::Pressure).filter(|p| p.is_finite()),
                })
            }
            // WebKit reports a touch screen as touch already, and a palm on
            // the screen must not end the pen.
            InputSource::Touchscreen => return Propagation::Proceed,
            // Barrels stay as they are: the tablet protocol releases held
            // buttons before the pen leaves, and the compositor may move the
            // pointer while the pen is near.
            _ => None,
        };
        if now != s.last {
            s.last = now;
            send(&window, now);
        }
        Propagation::Proceed
    });
}

/// The tool's hardware id and serial, unless GTK knows neither.
fn tool_ids(event: &gdk::Event) -> Option<(u64, u64)> {
    let tool = event.device_tool()?;
    let ids = (tool.hardware_id(), tool.serial());
    (ids != (0, 0)).then_some(ids)
}

impl Tracker {
    /// Remembers the pen tool, for `eraser_button`.
    fn saw_pen(&mut self, event: &gdk::Event) {
        if let Some(ids) = tool_ids(event) {
            self.pen_tool = Some(ids);
        }
    }

    /// Whether this eraser tool is the pen's eraser button: the same physical
    /// tool as the pen, switched while the button is held.
    fn eraser_button(&self, event: &gdk::Event) -> Option<bool> {
        Some(tool_ids(event)? == self.pen_tool?)
    }
}

fn send<R: Runtime>(window: &WebviewWindow<R>, pen: Option<Pen>) {
    let detail = match pen {
        Some(Pen {
            eraser,
            eraser_button,
            barrel,
            barrel2,
            pressure,
        }) => {
            let eraser_button = eraser_button.map_or_else(|| "null".to_owned(), |b| b.to_string());
            let pressure = pressure.map_or_else(|| "null".to_owned(), |p| p.to_string());
            format!(
                "{{pen:true,eraser:{eraser},eraserButton:{eraser_button},barrel:{barrel},barrel2:{barrel2},pressure:{pressure}}}"
            )
        }
        None => "{pen:false}".to_owned(),
    };
    let _ = window.eval(format!(
        "window.dispatchEvent(new CustomEvent('app-pen',{{detail:{detail}}}))"
    ));
}
