//! Every protocol type must survive a JSON round-trip, because the Node plugin writes the
//! same shapes by hand and any asymmetry shows up as a wrong classification, not a crash.

use afz_protocol::*;

fn round_trip<T>(value: &T) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + PartialEq,
{
    let text = serde_json::to_string(value).expect("serialize");
    let back: T = serde_json::from_str(&text).unwrap_or_else(|e| panic!("deserialize {text}: {e}"));
    assert_eq!(*value, back, "round-trip changed the value");
    back
}

fn scalar_types() -> Vec<TypeNode> {
    vec![
        TypeNode::null(),
        TypeNode::Scalar { scalar: ScalarKind::Undefined, bounds: None, length: None },
        TypeNode::boolean(),
        TypeNode::Scalar {
            scalar: ScalarKind::Number,
            bounds: Some(NumberBounds { int: true, finite_only: true, min: Some(0.0), max: Some(255.0) }),
            length: None,
        },
        TypeNode::Scalar { scalar: ScalarKind::BigInt, bounds: None, length: None },
        TypeNode::Scalar {
            scalar: ScalarKind::String,
            bounds: None,
            length: Some(LengthBounds { min: Some(1), max: Some(64) }),
        },
    ]
}

fn composite_types() -> Vec<TypeNode> {
    vec![
        TypeNode::StringFormat { format: "date-time".into() },
        TypeNode::Literal { values: vec!["USD".into(), "EUR".into(), 7.into()] },
        TypeNode::Collection {
            collection: CollectionKind::Set,
            element: Box::new(TypeNode::string()),
            key: None,
            length: Some(LengthBounds { min: None, max: Some(8) }),
        },
        TypeNode::Collection {
            collection: CollectionKind::Map,
            element: Box::new(TypeNode::number()),
            key: Some(Box::new(TypeNode::string())),
            length: None,
        },
        TypeNode::Object {
            name: Some("Money".into()),
            id: Some("Money".into()),
            fields: vec![
                Field { name: "amount".into(), ty: TypeNode::number(), required: true },
                Field {
                    name: "parent".into(),
                    ty: TypeNode::Ref {
                        name: "Money".into(),
                        fields: vec![RefField { name: "parent".into(), ty: TypeNode::null() }],
                    },
                    required: false,
                },
            ],
        },
        TypeNode::Tuple { items: vec![TypeNode::string(), TypeNode::boolean()] },
        TypeNode::Composite {
            composite: CompositeKind::Union,
            discriminant: Some("kind".into()),
            members: vec![TypeNode::Literal { values: vec!["card".into()] }, TypeNode::null()],
        },
        TypeNode::Composite {
            composite: CompositeKind::Intersection,
            discriminant: None,
            members: vec![TypeNode::boolean(), TypeNode::null()],
        },
        TypeNode::Buffer { length: Some(LengthBounds { min: Some(0), max: Some(4096) }) },
        TypeNode::promise(TypeNode::string()),
        TypeNode::Function {
            behavior: Some(FunctionBehavior::CaptureArguments),
            returns: Some(Box::new(TypeNode::boolean())),
        },
        TypeNode::Stream {
            iterable: true,
            element: Box::new(TypeNode::string()),
            max_items: Some(4),
        },
        TypeNode::Opaque { ty: "Db".into(), module: Some("src/db.ts".into()) },
        TypeNode::Ref {
            name: "Money".into(),
            fields: vec![RefField { name: "parent".into(), ty: TypeNode::null() }],
        },
    ]
}

#[test]
fn type_node_round_trips_every_kind() {
    let mut kinds = std::collections::BTreeSet::new();
    for ty in scalar_types().into_iter().chain(composite_types()) {
        kinds.insert(ty.kind());
        round_trip(&ty);
    }
    // The thirteen wire kinds, all reachable from the fixture set above.
    assert_eq!(kinds.len(), 13, "missing kinds: {kinds:?}");
    for expected in [
        Kind::Scalar,
        Kind::StringFormat,
        Kind::Literal,
        Kind::Collection,
        Kind::Object,
        Kind::Tuple,
        Kind::Composite,
        Kind::Buffer,
        Kind::Promise,
        Kind::Function,
        Kind::Stream,
        Kind::Opaque,
        Kind::Ref,
    ] {
        assert!(kinds.contains(&expected), "{expected:?} never exercised");
    }
}

