import { threadSearchMatchKey } from "@t3tools/client-runtime/state/thread-search";
import { createInboxReturnTracker } from "@t3tools/client-runtime/state/thread-inbox";
import { presentThreadShell } from "@t3tools/client-runtime/state/models";
import * as DateTime from "effect/DateTime";
import * as Schema from "effect/Schema";
import type { EnvironmentThreadShell } from "@t3tools/client-runtime/state/shell";
import {
  CommandId,
  EnvironmentId,
  MessageId,
  OrchestrationV2ThreadShell,
  ProjectId,
  ProviderInstanceId,
  RunId,
  ThreadId,
} from "@t3tools/contracts";
import { describe, expect, it, vi } from "vite-plus/test";

import type { PendingNewTask } from "../../state/use-pending-new-tasks";
import { makeRawThreadShell, makeThreadShellFixture } from "../../test-fixtures";
import { threadJumpTarget } from "../keyboard/threadKeyboardShortcuts";
import {
  buildThreadListV2Items,
  buildThreadListV2ListItems,
  resolveThreadListV2Status,
  threadListV2ListItemsAreEqual,
  threadHasUnseenCompletion,
  type ThreadListV2ListItem,
} from "./threadListV2";

const environmentId = EnvironmentId.make("environment-1");
const decodeThreadShell = Schema.decodeUnknownSync(OrchestrationV2ThreadShell);

function makeThread(
  input: Partial<EnvironmentThreadShell> & Pick<EnvironmentThreadShell, "id" | "title">,
): EnvironmentThreadShell {
  return makeThreadShellFixture({
    environmentId,
    ...input,
  });
}

const NOW = "2026-06-02T00:00:00.000Z";


describe("resolveThreadListV2Status", () => {
  it("presents decoded thread waits as Waiting while preserving requests and older servers", () => {
    const raw = makeRawThreadShell({
      status: "running",
      activeRunId: RunId.make("run-wait"),
      latestRunId: RunId.make("run-wait"),
    });
    const present = (shell: unknown) => presentThreadShell(environmentId, decodeThreadShell(shell));

    expect(resolveThreadListV2Status(present(raw))).toBe("working");
    const blocked = present({ ...raw, blockedOnThreadWait: true });
    expect(
      resolveThreadListV2Status({
        ...blocked,
        hasPendingApprovals: true,
        hasPendingUserInput: true,
      }),
    ).toBe("approval");
    expect(resolveThreadListV2Status({ ...blocked, hasPendingUserInput: true })).toBe("input");
    expect(resolveThreadListV2Status(blocked)).toBe("waiting");
    expect(resolveThreadListV2Status(present({ ...raw, blockedOnThreadWait: false }))).toBe(
      "working",
    );
  });

  it.each([
    { roster: "dev server", task: { kind: "command" }, status: "ready" },
    { roster: "dev server watchdog", task: { kind: "monitor" }, status: "ready" },
    { roster: "awaited command", task: { kind: "command", awaited: true }, status: "waiting" },
  ] as const)(
    "presents an unseen completion with a $roster roster as $status",
    ({ task, status }) => {
      const thread = presentThreadShell(
        environmentId,
        makeRawThreadShell({
          latestRunId: RunId.make("run-background-completion"),
          status: "completed",
          latestRunCompletedAt: DateTime.makeUnsafe(NOW),
          lastVisitedAt: DateTime.makeUnsafe("2026-06-01T23:59:00.000Z"),
          pendingBackgroundTasks: [{ taskId: "background-work", ...task }],
        }),
      );

      expect(resolveThreadListV2Status(thread)).toBe(status);
      expect(threadHasUnseenCompletion(thread)).toBe(true);
    },
  );
});

