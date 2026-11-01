import type { EnvironmentId, OrchestrationV2ProjectedTurnItem } from "@t3tools/contracts";
import {
  createThreadSearchResultsAtomFamily,
  makeThreadSearchKey,
  visibleThreadSearch,
  type EnvironmentThreadSearchMatch,
  type SettledThreadSearch,
  type ThreadSearchResultsState,
  type ThreadSearchScope,
} from "@t3tools/client-runtime/state/thread-search";
import { THREAD_SEARCH_QUERY_MAX_LENGTH } from "@t3tools/contracts";
import { messageSearchTokens } from "@t3tools/shared/paletteSearch";
import { useAtomValue } from "@effect/atom-react";
import * as Cause from "effect/Cause";
import { turnItemDetailRevision } from "@t3tools/client-runtime/work-log/item-detail";
import * as Option from "effect/Option";
import { AsyncResult, Atom } from "effect/reactivity";
import { useEffect, useMemo, useState } from "react";

import { appAtomRegistry } from "./atom-registry";
import { orchestrationEnvironment } from "./orchestration";
import { olderSettledThreads, threadFamilies } from "./threads";
import { projectEnvironment } from "./projects";
import { useEnvironmentQuery } from "./query";

const THREAD_SEARCH_DEBOUNCE_MS = 120;
const EMPTY_THREAD_SEARCH_MATCHES: ReadonlyArray<EnvironmentThreadSearchMatch> = Object.freeze([]);
const EMPTY_THREAD_SEARCH_ATOM = Atom.make<ThreadSearchResultsState>({
  matches: EMPTY_THREAD_SEARCH_MATCHES,
  isLoading: false,
  rows: [],
}).pipe(Atom.withLabel("mobile:thread-search:empty"));

const olderSettledBeforeAtom = Atom.family((environmentId: EnvironmentId) =>
  Atom.make((get) => get(olderSettledThreads.olderSettledAtom(environmentId))?.before ?? null).pipe(
    Atom.withLabel(`mobile:older-settled-before:${environmentId}`),
  ),
);
// Whether any of these environments leaves older settled threads out, so short queries matter.
const anyOlderSettledAtom = Atom.family((environmentIdsKey: string) =>
  Atom.make((get) =>
    (JSON.parse(environmentIdsKey) as ReadonlyArray<EnvironmentId>).some(
      (environmentId) => get(olderSettledBeforeAtom(environmentId)) !== null,
    ),
  ).pipe(Atom.withLabel(`mobile:any-older-settled:${environmentIdsKey}`)),
);
const threadSearch = createThreadSearchResultsAtomFamily({
  getSearchAtom: (environmentId, input) =>
    input.olderSettledBefore === undefined
      ? orchestrationEnvironment.threadSearch({ environmentId, input })
      : olderSettledThreads.searchAtom(environmentId, input),
  olderSettledBefore: olderSettledBeforeAtom,
  labelPrefix: "mobile:thread-search",
});

export function useDebouncedValue<A>(value: A, delayMs: number): A {
  const [debounced, setDebounced] = useState(value);

  // oxlint-disable-next-line t3code/no-unreasoned-state-sync -- timer that publishes the value after the delay and clears on change
  useEffect(() => {
    const timer = setTimeout(() => {
      setDebounced(value);
    }, delayMs);
    return () => {
      clearTimeout(timer);
    };
  }, [delayMs, value]);

  return debounced;
}

/**
 * Server matches for a thread search: message hits, and on environments that leave older settled
 * threads out of the shell, the rows of the best older matches, mounted into the thread list while
 * they show so the caller's own ranking finds them.
 */
export function useThreadSearch(
  environmentIds: ReadonlyArray<EnvironmentId>,
  query: string,
  scope: ThreadSearchScope = {},
): {
  readonly matches: ReadonlyArray<EnvironmentThreadSearchMatch>;
  /** The query `matches` are for, which lags the typed one while a search runs. */
  readonly query: string;
  readonly isPending: boolean;
} {
  // A pasted wall of text searches by its start rather than failing the request.
  const normalizedQuery = query.trim().slice(0, THREAD_SEARCH_QUERY_MAX_LENGTH).trim();
  const debouncedQuery = useDebouncedValue(normalizedQuery, THREAD_SEARCH_DEBOUNCE_MS);
  const anyOlderSettled = useAtomValue(anyOlderSettledAtom(JSON.stringify(environmentIds)));
  // Messages need a word of three or more letters; older settled titles match from two.
  const canSearch =
    environmentIds.length > 0 &&
    ((scope.titlesOnly !== true && messageSearchTokens(normalizedQuery).long.length > 0) ||
      (anyOlderSettled && normalizedQuery.length >= 2));
  const settledQuery = canSearch && normalizedQuery === debouncedQuery ? debouncedQuery : null;
  const searchKeyOf = (searchQuery: string) =>
    makeThreadSearchKey(environmentIds, searchQuery, scope);
  const searchKey = settledQuery === null ? null : searchKeyOf(settledQuery);
  const result = useAtomValue(
    searchKey === null ? EMPTY_THREAD_SEARCH_ATOM : threadSearch.results(searchKey),
  );
  const isPending = canSearch && (normalizedQuery !== debouncedQuery || result.isLoading);
  // What fast environments returned shows without waiting for a slow one;
  // callers keep shown rows in place with `nextShownOrder`.
  const ready =
    settledQuery !== null &&
    (!result.isLoading || result.matches.length > 0 || result.rows.length > 0)
      ? { query: settledQuery, matches: result.matches }
      : null;
  // Kept from earlier renders: React's pattern for information from previous renders.
  const [previous, setPrevious] = useState<SettledThreadSearch | null>(null);
  if (ready !== null && previous?.matches !== ready.matches) setPrevious(ready);
  const visible = canSearch
    ? visibleThreadSearch(normalizedQuery, ready, previous)
    : { query: "", matches: EMPTY_THREAD_SEARCH_MATCHES };
  // The shown results' rows stay mounted until newer results replace them.
  const shownKey = visible.query === "" ? null : searchKeyOf(visible.query);
  // oxlint-disable-next-line t3code/no-unreasoned-state-sync -- mounts the shown search results' rows into the shared thread registry until newer results replace them
  useEffect(() => {
    if (shownKey === null) return;
    const environments = (JSON.parse(shownKey) as [ReadonlyArray<EnvironmentId>])[0];
    const releases = environments.map((environmentId) =>
      threadFamilies.mountRows(
        appAtomRegistry,
        environmentId,
        `search:${shownKey}`,
        threadSearch.rows(shownKey, environmentId),
      ),
    );
    return () => {
      for (const release of releases) release();
    };
  }, [shownKey]);
  return {
    matches: visible.matches,
    query: visible.query,
    isPending,
  };
}

export function useTurnItemDetail(
  target: {
    readonly environmentId: EnvironmentId;
    readonly row: OrchestrationV2ProjectedTurnItem;
  } | null,
) {
  return useEnvironmentQuery(
    target === null
      ? null
      : orchestrationEnvironment.turnItem({
          environmentId: target.environmentId,
          input: {
            threadId: target.row.sourceThreadId,
            itemId: target.row.sourceItemId,
            revision: turnItemDetailRevision(target.row.item),
          },
        }),
  );
}
