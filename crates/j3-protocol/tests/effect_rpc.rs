//! Round-trips every frame captured from a real effect@4.0.5 RPC session
//! (see `docs/protocol/effect-rpc.md`).

use j3_protocol::effect_rpc::{
    CauseReason, ClientMessage, Exit, RequestId, ServerMessage, decode_client_frame,
    decode_server_frame,
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct Fixture {
    scenarios: Vec<Scenario>,
}

#[derive(Deserialize)]
struct Scenario {
    scenario: String,
    frames: Vec<Frame>,
}

#[derive(Deserialize)]
struct Frame {
    direction: String,
    raw: String,
}

fn fixture() -> Fixture {
    serde_json::from_str(include_str!("fixtures/effect_rpc/frames.json")).unwrap()
}

fn tags(raw: &Value) -> Vec<&str> {
    match raw {
        Value::Array(items) => items.iter().map(|v| v["_tag"].as_str().unwrap()).collect(),
        _ => vec![raw["_tag"].as_str().unwrap()],
    }
}

fn client_tag(m: &ClientMessage) -> &'static str {
    match m {
        ClientMessage::Request(_) => "Request",
        ClientMessage::Ack { .. } => "Ack",
        ClientMessage::Interrupt { .. } => "Interrupt",
        ClientMessage::Ping => "Ping",
        ClientMessage::Eof => "Eof",
        ClientMessage::Unknown(_) => "<unknown>",
    }
}

fn server_tag(m: &ServerMessage) -> &'static str {
    match m {
        ServerMessage::Chunk { .. } => "Chunk",
        ServerMessage::Exit { .. } => "Exit",
        ServerMessage::Defect { .. } => "Defect",
        ServerMessage::Pong => "Pong",
        ServerMessage::ClientProtocolError { .. } => "ClientProtocolError",
        ServerMessage::Request(_) => "Request",
        ServerMessage::Unknown(_) => "<unknown>",
    }
}

fn reencode<T: serde::Serialize>(raw: &Value, msgs: &[T]) -> Value {
    let encoded: Vec<Value> = msgs
        .iter()
        .map(|m| serde_json::to_value(m).unwrap())
        .collect();
    match raw {
        Value::Array(_) => Value::Array(encoded),
        _ => encoded.into_iter().next().unwrap(),
    }
}

#[test]
fn every_captured_frame_round_trips() {
    let mut checked = 0;
    for scenario in fixture().scenarios {
        for frame in scenario.frames {
            let Ok(raw) = serde_json::from_str::<Value>(&frame.raw) else {
                // The only non-JSON frame is the deliberately malformed one.
                assert_eq!(scenario.scenario, "raw-malformed-json");
                assert!(decode_client_frame(&frame.raw).is_err());
                continue;
            };
            let expected = tags(&raw);
            let (got, again): (Vec<&str>, Value) = match frame.direction.as_str() {
                "client->server" => {
                    let msgs = decode_client_frame(&frame.raw).unwrap();
                    (msgs.iter().map(client_tag).collect(), reencode(&raw, &msgs))
                }
                "server->client" => {
                    let msgs = decode_server_frame(&frame.raw).unwrap();
                    (msgs.iter().map(server_tag).collect(), reencode(&raw, &msgs))
                }
                other => panic!("bad direction {other}"),
            };
            assert_eq!(got, expected, "{}: {}", scenario.scenario, frame.raw);
            assert_eq!(again, raw, "{}: {}", scenario.scenario, frame.raw);
            checked += 1;
        }
    }
    assert!(checked >= 40, "only {checked} frames checked");
}

fn frames_of(name: &str) -> Vec<Frame> {
    fixture()
        .scenarios
        .into_iter()
        .find(|s| s.scenario == name)
        .unwrap()
        .frames
}

fn server(frame: &Frame) -> ServerMessage {
    assert_eq!(frame.direction, "server->client");
    decode_server_frame(&frame.raw).unwrap().remove(0)
}

#[test]
fn unary_exits_decode_to_expected_shapes() {
    let echo = frames_of("echo");
    match decode_client_frame(&echo[0].raw).unwrap().remove(0) {
        ClientMessage::Request(r) => {
            assert_eq!(r.id, RequestId::Number(0));
            assert_eq!(r.tag, "Echo");
            assert_eq!(r.payload, json!({"text": "ab", "n": 3}));
            assert!(r.headers.is_empty());
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        server(&echo[1]),
        ServerMessage::exit_success(0, json!({"text": "ababab", "len": 6}))
    );
    assert_eq!(
        server(&frames_of("tagged-error")[1]),
        ServerMessage::exit_fail(1, json!({"_tag": "NotFound", "id": "x1"}))
    );
    assert_eq!(
        server(&frames_of("defect")[1]),
        ServerMessage::exit_die(2, json!({"name": "Error", "message": "kaboom"}))
    );
    assert_eq!(
        server(&frames_of("raw-unknown-tag")[1]),
        ServerMessage::exit_die("100", json!("Unknown request tag: Nope"))
    );
}

#[test]
fn stream_frames_alternate_chunk_and_ack() {
    let f = frames_of("stream");
    assert_eq!(server(&f[1]), ServerMessage::chunk(3, vec![json!(1)]));
    assert_eq!(
        decode_client_frame(&f[2].raw).unwrap(),
        vec![ClientMessage::ack(3)]
    );
    assert_eq!(
        server(f.last().unwrap()),
        ServerMessage::exit_success(3, Value::Null)
    );
    let batched = frames_of("stream-batched");
    assert_eq!(
        server(&batched[1]),
        ServerMessage::chunk(4, vec![json!(1), json!(2), json!(3)])
    );
}

#[test]
fn interrupt_round_trip() {
    let f = frames_of("stream-interrupt");
    let interrupt = f
        .iter()
        .position(|fr| fr.raw.contains("\"Interrupt\"") && fr.direction == "client->server")
        .unwrap();
    assert_eq!(
        decode_client_frame(&f[interrupt].raw).unwrap(),
        vec![ClientMessage::interrupt(6)]
    );
    match server(&f[interrupt + 1]) {
        ServerMessage::Exit {
            request_id,
            exit: Exit::Failure { cause },
        } => {
            assert_eq!(request_id, RequestId::Number(6));
            assert!(matches!(
                cause[..],
                [CauseReason::Interrupt { fiber_id: Some(_) }]
            ));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn batched_client_frame_and_defect() {
    let f = frames_of("raw-batched-frame");
    let msgs = decode_client_frame(&f[0].raw).unwrap();
    assert!(matches!(
        msgs[..],
        [ClientMessage::Request(_), ClientMessage::Ping]
    ));
    assert_eq!(server(&f[2]), ServerMessage::pong());
    let d = frames_of("raw-malformed-json");
    assert!(matches!(server(&d[1]), ServerMessage::Defect { .. }));
}

#[test]
fn unknown_tags_are_preserved() {
    let raw = json!({"_tag": "Future", "x": 1});
    let c: ClientMessage = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(c, ClientMessage::Unknown(raw.clone()));
    assert_eq!(serde_json::to_value(&c).unwrap(), raw);
    let s: ServerMessage = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(s, ServerMessage::Unknown(raw));
    assert!(decode_client_frame("42").unwrap()[0] == ClientMessage::Unknown(json!(42)));
}

#[test]
fn interrupt_without_fiber_id_omits_it() {
    assert_eq!(
        serde_json::to_value(ServerMessage::exit_interrupt(9)).unwrap(),
        json!({"_tag": "Exit", "requestId": 9, "exit": {"_tag": "Failure", "cause": [{"_tag": "Interrupt"}]}})
    );
}
