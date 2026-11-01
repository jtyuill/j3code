import { describe, expect, it } from "@effect/vitest";
import {
  EnvironmentId,
  ORCHESTRATION_PROTOCOL_HEADER,
  ORCHESTRATION_PROTOCOL_VERSION_TEXT,
  ThreadId,
  type AuthSessionState,
  type OrchestrationV2ShellSnapshot,
  OrchestrationV2ThreadDetailSnapshot,
  OrchestrationV2ThreadBoundedSnapshot,
  OrchestrationV2ThreadTranscript,
  type OrchestrationV2ThreadHistoryPage,
} from "@t3tools/contracts";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Fiber from "effect/Fiber";
import * as Queue from "effect/Queue";
import * as Schema from "effect/Schema";
import * as TestClock from "effect/testing/TestClock";
import type { HttpClient } from "effect/http";

import { BearerConnectionTarget, type PreparedConnection } from "../connection/model.ts";
import { type RemoteEnvironmentRequestError } from "../rpc/http.ts";
import * as RpcHttp from "../rpc/http.ts";
import { fetchEnvironmentSessionState } from "./session.ts";
import { fetchEnvironmentShellSnapshot } from "./shellSnapshotHttp.ts";
import { fetchEnvironmentThreadTranscript } from "./threadTranscriptHttp.ts";
import { fetchEnvironmentBoundedThreadSnapshot } from "./boundedThreadSnapshotHttp.ts";
import { fetchEnvironmentThreadHistoryPage } from "./threadHistoryHttp.ts";
import { v2Projection } from "./orchestrationV2TestFixtures.ts";

const encodeBoundedSnapshot = Schema.encodeSync(OrchestrationV2ThreadBoundedSnapshot);
const encodeThreadTranscript = Schema.encodeSync(OrchestrationV2ThreadTranscript);

const HOST_ORIGIN = "http://shieldy:3780";
const TARGET = new BearerConnectionTarget({
  environmentId: EnvironmentId.make("environment-1"),
  label: "shieldy",
  connectionId: "host",
});
const PREPARED: PreparedConnection = {
  environmentId: TARGET.environmentId,
  label: TARGET.label,
  httpBaseUrl: `${HOST_ORIGIN}/`,
  socketUrl: "ws://shieldy:3780/ws",
  target: TARGET,
};
const AUTH = {
  policy: "remote-reachable",
  bootstrapMethods: [],
  sessionMethods: [],
  sessionCookieName: "j3_session",
} satisfies AuthSessionState["auth"];
const SESSION = {
  authenticated: true,
  auth: AUTH,
  scopes: ["orchestration:read", "orchestration:operate"],
} satisfies AuthSessionState;
const SHELL = {
  schemaVersion: 1,
  snapshotSequence: 1,
  projects: [],
  threads: [],
  archivedThreads: [],
} satisfies OrchestrationV2ShellSnapshot;
const THREAD = {
  snapshotSequence: 2,
  projection: v2Projection,
} satisfies OrchestrationV2ThreadDetailSnapshot;
const TRANSCRIPT = {
  threadId: v2Projection.thread.id,
  title: v2Projection.thread.title,
  updatedAt: v2Projection.updatedAt,
  items: v2Projection.visibleTurnItems,
} satisfies OrchestrationV2ThreadTranscript;
const BOUNDED_THREAD = {
  ...THREAD,
  historyCursor: "older-page",
  hasMoreHistory: true,
  latestLocalTurnOrdinal: 3,
} satisfies OrchestrationV2ThreadBoundedSnapshot;
const THREAD_HISTORY = {
  snapshotSequence: 2,
  items: [],
  nextCursor: null,
  hasMoreHistory: false,
} satisfies OrchestrationV2ThreadHistoryPage;

interface HttpInput {
  readonly prepared: PreparedConnection;
}

