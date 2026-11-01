//! HTTP routes and the attachment upload → asset round trip.

mod transport_support;

use std::sync::Arc;

use futures_util::{FutureExt, future::BoxFuture};
use j3_core::{
    attachments::AttachmentStore,
    http::auth_session_state,
    rpc::{Response, RpcHandler},
};
use j3_protocol::effect_rpc::{CauseReason, Exit};
use serde_json::{Value, json};
use transport_support::{TempDir, Ws, descriptor, http, start};

/// Routes the attachment tags to the store, like the engine does.
struct AttachmentsHandler(Arc<AttachmentStore>);

impl RpcHandler for AttachmentsHandler {
    fn call(&self, tag: String, payload: Value) -> BoxFuture<'static, Response> {
        let store = self.0.clone();
        async move {
            match store.handle(&tag, payload).await {
                Some(response) => response,
                None => Response::die(format!("Unknown request tag: {tag}")),
            }
        }
        .boxed()
    }
}

async fn setup() -> (Arc<AttachmentStore>, std::net::SocketAddr, TempDir) {
    let dir = TempDir::new("transport-http");
    let store = Arc::new(AttachmentStore::open(dir.0.clone()).unwrap());
    let addr = start(Arc::new(AttachmentsHandler(store.clone())), store.clone()).await;
    (store, addr, dir)
}

/// Runs one store call and returns its unary exit.
async fn call(store: &AttachmentStore, tag: &str, payload: Value) -> Exit {
    match store.handle(tag, payload).await.expect("attachment tag") {
        Response::Unary(exit) => exit,
        Response::Stream(_) => panic!("attachment methods are unary"),
    }
}

fn success(exit: Exit) -> Value {
    match exit {
        Exit::Success { value } => value,
        Exit::Failure { cause } => panic!("expected success, got {cause:?}"),
    }
}

fn die_message(exit: Exit) -> String {
    match exit {
        Exit::Failure { cause } => match cause.as_slice() {
            [CauseReason::Die { defect }] => defect.as_str().unwrap().to_owned(),
            other => panic!("expected a Die, got {other:?}"),
        },
        Exit::Success { value } => panic!("expected a Die, got success {value}"),
    }
}

fn fail_error(exit: Exit) -> Value {
    match exit {
        Exit::Failure { cause } => match cause.as_slice() {
            [CauseReason::Fail { error }] => error.clone(),
            other => panic!("expected a Fail, got {other:?}"),
        },
        Exit::Success { value } => panic!("expected a Fail, got success {value}"),
    }
}

#[tokio::test]
async fn transport_http_static_endpoints() {
    let (_store, addr, _dir) = setup().await;

    let response = http(addr, "GET", "/.well-known/t3/environment", &[], b"").await;
    assert_eq!(response.status, 200);
    assert_eq!(response.header("content-type"), Some("application/json"));
    assert_eq!(response.json(), descriptor());

    let response = http(addr, "GET", "/api/auth/session", &[], b"").await;
    assert_eq!(response.status, 200);
    assert_eq!(
        response.json(),
        json!({
            "authenticated": true,
            "auth": {"policy": "unsafe-no-auth", "bootstrapMethods": [], "sessionMethods": [], "sessionCookieName": "j3_session"},
            "scopes": ["orchestration:read", "orchestration:operate"],
            "permissions": ["orchestration:read", "orchestration:operate", "filesystem:read", "filesystem:write", "settings:write"],
        })
    );
    assert_eq!(response.json(), auth_session_state());

    for path in [
        "/api/orchestration/shell",
        "/api/orchestration/threads/t1/bounded",
        "/api/orchestration/threads/t1/history?cursor=x",
    ] {
        let response = http(
            addr,
            "GET",
            path,
            &[("x-t3-orchestration-protocol", "2")],
            b"",
        )
        .await;
        assert_eq!(response.status, 501, "{path}");
        assert!(
            serde_json::from_slice::<Value>(&response.body).is_err(),
            "{path}"
        );
    }
}

