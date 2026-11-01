import type { EnvironmentThreadShell } from "@t3tools/client-runtime/state/shell";
import { resolveThreadProviderStack } from "@t3tools/client-runtime/state/models";
import {
  threadSearchMatchKey,
  threadSearchSubject,
} from "@t3tools/client-runtime/state/thread-search";
import { foldSearchQuery, rankSearchItems, scoreThreadSearch } from "@t3tools/shared/paletteSearch";
import {
  createInboxReturnTracker,
  isThreadWorking,
  sortInboxThreadsByReturn,
  sortWorkingThreadsBySend,
} from "@t3tools/client-runtime/state/thread-inbox";
import { sortActiveThreadsByOrderKey } from "@t3tools/client-runtime/state/thread-sort";
import { isListedThreadLineage, type EnvironmentId, type ProjectId } from "@t3tools/contracts";

import type { ThreadListProvider } from "../../state/thread-list-environments";
import { relativeTime } from "../../lib/time";
import type { PendingNewTask } from "../../state/use-pending-new-tasks";

export const threadListInboxReturns = createInboxReturnTracker();

export function resolveThreadListV2ProviderDrivers(
  thread: Pick<EnvironmentThreadShell, "providerInstanceHistory" | "modelSelection" | "runtime">,
  providers: ReadonlyArray<ThreadListProvider> | undefined,
): ReadonlyArray<string> {
  if (providers === undefined) return [];
  const stack = resolveThreadProviderStack(thread);
  const drivers = stack.flatMap((instanceId) => {
    const driver = providers.find((provider) => provider.instanceId === instanceId)?.driver;
    return driver === undefined ? [] : [driver];
  });
  const currentDriver = providers.find(
    (provider) => provider.instanceId === stack[stack.length - 1],
  )?.driver;
  return currentDriver === undefined ? [] : drivers;
}

export type ThreadListV2Status =
  | "approval"
  | "input"
  | "working"
  | "waiting"
  | "failed"
  | "limited"
  | "ready";

export function threadHasUnseenCompletion(
  thread: Pick<EnvironmentThreadShell, "latestRun" | "lastVisitedAt">,
): boolean {
  const completedAt = thread.latestRun?.completedAt;
  if (!completedAt) return false;
  const completedAtMs = Date.parse(completedAt);
  if (Number.isNaN(completedAtMs)) return false;
  if (!thread.lastVisitedAt) return false;
  const lastVisitedAtMs = Date.parse(thread.lastVisitedAt);
  if (Number.isNaN(lastVisitedAtMs)) return true;
  return completedAtMs > lastVisitedAtMs;
}

export function resolveThreadListV2Status(
  thread: Pick<
    EnvironmentThreadShell,
    | "hasPendingApprovals"
    | "hasPendingUserInput"
    | "pendingSubagentRequest"
    | "runtime"
    | "blockedOnThreadWait"
  >,
): ThreadListV2Status {
  if (thread.hasPendingApprovals || thread.pendingSubagentRequest === "approval") {
    return "approval";
  }
  if (thread.hasPendingUserInput || thread.pendingSubagentRequest === "user_input") {
    return "input";
  }
  if (thread.runtime?.status === "running" && thread.blockedOnThreadWait === true) {
    return "waiting";
  }
  if (
    thread.runtime !== null &&
    ["preparing", "queued", "starting", "running", "waiting"].includes(thread.runtime.status)
  ) {
    return "working";
  }
  if (thread.runtime?.status === "idle") {
    return "waiting";
  }
  if (thread.runtime?.status === "failed") {
    return thread.runtime.lastErrorClass === "usage_limit" ? "limited" : "failed";
  }
  return "ready";
}

export interface ThreadListV2Item {
  readonly thread: EnvironmentThreadShell;
  readonly isLast: boolean;
}

export interface ThreadListV2Layout {
  readonly items: ThreadListV2Item[];
  readonly workingCount: number;
  readonly workingShelfHeaderIndex: number | null;
  /** Threads listed only because their messages match the search. */
  readonly contentOnlyThreadKeys: ReadonlySet<string>;
}

export interface ThreadListV2ThreadListItem {
  readonly type: "v2-thread";
  readonly key: string;
  readonly item: ThreadListV2Item;
  readonly timeLabel: string;
  readonly showTrailingDivider: boolean;
  /** Include queued state in item equality; outbox writes do not update thread shells. */
  readonly hasQueuedMessages: boolean;
}

export interface ThreadListV2PendingListItem {
  readonly type: "v2-pending";
  readonly key: string;
  readonly pendingTask: PendingNewTask;
  readonly showPendingDivider: boolean;
  readonly showTrailingDivider: boolean;
}