function makeHarness(reply: (requestNumber: number) => Response | Promise<Response>) {
  const calls: Array<{ readonly url: string; readonly init: RequestInit }> = [];
  const fetchFn: typeof fetch = async (request, init) => {
    calls.push({ url: String(request), init: init ?? {} });
    return reply(calls.length);
  };
  const input: HttpInput = { prepared: PREPARED };
  return {
    calls,
    input,
    httpLayer: RpcHttp.layerRemoteHttpClient(fetchFn),
  };
}

// Resolves "settled" instead of hanging when the load finishes without sending another request.
function nextRequestOrSettled(requests: Queue.Queue<number>, load: Fiber.Fiber<unknown, unknown>) {
  return Effect.raceFirst(
    Queue.take(requests).pipe(Effect.as("sent" as const)),
    Fiber.await(load).pipe(Effect.as("settled" as const)),
  );
}

const LOADERS: ReadonlyArray<{
  readonly name: string;
  readonly path: string;
  readonly response: unknown;
  readonly expected: unknown;
  readonly load: (
    input: HttpInput,
  ) => Effect.Effect<unknown, RemoteEnvironmentRequestError, HttpClient.HttpClient>;
}> = [
  {
    name: "session permissions",
    path: "/api/auth/session",
    response: SESSION,
    expected: SESSION,
    load: fetchEnvironmentSessionState,
  },
  {
    name: "shell snapshot",
    path: "/api/orchestration/shell",
    response: SHELL,
    expected: SHELL,
    load: fetchEnvironmentShellSnapshot,
  },
  {
    name: "thread transcript",
    path: `/api/orchestration/threads/${TRANSCRIPT.threadId}/transcript`,
    response: encodeThreadTranscript(TRANSCRIPT),
    expected: TRANSCRIPT,
    load: (input: HttpInput) =>
      fetchEnvironmentThreadTranscript({ ...input, threadId: TRANSCRIPT.threadId }),
  },
  {
    name: "bounded thread snapshot",
    path: `/api/orchestration/threads/${THREAD.projection.thread.id}/bounded`,
    response: encodeBoundedSnapshot(BOUNDED_THREAD),
    expected: BOUNDED_THREAD,
    load: (input: HttpInput) =>
      fetchEnvironmentBoundedThreadSnapshot({ ...input, threadId: THREAD.projection.thread.id }),
  },
  {
    name: "older thread history",
    path: `/api/orchestration/threads/${THREAD.projection.thread.id}/history`,
    response: THREAD_HISTORY,
    expected: THREAD_HISTORY,
    load: (input: HttpInput) =>
      fetchEnvironmentThreadHistoryPage({
        ...input,
        threadId: THREAD.projection.thread.id,
        cursor: "older-page",
      }),
  },
];

