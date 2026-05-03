// SPDX-License-Identifier: Apache-2.0

//! Bridge `tracing::*!` events from the wasm bundle to the Tauri host
//! so they land on the same stderr stream as `qsl-desktop`'s own logs.
//!
//! Why not `tracing-wasm`? The wasm bundle's logs go to the webview's
//! DevTools console, which is fine for browser debugging but means
//! operators have to open the inspector to see what the UI is doing.
//! Routing them back through a Tauri command lets `RUST_LOG=info`
//! show the full picture — host + UI — in one place.
//!
//! Shape: a custom `Layer` formats each event as a single string
//! (message + space-separated `key=value` fields), then fires a
//! `wasm_bindgen_futures::spawn_local` invoke of the `ui_log`
//! command. Failures inside the bridge surface via
//! `web_sys::console::error_1` directly — never via `tracing::*!`,
//! to avoid an infinite re-entry loop.
//!
//! Capped at INFO at the subscriber level. Debug-level UI events
//! would chatter too much over the IPC bridge to be useful, and we
//! already have the watchdog at the host for the perf-sensitive
//! paths.

use std::fmt::Write as _;

use serde::Serialize;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::Registry;
use wasm_bindgen::JsValue;

use crate::app::invoke;

/// Install the bridge as the global tracing subscriber for the wasm
/// bundle. Subsequent `tracing::*!` calls in any `qsl-ui` module
/// route through `ui_log` to the host. Idempotent at the
/// `set_global_default` boundary — the second call returns an error,
/// which we swallow because hot-reloaded test harnesses may try.
pub(crate) fn install() {
    let subscriber = Registry::default().with(HostBridgeLayer);
    let _ = tracing::subscriber::set_global_default(subscriber);
}

struct HostBridgeLayer;

impl<S: Subscriber> Layer<S> for HostBridgeLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        // Static cap: anything quieter than INFO is dropped at the
        // bridge so debug spam never crosses the IPC wire.
        if *metadata.level() > Level::INFO {
            return;
        }

        let level = level_to_str(*metadata.level());
        let target = metadata.target().to_string();

        let mut visitor = FieldCollector::default();
        event.record(&mut visitor);
        let message = visitor.into_string();

        let payload = LogPayload {
            level,
            target,
            message,
        };

        wasm_bindgen_futures::spawn_local(async move {
            let res = invoke::<()>("ui_log", serde_json::json!({ "input": payload })).await;
            if let Err(e) = res {
                // Avoid `tracing::*` here — that would re-enter this
                // layer and recurse forever on a host that's actively
                // dropping invokes.
                web_sys::console::error_1(&JsValue::from_str(&format!(
                    "[qsl-ui] host log bridge invoke failed: {e}"
                )));
            }
        });
    }
}

#[derive(Serialize)]
struct LogPayload {
    level: &'static str,
    target: String,
    message: String,
}

fn level_to_str(level: Level) -> &'static str {
    match level {
        Level::ERROR => "error",
        Level::WARN => "warn",
        Level::INFO => "info",
        Level::DEBUG => "debug",
        Level::TRACE => "trace",
    }
}

#[derive(Default)]
struct FieldCollector {
    message: String,
    fields: Vec<(String, String)>,
}

impl FieldCollector {
    fn into_string(mut self) -> String {
        if self.fields.is_empty() {
            return self.message;
        }
        if !self.message.is_empty() {
            self.message.push(' ');
        }
        for (i, (k, v)) in self.fields.iter().enumerate() {
            if i > 0 {
                self.message.push(' ');
            }
            // Best-effort write; format is `k=v`, no escaping for
            // values that contain spaces — operators reading stderr
            // can tell from the leading `[qsl_ui ...]` target what
            // they're looking at.
            let _ = write!(&mut self.message, "{k}={v}");
        }
        self.message
    }
}

impl Visit for FieldCollector {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields
                .push((field.name().to_string(), value.to_string()));
        }
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields
            .push((field.name().to_string(), value.to_string()));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields
            .push((field.name().to_string(), value.to_string()));
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields
            .push((field.name().to_string(), value.to_string()));
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.fields
            .push((field.name().to_string(), value.to_string()));
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let s = format!("{value:?}");
        if field.name() == "message" {
            self.message = s;
        } else {
            self.fields.push((field.name().to_string(), s));
        }
    }
}
