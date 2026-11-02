//! Round-trips of the ios-surface examples for the non-orchestration t3 shapes.

use j3_protocol::t3::assets::*;
use j3_protocol::t3::auth::*;
use j3_protocol::t3::background::*;
use j3_protocol::t3::environment::*;
use j3_protocol::t3::environment_http::*;
use j3_protocol::t3::filesystem::*;
use j3_protocol::t3::project::*;
use j3_protocol::t3::server::*;
use j3_protocol::t3::thread_search::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

fn round_trip<T: Serialize + DeserializeOwned + std::fmt::Debug>(value: Value) -> T {
    let decoded: T = serde_json::from_value(value.clone())
        .unwrap_or_else(|err| panic!("decode {}: {err}", std::any::type_name::<T>()));
    let encoded = serde_json::to_value(&decoded).expect("encode");
    assert_eq!(encoded, value, "{}", std::any::type_name::<T>());
    decoded
}

fn descriptor() -> Value {
    json!({"environmentId":"env-shieldy-3f9a","label":"shieldy","platform":{"os":"linux","arch":"x64"},
     "serverVersion":"0.1.0","orchestrationProtocolVersion":2,
     "capabilities":{"repositoryIdentity":false,"attachmentUploads":true,"threadStop":true,
       "providerWorkspaceSnapshotsOnDemand":true}})
}

fn provider() -> Value {
    json!({"instanceId":"omp","driver":"pi","displayName":"omp","enabled":true,"installed":true,"version":"14.2.0",
       "status":"ready","auth":{"status":"authenticated"},"checkedAt":"2026-11-01T12:00:00.000Z",
       "models":[{"slug":"anthropic/claude-sonnet-4-5","name":"Claude Sonnet 4.5","isCustom":false,"isDefault":true,
         "capabilities":{"optionDescriptors":[{"id":"thinking","label":"Thinking","type":"select",
           "options":[{"id":"off","label":"Off"},{"id":"low","label":"Low"},{"id":"medium","label":"Medium","isDefault":true},{"id":"high","label":"High"}]}]}}],
       "slashCommands":[],"skills":[]})
}

#[test]
fn h1_descriptor() {
    let d: ExecutionEnvironmentDescriptor = round_trip(descriptor());
    assert_eq!(d.orchestration_protocol_version, Some(2));
}

#[test]
fn h1_unknown_machine_decodes_as_absent() {
    let mut value = descriptor();
    value["platform"]["machine"] = json!("toaster");
    let d: ExecutionEnvironmentDescriptor = serde_json::from_value(value).unwrap();
    assert_eq!(d.platform.machine, None);
    let mut value = descriptor();
    value["platform"]["machine"] = json!("mac-mini");
    let d: ExecutionEnvironmentDescriptor = round_trip(value);
    assert_eq!(d.platform.machine, Some(EnvironmentMachineKind::MacMini));
}

#[test]
fn empty_capabilities_default_repository_identity() {
    let caps: ExecutionEnvironmentCapabilities = serde_json::from_value(json!({})).unwrap();
    assert_eq!(
        serde_json::to_value(&caps).unwrap(),
        json!({"repositoryIdentity": false})
    );
}

#[test]
fn h3_session() {
    let s: AuthSessionState = round_trip(json!({"authenticated":true,
     "auth":{"policy":"unsafe-no-auth","bootstrapMethods":[],"sessionMethods":[],"sessionCookieName":"j3_session"},
     "scopes":["orchestration:read","orchestration:operate"],
     "permissions":["orchestration:read","orchestration:operate","filesystem:read","filesystem:write","settings:write"]}));
    assert_eq!(s.permissions.unwrap().len(), 5);
}

#[test]
fn h3_unknown_permission_dropped() {
    let s: AuthSessionState = serde_json::from_value(json!({"authenticated":true,
     "auth":{"policy":"unsafe-no-auth","bootstrapMethods":[],"sessionMethods":[],"sessionCookieName":"j3"},
     "permissions":["orchestration:read","future:scope"]}))
    .unwrap();
    assert_eq!(
        s.permissions,
        Some(vec![AuthEnvironmentScope::OrchestrationRead])
    );
}