describe("search lists ranked matches", () => {
  const matchKey = (id: string) =>
    threadSearchMatchKey({ environmentId, threadId: ThreadId.make(id) });

  it("lists matches by rank, then threads listed for their messages", () => {
    const layout = buildThreadListV2Items({
      threads: [
        makeThread({ id: ThreadId.make("notes"), title: "Deploy notes" }),
        makeThread({ id: ThreadId.make("mentioned"), title: "Weekly notes" }),
        makeThread({ id: ThreadId.make("exact"), title: "Deploy" }),
        makeThread({ id: ThreadId.make("miss"), title: "Greeting" }),
      ],
      environmentId: null,
      searchQuery: "deploy",
      matchedThreadKeys: new Set([matchKey("mentioned"), matchKey("notes")]),
      now: NOW,
    });
    expect(layout.items.map((item) => item.thread.id)).toEqual(["exact", "notes", "mentioned"]);
    expect([...layout.contentOnlyThreadKeys]).toEqual([matchKey("mentioned")]);
  });


  it("forgives a typo even when a hidden thread matches the query exactly", () => {
    const { items } = buildThreadListV2Items({
      threads: [
        makeThread({ id: ThreadId.make("visible"), title: "Restore drag handles" }),
        makeThread({ id: ThreadId.make("archived"), title: "Drga notes", archivedAt: NOW }),
      ],
      environmentId: null,
      searchQuery: "drga",
      now: NOW,
    });

    expect(items.map((item) => item.thread.id)).toEqual(["visible"]);
  });

  it("scopes the flat list to every environment member of a logical project", () => {
    const remoteEnvironmentId = EnvironmentId.make("environment-remote");
    const projectId = ProjectId.make("project-1");
    const { items } = buildThreadListV2Items({
      threads: [
        makeThread({ id: ThreadId.make("local"), projectId, title: "Local" }),
        makeThread({
          environmentId: remoteEnvironmentId,
          id: ThreadId.make("remote"),
          projectId,
          title: "Remote",
        }),
      ],
      environmentId: null,
      projectRefs: [
        { environmentId, projectId },
        { environmentId: remoteEnvironmentId, projectId },
      ],
      searchQuery: "",
      now: NOW,
    });

    expect(items.map((item) => item.thread.id)).toEqual(["local", "remote"]);
  });
});

function makePendingTask(id: string): PendingNewTask {
  const creation = {
    projectId: ProjectId.make("project-1"),
  };
  return {
    kind: "pending",
    key: `pending-task:${id}`,
    environmentId,
    projectId: creation.projectId,
    projectTitle: undefined,
    projectCwd: undefined,
    title: id,
    createdAt: NOW,
    message: {
      environmentId,
      threadId: ThreadId.make(`thread-${id}`),
      messageId: MessageId.make(id),
      commandId: CommandId.make(`command-${id}`),
      text: id,
      attachments: [],
      createdAt: NOW,
      creation,
    },
    creation,
  };
}

describe("buildThreadListV2ListItems", () => {
  const layout = buildThreadListV2Items({
    threads: [
      makeThread({ id: ThreadId.make("first"), title: "first", createdAt: NOW }),
      makeThread({ id: ThreadId.make("second"), title: "second", createdAt: "2026-06-01T00:00:00.000Z" }),
    ],
    environmentId: null,
    searchQuery: "",
    now: NOW,
  });

  it("leads the list with unsent drafts and queued tasks, preserving section dividers", () => {
    const items = buildThreadListV2ListItems({
      items: layout.items,
      pendingTasks: [
        {
          kind: "draft",
          key: "draft-task:new-task:unsent",
          environmentId,
          projectId: ProjectId.make("project-1"),
          projectTitle: undefined,
          projectCwd: undefined,
          title: "unsent",
          createdAt: NOW,
          draftKey: "new-task:unsent",
          draft: {
            text: "unsent",
            attachments: [],
            project: { environmentId, projectId: ProjectId.make("project-1"), createdAt: NOW },
          },
        },
        makePendingTask("queued-1"),
        makePendingTask("queued-2"),
      ],
    });

    expect(
      items.map((item) =>
        item.type === "v2-pending"
          ? item.pendingTask.title
          : item.type === "v2-thread"
            ? item.item.thread.id
            : item.type,
      ),
    ).toEqual(["unsent", "queued-1", "queued-2", "first", "second"]);
    expect(
      items.flatMap((item) => (item.type === "v2-pending" ? [item.showPendingDivider] : [])),
    ).toEqual([true, false, false]);
    expect(
      items.map((item) =>
        item.type === "v2-thread" || item.type === "v2-pending" ? item.showTrailingDivider : null,
      ),
    ).toEqual([true, true, true, true, false]);
    expect(threadJumpTarget(items, "thread.jump.1")?.id).toBe("first");
    expect(threadJumpTarget(items, "thread.jump.2")?.id).toBe("second");
    expect(threadJumpTarget(items, "thread.jump.3")).toBeNull();
  });
});