#[test]
fn type_node_kind_tag_is_on_the_wire() {
    let json = serde_json::to_value(TypeNode::boolean()).unwrap();
    assert_eq!(json["kind"], "scalar");
    assert_eq!(json["scalar"], "boolean");
    // `null` is a scalar sub-kind, not a wire kind of its own.
    let json = serde_json::to_value(TypeNode::promise(TypeNode::null())).unwrap();
    assert_eq!(json["kind"], "promise");
    assert_eq!(json["type"]["kind"], "scalar");
    assert_eq!(json["type"]["scalar"], "null");
    let json = serde_json::to_value(TypeNode::Ref { name: "Money".into(), fields: Vec::new() }).unwrap();
    assert_eq!(json["kind"], "ref");
    assert_eq!(json["name"], "Money");
    // A `ref` with no fields omits the key rather than writing an empty array.
    assert!(json.get("fields").is_none());
}

#[test]
fn value_tree_round_trips_every_node() {
    let values = vec![
        ValueTree::null(),
        ValueTree::undefined(),
        ValueTree::bool(true),
        ValueTree::number(1.5),
        ValueTree::number(f64::NAN),
        ValueTree::number(f64::INFINITY),
        ValueTree::number(f64::NEG_INFINITY),
        ValueTree::new(ValueNode::BigInt { value: "-9223372036854775808".into() }),
        ValueTree::string(""),
        ValueTree::string("1 MiB".to_string()),
        ValueTree::new(ValueNode::Literal { value: serde_json::json!({"a": [1, null, true]}) }),
        ValueTree::new(ValueNode::Array { items: vec![ValueTree::number(0.0), ValueTree::null()] }),
        ValueTree::new(ValueNode::Set { items: vec![ValueTree::string("a")] }),
        ValueTree::new(ValueNode::Map {
            entries: vec![(ValueTree::string("k"), ValueTree::number(-1.0))],
        }),
        ValueTree::new(ValueNode::Object {
            fields: vec![("v".into(), ValueTree::number(10.0)), ("to".into(), ValueTree::string("ZZZ"))],
        }),
        ValueTree::new(ValueNode::Tuple { items: vec![ValueTree::bool(false)] }),
        ValueTree::new(ValueNode::Buffer { bytes: vec![0, 1, 2, 255] }),
        ValueTree::new(ValueNode::Function { behavior: FunctionBehavior::Throw, returns: None }),
        ValueTree::new(ValueNode::Function {
            behavior: FunctionBehavior::ReturnFirst,
            returns: Some(Box::new(ValueTree::number(0.0))),
        }),
        ValueTree::new(ValueNode::Stream { iterable: false, items: vec![ValueTree::number(1.0)] }),
        ValueTree::new(ValueNode::Opaque { ty: "Db".into() }),
    ];
    for v in &values {
        round_trip(v);
    }
    round_trip(&CallInput { arguments: values.clone(), receiver: Some(ValueTree::null()) });
    round_trip(&CallInput::default());
}

