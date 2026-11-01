/**
 * j3 conformance client: drives a server the way t3's client-runtime / mobile app does and lets
 * the contracts' schemas (packages/contracts) judge every response and stream item.
 *
 *   npm run conformance -- --url http://127.0.0.1:3780 [--model <provider/id>] [--project <dir>]
 */
import { randomUUID } from "node:crypto";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

import {
  AuthFilesystemReadScope,
  AuthOrchestrationOperateScope,
  AuthOrchestrationReadScope,
  ChatAttachmentId,
  CommandId,
  type ExecutionEnvironmentDescriptor,
  MessageId,
  type ModelSelection,
  ORCHESTRATION_PROTOCOL_VERSION,
  type OrchestrationV2Command,
  type OrchestrationV2Run,
  type OrchestrationV2RuntimeRequest,
  type OrchestrationV2TurnItem,
  ProjectId,
  ProviderInstanceId,
  RunId,
  ThreadId,
  sessionGrantsScope,
} from "@t3tools/contracts";

import { downloadBytes, environmentHttp, resolveRelativeUrl, uploadBytes } from "./http.ts";
import { Runner, check, fail } from "./report.ts";
import { Session, mobileSocketUrl } from "./session.ts";
import { ACTIVE_RUN_STATUSES, ShellView, ThreadView } from "./views.ts";
import type { Waiter } from "./watcher.ts";

const HELP = `Usage: npm run conformance -- --url <http-base-url> [options]

Options:
  --url <url>          Server HTTP base URL (default http://127.0.0.1:3780)
  --model <slug>       omp model slug for every modelSelection, e.g. anthropic/claude-sonnet-4-5
                       (default: the first model of provider "omp")
  --project <dir>      Project directory on the server host (default: this repository's root)
  --timeout <seconds>  Wait budget for each agent-driven step (default 300)
  --frames <file>      Append every WebSocket text frame as JSONL to <file>
  -h, --help           Show this help`;

const { values: args } = parseArgs({
  options: {
    url: { type: "string", default: "http://127.0.0.1:3780" },
    model: { type: "string" },
    project: { type: "string" },
    timeout: { type: "string", default: "300" },
    frames: { type: "string" },
    help: { type: "boolean", short: "h", default: false },
  },
  strict: true,
});

if (args.help) {
  process.stdout.write(`${HELP}\n`);
  process.exit(0);
}

const baseUrl = args.url;
const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const projectDir = resolve(args.project ?? repoRoot);
const agentTimeoutMs = Number(args.timeout) * 1000;
if (!Number.isFinite(agentTimeoutMs) || agentTimeoutMs <= 0) {
  process.stderr.write(`--timeout must be a positive number of seconds\n`);
  process.exit(2);
}
/** Budget for non-agent waits: unary replies and the stream items they cause. */
const QUICK_MS = 15_000;
/** The probe timeout of the client supervisor (`CR/connection/supervisor.ts`). */
const PROBE_MS = 3_000;
const OMP_INSTANCE_ID = "omp";

const TEST_PREAMBLE = "This is an automated protocol conformance test.";
const ASK_PROMPT = `${TEST_PREAMBLE} Use the ask tool exactly once to ask me which color I prefer, with exactly two options: "red" and "blue". Do not use any other tool. After I answer, reply with one short sentence that names the color I picked. The attached image is a test pixel; ignore it.`;
const QUEUE_FIRST_PROMPT = `${TEST_PREAMBLE} Use the bash tool to run exactly this command: sleep 15. Then reply with the single word: first.`;
const QUEUED_PROMPT = `${TEST_PREAMBLE} Do not use any tools. Reply with the single word: second.`;
const INTERRUPT_PROMPT = `${TEST_PREAMBLE} Use the bash tool to run exactly this command: sleep 30. Then reply with the single word: done.`;
const IDEMPOTENT_PROMPT = `${TEST_PREAMBLE} Do not use any tools. Reply with the single word: ok.`;

/** A 1×1 PNG. */
const PNG_BYTES = Uint8Array.from(
  Buffer.from(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==",
    "base64",
  ),
);
const IMAGE_NAME = "conformance.png";
const IMAGE_MIME = "image/png";

const TERMINAL_FAILURE_STATUSES: Readonly<Record<string, true>> = {
  failed: true,
  cancelled: true,
  interrupted: true,
  rolled_back: true,
};

const runner = new Runner();
const http = environmentHttp(baseUrl);
const socketUrl = mobileSocketUrl(baseUrl);
const sessions: Session[] = [];
const stamp = new Date().toISOString();

const openSession = (label: string): Session => {
  const session = new Session({ label, socketUrl, framesFile: args.frames });
  sessions.push(session);
  return session;
};