export interface ThreadListV2WorkingShelfListItem {
  readonly type: "v2-working-shelf";
  readonly key: "v2-working-shelf";
  readonly count: number;
  readonly expanded: boolean;
  /** Include disabled state in item equality; recycled cells ignore render-closure updates. */
  readonly disabled: boolean;
}

export type ThreadListV2ListItem =
  | ThreadListV2ThreadListItem
  | ThreadListV2PendingListItem
  | ThreadListV2WorkingShelfListItem;

export function isThreadListV2ListItem(value: {
  readonly type: string;
}): value is ThreadListV2ListItem {
  return value.type === "v2-thread" || value.type === "v2-pending" || value.type === "v2-working-shelf";
}

/** Compare rebuilt items by content so minute ticks do not re-render every mounted row. */
export function threadListV2ListItemsAreEqual(
  previous: ThreadListV2ListItem,
  item: ThreadListV2ListItem,
): boolean {
  switch (item.type) {
    case "v2-thread":
      return (
        previous.type === "v2-thread" &&
        previous.key === item.key &&
        previous.item.thread === item.item.thread &&
        previous.timeLabel === item.timeLabel &&
        previous.showTrailingDivider === item.showTrailingDivider &&
        previous.hasQueuedMessages === item.hasQueuedMessages
      );
    case "v2-pending":
      return (
        previous.type === "v2-pending" &&
        previous.key === item.key &&
        previous.pendingTask === item.pendingTask &&
        previous.showPendingDivider === item.showPendingDivider &&
        previous.showTrailingDivider === item.showTrailingDivider
      );
    case "v2-working-shelf":
      return (
        previous.type === "v2-working-shelf" &&
        previous.count === item.count &&
        previous.expanded === item.expanded &&
        previous.disabled === item.disabled
      );
  }
}

function resolveThreadListV2ItemTimeLabel(thread: EnvironmentThreadShell): string {
  if (resolveThreadListV2Status(thread) !== "ready" || threadHasUnseenCompletion(thread)) return "";
  return relativeTime(thread.latestUserMessageAt ?? thread.updatedAt ?? thread.createdAt);
}

export function buildThreadListV2ListItems(input: {
  readonly items: ReadonlyArray<ThreadListV2Item>;
  readonly pendingTasks: ReadonlyArray<PendingNewTask>;
  readonly workingCount?: number;
  readonly workingShelfExpanded?: boolean;
  readonly workingShelfHeaderIndex?: number | null;
  readonly queuedThreadKeys?: ReadonlySet<string>;
  readonly shelfPreferencesLoading?: boolean;
}): ThreadListV2ListItem[] {
  const queuedThreadKeys = input.queuedThreadKeys?.size ? input.queuedThreadKeys : undefined;
  const threadItems = input.items.map(
    (item): ThreadListV2ListItem => ({
      type: "v2-thread",
      key: `v2-thread:${item.thread.environmentId}:${item.thread.id}`,
      item,
      timeLabel: resolveThreadListV2ItemTimeLabel(item.thread),
      showTrailingDivider: false,
      hasQueuedMessages:
        queuedThreadKeys?.has(`${item.thread.environmentId}:${item.thread.id}`) === true,
    }),
  );
  const pendingItems = input.pendingTasks.map((pendingTask, index): ThreadListV2ListItem => ({
    type: "v2-pending",
    key: `v2-${pendingTask.key}`,
    pendingTask,
    showPendingDivider: index === 0,
    showTrailingDivider: false,
  }));
  const workingCount = input.workingCount ?? 0;
  const workingShelfHeaderIndex = input.workingShelfHeaderIndex ?? null;
  const activeEnd = workingShelfHeaderIndex ?? threadItems.length;
  const result: ThreadListV2ListItem[] = [...pendingItems, ...threadItems.slice(0, activeEnd)];
  if (workingShelfHeaderIndex !== null && workingCount > 0) {
    result.push({
      type: "v2-working-shelf",
      key: "v2-working-shelf",
      count: workingCount,
      expanded: input.workingShelfExpanded === true,
      disabled: input.shelfPreferencesLoading === true,
    });
    result.push(...threadItems.slice(workingShelfHeaderIndex));
  }
  return result.map((entry, index) => {
    if (entry.type === "v2-working-shelf") return entry;
    const next = result[index + 1];
    const showTrailingDivider =
      next?.type === "v2-thread" || (next?.type === "v2-pending" && !next.showPendingDivider);
    return showTrailingDivider === entry.showTrailingDivider
      ? entry
      : { ...entry, showTrailingDivider };
  });
}

