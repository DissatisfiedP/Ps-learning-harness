//! A touchpad pinch belongs to the page, not to WebKitGTK (Linux only;
//! docs/ui-design.md §7.6, "Zoom belongs to the map").
//!
//! WebKitGTK turns a touchpad pinch into a magnification of the whole page
//! (a bubble-phase `GtkGestureZoom` on the view) before the page sees any
//! event, and there is no setting or DOM event to stop it: pinching the Map
//! zoomed the entire app. GTK 3 emits a widget's `event` signal before its
//! bubble-phase gestures run and skips them when a handler stops the event,
//! so this handler takes every `GDK_TOUCHPAD_PINCH` the view receives and
//! forwards it to the page instead, as `app-pinch` window events
//! `{ phase: "begin" | "update" | "end", scale, x, y, dx, dy }` (`scale`
//! relative to the start, `x`/`y` the pointer in CSS px, `dx`/`dy` how far
//! the fingers' centre moved since the last event: libinput keeps a gesture a
//! pinch until the fingers lift, so the scroll that follows a pinch without
//! lifting arrives here and not as a wheel). The Map, the plan diagram and the
//! scratch pad zoom and pan themselves when a pinch begins over them;
//! anywhere else a pinch does nothing, like ctrl+wheel and the zoom keys
//! (Tauri's `zoomHotkeysEnabled` is off). Touch-screen pinches are touch
//! events, not touchpad ones: the page gets them as pointers, and those
//! surfaces take them from WebKit themselves (`ownTouches`, ui/zoomguard.js).

use gtk::gdk;
use gtk::glib::Propagation;
use gtk::prelude::*;
use tauri::{Manager, Runtime, WebviewWindow};

/// Takes the touchpad pinch over on the main window's webview.
pub fn route_to_page<R: Runtime>(app: &tauri::App<R>) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let target = window.clone();
    if let Err(e) = window.with_webview(move |platform| install(&platform.inner(), target)) {
        eprintln!("pinch: could not reach the webview ({e}); a pinch zooms the page");
    }
}

fn install<V: IsA<gtk::Widget>, R: Runtime>(view: &V, window: WebviewWindow<R>) {
    view.connect_event(move |_, event| {
        let Some(pinch) = event.downcast_ref::<gdk::EventTouchpadPinch>() else {
            return Propagation::Proceed;
        };
        // gdk 0.18's `is_phase()` folds the phase into a bool; read the field.
        let raw: &gdk::ffi::GdkEventTouchpadPinch = pinch.as_ref();
        let phase = match i32::from(raw.phase) {
            gdk::ffi::GDK_TOUCHPAD_GESTURE_PHASE_BEGIN => "begin",
            gdk::ffi::GDK_TOUCHPAD_GESTURE_PHASE_UPDATE => "update",
            _ => "end", // end or cancel
        };
        let (x, y) = pinch.position();
        let (dx, dy) = pinch.delta();
        send(&window, phase, pinch.scale(), [x, y, dx, dy]);
        Propagation::Stop
    });
}

fn send<R: Runtime>(window: &WebviewWindow<R>, phase: &str, scale: f64, at: [f64; 4]) {
    if !(scale.is_finite() && at.iter().all(|v| v.is_finite())) {
        return;
    }
    let [x, y, dx, dy] = at;
    let _ = window.eval(format!(
        "window.dispatchEvent(new CustomEvent('app-pinch',{{detail:{{phase:'{phase}',scale:{scale},x:{x},y:{y},dx:{dx},dy:{dy}}}}}))"
    ));
}