/** Waits for the socket to open; on failure reports the transport error the RPC layer saw. */
async function awaitOpen(session: Session): Promise<void> {
  const timeout = Promise.withResolvers<"timeout">();
  const timer = setTimeout(() => timeout.resolve("timeout"), QUICK_MS + 1_000);
  const state = await Promise.race([session.connected, timeout.promise]);
  clearTimeout(timer);
  if (state === "open") return;
  if (state === "timeout") fail(`WebSocket ${socketUrl} did not open within ${QUICK_MS / 1000}s`);
  // The protocol broadcasts the socket error to every pending request, including the config
  // subscription opened with the session.
  await session.config.until("the socket error", 2_000, () => undefined).catch(() => undefined);
  const ended = session.config.ended;
  const rpcError = ended === undefined || ended.ok ? "socket closed before opening" : ended.error;
  // WebSocket error events carry no detail; a plain GET of the same URL shows refusals and 4xx.
  const httpUrl = socketUrl.replace(/^ws/, "http");
  const probe = await fetch(httpUrl, { signal: AbortSignal.timeout(5_000) }).then(
    async (response) =>
      `plain GET of the URL answered ${response.status}: ${(await response.text()).slice(0, 300)}`,
    (error: unknown) =>
      `plain GET of the URL failed: ${error instanceof Error && error.cause instanceof Error ? error.cause.message : String(error)}`,
  );
  fail(`WebSocket ${socketUrl} did not open:\n${rpcError}\n${probe}`);
}

/** Handshake per `CR/rpc/session.ts`: the first config item is a snapshot for this environment. */
async function handshake(session: Session, descriptor: ExecutionEnvironmentDescriptor) {
  const config = await session.firstConfig(QUICK_MS);
  check(
    config.environment.environmentId === descriptor.environmentId,
    `config.environment.environmentId "${config.environment.environmentId}" does not match the descriptor's "${descriptor.environmentId}" (client raises environmentMismatchError)`,
  );
  return config;
}

const dispatch = (session: Session, command: OrchestrationV2Command) =>
  session.call(`orchestration.dispatchCommand ${command.type}`, QUICK_MS, (client) =>
    client["orchestration.dispatchCommand"](command),
  );

const newCommandId = () => CommandId.make(randomUUID());

const isTerminalFailure = (run: OrchestrationV2Run | undefined): run is OrchestrationV2Run =>
  run !== undefined && TERMINAL_FAILURE_STATUSES[run.status] === true;

function findItem<T extends OrchestrationV2TurnItem["type"]>(
  view: ThreadView,
  type: T,
  match: (item: Extract<OrchestrationV2TurnItem, { type: T }>) => boolean,
): Extract<OrchestrationV2TurnItem, { type: T }> | undefined {
  for (const item of view.turnItems.values()) {
    if (item.type === type && match(item as Extract<OrchestrationV2TurnItem, { type: T }>)) {
      return item as Extract<OrchestrationV2TurnItem, { type: T }>;
    }
  }
  return undefined;
}

/**
 * Waits for the run of `messageId` to have reached `status` (now or earlier), failing fast if it
 * ends in another state.
 */
function awaitRunStatus(
  watcher: Waiter,
  view: ThreadView,
  messageId: string,
  status: OrchestrationV2Run["status"],
  timeoutMs: number,
): Promise<OrchestrationV2Run> {
  return watcher.until(`the run for message ${messageId} to be "${status}"`, timeoutMs, () => {
    const run = view.runForMessage(messageId);
    if (run === undefined) return undefined;
    if (view.runStatusHistory.get(run.id)?.includes(status) === true) return run;
    if (TERMINAL_FAILURE_STATUSES[run.status] === true || run.status === "completed") {
      fail(
        `run ${run.id} for message ${messageId} ended as "${run.status}" (history: ${view.runStatusHistory.get(run.id)?.join("→")})`,
      );
    }
    return undefined;
  });
}

// ---------------------------------------------------------------------------------------------

process.stdout.write(`j3 conformance against ${baseUrl}\nWebSocket ${socketUrl}\n\n`);

let descriptor: ExecutionEnvironmentDescriptor | undefined;
let modelSelection: ModelSelection | undefined;
const session1 = openSession("session1");

await runner.step("descriptor", "GET /.well-known/t3/environment", [], async () => {
  descriptor = await http.descriptor();
  check(
    descriptor.orchestrationProtocolVersion === ORCHESTRATION_PROTOCOL_VERSION,
    `orchestrationProtocolVersion is ${descriptor.orchestrationProtocolVersion ?? "absent (the client assumes 1)"}, expected ${ORCHESTRATION_PROTOCOL_VERSION}`,
  );
  return `environmentId=${descriptor.environmentId} serverVersion=${descriptor.serverVersion}`;
});

await runner.step("ws.connect", "WebSocket /ws with the mobile query params", [], async () => {
  await awaitOpen(session1);
});

await runner.step(
  "config.handshake",
  "subscribeServerConfig: first snapshot, environmentId, provider omp",
  ["descriptor", "ws.connect"],
  async () => {
    const config = await handshake(session1, descriptor!);
    const provider = config.providers.find((entry) => entry.instanceId === OMP_INSTANCE_ID);
    if (provider === undefined) {
      return fail(
        `no provider with instanceId "${OMP_INSTANCE_ID}"; got [${config.providers.map((entry) => entry.instanceId).join(", ")}]`,
      );
    }
    // The model picker skips these (`M/lib/modelOptions.ts`).
    check(provider.enabled, `provider omp is disabled`);
    check(provider.installed, `provider omp is not installed`);
    check(provider.auth.status !== "unauthenticated", `provider omp is unauthenticated`);
    check(provider.models.length > 0, `provider omp lists no models`);
    const model = args.model ?? provider.models[0]!.slug;
    // The outbox refuses a selection that is not in the config.
    check(
      provider.models.some((entry) => entry.slug === model),
      `--model ${model} is not among omp's models: ${provider.models.map((entry) => entry.slug).join(", ")}`,
    );
    modelSelection = { instanceId: ProviderInstanceId.make(OMP_INSTANCE_ID), model };
    return `omp (driver ${provider.driver}, ${provider.status}) with ${provider.models.length} model(s); using ${model}`;
  },
);