#[tokio::test]
async fn transport_upload_then_asset_round_trip() {
    let (store, addr, _dir) = setup().await;
    let bytes = b"0123456789A";
    let mut ws = Ws::connect(addr).await;

    ws.request(
        json!(0),
        "attachments.createUploadUrl",
        json!({"type": "image", "name": "IMG 1.png", "mimeType": "image/png", "sizeBytes": bytes.len()}),
    )
    .await;
    let created = ws.recv().await;
    assert_eq!(created["exit"]["_tag"], "Success", "{created}");
    let issued = &created["exit"]["value"];
    let attachment_id = issued["attachmentId"].as_str().unwrap().to_owned();
    assert!(
        attachment_id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    );
    let upload_url = issued["relativeUrl"].as_str().unwrap().to_owned();
    assert_eq!(
        upload_url,
        format!("/api/attachments/upload/{attachment_id}")
    );
    assert!(issued["expiresAt"].is_u64());

    // Not uploaded yet: no asset URL.
    ws.request(
        json!(1),
        "assets.createUrl",
        json!({"resource": {"_tag": "attachment", "attachmentId": attachment_id}}),
    )
    .await;
    let missing = ws.recv().await;
    assert_eq!(
        missing["exit"]["cause"][0]["error"]["_tag"],
        "AssetAttachmentNotFoundError"
    );

    // Content-Length must match the declared size.
    let response = http(
        addr,
        "POST",
        &upload_url,
        &[("content-type", "image/png")],
        &bytes[..5],
    )
    .await;
    assert_eq!(response.status, 400);
    let response = http(
        addr,
        "POST",
        &upload_url,
        &[("content-type", "image/png")],
        bytes,
    )
    .await;
    assert_eq!(response.status, 204);
    assert!(response.body.is_empty());
    // A retry with the same bytes is fine; different bytes are refused.
    let response = http(addr, "POST", &upload_url, &[], bytes).await;
    assert_eq!(response.status, 204);
    let response = http(addr, "POST", &upload_url, &[], b"xxxxxxxxxxx").await;
    assert_eq!(response.status, 409);

    ws.request(
        json!(2),
        "assets.createUrl",
        json!({"resource": {"_tag": "attachment", "attachmentId": attachment_id, "mimeType": "image/png", "disposition": "inline"}}),
    )
    .await;
    let asset = ws.recv().await;
    assert_eq!(asset["exit"]["_tag"], "Success", "{asset}");
    let asset_url = asset["exit"]["value"]["relativeUrl"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        asset_url,
        format!("/api/assets/{attachment_id}/IMG%201.png")
    );
    assert!(asset["exit"]["value"]["expiresAt"].is_u64());

    let full = http(addr, "GET", &asset_url, &[], b"").await;
    assert_eq!(full.status, 200);
    assert_eq!(full.body, bytes);
    assert_eq!(full.header("content-type"), Some("image/png"));
    assert_eq!(full.header("content-length"), Some("11"));
    assert_eq!(full.header("cache-control"), Some("private, max-age=3600"));
    assert_eq!(full.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(full.header("accept-ranges"), Some("bytes"));
    let etag = full.header("etag").unwrap().to_owned();

    let partial = http(addr, "GET", &asset_url, &[("range", "bytes=2-5")], b"").await;
    assert_eq!(partial.status, 206);
    assert_eq!(partial.body, b"2345");
    assert_eq!(partial.header("content-range"), Some("bytes 2-5/11"));
    assert_eq!(partial.header("content-length"), Some("4"));
    assert_eq!(partial.header("content-type"), Some("image/png"));

    let suffix = http(addr, "GET", &asset_url, &[("range", "bytes=-3")], b"").await;
    assert_eq!(suffix.status, 206);
    assert_eq!(suffix.body, b"89A");
    assert_eq!(suffix.header("content-range"), Some("bytes 8-10/11"));

    let open_ended = http(addr, "GET", &asset_url, &[("range", "bytes=9-")], b"").await;
    assert_eq!(open_ended.status, 206);
    assert_eq!(open_ended.body, b"9A");

    let unsatisfiable = http(addr, "GET", &asset_url, &[("range", "bytes=11-")], b"").await;
    assert_eq!(unsatisfiable.status, 416);
    assert_eq!(unsatisfiable.header("content-range"), Some("bytes */11"));
    assert!(unsatisfiable.body.is_empty());

    // Malformed and multi-range headers are ignored.
    for range in ["bytes=5-2", "items=0-1", "bytes=0-1,4-5"] {
        let response = http(addr, "GET", &asset_url, &[("range", range)], b"").await;
        assert_eq!(response.status, 200, "{range}");
        assert_eq!(response.body, bytes);
    }

    // If-Range: a matching validator honours Range, anything else sends all.
    let matched = http(
        addr,
        "GET",
        &asset_url,
        &[("range", "bytes=0-1"), ("if-range", &etag)],
        b"",
    )
    .await;
    assert_eq!(matched.status, 206);
    assert_eq!(matched.body, b"01");
    for if_range in ["\"other\"", "Wed, 21 Oct 2015 07:28:00 GMT"] {
        let response = http(
            addr,
            "GET",
            &asset_url,
            &[("range", "bytes=0-1"), ("if-range", if_range)],
            b"",
        )
        .await;
        assert_eq!(response.status, 200, "{if_range}");
        assert_eq!(response.body, bytes);
    }

    // HEAD ignores Range and sends no body.
    let head = http(addr, "HEAD", &asset_url, &[("range", "bytes=0-1")], b"").await;
    assert_eq!(head.status, 200);
    assert_eq!(head.header("content-length"), Some("11"));

    // The engine reads the bytes back for omp.
    let (read, mime_type) = store.read(&attachment_id).await.unwrap();
    assert_eq!(read, bytes);
    assert_eq!(mime_type, "image/png");

    // Delete removes it everywhere.
    ws.request(
        json!(3),
        "attachments.delete",
        json!({"attachmentId": attachment_id}),
    )
    .await;
    assert_eq!(
        ws.recv().await,
        json!({"_tag":"Exit","requestId":3,"exit":{"_tag":"Success","value":null}})
    );
    assert_eq!(http(addr, "GET", &asset_url, &[], b"").await.status, 404);
    assert_eq!(
        http(addr, "POST", &upload_url, &[], bytes).await.status,
        404
    );
    assert!(store.read(&attachment_id).await.is_err());
}

#[tokio::test]
async fn transport_unknown_asset_404() {
    let (store, addr, _dir) = setup().await;
    for path in [
        "/api/assets/att_unknown/file.png",
        "/api/assets/..%2F..%2Fetc/passwd",
        "/api/assets/UPPER/x.png",
    ] {
        assert_eq!(
            http(addr, "GET", path, &[], b"").await.status,
            404,
            "{path}"
        );
    }
    let response = http(
        addr,
        "POST",
        "/api/attachments/upload/att_unknown",
        &[],
        b"abc",
    )
    .await;
    assert_eq!(response.status, 404);

    let resource =
        json!({"_tag": "attachment", "attachmentId": "att_unknown", "fileName": "a.png"});
    let exit = call(&store, "assets.createUrl", json!({"resource": resource})).await;
    assert_eq!(
        fail_error(exit),
        json!({"_tag": "AssetAttachmentNotFoundError", "resource": resource})
    );
}

#[tokio::test]
async fn transport_attachment_payload_validation() {
    let (store, addr, _dir) = setup().await;

    for (payload, path) in [
        (
            json!({"name": "a.bmp", "mimeType": "image/bmp", "sizeBytes": 3}),
            "[\"mimeType\"]",
        ),
        (
            json!({"name": "a.png", "mimeType": "image/png", "sizeBytes": 0}),
            "[\"sizeBytes\"]",
        ),
        (
            json!({"name": "a.png", "mimeType": "image/png", "sizeBytes": 10 * 1024 * 1024 + 1}),
            "[\"sizeBytes\"]",
        ),
        (
            json!({"name": "a.png", "mimeType": "image/png", "sizeBytes": 1.5}),
            "[\"sizeBytes\"]",
        ),
        (
            json!({"name": "   ", "mimeType": "image/png", "sizeBytes": 3}),
            "[\"name\"]",
        ),
        (
            json!({"mimeType": "image/png", "sizeBytes": 3}),
            "[\"name\"]",
        ),
        (
            json!({"type": "video", "name": "a", "mimeType": "image/png", "sizeBytes": 3}),
            "[\"type\"]",
        ),
        (
            json!({"type": "file", "name": "a", "mimeType": "x", "sizeBytes": 50 * 1024 * 1024 + 1}),
            "[\"sizeBytes\"]",
        ),
    ] {
        let message =
            die_message(call(&store, "attachments.createUploadUrl", payload.clone()).await);
        assert!(
            message.ends_with(&format!("\n  at {path}")),
            "{payload}: {message}"
        );
    }

    // Files are accepted and served as downloads.
    let issued = success(
        call(
            &store,
            "attachments.createUploadUrl",
            json!({"type": "file", "name": " notes ä.txt ", "mimeType": "text/plain", "sizeBytes": 2}),
        )
        .await,
    );
    let upload_url = issued["relativeUrl"].as_str().unwrap();
    assert_eq!(http(addr, "POST", upload_url, &[], b"hi").await.status, 204);
    let id = issued["attachmentId"].as_str().unwrap();
    let asset = success(
        call(
            &store,
            "assets.createUrl",
            json!({"resource": {"_tag": "attachment", "attachmentId": id}}),
        )
        .await,
    );
    let response = http(
        addr,
        "GET",
        asset["relativeUrl"].as_str().unwrap(),
        &[],
        b"",
    )
    .await;
    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"hi");
    assert_eq!(response.header("content-type"), Some("text/plain"));
    assert_eq!(
        response.header("content-disposition"),
        Some("attachment; filename=\"notes _.txt\"; filename*=UTF-8''notes%20%C3%A4.txt")
    );

    // Favicons are always "missing".
    let favicon = success(
        call(
            &store,
            "assets.createUrl",
            json!({"resource": {"_tag": "project-favicon", "cwd": "/repo"}}),
        )
        .await,
    );
    assert!(
        favicon["relativeUrl"]
            .as_str()
            .unwrap()
            .ends_with("/project-favicon-missing")
    );

    // Unsupported resources fail with a typed error that echoes them.
    let resource = json!({"_tag": "workspace-file", "threadId": "t1", "path": "a.png"});
    assert_eq!(
        fail_error(call(&store, "assets.createUrl", json!({"resource": resource})).await),
        json!({"_tag": "AssetWorkspaceContextNotFoundError", "resource": resource})
    );
    die_message(call(&store, "assets.createUrl", json!({})).await);

    // Deleting an unknown id succeeds; a blank id is a bad payload.
    assert_eq!(
        success(
            call(
                &store,
                "attachments.delete",
                json!({"attachmentId": "att_nope"})
            )
            .await
        ),
        Value::Null
    );
    die_message(call(&store, "attachments.delete", json!({"attachmentId": " "})).await);

    assert!(store.handle("server.probe", json!({})).await.is_none());
}