const BASE_MS = Date.parse(NOW);
const isoAt = (ms: number) => new Date(ms).toISOString();
const MINUTE_MS = 60_000;

describe("threadListV2ListItemsAreEqual", () => {
  const thread = makeThread({
    id: ThreadId.make("eq"),
    title: "eq",
    latestUserMessageAt: isoAt(BASE_MS - 5 * MINUTE_MS),
  });
  const queued = makePendingTask("eq-queued");
  const build = (queuedThreadKeys: ReadonlySet<string> = new Set()) =>
    buildThreadListV2ListItems({
      items: buildThreadListV2Items({
        threads: [thread],
        environmentId: null,
        searchQuery: "",
        now: NOW,
      }).items,
      pendingTasks: [queued],
      queuedThreadKeys,
    });
  const threadItem = (items: ThreadListV2ListItem[]) =>
    items.find((item) => item.type === "v2-thread")!;

  it("treats rebuilt wrappers over identical rows as equal", () => {
    const first = build();
    const second = build();
    expect(first.length).toBe(second.length);
    for (let index = 0; index < first.length; index += 1) {
      expect(first[index]).not.toBe(second[index]);
      expect(threadListV2ListItemsAreEqual(first[index]!, second[index]!)).toBe(true);
    }
  });

  it("notices queued outbox messages on the matching row", () => {
    const queuedItem = threadItem(build(new Set([`${environmentId}:eq`])));
    const plainItem = threadItem(build());
    expect(queuedItem.type === "v2-thread" && queuedItem.hasQueuedMessages).toBe(true);
    expect(threadListV2ListItemsAreEqual(queuedItem, plainItem)).toBe(false);
  });

  it("notices a trailing-divider flip caused by a neighbour change", () => {
    const threadA = makeThread({ id: ThreadId.make("flip-a"), title: "flip a", createdAt: NOW });
    const threadB = makeThread({
      id: ThreadId.make("flip-b"),
      title: "flip b",
      createdAt: "2026-06-01T00:00:00.000Z",
    });
    const items = (threads: ReadonlyArray<EnvironmentThreadShell>) =>
      buildThreadListV2ListItems({
        items: buildThreadListV2Items({ threads, environmentId: null, searchQuery: "", now: NOW })
          .items,
        pendingTasks: [],
      });
    const firstA = items([threadA, threadB])[0]!;
    const secondA = items([threadA])[0]!;
    expect(firstA.type === "v2-thread" && firstA.showTrailingDivider).toBe(true);
    expect(secondA.type === "v2-thread" && secondA.showTrailingDivider).toBe(false);
    expect(threadListV2ListItemsAreEqual(firstA, secondA)).toBe(false);
  });
});