await runner.step("auth.session", "GET /api/auth/session grants", [], async () => {
  const session = await http.authSession();
  const required = [AuthOrchestrationReadScope, AuthOrchestrationOperateScope, AuthFilesystemReadScope];
  const missing = required.filter((scope) => !sessionGrantsScope(session, scope));
  check(
    missing.length === 0,
    `the session does not grant ${missing.join(", ")} (authenticated=${session.authenticated}); the app blocks those actions locally`,
  );
  return `policy=${session.auth.policy} grants ${required.join(", ")}`;
});

await runner.step("server.probe", "server.probe", ["ws.connect"], async () => {
  await session1.call("server.probe", PROBE_MS, (client) => client["server.probe"]({}));
});

await runner.step(
  "filesystem.browse",
  "filesystem.browse of the project's parent",
  ["ws.connect"],
  async () => {
    const parent = dirname(projectDir);
    // The add-project screen sends the directory part of the typed path.
    const partialPath = parent.endsWith("/") ? parent : `${parent}/`;
    const result = await session1.call("filesystem.browse", QUICK_MS, (client) =>
      client["filesystem.browse"]({ partialPath }),
    );
    check(
      result.entries.some((entry) => entry.fullPath.replace(/\/+$/, "") === projectDir),
      `entries of ${partialPath} do not include ${projectDir}: ${result.entries
        .slice(0, 20)
        .map((entry) => entry.fullPath)
        .join(", ")}`,
    );
    return `parentPath=${result.parentPath}, ${result.entries.length} entries`;
  },
);

const shellView = new ShellView(undefined);
let shell: Waiter | undefined;

await runner.step(
  "shell.subscribe",
  "orchestration.subscribeShell: snapshot then synchronized",
  ["config.handshake"],
  async () => {
    const watcher = session1.watch(
      "orchestration.subscribeShell",
      (client) => client["orchestration.subscribeShell"]({ requestCompletionMarker: true }),
      (item) => shellView.apply(item),
      () => shellView.describe(),
    );
    shell = watcher;
    await watcher.until("snapshot then synchronized", QUICK_MS, () => {
      if (shellView.firstKind !== undefined && shellView.firstKind !== "snapshot") {
        fail(`the first shell item is "${shellView.firstKind}", expected "snapshot"`);
      }
      return shellView.synchronized ? true : undefined;
    });
    return `snapshotSequence=${shellView.snapshotSequence}, ${shellView.projects.size} project(s), ${shellView.threads.size} thread(s)`;
  },
);

const projectId = ProjectId.make(randomUUID());

await runner.step("project.create", "projects.mutate project.create", ["ws.connect"], async () => {
  const project = await session1.call("projects.mutate", QUICK_MS, (client) =>
    client["projects.mutate"]({
      type: "project.create",
      commandId: newCommandId(),
      projectId,
      title: `j3 conformance ${stamp}`,
      workspaceRoot: projectDir,
    }),
  );
  check(project.id === projectId, `returned project id ${project.id}, expected ${projectId}`);
  return `workspaceRoot=${project.workspaceRoot}`;
});

await runner.step(
  "shell.project.updated",
  "subscribeShell delivers project.updated for the new project",
  ["shell.subscribe", "project.create"],
  async () => {
    await shell!.until(`project.updated for ${projectId}`, QUICK_MS, () =>
      shellView.projects.has(projectId) ? true : undefined,
    );
  },
);

let attachment:
  | { id: string; relativeUrl: string; assetUrl?: string }
  | undefined;

await runner.step(
  "attachment.upload",
  "attachments.createUploadUrl → POST bytes",
  ["ws.connect"],
  async () => {
    const created = await session1.call("attachments.createUploadUrl", QUICK_MS, (client) =>
      client["attachments.createUploadUrl"]({
        name: IMAGE_NAME,
        mimeType: IMAGE_MIME,
        sizeBytes: PNG_BYTES.byteLength,
      }),
    );
    const status = await uploadBytes(
      resolveRelativeUrl(baseUrl, created.relativeUrl),
      PNG_BYTES,
      IMAGE_MIME,
    );
    attachment = { id: created.attachmentId, relativeUrl: created.relativeUrl };
    return `attachmentId=${created.attachmentId} → ${status}`;
  },
);