#[test]
fn provenance_rides_flat_beside_the_node_tag() {
    // The plugin reads one flat object per node: its `node` tag, its fields, and provenance.
    let tree = ValueTree::boundary(ValueNode::String { value: String::new() });
    let json = serde_json::to_value(&tree).unwrap();
    assert_eq!(json["node"], "string");
    assert_eq!(json["value"], "");
    assert_eq!(json["provenance"], "boundary");
    assert_eq!(tree.provenance, Provenance::Boundary);

    // An absent provenance is a generated node, so a plugin that omits it stays readable.
    let back: ValueTree = serde_json::from_str(r#"{"node":"null"}"#).unwrap();
    assert_eq!(back, ValueTree::null());
    assert_eq!(back.provenance, Provenance::Generated);

    let mutated = ValueTree::with_provenance(
        ValueNode::number(1.0),
        Provenance::Mutated { op: "boundary".into(), from: "afz:t:44be".into() },
    );
    let json = serde_json::to_value(&mutated).unwrap();
    assert_eq!(json["provenance"]["mutated"]["op"], "boundary");
    round_trip(&mutated);
}

#[test]
fn special_numbers_survive_json() {
    // JSON has no token for NaN or Infinity, so they travel as names and stay distinguishable.
    for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let tree = ValueTree::number(v);
        let back = round_trip(&tree);
        let ValueNode::Number { value } = back.node else { panic!("expected number") };
        match v {
            x if x.is_nan() => assert!(value.as_f64().is_nan()),
            x if x == f64::INFINITY => assert_eq!(value.as_f64(), f64::INFINITY),
            _ => assert_eq!(value.as_f64(), f64::NEG_INFINITY),
        }
    }
}

#[test]
fn node_paths_address_every_descendant() {
    let tree = ValueTree::new(ValueNode::Object {
        fields: vec![
            ("items".into(), ValueTree::new(ValueNode::Array { items: vec![ValueTree::number(7.0)] })),
            ("map".into(), ValueTree::new(ValueNode::Map { entries: vec![(ValueTree::string("k"), ValueTree::string("v"))] })),
        ],
    });
    let paths = tree.paths();
    // root, items, items[0], map, map[0].key, map[0].value
    assert_eq!(paths.len(), 6, "paths were {paths:?}");
    assert_eq!(paths[0], NodePath::root());

    let target = NodePath { steps: vec![Step::Field("items".into()), Step::Index(0)] };
    let found = target.get(&tree).expect("the path exists");
    assert_eq!(found.node, ValueNode::Number { value: NumberValue::Finite(7.0) });

    let mut replaced = tree.clone();
    assert!(target.replace(&mut replaced, ValueTree::boundary(ValueNode::number(8.0))));
    let found = target.get(&replaced).expect("still there");
    assert_eq!(found.node, ValueNode::Number { value: NumberValue::Finite(8.0) });
    assert_eq!(found.provenance, Provenance::Boundary);
    // The untouched sibling keeps its value, so a mutation cannot leak across fields.
    assert_eq!(NodePath { steps: vec![Step::Field("map".into())] }.get(&replaced).unwrap().node,
               NodePath { steps: vec![Step::Field("map".into())] }.get(&tree).unwrap().node);

    // A path through a missing index or a wrong kind resolves to nothing rather than panicking.
    assert!(NodePath { steps: vec![Step::Field("items".into()), Step::Index(9)] }.get(&tree).is_none());
    assert!(!NodePath { steps: vec![Step::Index(0)] }.replace(&mut replaced, ValueTree::null()));
}

#[test]
fn provenance_round_trips() {
    round_trip(&Provenance::Generated);
    round_trip(&Provenance::Boundary);
    round_trip(&Provenance::Mutated { op: "resize".into(), from: "afz:t:44be".into() });
    round_trip(&Provenance::Transplanted { from_entry: "c-7".into() });
    round_trip(&Provenance::Minimized { step: 3 });
    round_trip(&Provenance::User);
}

fn frame() -> FaultFrame {
    FaultFrame {
        file: "src/config.ts".into(),
        line: 88,
        column: Some(19),
        fn_name: Some("normalize".into()),
    }
}

#[test]
fn outcome_round_trips_all_three_variants() {
    round_trip(&Outcome::Ok(OkOutcome {
        duration_ms: 1.25,
        memory_bytes: Some(4096),
        coverage: Some(vec![1, 7, 4096]),
        value: Some(ValueTree::bool(false)),
    }));
    round_trip(&Outcome::Ok(OkOutcome::default()));
    round_trip(&Outcome::Rejected(RejectedOutcome {
        duration_ms: 0.5,
        coverage: Some(vec![3]),
        via: "guard-site".into(),
        detail: "RangeError: unsupported currency".into(),
    }));
    round_trip(&Outcome::Failed(FailedOutcome {
        duration_ms: 0.75,
        coverage: Some(vec![9]),
        via: "engine-error-outside-declared-channel".into(),
        fault: FaultSignature {
            class: "TypeError".into(),
            message: "Cannot read properties of undefined (reading 'map')".into(),
            frames: vec![frame()],
        },
    }));
}

