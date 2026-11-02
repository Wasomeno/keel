//! Request dispatch. `hello` is always local. Other methods go to the engine host
//! when the binary has installed one.

use keel_proto::{RpcErrorBody, RpcErrorCode, RpcFailure, RpcRequest};
use serde::Serialize;

use crate::PROTO_VERSION;
use crate::boot::Identity;
use crate::host::{EngineHost, Outbound};

#[derive(Clone, Copy)]
pub enum Transport {
    Duplex,
    Socket,
}

impl Transport {
    pub fn name(self) -> &'static str {
        match self {
            Transport::Duplex => "duplex",
            Transport::Socket => "socket",
        }
    }
}

pub enum Step {
    Reply(String),
    ReplyClose(String),
    Close,
}

#[derive(Serialize)]
struct Success<'a, T: Serialize> {
    id: &'a str,
    result: T,
}

#[derive(Serialize)]
struct HelloBody<'a> {
    device_id: &'a str,
    scope: &'a str,
    proto: u32,
    /// This engine's build stamp (`crate::build`).
    build: u64,
}

#[derive(Serialize)]
struct StatusBody<'a> {
    scope: &'a str,
    lock: bool,
    live_runs: u64,
    last_watchdog: Option<&'a str>,
}

#[derive(serde::Deserialize)]
struct HelloParams {
    token: String,
    proto: u32,
}

pub fn step(
    text: &str,
    hello_done: &mut bool,
    transport: Transport,
    identity: &Identity,
    token: &str,
    host: Option<&dyn EngineHost>,
    outbound: &Outbound,
) -> Step {
    if text.len() > crate::MAX_FRAME_BYTES {
        return Step::Close;
    }
    let request: RpcRequest = match serde_json::from_str(text) {
        Ok(request) => request,
        Err(_) => return Step::Close,
    };
    if request.method == "hello" {
        return hello(&request, hello_done, transport, identity, token);
    }
    if !*hello_done {
        tracing::info!(target: "keel_rpc", transport = transport.name(), method = %request.method, "hello rejected: request before hello");
        return Step::ReplyClose(failure(&request.id, RpcErrorCode::BadToken, "bad token"));
    }
    if request.method == "engine_status" {
        let (live_runs, watchdog) = match host {
            Some(host) => (host.live_runs(), host.last_watchdog()),
            None => (0, None),
        };
        let body = Success {
            id: &request.id,
            result: StatusBody {
                scope: &identity.scope,
                lock: true,
                live_runs,
                last_watchdog: watchdog.as_deref(),
            },
        };
        return match serde_json::to_string(&body) {
            Ok(json) => Step::Reply(json),
            Err(_) => Step::Close,
        };
    }
    if let Some(host) = host
        && let Some(json) = host.handle(&request, outbound)
    {
        return Step::Reply(json);
    }
    Step::Reply(failure(
        &request.id,
        RpcErrorCode::UnknownMethod,
        "no such method",
    ))
}

fn hello(
    request: &RpcRequest,
    hello_done: &mut bool,
    transport: Transport,
    identity: &Identity,
    token: &str,
) -> Step {
    let params: HelloParams = match serde_json::from_value(request.params.clone()) {
        Ok(params) => params,
        Err(_) => {
            tracing::info!(target: "keel_rpc", transport = transport.name(), "hello rejected: bad params");
            return Step::Close;
        }
    };
    if params.proto != PROTO_VERSION {
        tracing::info!(target: "keel_rpc", transport = transport.name(), proto = params.proto, "hello rejected: proto mismatch");
        return Step::Close;
    }
    if !token_ok(&params.token, token, transport) {
        // Never log the token itself.
        tracing::info!(target: "keel_rpc", transport = transport.name(), "hello rejected: bad token");
        return Step::ReplyClose(failure(&request.id, RpcErrorCode::BadToken, "bad token"));
    }
    *hello_done = true;
    let body = Success {
        id: &request.id,
        result: HelloBody {
            device_id: &identity.device_id,
            scope: &identity.scope,
            proto: PROTO_VERSION,
            build: crate::build(),
        },
    };
    match serde_json::to_string(&body) {
        Ok(json) => Step::Reply(json),
        Err(_) => Step::Close,
    }
}

fn token_ok(presented: &str, expected: &str, transport: Transport) -> bool {
    if presented.is_empty() {
        return matches!(transport, Transport::Duplex);
    }
    if presented.len() != expected.len() {
        return false;
    }
    let mut diff = 0u8;
    for (left, right) in presented.bytes().zip(expected.bytes()) {
        diff |= left ^ right;
    }
    diff == 0
}

pub(crate) fn failure(id: &str, code: RpcErrorCode, message: &str) -> String {
    let body = RpcFailure {
        id: id.to_string(),
        error: RpcErrorBody {
            code,
            message: message.to_string(),
        },
    };
    serde_json::to_string(&body).unwrap_or_default()
}

#[derive(Serialize)]
struct HelloParamsBody<'a> {
    token: &'a str,
    proto: u32,
}

#[derive(Serialize)]
struct HelloRequest<'a> {
    id: &'a str,
    method: &'static str,
    params: HelloParamsBody<'a>,
}

#[derive(Serialize)]
struct StatusRequest<'a> {
    id: &'a str,
    method: &'static str,
    params: EmptyParams,
}

#[derive(Serialize)]
struct EmptyParams {}

pub fn hello_request(id: &str, token: &str) -> String {
    serde_json::to_string(&HelloRequest {
        id,
        method: "hello",
        params: HelloParamsBody {
            token,
            proto: PROTO_VERSION,
        },
    })
    .unwrap_or_default()
}

pub fn engine_status_request(id: &str) -> String {
    serde_json::to_string(&StatusRequest {
        id,
        method: "engine_status",
        params: EmptyParams {},
    })
    .unwrap_or_default()
}