await runner.step(
  "attachment.asset",
  "assets.createUrl → GET returns the uploaded bytes",
  ["attachment.upload"],
  async () => {
    const created = await session1.call("assets.createUrl", QUICK_MS, (client) =>
      client["assets.createUrl"]({
        resource: {
          _tag: "attachment",
          attachmentId: attachment!.id,
          fileName: IMAGE_NAME,
          mimeType: IMAGE_MIME,
        },
      }),
    );
    const url = resolveRelativeUrl(baseUrl, created.relativeUrl);
    const download = await downloadBytes(url);
    check(download.status === 200, `GET ${url} answered ${download.status}, expected 200`);
    check(
      Buffer.from(download.bytes).equals(PNG_BYTES),
      `GET ${url} returned ${download.bytes.byteLength} bytes that differ from the ${PNG_BYTES.byteLength} uploaded`,
    );
    check(
      download.contentType?.startsWith(IMAGE_MIME) === true,
      `GET ${url} content-type is ${download.contentType ?? "absent"}, expected ${IMAGE_MIME}`,
    );
    attachment!.assetUrl = created.relativeUrl;
    return created.relativeUrl;
  },
);

const threadId = ThreadId.make(randomUUID());
const firstMessageId = MessageId.make(randomUUID());

await runner.step(
  "thread.launch",
  "orchestration.launchThread with initialMessage (+ image)",
  ["config.handshake", "project.create"],
  async () => {
    const attachments =
      attachment === undefined
        ? []
        : [
            {
              type: "image" as const,
              id: ChatAttachmentId.make(attachment.id),
              name: IMAGE_NAME,
              mimeType: IMAGE_MIME,
              sizeBytes: PNG_BYTES.byteLength,
            },
          ];
    const result = await session1.call("orchestration.launchThread", QUICK_MS, (client) =>
      client["orchestration.launchThread"]({
        commandId: newCommandId(),
        creationSource: "mobile",
        threadId,
        projectId,
        title: "Conformance: ask tool",
        generateTitle: true,
        modelSelection: modelSelection!,
        runtimeMode: "full-access",
        interactionMode: "default",
        workspaceStrategy: { type: "root" },
        initialMessage: { messageId: firstMessageId, text: ASK_PROMPT, attachments },
      }),
    );
    check(result.threadId === threadId, `launch returned threadId ${result.threadId}, expected ${threadId}`);
    check(result.projection.thread.id === threadId, `projection.thread.id is ${result.projection.thread.id}`);
    check(!result.resumed, `resumed is true for a fresh commandId`);
    return `sequence=${result.sequence ?? "absent"}, ${attachments.length} attachment(s)`;
  },
);

const view = new ThreadView(threadId, undefined);
let thread: Waiter | undefined;

await runner.step(
  "thread.subscribe",
  "orchestration.subscribeThread: snapshot then synchronized",
  ["thread.launch"],
  async () => {
    const watcher = session1.watch(
      "orchestration.subscribeThread",
      (client) =>
        client["orchestration.subscribeThread"]({
          threadId,
          requestCompletionMarker: true,
          acceptDedupedBodies: true,
        }),
      (item) => view.apply(item),
      () => view.describe(),
    );
    thread = watcher;
    await watcher.until("snapshot then synchronized", QUICK_MS, () => {
      if (view.firstKind !== undefined && view.firstKind !== "snapshot") {
        fail(`the first thread item is "${view.firstKind}", expected "snapshot"`);
      }
      return view.synchronized ? true : undefined;
    });
    check(view.thread?.id === threadId, `snapshot projection.thread.id is ${view.thread?.id}`);
    return `snapshotSequence=${view.sequences.cursor}`;
  },
);

await runner.step(
  "thread.user-message",
  "user message keeps the client messageId (item, message, run.userMessageId)",
  ["thread.subscribe"],
  async () => {
    const found = await thread!.until(
      `a user_message item, a message and a run for messageId ${firstMessageId}`,
      QUICK_MS,
      () => {
        const item = findItem(view, "user_message", (entry) => entry.messageId === firstMessageId);
        const message = view.messages.get(firstMessageId);
        const run = view.runForMessage(firstMessageId);
        return item !== undefined && message !== undefined && run !== undefined
          ? { item, message, run }
          : undefined;
      },
    );
    check(found.message.role === "user", `message ${firstMessageId} has role ${found.message.role}`);
    check(
      found.item.inputIntent === "turn_start",
      `user_message inputIntent is ${found.item.inputIntent}, expected turn_start`,
    );
    return `run ${found.run.id}`;
  },
);

let askRunId: string | undefined;
let pending:
  | { request: OrchestrationV2RuntimeRequest; questionId: string; answer: string | string[] }
  | undefined;

await runner.step(
  "ask.waiting",
  "run waits on a pending user_input request with a matching user_input_request item",
  ["thread.user-message"],
  async () => {
    const found = await thread!.until(
      `the run to be "waiting" on a pending user_input request with its user_input_request item`,
      agentTimeoutMs,
      () => {
        const run = view.runForMessage(firstMessageId);
        if (run === undefined) return undefined;
        if (run.status === "completed" || TERMINAL_FAILURE_STATUSES[run.status] === true) {
          fail(
            `run ${run.id} ended as "${run.status}" without asking (history: ${view.runStatusHistory.get(run.id)?.join("→")}); assistant text: ${view.assistantText(run.id).slice(0, 300)}`,
          );
        }
        if (run.status !== "waiting") return undefined;
        for (const request of view.runtimeRequests.values()) {
          if (request.kind !== "user_input" || request.status !== "pending") continue;
          const item = findItem(view, "user_input_request", (entry) => entry.requestId === request.id);
          if (item !== undefined) return { run, request, item };
        }
        return undefined;
      },
    );
    askRunId = found.run.id;
    const question = found.item.questions[0];
    if (question === undefined) return fail(`user_input_request ${found.item.id} has no questions`);
    const option = question.options.find((entry) => /blue/i.test(entry.value ?? entry.label));
    if (option === undefined) {
      return fail(
        `question "${question.question}" has no "blue" option: ${question.options.map((entry) => entry.label).join(", ")}`,
      );
    }
    // `CR/state/pendingUserInput.ts`: the option value, else its label; arrays for multi-select.
    const value = option.value ?? option.label;
    pending = {
      request: found.request,
      questionId: question.id,
      answer: question.multiSelect === true ? [value] : value,
    };
    return `request ${found.request.id}: "${question.question}" [${question.options.map((entry) => entry.label).join(", ")}]`;
  },
);