#[test]
fn outcome_kind_tag_and_helpers() {
    let lost = Outcome::plugin_lost(2000.0, "plugin exited with code 1");
    let json = serde_json::to_value(&lost).unwrap();
    assert_eq!(json["kind"], "failed");
    assert_eq!(json["via"], "plugin-lost");
    assert_eq!(json["fault"]["class"], "PluginLost");
    assert_eq!(lost.kind(), "failed");
    assert_eq!(lost.via(), Some("plugin-lost"));
    // A lost plugin has no stack, so identity falls back to class alone.
    assert!(lost.fault().unwrap().top_frame().is_none());
    assert!(json["fault"].get("frames").is_none());
    let with_frame = Outcome::Failed(FailedOutcome {
        duration_ms: 1.0,
        coverage: None,
        via: "engine-error-outside-declared-channel".into(),
        fault: FaultSignature { class: "TypeError".into(), message: "m".into(), frames: vec![frame()] },
    });
    assert_eq!(with_frame.fault().unwrap().top_frame().unwrap().file, "src/config.ts");
    assert_eq!(Outcome::Ok(OkOutcome::default()).via(), None);
}

#[test]
fn fault_frame_omits_absent_optionals() {
    let json = serde_json::to_value(FaultFrame { file: "a.ts".into(), line: 3, column: None, fn_name: None }).unwrap();
    assert_eq!(json, serde_json::json!({"file": "a.ts", "line": 3}));
}

#[test]
fn handshake_and_capabilities_round_trip() {
    let hs = Handshake {
        protocol_version: PROTOCOL_VERSION.into(),
        name: "autofuzz-harness-js".into(),
        version: "0.1.0".into(),
        target_class: TargetClass::DesignTime,
        capabilities: vec![
            Capability::Coverage,
            Capability::Stateful,
            Capability::Minimize,
            Capability::SideEffects,
            Capability::Concurrent,
        ],
        languages: vec!["typescript".into(), "javascript".into()],
        discover_requires_execution: false,
    };
    let back = round_trip(&hs);
    assert_eq!(back.capabilities.len(), 5);
    assert_eq!(back.protocol_version, "afz/1");

    for cap in [
        Capability::Coverage,
        Capability::Stateful,
        Capability::Minimize,
        Capability::SideEffects,
        Capability::Concurrent,
    ] {
        let json = serde_json::to_value(cap).unwrap();
        let back: Capability = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(back, cap);
        assert!(json.as_str().unwrap().chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_alphabetic()),
            "capability names are single lowercase words: {json}");
    }
    assert_eq!(serde_json::to_value(TargetClass::DesignTime).unwrap(), "design-time");
    assert_eq!(serde_json::to_value(TargetClass::Runtime).unwrap(), "runtime");
}

fn descriptor() -> TargetDescriptor {
    TargetDescriptor {
        id: "afz:t:44be".into(),
        target_class: TargetClass::DesignTime,
        origin: Origin {
            file: Some("src/currency.ts".into()),
            line: Some(3),
            module: Some("src/currency.ts".into()),
            http: None,
        },
        name: "convert".into(),
        parameters: vec![
            Parameter { name: "amount".into(), ty: TypeNode::number(), required: true },
            Parameter {
                name: "to".into(),
                ty: TypeNode::Literal { values: vec!["USD".into(), "EUR".into()] },
                required: true,
            },
        ],
        returns: Some(TypeNode::promise(TypeNode::number())),
        effects: Effects {
            async_: true,
            throws: true,
            streams: false,
            callbacks: false,
            mutates_state: false,
        },
        guard_sites: vec![5],
        materialization: Materialization::Constructible,
    }
}

