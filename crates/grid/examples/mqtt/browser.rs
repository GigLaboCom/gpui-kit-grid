//! The browser's transport: a `web_sys::WebSocket`, whose callbacks only
//! queue events that the session takes on its next poll, on the page's one
//! thread. A browser sends no headers of our choosing on a WebSocket, so
//! the MQTT client id says who connects. In the browser only: the web
//! apps that include this module (`crates/grid-web/apps/{ships,transit}`)
//! depend on `wasm-bindgen`, `js-sys` and `web-sys`.

use std::{cell::RefCell, rc::Rc};

use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};
use web_sys::{BinaryType, CloseEvent, MessageEvent, WebSocket};

use super::socket::{Opener, Socket, SocketEvent};

/// Opens browser WebSockets.
#[derive(Default)]
pub struct BrowserOpener;

impl Opener for BrowserOpener {
    fn open(&mut self, url: &str, protocol: &str) -> Result<Box<dyn Socket>, String> {
        BrowserSocket::open(url, protocol)
            .map(|socket| Box::new(socket) as Box<dyn Socket>)
            .map_err(|error| format!("{error:?}"))
    }
}

/// A browser WebSocket with its callbacks; dropping it closes the socket.
struct BrowserSocket {
    socket: WebSocket,
    queue: Rc<RefCell<Vec<SocketEvent>>>,
    _callbacks: [Closure<dyn FnMut(JsValue)>; 4],
}

impl BrowserSocket {
    fn open(url: &str, protocol: &str) -> Result<Self, JsValue> {
        let socket = WebSocket::new_with_str(url, protocol)?;
        socket.set_binary_type(BinaryType::Arraybuffer);
        let queue: Rc<RefCell<Vec<SocketEvent>>> = Rc::default();
        let callback = |make: fn(JsValue) -> SocketEvent| {
            let queue = queue.clone();
            Closure::<dyn FnMut(JsValue)>::new(move |value: JsValue| {
                queue.borrow_mut().push(make(value));
            })
        };
        let callbacks = [
            callback(|_| SocketEvent::Open),
            callback(|event| {
                // A text message is not MQTT: it arrives empty, which the
                // client's decoder waits on harmlessly.
                let bytes = event
                    .dyn_into::<MessageEvent>()
                    .ok()
                    .and_then(|event| event.data().dyn_into::<js_sys::ArrayBuffer>().ok())
                    .map(|buffer| js_sys::Uint8Array::new(&buffer).to_vec())
                    .unwrap_or_default();
                SocketEvent::Data(bytes)
            }),
            callback(|event| {
                let reason = event
                    .dyn_into::<CloseEvent>()
                    .map(|event| match event.reason() {
                        reason if reason.is_empty() => format!("closed ({})", event.code()),
                        reason => reason,
                    })
                    .unwrap_or_else(|_| "closed".into());
                SocketEvent::Closed(reason)
            }),
            callback(|_| SocketEvent::Closed("socket error".into())),
        ];
        socket.set_onopen(Some(callbacks[0].as_ref().unchecked_ref()));
        socket.set_onmessage(Some(callbacks[1].as_ref().unchecked_ref()));
        socket.set_onclose(Some(callbacks[2].as_ref().unchecked_ref()));
        socket.set_onerror(Some(callbacks[3].as_ref().unchecked_ref()));
        Ok(Self {
            socket,
            queue,
            _callbacks: callbacks,
        })
    }
}

impl Socket for BrowserSocket {
    fn poll(&mut self, out: &mut Vec<SocketEvent>) {
        out.append(&mut self.queue.borrow_mut());
    }

    fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.socket
            .send_with_u8_array(bytes)
            .map_err(|error| format!("{error:?}"))
    }
}

impl Drop for BrowserSocket {
    fn drop(&mut self) {
        self.socket.set_onopen(None);
        self.socket.set_onmessage(None);
        self.socket.set_onclose(None);
        self.socket.set_onerror(None);
        let _ = self.socket.close();
    }
}