await runner.step(
  "shell.waiting",
  "subscribeShell thread.updated: waiting + pendingRuntimeRequest user_input",
  ["ask.waiting", "shell.subscribe"],
  async () => {
    const row = await shell!.until(
      `thread ${threadId} to show "waiting" with pendingRuntimeRequest ${pending!.request.id}`,
      QUICK_MS,
      () => {
        const entry = shellView.threads.get(threadId);
        return entry !== undefined &&
          (entry.activityRunStatus ?? entry.status) === "waiting" &&
          entry.pendingRuntimeRequest?.kind === "user_input" &&
          entry.pendingRuntimeRequest.id === pending!.request.id
          ? entry
          : undefined;
      },
    );
    return `activeRunId=${row.activeRunId}`;
  },
);

await runner.step(
  "ask.respond",
  'runtime-request.respond answers "blue"',
  ["ask.waiting"],
  async () => {
    const result = await dispatch(session1, {
      type: "runtime-request.respond",
      commandId: newCommandId(),
      threadId,
      requestId: pending!.request.id,
      answers: { [pending!.questionId]: pending!.answer },
    });
    return `answers ${JSON.stringify({ [pending!.questionId]: pending!.answer })} → sequence ${result.sequence}`;
  },
);

await runner.step(
  "ask.completed",
  'request resolved, run completed, assistant text names "blue"',
  ["ask.respond"],
  async () => {
    let completedSeenAt: number | undefined;
    const text = await thread!.until(
      `request ${pending!.request.id} resolved, run ${askRunId} completed with assistant text containing "blue"`,
      agentTimeoutMs,
      () => {
        const run = view.runs.get(askRunId!);
        if (isTerminalFailure(run)) {
          fail(`run ${run.id} ended as "${run.status}" (history: ${view.runStatusHistory.get(run.id)?.join("→")})`);
        }
        const request = view.runtimeRequests.get(pending!.request.id);
        if (run?.status !== "completed" || request?.status === "pending") return undefined;
        const assistant = view.assistantText(run.id);
        if (/blue/i.test(assistant)) return assistant;
        // Items may land just after the terminal run update; allow a short grace period.
        completedSeenAt ??= Date.now();
        if (Date.now() - completedSeenAt > 5_000) {
          fail(`run ${run.id} completed but its assistant text does not mention blue: ${JSON.stringify(assistant.slice(0, 300))}`);
        }
        return undefined;
      },
    );
    const request = view.runtimeRequests.get(pending!.request.id);
    check(
      request?.status === "resolved",
      `runtime request ${pending!.request.id} is "${request?.status}", expected "resolved"`,
    );
    return JSON.stringify(text.trim().slice(0, 120));
  },
);

await runner.step(
  "shell.completed",
  "subscribeShell thread.updated: idle again, no pending request",
  ["ask.completed", "shell.subscribe"],
  async () => {
    const row = await shell!.until(
      `thread ${threadId} to show no active run and no pending request`,
      QUICK_MS,
      () => {
        const entry = shellView.threads.get(threadId);
        return entry !== undefined &&
          ACTIVE_RUN_STATUSES[entry.activityRunStatus ?? entry.status] !== true &&
          entry.pendingRuntimeRequest === null &&
          entry.latestRunId === askRunId
          ? entry
          : undefined;
      },
    );
    return `status=${row.status}`;
  },
);

/**
 * The agent steps below each start from an idle thread, so one misbehaving turn (e.g. the agent
 * not using the ask tool) does not hide the queue, interrupt and idempotency checks.
 */
const awaitIdle = () =>
  thread!.until("the thread to have no active or queued run", agentTimeoutMs, () =>
    [...view.runs.values()].every(
      (run) => ACTIVE_RUN_STATUSES[run.status] !== true && run.status !== "queued",
    )
      ? true
      : undefined,
  );

const queueFirstMessageId = MessageId.make(randomUUID());
const queuedMessageId = MessageId.make(randomUUID());
let queueFirstRun: OrchestrationV2Run | undefined;
let queuedRun: OrchestrationV2Run | undefined;

const messageDispatch = (
  messageId: string,
  text: string,
  dispatchMode: { type: "start_immediately" } | { type: "queue_after_active" },
  commandId = newCommandId(),
): OrchestrationV2Command => ({
  type: "message.dispatch",
  commandId,
  createdBy: "user",
  creationSource: "mobile",
  threadId,
  messageId: MessageId.make(messageId),
  text,
  attachments: [],
  modelSelection: modelSelection!,
  dispatchMode,
});

