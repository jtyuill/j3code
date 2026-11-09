use j3_protocol::v2::methods::{
    Delivery, GitNext, GitRunItem, METHODS, SendParams, TERMINAL_ATTACH, TURNS_SEND, TerminalItem,
    TranscriptEntry, UpdateProjectParams,
};
use serde_json::json;

#[test]
fn send_params_decode_minimal_and_delivery_values() {
    let p: SendParams =
        serde_json::from_value(json!({"threadId": "t1", "text": "hi", "delivery": "auto"}))
            .unwrap();
    assert!(p.attachments.is_empty() && p.model.is_none() && p.command_id.is_none());
    for (wire, value) in [
        ("auto", Delivery::Auto),
        ("queue", Delivery::Queue),
        ("steer", Delivery::Steer),
        ("restart", Delivery::Restart),
    ] {
        assert_eq!(serde_json::to_value(value).unwrap(), json!(wire));
    }
    assert!(METHODS.contains(&TURNS_SEND) && METHODS.contains(&TERMINAL_ATTACH));

    // `defaultModel: null` clears; absent keeps.
    let clear: UpdateProjectParams =
        serde_json::from_value(json!({"projectId": "p", "defaultModel": null})).unwrap();
    assert_eq!(clear.default_model, Some(None));
    let keep: UpdateProjectParams = serde_json::from_value(json!({"projectId": "p"})).unwrap();
    assert_eq!(keep.default_model, None);
}

#[test]
fn transcript_entries_tag_by_camel_case_kind() {
    let wire = json!([
        {"kind": "toolCall", "name": "bash", "input": {"command": "ls"}},
        {"kind": "toolResult", "name": "bash", "text": "ok", "failed": false},
    ]);
    let entries: Vec<TranscriptEntry> = serde_json::from_value(wire.clone()).unwrap();
    assert!(matches!(
        &entries[0],
        TranscriptEntry::ToolCall { intent: None, .. }
    ));
    assert_eq!(serde_json::to_value(&entries).unwrap(), wire);
}

#[test]
fn stream_items_tag_by_type() {
    let item = TerminalItem::Exited {
        exit_code: Some(1),
        exit_signal: None,
    };
    assert_eq!(
        serde_json::to_value(&item).unwrap(),
        json!({"type": "exited", "exitCode": 1})
    );
    let snap: TerminalItem =
        serde_json::from_value(json!({"type": "snapshot", "history": "$ ", "state": "running"}))
            .unwrap();
    assert!(matches!(snap, TerminalItem::Snapshot(_)));
    let done: GitRunItem = serde_json::from_value(json!({"type": "finished", "result": {
        "title": "Pushed", "next": {"kind": "open_pr", "label": "View PR", "url": "u"}}}))
    .unwrap();
    let GitRunItem::Finished { result } = done else {
        panic!()
    };
    assert!(matches!(result.next, Some(GitNext::OpenPr { .. })));
}
