// SPDX-License-Identifier: Apache-2.0

//! Behaviour tests for `qsl_telemetry::time_op!`.
//!
//! Each test installs a custom `tracing-subscriber` `Layer` that
//! captures every event into a `Vec<CapturedEvent>` and asserts on
//! level, target, and the `elapsed_ms` field. Subscribers are scoped
//! to the test thread via `set_default`'s drop guard.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::Registry;

#[derive(Debug, Clone)]
struct CapturedEvent {
    target: String,
    level: Level,
    fields: Vec<(String, String)>,
}

#[derive(Default, Clone)]
struct Capture {
    events: Arc<Mutex<Vec<CapturedEvent>>>,
}

impl Capture {
    fn snapshot(&self) -> Vec<CapturedEvent> {
        self.events.lock().unwrap().clone()
    }
}

struct Recorder<'a>(&'a mut Vec<(String, String)>);

impl<'a> Visit for Recorder<'a> {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.push((field.name().to_string(), value.to_string()));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.push((field.name().to_string(), value.to_string()));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.push((field.name().to_string(), value.to_string()));
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.push((field.name().to_string(), value.to_string()));
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0
            .push((field.name().to_string(), format!("{value:?}")));
    }
}

impl<S: Subscriber> Layer<S> for Capture {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut fields = Vec::new();
        event.record(&mut Recorder(&mut fields));
        let metadata = event.metadata();
        self.events.lock().unwrap().push(CapturedEvent {
            target: metadata.target().to_string(),
            level: *metadata.level(),
            fields,
        });
    }
}

fn install_capture() -> (tracing::subscriber::DefaultGuard, Capture) {
    let cap = Capture::default();
    let subscriber = Registry::default().with(cap.clone());
    let guard = tracing::subscriber::set_default(subscriber);
    (guard, cap)
}

fn field<'a>(ev: &'a CapturedEvent, name: &str) -> Option<&'a str> {
    ev.fields
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

#[tokio::test]
async fn warns_when_body_exceeds_limit() {
    let (_g, cap) = install_capture();
    let _: () = qsl_telemetry::time_op!(
        target: "qsl::slow::test",
        limit_ms: 5_u64,
        op: "sleepy",
        fields: { account = "acme", folder = "INBOX" },
        async {
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    );
    let events = cap.snapshot();
    assert_eq!(events.len(), 1, "expected one event, got {events:?}");
    let e = &events[0];
    assert_eq!(e.target, "qsl::slow::test");
    assert_eq!(e.level, Level::WARN);
    assert_eq!(field(e, "op"), Some("sleepy"));
    let elapsed: u64 = field(e, "elapsed_ms")
        .expect("elapsed_ms field")
        .parse()
        .expect("elapsed_ms parses as u64");
    assert!(elapsed >= 30, "elapsed_ms = {elapsed}");
    assert_eq!(field(e, "limit_ms"), Some("5"));
    assert_eq!(field(e, "account"), Some("acme"));
    assert_eq!(field(e, "folder"), Some("INBOX"));
}

#[tokio::test]
async fn debugs_when_body_within_limit() {
    let (_g, cap) = install_capture();
    let _: () = qsl_telemetry::time_op!(
        target: "qsl::slow::test",
        limit_ms: 1_000_u64,
        op: "quick",
        fields: { count = 7_u64 },
        async {}
    );
    let events = cap.snapshot();
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert_eq!(e.level, Level::DEBUG);
    assert_eq!(field(e, "op"), Some("quick"));
    assert_eq!(field(e, "count"), Some("7"));
    assert!(field(e, "elapsed_ms").is_some());
    assert_eq!(field(e, "limit_ms"), None, "debug arm omits limit_ms");
}

#[tokio::test]
async fn passes_through_ok_value() {
    let (_g, _cap) = install_capture();
    let r: Result<u32, &'static str> = qsl_telemetry::time_op!(
        target: "qsl::slow::test",
        limit_ms: 1_000_u64,
        op: "ret_ok",
        async { Ok::<_, &'static str>(42_u32) },
    );
    assert_eq!(r, Ok(42));
}

#[tokio::test]
async fn passes_through_err_value() {
    let (_g, cap) = install_capture();
    let r: Result<u32, &'static str> = qsl_telemetry::time_op!(
        target: "qsl::slow::test",
        limit_ms: 1_000_u64,
        op: "ret_err",
        async { Err::<u32, _>("boom") },
    );
    assert_eq!(r, Err("boom"));
    // Even on Err the timing event still fires.
    let events = cap.snapshot();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].level, Level::DEBUG);
}

/// Regression: the macro must NOT capture the body's return value as a
/// log field. If a future refactor accidentally splices the body's
/// return into the warn/debug call, a function that returns a bearer
/// token or refresh token would leak it on every slow log.
#[tokio::test]
async fn time_op_does_not_log_body_return_value() {
    let (_g, cap) = install_capture();
    // A value that would be unmistakable in a leak: includes both a
    // `Bearer` prefix and a JWT-shaped middle segment.
    let secret = "Bearer eyJhbGciOiJIUzI1NiJ9.payloadbits.signature";
    let _: String = qsl_telemetry::time_op!(
        target: "qsl::slow::test",
        limit_ms: 1_u64,
        op: "secret_returner",
        async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            secret.to_string()
        }
    );
    let events = cap.snapshot();
    assert_eq!(events.len(), 1, "expected one (warn) event");
    for e in &events {
        for (k, v) in &e.fields {
            assert!(!v.contains("Bearer"), "field {k} leaked Bearer: {v}");
            assert!(!v.contains("eyJ"), "field {k} leaked JWT shape: {v}");
            assert!(
                !v.contains("payloadbits"),
                "field {k} leaked secret middle: {v}"
            );
        }
    }
}

#[tokio::test]
async fn no_fields_compiles_and_works() {
    let (_g, cap) = install_capture();
    let _: () = qsl_telemetry::time_op!(
        target: "qsl::slow::test",
        limit_ms: 1_000_u64,
        op: "bare",
        async {},
    );
    let events = cap.snapshot();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].level, Level::DEBUG);
}