await runner.step(
  "queue.start",
  "message.dispatch start_immediately → run running",
  ["thread.subscribe"],
  async () => {
    await awaitIdle();
    const result = await dispatch(
      session1,
      messageDispatch(queueFirstMessageId, QUEUE_FIRST_PROMPT, { type: "start_immediately" }),
    );
    queueFirstRun = await awaitRunStatus(thread!, view, queueFirstMessageId, "running", agentTimeoutMs);
    return `sequence ${result.sequence}, run ${queueFirstRun.id}`;
  },
);

await runner.step(
  "queue.enqueue",
  "message.dispatch queue_after_active while running → queued run",
  ["queue.start"],
  async () => {
    const result = await dispatch(
      session1,
      messageDispatch(queuedMessageId, QUEUED_PROMPT, { type: "queue_after_active" }),
    );
    const run = await thread!.until(`a run for message ${queuedMessageId}`, QUICK_MS, () =>
      view.runForMessage(queuedMessageId),
    );
    queuedRun = run;
    const history = view.runStatusHistory.get(run.id) ?? [];
    check(
      history[0] === "queued",
      `run ${run.id} was first seen as "${history[0]}", expected "queued" (history: ${history.join("→")})`,
    );
    const item = await thread!.until(`the user_message item for ${queuedMessageId}`, QUICK_MS, () =>
      findItem(view, "user_message", (entry) => entry.messageId === queuedMessageId),
    );
    check(
      item.inputIntent === "queued_turn",
      `queued user_message inputIntent is ${item.inputIntent}, expected queued_turn`,
    );
    return `sequence ${result.sequence}, run ${run.id} queuePosition=${run.queuePosition ?? "absent"}`;
  },
);

await runner.step(
  "queue.drain",
  "active run completes, then the queued run starts and completes",
  ["queue.enqueue"],
  async () => {
    await awaitRunStatus(thread!, view, queueFirstMessageId, "completed", agentTimeoutMs);
    await awaitRunStatus(thread!, view, queuedMessageId, "completed", agentTimeoutMs);
    const history = view.runStatusHistory.get(queuedRun!.id) ?? [];
    check(
      history.includes("starting") || history.includes("running"),
      `queued run ${queuedRun!.id} completed without being seen starting/running (history: ${history.join("→")})`,
    );
    const item = findItem(view, "user_message", (entry) => entry.messageId === queuedMessageId);
    check(
      item?.status === "completed",
      `queued user_message item status is ${item?.status}, expected completed once the run started`,
    );
    return `history ${history.join("→")}`;
  },
);

const interruptMessageId = MessageId.make(randomUUID());
let interruptRun: OrchestrationV2Run | undefined;

await runner.step(
  "interrupt.running",
  "message.dispatch a long-running prompt (bash sleep 30)",
  ["thread.subscribe"],
  async () => {
    await awaitIdle();
    await dispatch(
      session1,
      messageDispatch(interruptMessageId, INTERRUPT_PROMPT, { type: "start_immediately" }),
    );
    interruptRun = await awaitRunStatus(thread!, view, interruptMessageId, "running", agentTimeoutMs);
    // Give the agent time to start the tool so the interrupt lands mid-command.
    const tool = await thread!
      .until("a command item in the run", 30_000, () =>
        view
          .itemsOfRun(interruptRun!.id)
          .find((item) => item.type === "command_execution" || item.type === "dynamic_tool"),
      )
      .catch(() => undefined);
    return `run ${interruptRun.id}${tool === undefined ? " (no tool item seen within 30s)" : `, ${tool.type} running`}`;
  },
);

await runner.step(
  "run.interrupt",
  "run.interrupt holdQueue → run interrupted",
  ["interrupt.running"],
  async () => {
    const result = await dispatch(session1, {
      type: "run.interrupt",
      commandId: newCommandId(),
      threadId,
      runId: RunId.make(interruptRun!.id),
      holdQueue: true,
    });
    await thread!.until(`run ${interruptRun!.id} to be "interrupted"`, QUICK_MS * 2, () => {
      const run = view.runs.get(interruptRun!.id);
      if (run !== undefined && run.status !== "interrupted" && ACTIVE_RUN_STATUSES[run.status] !== true) {
        fail(`run ${run.id} ended as "${run.status}" instead of "interrupted"`);
      }
      return run?.status === "interrupted" ? run : undefined;
    });
    // §4.5 Stop: a run_interrupt_request item, then a run_interrupt_result item.
    await thread!.until(`run_interrupt_request and run_interrupt_result items`, QUICK_MS, () => {
      const types = new Set(view.itemsOfRun(interruptRun!.id).map((item) => item.type));
      return types.has("run_interrupt_request") && types.has("run_interrupt_result") ? true : undefined;
    });
    return `sequence ${result.sequence}`;
  },
);

const idempotentMessageId = MessageId.make(randomUUID());

