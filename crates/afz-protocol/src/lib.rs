//! autofuzz wire contract.
//!
//! This crate holds types and framing only. It decides nothing: no scheduling, no
//! classification, no oracle policy. Both sides of the plugin boundary compile against
//! these shapes, and `cargo run -p afz-xtask -- schema` emits the committed JSON Schema
//! from them, so a protocol drift is a compile error rather than a runtime surprise.

pub mod methods;
pub mod outcome;
pub mod rpc;
pub mod targets;
pub mod types;
pub mod value;

pub use methods::{
    CoverageMapResp, DiscoverReq, DiscoverResp, HandshakeResp, InvokeReq, InvokeResp, MinimizeReq,
    MinimizeResp, PrepareReq, PrepareResp, ReadyResp, ReproducerReq, ReproducerResp,
};
pub use outcome::{EdgeId, FailedOutcome, FaultFrame, FaultSignature, OkOutcome, Outcome, RejectedOutcome};
pub use rpc::{
    buffered, buffered_writer, parse_request, parse_response, read_line, write_line, ErrorObject,
    Failure, FrameError, JsonRpcVersion, Method, Request, Response, Success, INVALID_REQUEST,
    METHOD_NOT_FOUND, PARSE_ERROR, PLUGIN_ERROR,
};
pub use targets::{
    Blocker, Capability, CoverageEntry, CoverageMap, Discovery, Effects, Handshake, HttpOrigin,
    Materialization, Origin, Parameter, SkipReason, SkippedTarget, TargetClass, TargetDescriptor,
    PROTOCOL_VERSION, SUPPORTED_VERSIONS, is_deprecated, schema_dir,
};
pub use types::{
    CollectionKind, CompositeKind, Field, FunctionBehavior, Kind, LengthBounds, NumberBounds,
    RefField, ScalarKind, TypeNode,
};
pub use value::{
    CallInput, KeySide, MaterializedCall, NodePath, NumberValue, Provenance, SpecialNumber, Step,
    ValueNode, ValueTree,
};