export function buildThreadListV2Items(input: {
  readonly threads: ReadonlyArray<EnvironmentThreadShell>;
  readonly environmentId: EnvironmentId | null;
  readonly projectRefs?: ReadonlyArray<{
    readonly environmentId: EnvironmentId;
    readonly projectId: ProjectId;
  }> | null;
  readonly searchQuery: string;
  readonly matchedThreadKeys?: ReadonlySet<string>;
  readonly now: string;
  readonly workingShelfEnabled?: boolean;
  readonly workingShelfExpanded?: boolean;
  readonly isWorking?: (thread: EnvironmentThreadShell) => boolean;
  readonly inboxReturnAt?: (thread: EnvironmentThreadShell) => number | undefined;
  readonly selectedThreadKey?: string | null;
}): ThreadListV2Layout {
  const query = foldSearchQuery(input.searchQuery);
  const nowMs = Date.parse(input.now);
  const projectKeys = input.projectRefs
    ? new Set(input.projectRefs.map((ref) => `${ref.environmentId}:${ref.projectId}`))
    : null;

  const workingShelfEnabled = input.workingShelfEnabled === true;
  const active: EnvironmentThreadShell[] = [];
  const working: EnvironmentThreadShell[] = [];
  const listedThreads = input.threads.filter(
    (thread) =>
      thread.archivedAt === null &&
      isListedThreadLineage(thread.lineage) &&
      (input.environmentId === null || thread.environmentId === input.environmentId) &&
      (projectKeys === null || projectKeys.has(`${thread.environmentId}:${thread.projectId}`)),
  );
  // The same matches as the command palette, typo fallback included. Only
  // listed threads are ranked: a hidden exact match must not turn typos off.
  const rankedMatches =
    query.length > 0
      ? rankSearchItems(listedThreads, (thread, options) =>
          scoreThreadSearch(query, threadSearchSubject(thread, null), nowMs, {
            ...options,
            picks: false,
          }),
        ).map(({ item }) => item)
      : null;
  const matchingThreads = rankedMatches === null ? null : new Set(rankedMatches);
  // Threads listed only because their messages match; only their rows show
  // the message and open at it.
  const contentOnlyThreadKeys = new Set<string>();
  for (const thread of listedThreads) {
    if (matchingThreads !== null && !matchingThreads.has(thread)) {
      const key = threadSearchMatchKey({
        environmentId: thread.environmentId,
        threadId: thread.id,
      });
      if (input.matchedThreadKeys?.has(key) !== true) continue;
      contentOnlyThreadKeys.add(key);
    }
    if (workingShelfEnabled && (input.isWorking ?? isThreadWorking)(thread)) {
      working.push(thread);
    } else {
      active.push(thread);
    }
  }

  const withLast = (threads: ReadonlyArray<EnvironmentThreadShell>): ThreadListV2Item[] =>
    threads.map((thread, index) => ({ thread, isLast: index === threads.length - 1 }));

  if (rankedMatches !== null) {
    // Searching lists every match by rank, as the palette and the web sidebar
    // do, then threads listed for their messages in the server's order.
    const threadByKey = new Map(
      listedThreads.map((thread) => [
        threadSearchMatchKey({ environmentId: thread.environmentId, threadId: thread.id }),
        thread,
      ]),
    );
    const contentOnly = [...(input.matchedThreadKeys ?? [])].flatMap((key) => {
      const thread = contentOnlyThreadKeys.has(key) ? threadByKey.get(key) : undefined;
      return thread ? [thread] : [];
    });
    return {
      items: withLast([...rankedMatches, ...contentOnly]),
      workingCount: 0,
      workingShelfHeaderIndex: null,
      contentOnlyThreadKeys,
    };
  }

  const orderedActive = workingShelfEnabled
    ? sortInboxThreadsByReturn(active, input.inboxReturnAt)
    : sortActiveThreadsByOrderKey(active);
  const orderedWorking = sortWorkingThreadsBySend(working);
  const selectedThreadKey = input.selectedThreadKey ?? null;
  const visibleWorking =
    input.workingShelfExpanded === true
      ? orderedWorking
      : orderedWorking.filter(
          (thread) => `${thread.environmentId}:${thread.id}` === selectedThreadKey,
        );
  return {
    items: withLast([...orderedActive, ...visibleWorking]),
    workingCount: orderedWorking.length,
    workingShelfHeaderIndex: orderedWorking.length > 0 ? orderedActive.length : null,
    contentOnlyThreadKeys,
  };
}