await runner.step(
  "dispatch.idempotent",
  "the same message.dispatch commandId twice → same sequence, one run",
  ["thread.subscribe"],
  async () => {
    await awaitIdle();
    const command = messageDispatch(idempotentMessageId, IDEMPOTENT_PROMPT, {
      type: "start_immediately",
    });
    const first = await dispatch(session1, command);
    const second = await dispatch(session1, command);
    check(
      first.sequence === second.sequence,
      `sequences differ: ${first.sequence} then ${second.sequence} for commandId ${command.commandId}`,
    );
    await awaitRunStatus(thread!, view, idempotentMessageId, "completed", agentTimeoutMs);
    const runs = [...view.runs.values()].filter((run) => run.userMessageId === idempotentMessageId);
    check(runs.length === 1, `${runs.length} runs exist for message ${idempotentMessageId}`);
    const items = [...view.turnItems.values()].filter(
      (item) => item.type === "user_message" && item.messageId === idempotentMessageId,
    );
    check(items.length === 1, `${items.length} user_message items exist for ${idempotentMessageId}`);
    return `sequence ${first.sequence} twice`;
  },
);

const renamedTitle = `j3 conformance renamed ${stamp}`;

await runner.step(
  "thread.rename",
  "thread.metadata.update title → thread event + shell row",
  ["thread.subscribe"],
  async () => {
    await dispatch(session1, {
      type: "thread.metadata.update",
      commandId: newCommandId(),
      threadId,
      title: renamedTitle,
    });
    await thread!.until(`thread.metadata-updated with the new title`, QUICK_MS, () =>
      view.thread?.title === renamedTitle ? true : undefined,
    );
    if (runner.passed("shell.subscribe")) {
      await shell!.until(`the shell row to carry the new title`, QUICK_MS, () =>
        shellView.threads.get(threadId)?.title === renamedTitle ? true : undefined,
      );
    }
  },
);

await runner.step(
  "thread.projection",
  "orchestration.getThreadProjection matches the streamed state",
  ["thread.subscribe"],
  async () => {
    const projection = await session1.call("orchestration.getThreadProjection", QUICK_MS, (client) =>
      client["orchestration.getThreadProjection"]({ threadId }),
    );
    check(projection.thread.id === threadId, `projection.thread.id is ${projection.thread.id}`);
    if (runner.passed("thread.rename")) {
      check(projection.thread.title === renamedTitle, `projection title is "${projection.thread.title}"`);
    }
    for (const run of projection.runs) {
      const streamed = view.runs.get(run.id);
      check(streamed !== undefined, `projection run ${run.id} never appeared on subscribeThread`);
      check(
        streamed.status === run.status,
        `run ${run.id} is "${run.status}" in the projection but "${streamed.status}" on the stream`,
      );
    }
    for (const id of view.runs.keys()) {
      check(projection.runs.some((run) => run.id === id), `streamed run ${id} is missing from the projection`);
    }
    // §4.3: the feed is built only from visibleTurnItems, positions 0..n-1.
    const visible = projection.visibleTurnItems;
    check(visible.length > 0, `visibleTurnItems is empty`);
    visible.forEach((row, index) => {
      check(row.position === index, `visibleTurnItems[${index}].position is ${row.position}`);
      check(row.sourceThreadId === threadId, `visibleTurnItems[${index}].sourceThreadId is ${row.sourceThreadId}`);
      check(row.sourceItemId === row.item.id, `visibleTurnItems[${index}].sourceItemId ≠ item.id`);
    });
    check(
      visible.some((row) => row.item.type === "user_message" && row.item.messageId === firstMessageId),
      `visibleTurnItems lacks the first user message ${firstMessageId}`,
    );
    return `${projection.runs.length} runs, ${projection.turnItems.length} items, ${visible.length} visible`;
  },
);

// Reconnect: a new transport session, as after a network change.
let session1ConfigOpen = false;
/** Opened by `reconnect.session`; every later session2 step requires that step. */
let session2!: Session;
const resumeAfter = view.sequences.cursor;
const view2 = new ThreadView(threadId, resumeAfter);
const shellView2 = new ShellView(undefined);
let thread2: Waiter | undefined;
let shell2: Waiter | undefined;

await runner.step(
  "reconnect.session",
  "close the socket, reconnect, config handshake again",
  ["config.handshake"],
  async () => {
    session1ConfigOpen = session1.config.ended === undefined;
    await session1.close();
    session2 = openSession("session2");
    await awaitOpen(session2);
    await handshake(session2, descriptor!);
  },
);

await runner.step(
  "reconnect.thread",
  "subscribeThread afterSequence: no going backwards, synchronized arrives",
  ["reconnect.session", "thread.subscribe"],
  async () => {
    const watcher = session2.watch(
      "orchestration.subscribeThread",
      (client) =>
        client["orchestration.subscribeThread"]({
          threadId,
          ...(resumeAfter === undefined ? {} : { afterSequence: resumeAfter }),
          requestCompletionMarker: true,
          acceptDedupedBodies: true,
        }),
      (item) => view2.apply(item),
      () => view2.describe(),
    );
    thread2 = watcher;
    await watcher.until("synchronized", QUICK_MS, () => (view2.synchronized ? true : undefined));
    check(
      view2.sequences.violations.length === 0,
      view2.sequences.violations.join("\n"),
    );
    return `afterSequence=${resumeAfter}: ${view2.snapshots} snapshot(s), ${view2.events} event(s), ${view2.sequences.overlap} overlapping`;
  },
);