#[test]
fn target_descriptor_round_trips() {
    let d = descriptor();
    round_trip(&d);
    let json = serde_json::to_value(&d).unwrap();
    assert_eq!(json["effects"]["async"], true);
    assert_eq!(json["guardSites"], serde_json::json!([5]));
    assert_eq!(json["materialization"], "constructible");
    assert!(d.is_fuzzable());

    let runtime = TargetDescriptor {
        id: "afz:t:9c1f".into(),
        target_class: TargetClass::Runtime,
        origin: Origin {
            http: Some(HttpOrigin { method: "POST".into(), path: "/v1/tokens".into() }),
            ..Default::default()
        },
        name: "POST /v1/tokens".into(),
        parameters: vec![Parameter {
            name: "body".into(),
            ty: TypeNode::Opaque { ty: "Db".into(), module: Some("src/db.ts".into()) },
            required: true,
        }],
        returns: None,
        effects: Effects::default(),
        guard_sites: Vec::new(),
        materialization: Materialization::Opaque,
    };
    let json = serde_json::to_value(&runtime).unwrap();
    assert_eq!(json["origin"]["http"]["method"], "POST");
    assert!(!runtime.is_fuzzable());
    round_trip(&runtime);

    for m in [Materialization::Constructible, Materialization::FixtureRequired, Materialization::Opaque] {
        assert!(serde_json::to_value(m).unwrap().as_str().is_some());
        round_trip(&m);
    }
}

#[test]
fn skipped_target_round_trips_with_blocker() {
    let s = SkippedTarget {
        target_id: "afz:t:9c1f".into(),
        name: "UserRepo.findById".into(),
        reason: SkipReason::UnconstructibleParameter,
        blocked_by: Some(Blocker {
            type_: "Db".into(),
            module: Some("src/db.ts".into()),
            path: "receiver.db".into(),
        }),
        unlocks_targets: Some(14),
        materialization: Some(Materialization::FixtureRequired),
        detail: None,
    };
    let back = round_trip(&s);
    let json = serde_json::to_value(&back).unwrap();
    assert_eq!(json["reason"], "unconstructible-parameter");
    assert_eq!(json["blockedBy"]["type"], "Db");
    assert_eq!(json["blockedBy"]["path"], "receiver.db");
    assert_eq!(json["unlocksTargets"], 14);
    assert!(json.get("detail").is_none());

    for r in [
        SkipReason::UnconstructibleParameter,
        SkipReason::DiscoveryError,
        SkipReason::NotPublic,
        SkipReason::Excluded,
        SkipReason::UnsupportedSignature,
        SkipReason::TargetDisabled,
    ] {
        round_trip(&r);
    }
}

#[test]
fn discovery_and_coverage_map_round_trip() {
    let d = Discovery {
        targets: vec![descriptor()],
        skipped: vec![SkippedTarget {
            target_id: "afz:t:1".into(),
            name: "secret".into(),
            reason: SkipReason::NotPublic,
            blocked_by: None,
            unlocks_targets: None,
            materialization: None,
            detail: Some("not exported".into()),
        }],
    };
    round_trip(&d);
    round_trip(&Discovery::default());

    let map = CoverageMap {
        entries: vec![CoverageEntry { edge: 41, url: "src/currency.ts".into(), start: 120, end: 188 }],
    };
    round_trip(&map);
    round_trip(&CoverageMap::default());
}

