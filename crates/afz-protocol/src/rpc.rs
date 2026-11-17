//! The autofuzz method vocabulary, carried over the pluginrpc wire mechanism.
//!
//! Framing, the envelopes, request ids, the error codes, and loss come from [`plugin_rpc`], the
//! mechanism this engine shares with any other host that speaks `prpc/1`
//! (<https://github.com/DustinEwan/pluginrpc>). What lives here is the profile's half: the eight
//! methods a harness plugin may answer. A plugin author writing a Rust harness compiles against
//! this module and still gets the framing for free, exactly as before; only the mechanism's
//! source moved.
//!
//! The mechanism names no method, so `Method` is what [`MethodName`] is implemented for here, and
//! `plugin_rpc` carries only the wire string.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use plugin_rpc::{
    buffered, buffered_writer, parse_request, parse_response, read_line, write_line, ErrorObject,
    Failure, JsonRpcVersion, MessageError, MethodName, Request, Response, Success, INVALID_REQUEST,
    METHOD_NOT_FOUND, PARSE_ERROR, PLUGIN_ERROR,
};

/// The eight methods a harness plugin may answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Method {
    Handshake,
    Discover,
    Prepare,
    Invoke,
    Minimize,
    Reproducer,
    CoverageMap,
    Ready,
}

/// The mechanism decides a `-32601` from `from_wire` and knows nothing else about these names.
impl MethodName for Method {
    fn name(&self) -> &'static str {
        match self {
            Method::Handshake => "handshake",
            Method::Discover => "discover",
            Method::Prepare => "prepare",
            Method::Invoke => "invoke",
            Method::Minimize => "minimize",
            Method::Reproducer => "reproducer",
            Method::CoverageMap => "coverageMap",
            Method::Ready => "ready",
        }
    }

    fn from_wire(name: &str) -> Option<Self> {
        match name {
            "handshake" => Some(Method::Handshake),
            "discover" => Some(Method::Discover),
            "prepare" => Some(Method::Prepare),
            "invoke" => Some(Method::Invoke),
            "minimize" => Some(Method::Minimize),
            "reproducer" => Some(Method::Reproducer),
            "coverageMap" => Some(Method::CoverageMap),
            "ready" => Some(Method::Ready),
            _ => None,
        }
    }
}
