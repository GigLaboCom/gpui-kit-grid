//! Whether the visitor is watching: the page's visibility and a count of
//! presses, keys, scrolls and pointer moves, from listeners on the window
//! (in the capture phase, so the canvas's own handlers cannot hide them).
//! The view polls it and closes the live stream on a tab hidden for a
//! minute (opening it again when the tab is shown) or after minutes
//! without an interaction (`attendance.rs` of the example).

use std::{cell::Cell, rc::Rc};

use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};

use crate::wiki::Presence;

/// The interactions that count.
const INTERACTIONS: [&str; 5] = [
    "pointerdown",
    "pointermove",
    "keydown",
    "wheel",
    "touchstart",
];

pub struct PagePresence {
    hidden: Rc<Cell<bool>>,
    interactions: Rc<Cell<u64>>,
    _listeners: Vec<Closure<dyn FnMut(JsValue)>>,
}

impl PagePresence {
    /// Listens to the page; listens to nothing outside a browser window.
    pub fn listen() -> Self {
        let hidden = Rc::new(Cell::new(false));
        let interactions = Rc::new(Cell::new(0u64));
        let mut listeners = Vec::new();
        if let Some(window) = web_sys::window() {
            if let Some(document) = window.document() {
                hidden.set(document.hidden());
                let listener = Closure::<dyn FnMut(JsValue)>::new({
                    let (hidden, document) = (hidden.clone(), document.clone());
                    move |_| hidden.set(document.hidden())
                });
                let _ = document.add_event_listener_with_callback(
                    "visibilitychange",
                    listener.as_ref().unchecked_ref(),
                );
                listeners.push(listener);
            }
            let listener = Closure::<dyn FnMut(JsValue)>::new({
                let interactions = interactions.clone();
                move |_| interactions.set(interactions.get().wrapping_add(1))
            });
            for kind in INTERACTIONS {
                let _ = window.add_event_listener_with_callback_and_bool(
                    kind,
                    listener.as_ref().unchecked_ref(),
                    true,
                );
            }
            listeners.push(listener);
        }
        Self {
            hidden,
            interactions,
            _listeners: listeners,
        }
    }
}

impl Presence for PagePresence {
    fn is_hidden(&self) -> bool {
        self.hidden.get()
    }

    fn interactions(&self) -> u64 {
        self.interactions.get()
    }
}