#[test]
fn m1_snapshot_event() {
    let event = json!({"version":1,"type":"snapshot","config":{
     "environment":descriptor(),
     "auth":{"policy":"unsafe-no-auth","bootstrapMethods":[],"sessionMethods":[],"sessionCookieName":"j3_session"},
     "cwd":"/home/jacob","keybindingsConfigPath":"/home/jacob/.j3/keybindings.json","keybindings":[],"issues":[],
     "providers":[provider()],
     "availableEditors":[],
     "observability":{"logsDirectoryPath":"/home/jacob/.j3/logs","localTracingEnabled":false,"otlpTracesEnabled":false,"otlpMetricsEnabled":false,"otlpLogsEnabled":false},
     "settings":{}}});
    let decoded: ServerConfigStreamEvent = round_trip(event);
    assert!(matches!(decoded, ServerConfigStreamEvent::Snapshot(_)));
}

#[test]
fn m1_observability_logs_default() {
    let o: ServerObservability = serde_json::from_value(json!({"logsDirectoryPath":"/l","localTracingEnabled":false,"otlpTracesEnabled":false,"otlpMetricsEnabled":false})).unwrap();
    assert!(!o.otlp_logs_enabled);
}

#[test]
fn m1_payload_and_provider_statuses() {
    round_trip::<SubscribeServerConfigInput>(json!({"workspaceSnapshotsOnDemand":true}));
    let decoded: ServerConfigStreamEvent = round_trip(
        json!({"version":1,"type":"providerStatuses","payload":{"providers":[provider()]}}),
    );
    assert!(matches!(
        decoded,
        ServerConfigStreamEvent::ProviderStatuses(_)
    ));
}

#[test]
fn m1_unknown_event_kept_verbatim() {
    let value = json!({"version":1,"type":"futureThing","x":[1]});
    let decoded: ServerConfigStreamEvent = round_trip(value);
    assert!(matches!(decoded, ServerConfigStreamEvent::Unknown(_)));
}

#[test]
fn m2_probe() {
    round_trip::<ServerProbeInput>(json!({}));
    round_trip::<ServerProbeResult>(json!({}));
}

#[test]
fn m3_refresh_providers() {
    round_trip::<ServerRefreshProvidersInput>(
        json!({"instanceId":"omp","cwd":"/home/jacob/Developer/j3code"}),
    );
    let mut p = provider();
    p["workspaceSnapshots"] = json!([{"cwd":"/home/jacob/Developer/j3code","checkedAt":"2026-11-01T12:00:00.000Z",
       "slashCommands":[{"name":"compact","description":"Compact the context","input":{"hint":"[focus]"}}],
       "skills":[{"name":"jev","path":"/home/jacob/.omp/agent/skills/jev/SKILL.md","enabled":true,"description":"Calibrated judgments"}]}]);
    p["installedVersion"] = Value::Null;
    round_trip::<ServerProviderUpdatedPayload>(json!({"providers":[p]}));
}

#[test]
fn provider_defaults_slash_commands_and_skills() {
    let mut p = provider();
    p.as_object_mut().unwrap().remove("slashCommands");
    p.as_object_mut().unwrap().remove("skills");
    let decoded: ServerProvider = serde_json::from_value(p).unwrap();
    assert!(decoded.slash_commands.is_empty() && decoded.skills.is_empty());
    assert_eq!(decoded.installed_version, None);
}

