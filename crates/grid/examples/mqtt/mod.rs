//! Live feeds over MQTT on a WebSocket, as the `ships` example (Digitraffic's
//! AIS) and the `transit` example (Helsinki's HSL HFP) read them, with no
//! server of ours between:
//!
//! - `client`: MQTT 3.1.1 as a subscriber needs it, without IO;
//! - `socket`: the transport's seam ([`Socket`], [`Opener`]) and the
//!   reconnect [`Backoff`];
//! - `session`: one subscription kept up over an [`Opener`]: connect,
//!   subscribe, keep alive, notice silence, come back after the backoff;
//! - `native`: TLS with rustls and the WebSocket with tungstenite, and one
//!   HTTPS GET by hand; native only;
//! - `browser`: the page's WebSocket; in the browser only.
//!
//! The folder has no `main.rs`, so cargo does not take it for an example.
//! An example includes it from its `app.rs`, which the native `main.rs`
//! and the browser build (`crates/grid-web/apps/<app>`) both include:
//!
//! ```ignore
//! #[path = "../mqtt/mod.rs"]
//! pub mod mqtt;
//! ```
//!
//! Each example uses part of it; the rest is dead code there.
#![allow(dead_code)]

#[cfg(target_family = "wasm")]
pub mod browser;
pub mod client;
#[cfg(not(target_family = "wasm"))]
pub mod native;
pub mod session;
pub mod socket;
