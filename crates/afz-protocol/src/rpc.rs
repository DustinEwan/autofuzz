//! JSON-RPC 2.0 framing: one message per line on stdio, request `id` as `u64`, six methods.
//!
//! The framing helpers are shared by the engine client and by plugins written in Rust, so
//! a plugin author does not reimplement line-delimited framing.

use std::io::{BufRead, BufReader, BufWriter, Read, Write};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The six methods a harness plugin may answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Method {
    Handshake,
    Discover,
    Prepare,
    Invoke,
    Minimize,
    CoverageMap,
}

impl Method {
    pub fn name(&self) -> &'static str {
        match self {
            Method::Handshake => "handshake",
            Method::Discover => "discover",
            Method::Prepare => "prepare",
            Method::Invoke => "invoke",
            Method::Minimize => "minimize",
            Method::CoverageMap => "coverageMap",
        }
    }

    pub fn from_wire(name: &str) -> Option<Method> {
        match name {
            "handshake" => Some(Method::Handshake),
            "discover" => Some(Method::Discover),
            "prepare" => Some(Method::Prepare),
            "invoke" => Some(Method::Invoke),
            "minimize" => Some(Method::Minimize),
            "coverageMap" => Some(Method::CoverageMap),
            _ => None,
        }
    }
}

/// JSON-RPC parse error.
pub const PARSE_ERROR: i64 = -32700;
/// JSON-RPC invalid request.
pub const INVALID_REQUEST: i64 = -32600;
/// JSON-RPC method not found.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// Application-level failure inside the plugin.
pub const PLUGIN_ERROR: i64 = -32000;

/// A request. `id` is assigned by the engine and echoed on the matching response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub jsonrpc: JsonRpcVersion,
    pub id: u64,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

impl Request {
    pub fn new(id: u64, method: Method, params: impl Serialize) -> Self {
        Request {
            jsonrpc: JsonRpcVersion::V2,
            id,
            method: method.name().to_string(),
            params: Some(serde_json::to_value(params).expect("request params are serializable")),
        }
    }

    /// A method with no parameters.
    pub fn empty(id: u64, method: Method) -> Self {
        Request { jsonrpc: JsonRpcVersion::V2, id, method: method.name().to_string(), params: None }
    }
}

/// The only protocol revision this build speaks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum JsonRpcVersion {
    #[default]
    #[serde(rename = "2.0")]
    V2,
}

/// A successful response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Success {
    pub jsonrpc: JsonRpcVersion,
    pub id: u64,
    pub result: serde_json::Value,
}

/// An error response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ErrorObject {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// A failure response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Failure {
    pub jsonrpc: JsonRpcVersion,
    pub id: u64,
    pub error: ErrorObject,
}

/// A response, discriminated by the presence of `result` or `error`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Response {
    Success(Success),
    Failure(Failure),
}

impl Response {
    pub fn ok(id: u64, result: impl Serialize) -> Self {
        Response::Success(Success {
            jsonrpc: JsonRpcVersion::V2,
            id,
            result: serde_json::to_value(result).expect("result is serializable"),
        })
    }

    pub fn err(id: u64, code: i64, message: impl Into<String>) -> Self {
        Response::Failure(Failure {
            jsonrpc: JsonRpcVersion::V2,
            id,
            error: ErrorObject { code, message: message.into(), data: None },
        })
    }

    pub fn id(&self) -> u64 {
        match self {
            Response::Success(s) => s.id,
            Response::Failure(f) => f.id,
        }
    }
}

/// Anything that can go wrong on the pipe. A lost plugin is not an error to report to the
/// user; callers map it onto an `Outcome`.
#[derive(Debug, Error)]
pub enum FrameError {
    #[error("io on the plugin pipe: {0}")]
    Io(#[from] std::io::Error),
    #[error("message is not serializable: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("malformed json-rpc line: {0}")]
    Malformed(String),
    #[error("plugin closed the pipe")]
    Eof,
}

/// Write one message as a single line.
pub fn write_line(writer: &mut impl Write, msg: &impl Serialize) -> Result<(), FrameError> {
    let mut line = serde_json::to_string(msg)?;
    line.push('\n');
    writer.write_all(line.as_bytes())?;
    writer.flush()?;
    Ok(())
}

/// Read one message. `Ok(None)` means the peer closed the pipe, which the engine treats as
/// a lost plugin rather than an error.
pub fn read_line(reader: &mut impl BufRead, buf: &mut String) -> Result<Option<String>, FrameError> {
    buf.clear();
    let n = reader.read_line(buf)?;
    if n == 0 {
        return Ok(None);
    }
    let trimmed = buf.trim_end_matches(['\n', '\r']);
    if trimmed.is_empty() {
        return Ok(None);
    }
    Ok(Some(trimmed.to_string()))
}

/// Parse one response line.
pub fn parse_response(line: &str) -> Result<Response, FrameError> {
    serde_json::from_str(line).map_err(|e| FrameError::Malformed(format!("{e}: {line}")))
}

/// Parse one request line, as a plugin server does.
pub fn parse_request(line: &str) -> Result<Request, FrameError> {
    serde_json::from_str(line).map_err(|e| FrameError::Malformed(format!("{e}: {line}")))
}

/// A line reader over a byte stream, sized so a large `minimize` payload does not thrash.
pub fn buffered<R: Read>(read: R) -> BufReader<R> {
    BufReader::with_capacity(1 << 16, read)
}

/// A line writer over a byte stream.
pub fn buffered_writer<W: Write>(write: W) -> BufWriter<W> {
    BufWriter::with_capacity(1 << 16, write)
}