#[test]
fn m10_upload_url() {
    let input: AttachmentCreateUploadUrlInput = round_trip(
        json!({"type":"image","name":"IMG_0001.jpg","mimeType":"image/jpeg","sizeBytes":183022}),
    );
    assert!(matches!(input, AttachmentCreateUploadUrlInput::Image(_)));
    let input: AttachmentCreateUploadUrlInput =
        round_trip(json!({"name":"IMG_0001.jpg","mimeType":"image/jpeg","sizeBytes":183022}));
    let AttachmentCreateUploadUrlInput::Image(image) = &input else {
        panic!("expected image, got {input:?}");
    };
    assert!(image.kind.is_none());
    let input: AttachmentCreateUploadUrlInput = round_trip(
        json!({"type":"file","name":"notes.pdf","mimeType":"application/pdf","sizeBytes":1024}),
    );
    assert!(matches!(input, AttachmentCreateUploadUrlInput::File(_)));
    round_trip::<AttachmentCreateUploadUrlResult>(
        json!({"attachmentId":"att_9k2m","relativeUrl":"/api/attachments/upload/att_9k2m.sig","expiresAt":1793534400000_u64}),
    );
}

#[test]
fn m11_asset_url() {
    round_trip::<AssetCreateUrlInput>(
        json!({"resource":{"_tag":"attachment","attachmentId":"att_9k2m","mimeType":"image/jpeg","disposition":"inline"}}),
    );
    round_trip::<AssetCreateUrlInput>(
        json!({"resource":{"_tag":"project-favicon","cwd":"/home/jacob/Developer/j3code","path":"favicon.png"}}),
    );
    round_trip::<AssetCreateUrlResult>(
        json!({"relativeUrl":"/api/assets/tok_ab12/IMG_0001.jpg","expiresAt":1793534400000_u64,"imageDimensions":{"width":640,"height":480}}),
    );
}

#[test]
fn m11_access_error_tags() {
    let resource = AssetResource::Attachment {
        attachment_id: "att_9k2m".into(),
        file_name: None,
        mime_type: None,
        disposition: None,
        render_intent: None,
    };
    let error = AssetAccessError::AttachmentNotFound(AssetAttachmentNotFoundError::new(resource));
    let value = serde_json::to_value(&error).unwrap();
    assert_eq!(
        value,
        json!({"_tag":"AssetAttachmentNotFoundError","resource":{"_tag":"attachment","attachmentId":"att_9k2m"}})
    );
    assert_eq!(round_trip::<AssetAccessError>(value), error);
    let legacy: AssetAccessError = round_trip(json!({"_tag":"AssetGitHubMediaUrlValidationError"}));
    assert!(matches!(
        legacy,
        AssetAccessError::SourceControlMediaUrlValidation(_)
    ));
}

#[test]
fn m12_projects_mutate() {
    let m: ProjectMutation = round_trip(
        json!({"type":"project.create","commandId":"0b1e","projectId":"8c7d","title":"j3code","workspaceRoot":"/home/jacob/Developer/j3code"}),
    );
    assert!(matches!(m, ProjectMutation::Create(_)));
    round_trip::<ProjectMutation>(
        json!({"type":"project.update","commandId":"c2","projectId":"8c7d","title":"j3","defaultModelSelection":null,"projectIcon":{"kind":"emoji","emoji":"🦀"},"archived":true}),
    );
    round_trip::<ProjectMutation>(
        json!({"type":"project.delete","commandId":"c3","projectId":"8c7d","force":true}),
    );
    round_trip::<Project>(
        json!({"id":"8c7d","title":"j3code","workspaceRoot":"/home/jacob/Developer/j3code","defaultModelSelection":null,"scripts":[],"createdAt":"2026-11-01T12:00:00.000Z","updatedAt":"2026-11-01T12:00:00.000Z","deletedAt":null}),
    );
    round_trip::<ProjectShell>(
        json!({"id":"8c7d","title":"j3code","workspaceRoot":"/w","repositoryIdentity":null,"defaultModelSelection":null,"scripts":[{"id":"s","name":"Test","command":"cargo test","icon":"test","runOnWorktreeCreate":false,"async":true}],"createdAt":"2026-11-01T12:00:00.000Z","updatedAt":"2026-11-01T12:00:00.000Z"}),
    );
}

