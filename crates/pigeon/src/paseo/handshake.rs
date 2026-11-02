//! The first frames of a socket (spec 7.1.3, 7.2, 7.3): `ping` answered at once (before `hello` too), `hello`
//! checked against the accepted protocol versions, session messages before `hello` ignored, the rest dispatched.

use std::sync::Arc;

use serde_json::Value;

use super::dispatch;
use super::messages::{self, PONG, PROTOCOLS, hello_rejected, server_info, str_field};
use super::transport::Conn;
use crate::App;
use crate::feeds::Hub;

/// A frame that keeps a socket alive without asking anything (`ping`, `client_heartbeat`): it does not make the
/// socket busy for the drain's request-free rule.
#[must_use]
pub fn is_heartbeat(frame: &Value) -> bool {
    match str_field(frame, "type") {
        Some("ping") => true,
        Some("session") => frame
            .get("message")
            .and_then(|m| str_field(m, "type"))
            .is_some_and(|t| t == "client_heartbeat"),
        _ => false,
    }
}

/// Handles one inbound frame. `hold` is held until the frame's handler ends (large messages, 7.5.4; transfers, 11.2).
pub fn frame(app: &App, hub: &Hub, conn: &Arc<Conn>, frame: &Value, hold: crate::paseo::transport::Hold) {
    match str_field(frame, "type") {
        Some("ping") => {
            conn.send("pong", PONG.to_owned());
        }
        Some("hello") => hello(app, conn, frame),
        Some("session") => {
            // session frames before hello are ignored (A2.F3)
            if !conn.hello() {
                return;
            }
            let Some(message) = frame.get("message").filter(|m| m.is_object()).cloned() else {
                return;
            };
            dispatch::message(app, hub, conn, message, hold);
        }
        _ => {}
    }
}

fn hello(app: &App, conn: &Arc<Conn>, frame: &Value) {
    let protocol = frame.get("protocolVersion").and_then(Value::as_u64);
    let client_type = str_field(frame, "clientType").unwrap_or("").to_owned();
    let app_version = str_field(frame, "appVersion").unwrap_or("").to_owned();
    let label = protocol.map_or_else(|| "none".to_owned(), |p| p.to_string());
    app.0.metrics.hello(&label, &client_type);
    let Some(protocol) = protocol.filter(|p| PROTOCOLS.contains(p)) else {
        tracing::info!(conn = %conn.id, protocol = %label, client_type, app_version, "hello rejected");
        conn.send("hello.rejected", hello_rejected());
        conn.close_after_queued(1002, "incompatible protocol");
        return;
    };
    tracing::info!(conn = %conn.id, protocol, client_type, app_version, "hello");
    conn.set_hello(protocol, &client_type, &app_version);
    let product = app.identities().product_name();
    let version = if app_version.is_empty() {
        None
    } else {
        Some(app_version.as_str())
    };
    conn.reply("status", &server_info(&product, version, protocol));
    let _ = messages::DEFAULT_VERSION;
}
