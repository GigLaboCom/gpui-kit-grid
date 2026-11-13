//! Wikimedia's stream in the browser: an `EventSource`, which the page
//! opens only when the visitor presses Start, one at a time.
//!
//! The callbacks only queue events; one task on the page's main thread
//! hands each event's data to the inbox, which parses it. When the
//! connection ends, the browser retries on its own with `Last-Event-ID`
//! once, after a connection that stayed up (the server closes every
//! connection after 15 minutes); otherwise the task closes it, waits as
//! the shared [`Backoff`] says (exponential, with jitter) and opens a new
//! one from the last event's time (`?since=`), since a new `EventSource`
//! cannot send `Last-Event-ID`. A minute without an event counts as a lost
//! connection; after too many failures in a row the task gives up and the
//! view falls back to the recording.

use std::{any::Any, sync::Arc, time::Duration};

use gpui_kit::{App, AsyncApp};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};
use web_sys::{EventSource, MessageEvent};
// The executor's clock, `web_time` in the browser.
use web_time::Instant;

use crate::wiki::{
    StreamTransport,
    source::{Backoff, Inbox, STABLE, STREAM_URL, resume_time_ms},
};

/// How often the task checks the connection.
const TICK: Duration = Duration::from_secs(1);
/// No event for this long on an open stream: it is lost.
const SILENCE: Duration = Duration::from_secs(60);
/// No answer for this long: the attempt failed.
const OPEN_TIMEOUT: Duration = Duration::from_secs(20);

/// `EventSource.readyState` while the browser reconnects.
const CONNECTING: u16 = 0;

/// The stream over the browser's `EventSource`.
pub struct BrowserStream;

impl StreamTransport for BrowserStream {
    fn open(&self, inbox: Arc<Inbox>, cx: &mut App) -> Box<dyn Any> {
        Box::new(cx.spawn(async move |cx| read(inbox, cx).await))
    }
}

/// What the callbacks queue for the task.
enum Event {
    Open,
    Message {
        data: String,
        id: String,
    },
    /// The browser reports an error, and whether it is reconnecting.
    Error {
        retrying: bool,
    },
    Tick,
}

/// An `EventSource` with its callbacks; dropping it closes the stream.
struct Source {
    source: EventSource,
    _callbacks: [Closure<dyn FnMut(JsValue)>; 3],
}

impl Source {
    fn open(url: &str, events: &async_channel::Sender<Event>) -> Result<Self, JsValue> {
        let source = EventSource::new(url)?;
        let queue = |make: Box<dyn Fn(JsValue) -> Event>| {
            let events = events.clone();
            Closure::<dyn FnMut(JsValue)>::new(move |value: JsValue| {
                let _ = events.try_send(make(value));
            })
        };
        let state = source.clone();
        let callbacks = [
            queue(Box::new(|_| Event::Open)),
            queue(Box::new(|event| {
                let event = event.dyn_into::<MessageEvent>().ok();
                Event::Message {
                    data: event
                        .as_ref()
                        .and_then(|event| event.data().as_string())
                        .unwrap_or_default(),
                    id: event.map(|event| event.last_event_id()).unwrap_or_default(),
                }
            })),
            queue(Box::new(move |_| Event::Error {
                retrying: state.ready_state() == CONNECTING,
            })),
        ];
        source.set_onopen(Some(callbacks[0].as_ref().unchecked_ref()));
        source.set_onmessage(Some(callbacks[1].as_ref().unchecked_ref()));
        source.set_onerror(Some(callbacks[2].as_ref().unchecked_ref()));
        Ok(Self {
            source,
            _callbacks: callbacks,
        })
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        self.source.set_onopen(None);
        self.source.set_onmessage(None);
        self.source.set_onerror(None);
        self.source.close();
    }
}

/// The address to (re)connect to: from the last event's time, if the
/// inbox still resumes, else from now.
fn stream_url(inbox: &Inbox) -> String {
    let since = inbox
        .resume_id()
        .and_then(|id| resume_time_ms(&id))
        .map(|ms| js_sys::Date::new(&JsValue::from_f64(ms as f64)).to_iso_string());
    match since {
        Some(since) => format!(
            "{STREAM_URL}?since={}",
            js_sys::encode_uri_component(&String::from(since))
        ),
        None => STREAM_URL.into(),
    }
}

/// Connects, reads and reconnects until the task is dropped, or until the
/// backoff gives up.
async fn read(inbox: Arc<Inbox>, cx: &mut AsyncApp) {
    let executor = cx.background_executor().clone();
    let mut backoff = Backoff::new();
    let mut rng = fastrand::Rng::with_seed(js_sys::Date::now() as u64);
    loop {
        inbox.connecting();
        let (events, received) = async_channel::unbounded();
        let ticker = cx.spawn({
            let (events, executor) = (events.clone(), executor.clone());
            async move |_| {
                loop {
                    executor.timer(TICK).await;
                    if events.send(Event::Tick).await.is_err() {
                        return;
                    }
                }
            }
        });
        let started = executor.now();
        let mut opened: Option<Instant> = None;
        let mut live_for = Duration::ZERO;
        let mut browser_retried = false;
        let reason = match Source::open(&stream_url(&inbox), &events) {
            Ok(source) => {
                drop(events);
                let mut heard = started;
                let reason = loop {
                    let Ok(event) = received.recv().await else {
                        break "the page stopped the stream".to_owned();
                    };
                    let now = executor.now();
                    match event {
                        Event::Open => {
                            inbox.opened();
                            opened = Some(now);
                            heard = now;
                            browser_retried = false;
                        }
                        Event::Message { data, id } => {
                            heard = now;
                            inbox.receive(&data, &id);
                        }
                        Event::Error { retrying } => {
                            if let Some(at) = opened.take() {
                                live_for = live_for.max(now.saturating_duration_since(at));
                            }
                            // The server's close of a good connection: the
                            // browser is back at once, with Last-Event-ID.
                            if retrying && !browser_retried && live_for >= STABLE {
                                browser_retried = true;
                                heard = now;
                                inbox.connecting();
                                continue;
                            }
                            break "the connection failed".to_owned();
                        }
                        Event::Tick => {
                            let quiet = now.saturating_duration_since(heard);
                            if opened.is_some() && quiet > SILENCE {
                                break "no event for a minute".to_owned();
                            }
                            if opened.is_none() && quiet > OPEN_TIMEOUT {
                                break "no answer".to_owned();
                            }
                        }
                    }
                };
                drop(source);
                reason
            }
            Err(error) => format!("{error:?}"),
        };
        drop(ticker);
        if let Some(at) = opened {
            live_for = live_for.max(executor.now().saturating_duration_since(at));
        }
        backoff.was_live_for(live_for);
        let Some(wait) = backoff.failed(rng.f64()) else {
            inbox.unreachable(reason);
            return;
        };
        inbox.waiting(backoff.attempt(), wait, reason);
        executor.timer(wait).await;
    }
}
