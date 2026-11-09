//! Protocol v2 shapes that clients and servers of different builds rely on
//! (`docs/protocol/wire.md`).

use j3_protocol::v2::{
    entities::{Item, ItemKind, ItemStatus, TextItem, ToolDetail, ToolItem},
    envelope::{ClientFrame, Error, ErrorCode, ServerFrame},
};
use serde_json::{Value, json};

#[test]
fn item_kind_fields_sit_beside_common_fields() {
    let item = Item {
        id: "i1".into(),
        thread_id: "t1".into(),
        turn_id: "u1".into(),
        ordinal: 3,
        status: ItemStatus::Running,
        started_at: "2026-11-09T11:18:00.000Z".into(),
        ended_at: None,
        kind: ItemKind::Text(TextItem { text: "hi".into() }),
    };
    let encoded = serde_json::to_value(&item).unwrap();
    assert_eq!(
        encoded,
        json!({"id":"i1","threadId":"t1","turnId":"u1","ordinal":3,"status":"running",
               "startedAt":"2026-11-09T11:18:00.000Z","kind":"text","text":"hi"})
    );
    assert_eq!(serde_json::from_value::<Item>(encoded).unwrap(), item);
}

#[test]
fn unknown_item_kind_and_tool_detail_still_decode() {
    let item: Item = serde_json::from_value(json!({
        "id":"i1","threadId":"t1","turnId":"u1","ordinal":1,"status":"completed",
        "startedAt":"2026-11-09T11:18:00.000Z","kind":"hologram","beams":4
    }))
    .unwrap();
    assert_eq!(item.kind, ItemKind::Unknown);

    let tool: ToolItem = serde_json::from_value(json!({
        "callId":"c1","name":"x","input":{},"detail":{"type":"telepathy","target":"y"}
    }))
    .unwrap();
    assert_eq!(tool.detail, Some(ToolDetail::Unknown));
}

#[test]
fn frames_use_the_t_tag() {
    let call: ClientFrame =
        serde_json::from_value(json!({"t":"call","id":7,"method":"threads.get"})).unwrap();
    assert_eq!(
        call,
        ClientFrame::Call {
            id: 7,
            method: "threads.get".into(),
            params: json!({})
        }
    );
    let err = ServerFrame::Err {
        id: None,
        error: Error::bad_request("frame is not JSON"),
    };
    assert_eq!(
        serde_json::to_value(&err).unwrap(),
        json!({"t":"err","id":null,"error":{"code":"bad_request","message":"frame is not JSON"}})
    );
}

#[test]
fn unknown_error_code_decodes_to_other() {
    let error: Error =
        serde_json::from_value(json!({"code":"quota","message":"m","data":{"x":1}})).unwrap();
    assert_eq!(error.code, ErrorCode::Other);
    assert_eq!(error.data, Some(json!({"x":1})));
    let _: Value = serde_json::to_value(&error).unwrap();
}