#[test]
fn tagged_errors_serialize_with_tag() {
    let error = ProjectMutationError::new("0b1e".into(), "boom");
    assert_eq!(
        serde_json::to_value(&error).unwrap(),
        json!({"_tag":"ProjectMutationError","commandId":"0b1e","message":"boom"})
    );
    let error = EnvironmentAuthorizationError::new("no", AuthEnvironmentScope::FilesystemRead);
    round_trip::<EnvironmentAuthorizationError>(serde_json::to_value(&error).unwrap());
    assert!(
        serde_json::from_value::<ProjectMutationError>(
            json!({"_tag":"Other","commandId":"c","message":"m"})
        )
        .is_err()
    );
    round_trip::<ServerSettingsError>(
        json!({"_tag":"ServerSettingsError","settingsPath":"/s.json","operation":"write-file","cause":{"x":1}}),
    );
    round_trip::<KeybindingsConfigError>(
        json!({"_tag":"KeybindingsConfigParseError","configPath":"/k","detail":"bad"}),
    );
    round_trip::<ProviderSetupError>(
        json!({"_tag":"ProviderSetupError","instanceId":"omp","operation":"refresh","detail":"d"}),
    );
    round_trip::<AttachmentUploadSigningKeyError>(
        json!({"_tag":"AttachmentUploadSigningKeyError","cause":"missing key"}),
    );
}

#[test]
fn m13_filesystem_browse() {
    round_trip::<FilesystemBrowseInput>(json!({"partialPath":"~/Developer/"}));
    round_trip::<FilesystemBrowseResult>(
        json!({"parentPath":"/home/jacob/Developer","entries":[{"name":"j3code","fullPath":"/home/jacob/Developer/j3code"}]}),
    );
    round_trip::<FilesystemBrowseError>(
        json!({"_tag":"FilesystemBrowseError","partialPath":"C:\\x","failure":"windows_path_unsupported","message":"no"}),
    );
}

#[test]
fn m14_client_activity() {
    round_trip::<ClientActivityReportInput>(json!({
        "environmentId":"env-shieldy-3f9a","clientId":"ios-1","clientKind":"mobile",
        "visible":true,"focused":true,"recentlyInteracted":false,"appState":"active","lowPowerMode":"false",
        "scopes":[{"type":"server-config"},{"type":"provider-status","instanceId":"omp"},{"type":"thread","threadId":"t1"},{"type":"vcs-status","cwd":"/w"}],
        "ttlMs":30000,"observedAt":"2026-11-01T12:00:00.000Z"}));
}

#[test]
fn m15_attachment_delete() {
    round_trip::<AttachmentDeleteInput>(json!({"attachmentId":"att_9k2m"}));
}

#[test]
fn m17_thread_search() {
    round_trip::<OrchestrationSearchThreadsInput>(json!({"query":"refactor","limit":20}));
    round_trip::<OrchestrationSearchThreadsResult>(json!({"matches":[]}));
    round_trip::<OrchestrationSearchThreadsResult>(
        json!({"matches":[{"threadId":"t1","projectId":"p1","source":"user","snippet":"refactor the parser","messageCreatedAt":null}]}),
    );
}

#[test]
fn m19_read_file() {
    round_trip::<ProjectReadFileInput>(
        json!({"cwd":"/home/jacob/Developer/j3code","relativePath":"t3.json","allowMissing":true}),
    );
    round_trip::<ProjectReadFileResult>(
        json!({"relativePath":"t3.json","contents":"","byteLength":0,"truncated":false,"exists":false}),
    );
}

#[test]
fn m19_read_file_text_and_errors() {
    round_trip::<ProjectReadFileResult>(
        json!({"relativePath":"src/main.rs","contents":"fn main() {}\n","byteLength":13,"truncated":false}),
    );
    let error: ProjectReadFileError = round_trip(json!({
        "_tag":"ProjectReadFileError","cwd":"/w","relativePath":"logo.png","failure":"binary_file",
        "resolvedPath":"/w/logo.png","message":"Workspace file 'logo.png' in '/w' is binary and cannot be previewed as text."}));
    assert_eq!(error.failure, Some(ProjectFileFailure::BinaryFile));
    round_trip::<ProjectReadFileError>(json!({
        "_tag":"ProjectReadFileError","cwd":"/w","relativePath":"gone.txt","failure":"operation_failed",
        "operation":"realpath-target","operationPath":"/w/gone.txt","message":"No such file or directory"}));
}