describe("environment HTTP requests", () => {
  it.effect.each(LOADERS)("rejects an invalid $name response", (loader) =>
    Effect.gen(function* () {
      const harness = makeHarness(() => Response.json({}));
      const result = yield* loader
        .load(harness.input)
        .pipe(Effect.provide(harness.httpLayer), Effect.asVoid, Effect.flip);
      expect(result._tag).toBe("RemoteEnvironmentAuthInvalidJsonError");
      expect(harness.calls).toHaveLength(1);
    }),
  );

  it.effect("keeps the status of a shell snapshot error that is not a declared error", () =>
    Effect.gen(function* () {
      const harness = makeHarness(() => Response.json({ error: "bad_gateway" }, { status: 502 }));
      const error = yield* fetchEnvironmentShellSnapshot(harness.input).pipe(
        Effect.provide(harness.httpLayer),
        Effect.flip,
      );
      expect(error).toMatchObject({
        _tag: "RemoteEnvironmentAuthUndeclaredStatusError",
        status: 502,
      });
    }),
  );

  it.effect.each(LOADERS)("sends $name to the host without credentials", (loader) =>
    Effect.gen(function* () {
      const harness = makeHarness(() => Response.json(loader.response));
      const result = yield* loader.load(harness.input).pipe(Effect.provide(harness.httpLayer));

      expect(result).toEqual(loader.expected);
      expect(harness.calls).toHaveLength(1);
      const call = harness.calls[0]!;
      const url = new URL(call.url);
      expect(url.origin).toBe(HOST_ORIGIN);
      expect(url.pathname).toBe(loader.path);
      expect(call.init.method).toBe("GET");
      const headers = new Headers(call.init.headers);
      expect(headers.get("authorization")).toBeNull();
      expect(headers.get("dpop")).toBeNull();
      expect(call.init.credentials).toBeUndefined();
      if (loader.path.startsWith("/api/orchestration/")) {
        expect(headers.get(ORCHESTRATION_PROTOCOL_HEADER)).toBe(
          ORCHESTRATION_PROTOCOL_VERSION_TEXT,
        );
      }
      if (loader.name === "older thread history") {
        expect(url.searchParams.get("cursor")).toBe("older-page");
      }
    }),
  );

  it.effect("percent-encodes a thread id that needs it in the request path", () =>
    Effect.gen(function* () {
      const threadId = ThreadId.make("mcp:3534bc83-1c17-4a1e-9118-601c2766d355");
      const harness = makeHarness(() => Response.json(encodeThreadTranscript(TRANSCRIPT)));
      yield* fetchEnvironmentThreadTranscript({ ...harness.input, threadId }).pipe(
        Effect.provide(harness.httpLayer),
      );
      expect(new URL(harness.calls[0]!.url).pathname).toContain("/mcp%3A3534bc83-");
    }),
  );

  it.effect(
    "waits longer for session permissions after the environment is too slow to answer",
    () =>
      Effect.gen(function* () {
        const requests = yield* Queue.unbounded<number>();
        const { promise: answered, resolve: answer } = Promise.withResolvers<void>();
        const harness = makeHarness((requestNumber) => {
          Queue.offerUnsafe(requests, requestNumber);
          return requestNumber === 1
            ? new Promise<Response>(() => {})
            : answered.then(() => Response.json(SESSION));
        });
        const load = yield* fetchEnvironmentSessionState({
          prepared: PREPARED,
          timeoutMs: 1_000,
        }).pipe(Effect.provide(harness.httpLayer), Effect.forkChild({ startImmediately: true }));

        yield* Queue.take(requests);
        yield* TestClock.adjust("1 second");
        expect(yield* nextRequestOrSettled(requests, load)).toBe("sent");
        yield* TestClock.adjust("1500 millis");
        answer();

        expect(yield* Fiber.await(load)).toStrictEqual(Exit.succeed(SESSION));
        expect(harness.calls).toHaveLength(2);
      }),
  );

  it.effect("gives up on session permissions once a 60 second wait also times out", () =>
    Effect.gen(function* () {
      const requests = yield* Queue.unbounded<number>();
      const harness = makeHarness((requestNumber) => {
        Queue.offerUnsafe(requests, requestNumber);
        return new Promise<Response>(() => {});
      });
      const load = yield* fetchEnvironmentSessionState({
        prepared: PREPARED,
        timeoutMs: 15_000,
      }).pipe(Effect.provide(harness.httpLayer), Effect.forkChild({ startImmediately: true }));

      yield* Queue.take(requests);
      yield* TestClock.adjust("15 seconds");
      expect(yield* nextRequestOrSettled(requests, load)).toBe("sent");
      yield* TestClock.adjust("30 seconds");
      expect(yield* nextRequestOrSettled(requests, load)).toBe("sent");
      yield* TestClock.adjust("60 seconds");
      const exit = yield* Fiber.await(load);

      expect(exit).toMatchObject({
        _tag: "Failure",
        cause: {
          reasons: [{ error: { _tag: "RemoteEnvironmentAuthTimeoutError", timeoutMs: 60_000 } }],
        },
      });
      expect(harness.calls).toHaveLength(3);
    }),
  );
});