describe("Working section beta", () => {
  const running = {
    status: "running" as const,
    activeRunId: null,
    providerInstanceId: ProviderInstanceId.make("codex"),
    providerName: "Codex",
    lastError: null,
    updatedAt: NOW,
  };
  const finishedAt = (id: string, completedAt: string) =>
    makeThread({
      id: ThreadId.make(id),
      title: id,
      createdAt: "2026-06-01T00:00:00.000Z",
      latestRun: {
        runId: RunId.make(`run-${id}`),
        status: "completed",
        requestedAt: "2026-06-01T00:00:00.000Z",
        startedAt: "2026-06-01T00:00:00.000Z",
        completedAt,
        assistantMessageId: null,
      },
    });
  const threads = [
    finishedAt("finished-early", "2026-06-01T01:00:00.000Z"),
    finishedAt("finished-late", "2026-06-01T03:00:00.000Z"),
    makeThread({ id: ThreadId.make("working"), title: "working", runtime: running }),
    makeThread({
      id: ThreadId.make("asks-approval"),
      title: "asks-approval",
      createdAt: "2026-06-01T02:00:00.000Z",
      runtime: running,
      hasPendingApprovals: true,
    }),
  ];
  const build = (input: Partial<Parameters<typeof buildThreadListV2Items>[0]> = {}) =>
    buildThreadListV2Items({
      threads,
      environmentId: null,
      searchQuery: "",
      now: NOW,
      workingShelfEnabled: true,
      ...input,
    });
  const ids = (layout: ReturnType<typeof buildThreadListV2Items>) =>
    layout.items.map((item) => item.thread.id);

  it("folds working threads into a collapsed shelf", () => {
    const layout = build();
    expect(ids(layout)).toEqual(["finished-late", "asks-approval", "finished-early"]);
    expect(layout.workingCount).toBe(1);
    expect(layout.workingShelfHeaderIndex).toBe(3);

    const off = build({ workingShelfEnabled: false });
    expect(ids(off)).toContain("working");
    expect(off.workingCount).toBe(0);
  });

  it("keeps an inactive row in Working during the shared grace and returns it on expiry", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(NOW));
    const tracker = createInboxReturnTracker();
    const working = makeThread({ id: ThreadId.make("grace"), title: "grace", runtime: running });
    const inactive = { ...working, runtime: null };
    const render = () =>
      build({
        threads: [inactive],
        workingShelfExpanded: true,
        isWorking: tracker.isWorking,
        inboxReturnAt: tracker.returnedAt,
      });
    try {
      tracker.observe([working]);
      tracker.observe([inactive]);
      expect(render().workingCount).toBe(1);
      expect(render().items[0]?.thread.runtime).toBeNull();
      vi.advanceTimersByTime(2_000);
      expect(render().workingCount).toBe(0);
      expect(ids(render())).toEqual(["grace"]);
      expect(tracker.returnedAt(inactive)).toBe(Date.parse(NOW) + 2_000);
    } finally {
      tracker.observe(null);
      vi.useRealTimers();
    }
  });

  it("orders the inbox by the latest return this device observed", () => {
    const layout = build({
      inboxReturnAt: (thread) =>
        thread.id === "finished-early" ? Date.parse("2026-06-01T04:00:00.000Z") : undefined,
    });
    expect(ids(layout)).toEqual(["finished-early", "finished-late", "asks-approval"]);
  });


  it("places the shelf after queued tasks and active threads", () => {
    const layout = buildThreadListV2Items({
      threads: [
        makeThread({ id: ThreadId.make("active"), title: "active" }),
        makeThread({ id: ThreadId.make("working"), title: "working", runtime: running }),
      ],
      environmentId: null,
      searchQuery: "",
      now: NOW,
      workingShelfEnabled: true,
      workingShelfExpanded: true,
    });
    const items = buildThreadListV2ListItems({
      items: layout.items,
      pendingTasks: [makePendingTask("queued")],
      workingCount: layout.workingCount,
      workingShelfExpanded: true,
      workingShelfHeaderIndex: layout.workingShelfHeaderIndex,
    });
    expect(
      items.map((item) =>
        item.type === "v2-thread"
          ? item.item.thread.id
          : item.type === "v2-pending"
            ? item.pendingTask.title
            : item.type,
      ),
    ).toEqual(["queued", "active", "v2-working-shelf", "working"]);
  });
});