await runner.step(
  "reconnect.shell",
  "subscribeShell on the new session: snapshot then synchronized",
  ["reconnect.session"],
  async () => {
    const watcher = session2.watch(
      "orchestration.subscribeShell",
      (client) => client["orchestration.subscribeShell"]({ requestCompletionMarker: true }),
      (item) => shellView2.apply(item),
      () => shellView2.describe(),
    );
    shell2 = watcher;
    await watcher.until("snapshot then synchronized", QUICK_MS, () => {
      if (shellView2.firstKind !== undefined && shellView2.firstKind !== "snapshot") {
        fail(`the first shell item is "${shellView2.firstKind}", expected "snapshot"`);
      }
      return shellView2.synchronized ? true : undefined;
    });
    if (runner.passed("project.create")) {
      check(shellView2.projects.has(projectId), `the snapshot lacks project ${projectId}`);
    }
    if (runner.passed("thread.launch")) {
      check(shellView2.threads.has(threadId), `the snapshot lacks active thread ${threadId}`);
    }
  },
);

await runner.step(
  "thread.archive",
  "thread.archive → thread.archived event, leaves the active shell list",
  ["reconnect.thread", "reconnect.shell"],
  async () => {
    await dispatch(session2, { type: "thread.archive", commandId: newCommandId(), threadId });
    await thread2!.until("a thread.archived event with archivedAt set", QUICK_MS, () =>
      view2.lastThreadEvent === "thread.archived" && view2.thread?.archivedAt != null ? true : undefined,
    );
    await shell2!.until("the thread to leave the active list", QUICK_MS, () =>
      shellView2.threads.has(threadId) ? undefined : true,
    );
  },
);

await runner.step(
  "archived.snapshot",
  "orchestration.getArchivedShellSnapshot contains the thread",
  ["thread.archive"],
  async () => {
    const snapshot = await session2.call("orchestration.getArchivedShellSnapshot", QUICK_MS, (client) =>
      client["orchestration.getArchivedShellSnapshot"]({}),
    );
    const row = snapshot.threads.find((entry) => entry.id === threadId);
    check(row !== undefined, `archived snapshot lacks ${threadId} (${snapshot.threads.length} threads)`);
    check(row.archivedAt !== null, `archived row has archivedAt null`);
    return `snapshotSequence=${snapshot.snapshotSequence}, ${snapshot.threads.length} archived`;
  },
);

await runner.step(
  "thread.unarchive",
  "thread.unarchive → thread.unarchived event, back in the active shell list",
  ["thread.archive"],
  async () => {
    await dispatch(session2, { type: "thread.unarchive", commandId: newCommandId(), threadId });
    await thread2!.until("a thread.unarchived event with archivedAt null", QUICK_MS, () =>
      view2.lastThreadEvent === "thread.unarchived" && view2.thread?.archivedAt === null ? true : undefined,
    );
    await shell2!.until("the thread to return to the active list", QUICK_MS, () =>
      shellView2.threads.has(threadId) ? true : undefined,
    );
  },
);

await runner.step(
  "thread.delete",
  "thread.delete → thread.deleted event, removed from the shell",
  ["reconnect.thread", "reconnect.shell"],
  async () => {
    await dispatch(session2, { type: "thread.delete", commandId: newCommandId(), threadId });
    await thread2!.until("a thread.deleted event", QUICK_MS, () => (view2.deleted ? true : undefined));
    await shell2!.until("thread.removed", QUICK_MS, () =>
      shellView2.removedThreads.has(threadId) &&
      !shellView2.threads.has(threadId) &&
      !shellView2.archivedThreads.has(threadId)
        ? true
        : undefined,
    );
  },
);

await runner.step(
  "project.delete",
  "projects.mutate project.delete → shell project.removed (cleanup)",
  ["reconnect.shell", "project.create"],
  async () => {
    await session2.call("projects.mutate", QUICK_MS, (client) =>
      client["projects.mutate"]({ type: "project.delete", commandId: newCommandId(), projectId }),
    );
    await shell2!.until("project.removed", QUICK_MS, () =>
      shellView2.removedProjects.has(projectId) ? true : undefined,
    );
  },
);

await runner.step(
  "config.open",
  "subscribeServerConfig stayed open for each session's lifetime",
  ["reconnect.session"],
  async () => {
    check(session1ConfigOpen, `session1's config stream ended before the socket closed: ${JSON.stringify(session1.config.ended)}`);
    check(session2.config.ended === undefined, `session2's config stream ended: ${JSON.stringify(session2.config.ended)}`);
  },
);

await runner.step(
  "sequences",
  "stream sequences only increase (thread + shell, both sessions)",
  ["thread.subscribe"],
  async () => {
    const violations = [view, view2, shellView, shellView2].flatMap(
      (entry) => entry.sequences.violations,
    );
    check(violations.length === 0, violations.join("\n"));
    const unknown = [...view.unknownEventTypes, ...view2.unknownEventTypes];
    return unknown.length === 0 ? undefined : `unknown event types (skipped by the client): ${unknown.join(", ")}`;
  },
);

await Promise.all(sessions.map((session) => session.close()));

const { pass, fail: failed, skip } = runner.summary();
process.stdout.write(`\n${pass} passed, ${failed} failed, ${skip} skipped\n`);
process.exit(failed > 0 || skip > 0 ? 1 : 0);