#[test]
fn list_entries() {
    assert_eq!(PROJECTS_LIST_ENTRIES, "projects.listEntries");
    assert_eq!(PROJECTS_READ_FILE, "projects.readFile");
    round_trip::<ProjectListEntriesInput>(json!({"cwd":"/home/jacob/Developer/j3code"}));
    let input: ProjectListEntriesInput =
        round_trip(json!({"cwd":"/home/jacob/Developer/j3code","directoryPath":"crates/j3-core"}));
    assert_eq!(input.directory_path.as_deref(), Some("crates/j3-core"));
    let result: ProjectListEntriesResult = round_trip(json!({"entries":[
        {"path":"crates/j3-core/src","kind":"directory"},
        {"path":"crates/j3-core/target","kind":"directory","ignored":true},
        {"path":"crates/j3-core/Cargo.toml","kind":"file"}],"truncated":false}));
    assert_eq!(result.entries[1].ignored, Some(true));
    assert_eq!(result.entries[2].kind, ProjectEntryKind::File);
    let error: ProjectListEntriesError = round_trip(json!({
        "_tag":"ProjectListEntriesError","cwd":"/w","failure":"directory_list_failed",
        "detail":"../etc is outside the workspace","message":"Failed to list workspace entries in '/w'."}));
    assert_eq!(
        error.failure,
        Some(ProjectEntriesFailure::DirectoryListFailed)
    );
}

#[test]
fn m20_search_entries() {
    round_trip::<ProjectSearchEntriesInput>(
        json!({"cwd":"/home/jacob/Developer/j3code","query":"main","limit":50,"kind":"file"}),
    );
    round_trip::<ProjectSearchEntriesResult>(
        json!({"entries":[{"path":"src/main.rs","kind":"file"}],"truncated":false}),
    );
    round_trip::<ProjectSearchEntriesResult>(json!({"entries":[],"truncated":false}));
}

#[test]
fn m21_get_metadata() {
    round_trip::<FilesystemGetMetadataInput>(json!({"paths":["/a","~/b"]}));
    round_trip::<FilesystemGetMetadataResult>(
        json!({"entries":[null,{"kind":"file","byteLength":12,"mimeType":"text/plain"}]}),
    );
}

#[test]
fn m22_update_settings() {
    round_trip::<ServerUpdateSettingsInput>(
        json!({"patch":{"favoriteModels":[{"instanceId":"omp","model":"anthropic/claude-sonnet-4-5"}]}}),
    );
}

#[test]
fn http_error_bodies() {
    round_trip::<EnvironmentAuthInvalidError>(
        json!({"_tag":"EnvironmentAuthInvalidError","code":"auth_invalid","reason":"missing_credential","dpopFailureReason":"replay","traceId":"tr"}),
    );
    round_trip::<EnvironmentScopeRequiredError>(
        json!({"_tag":"EnvironmentScopeRequiredError","code":"insufficient_scope","requiredScope":"orchestration:read","requiredPermission":"filesystem:read","traceId":"tr"}),
    );
    let e = EnvironmentResourceNotFoundError::new(
        EnvironmentResourceNotFoundReason::ThreadNotFound,
        "tr",
    );
    assert_eq!(
        serde_json::to_value(&e).unwrap(),
        json!({"_tag":"EnvironmentResourceNotFoundError","code":"not_found","reason":"thread_not_found","traceId":"tr"})
    );
    round_trip::<OrchestrationProtocolIncompatibleBody>(
        json!({"code":"orchestration_protocol_incompatible","message":"Update this client.","orchestrationProtocolVersion":2}),
    );
}
