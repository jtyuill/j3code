import type { EnvironmentId } from "@t3tools/contracts";
import { useMemo, useSyncExternalStore } from "react";

import { scopedProjectKey } from "../../lib/scopedEntities";
import type { PendingNewTask } from "../../state/use-pending-new-tasks";
import {
  buildThreadListV2Items,
  buildThreadListV2ListItems,
  threadListInboxReturns,
} from "./threadListV2";

/**
 * Screens' select handlers read the content-only key set; a fresh one per list rebuild gave
 * every row a new handler whenever any thread shell changed. Keyed by a string, the set keeps
 * its identity until its keys change.
 */
function useKeySet(newlineSeparatedKeys: string): ReadonlySet<string> {
  return useMemo(
    () => new Set(newlineSeparatedKeys.length === 0 ? [] : newlineSeparatedKeys.split("\n")),
    [newlineSeparatedKeys],
  );
}

type LayoutInput = Parameters<typeof buildThreadListV2Items>[0];

/**
 * The React Compiler memoizes on the values a callback reads and drops unread entries from a
 * dependency list, so the version that marks an expired grace must be read to rebuild the list.
 */
function observeInbox(
  threads: Parameters<typeof threadListInboxReturns.observe>[0],
  _expiredGraceVersion: number,
) {
  return threadListInboxReturns.observe(threads);
}

/**
 * Home's and the thread sidebar's thread list. Its result is one value a screen only reads:
 * built inline, the list's inputs stayed open to mutation across the screen's other hooks, so
 * the React Compiler cached none of it and every render of the screen re-rendered every row.
 */
export function useThreadListV2(input: {
  readonly threads: LayoutInput["threads"];
  readonly pendingTasks: ReadonlyArray<PendingNewTask>;
  readonly environmentId: EnvironmentId | null;
  readonly projectRefs: LayoutInput["projectRefs"];
  readonly searchQuery: string;
  /** Threads that match the search by content, keyed like `threadSearchMatchKey`. */
  readonly searchMatches: ReadonlyMap<string, unknown>;
  readonly queuedThreadKeys: ReadonlySet<string>;
  readonly now: string;
  readonly shelfPreferencesLoaded: boolean;
  readonly workingShelfEnabled: boolean;
  readonly workingShelfExpanded: boolean;
  readonly selectedThreadKey: string | null;
}) {
  const { threads, workingShelfEnabled } = input;
  // A thread can stop working when the threads change, or when its exit grace expires.
  const inboxVersion = useSyncExternalStore(
    threadListInboxReturns.subscribe,
    threadListInboxReturns.getSnapshot,
  );
  const inbox = useMemo(
    () => observeInbox(workingShelfEnabled ? threads : null, inboxVersion),
    [threads, workingShelfEnabled, inboxVersion],
  );
  const layout = buildThreadListV2Items({
    threads,
    environmentId: input.environmentId,
    projectRefs: input.projectRefs,
    searchQuery: input.searchQuery,
    matchedThreadKeys: new Set(input.searchMatches.keys()),
    now: input.now,
    workingShelfEnabled,
    workingShelfExpanded: input.workingShelfExpanded,
    inboxReturnAt: inbox.returnedAt,
    isWorking: inbox.isWorking,
    selectedThreadKey: input.selectedThreadKey,
  });
  const contentOnlyThreadKeys = useKeySet([...layout.contentOnlyThreadKeys].sort().join("\n"));
  const scopedProjectKeys =
    input.projectRefs == null
      ? null
      : new Set(input.projectRefs.map((ref) => scopedProjectKey(ref.environmentId, ref.projectId)));
  const searchQuery = input.searchQuery.trim().toLocaleLowerCase();
  const pendingTasks = input.pendingTasks.filter(
    (pendingTask) =>
      (input.environmentId === null || pendingTask.environmentId === input.environmentId) &&
      (scopedProjectKeys === null ||
        scopedProjectKeys.has(
          scopedProjectKey(pendingTask.environmentId, pendingTask.projectId),
        )) &&
      (searchQuery.length === 0 || pendingTask.title.toLocaleLowerCase().includes(searchQuery)),
  );
  const items = buildThreadListV2ListItems({
    items: layout.items,
    pendingTasks,
    workingCount: layout.workingCount,
    workingShelfExpanded: input.workingShelfExpanded,
    workingShelfHeaderIndex: layout.workingShelfHeaderIndex,
    queuedThreadKeys: input.queuedThreadKeys,
    shelfPreferencesLoading: !input.shelfPreferencesLoaded,
  });
  return { layout, items, contentOnlyThreadKeys };
}