#[test]
fn method_payloads_round_trip() {
    round_trip(&DiscoverReq {
        root: "/src/example".into(),
        include: Some(vec!["src/**/*.ts".into()]),
        exclude: Some(vec!["**/*.test.ts".into()]),
        config_path: Some("/src/example/autofuzz.json".into()),
    });
    round_trip(&DiscoverReq { root: ".".into(), ..Default::default() });
    round_trip(&PrepareReq { target_id: "afz:t:44be".into(), values: vec![ValueTree::number(10.0)] });
    round_trip(&PrepareResp::Ok {
        materialized: MaterializedCall { handle: "h-1".into(), summary: "Money(10, USD)".into() },
    });
    round_trip(&PrepareResp::Skip { reason: SkipReason::UnconstructibleParameter });
    round_trip(&InvokeReq {
        invocation_id: 91207,
        target_id: "afz:t:44be".into(),
        values: vec![ValueTree::string("USD")],
        deadline_ms: 2000,
        collect_coverage: true,
    });
    round_trip(&MinimizeReq {
        target_id: "afz:t:44be".into(),
        values: vec![ValueTree::null()],
        fault: FaultSignature { class: "TypeError".into(), message: "m".into(), frames: vec![frame()] },
        budget_ms: 5000,
    });
    round_trip(&MinimizeResp { values: vec![ValueTree::null()], steps: 12 });
}

#[test]
fn prepare_resp_tags_on_ok_boolean() {
    let json = serde_json::to_value(PrepareResp::Skip { reason: SkipReason::Excluded }).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["reason"], "excluded");
}

#[test]
fn rpc_frames_round_trip() {
    let req = Request::new(7, Method::Invoke, InvokeReq {
        invocation_id: 7,
        target_id: "afz:t:44be".into(),
        values: Vec::new(),
        deadline_ms: 2000,
        collect_coverage: false,
    });
    round_trip(&req);
    let json = serde_json::to_value(&req).unwrap();
    assert_eq!(json["jsonrpc"], "2.0");
    assert_eq!(json["id"], 7);
    assert_eq!(json["method"], "invoke");

    round_trip(&Request::empty(8, Method::Handshake));
    round_trip(&Response::ok(7, Outcome::Ok(OkOutcome::default())));
    round_trip(&Response::err(7, METHOD_NOT_FOUND, "no such method"));

    let ok = Response::ok(3, ValueTree::bool(true));
    let line = serde_json::to_string(&ok).unwrap();
    let back = parse_response(&line).unwrap();
    assert_eq!(back, ok);
    assert_eq!(back.id(), 3);
    let err = Response::err(4, PLUGIN_ERROR, "boom");
    let line = serde_json::to_string(&err).unwrap();
    let back = parse_response(&line).unwrap();
    assert_eq!(back, err);
    let Response::Failure(f) = back else { panic!("expected failure") };
    assert_eq!(f.error.code, PLUGIN_ERROR);
    assert!(f.error.data.is_none());
}

#[test]
fn method_names_are_the_six_wire_methods() {
    for m in [
        Method::Handshake,
        Method::Discover,
        Method::Prepare,
        Method::Invoke,
        Method::Minimize,
        Method::CoverageMap,
    ] {
        let name = m.name();
        assert_eq!(Method::from_wire(name), Some(m), "{name} does not round-trip");
    }
    assert_eq!(Method::from_wire("coverageMap"), Some(Method::CoverageMap));
    assert_eq!(Method::from_wire("shutdown"), None);
}

#[test]
fn framing_reads_and_writes_one_message_per_line() {
    let mut out = Vec::new();
    write_line(&mut out, &Request::empty(1, Method::Handshake)).unwrap();
    write_line(&mut out, &Response::ok(1, Handshake {
        protocol_version: PROTOCOL_VERSION.into(),
        name: "fake".into(),
        version: "0".into(),
        target_class: TargetClass::DesignTime,
        capabilities: vec![],
        languages: vec![],
        discover_requires_execution: false,
    }))
    .unwrap();
    assert_eq!(out.iter().filter(|b| **b == b'\n').count(), 2, "exactly two lines");

    let mut reader = buffered(out.as_slice());
    let mut buf = String::new();
    let line = read_line(&mut reader, &mut buf).unwrap().unwrap();
    let req = parse_request(&line).unwrap();
    assert_eq!(req.method, "handshake");
    let line = read_line(&mut reader, &mut buf).unwrap().unwrap();
    assert_eq!(parse_response(&line).unwrap().id(), 1);
    assert!(read_line(&mut reader, &mut buf).unwrap().is_none(), "EOF is a closed pipe, not an error");
    assert!(parse_response("{ not json").is_err());
}

