import { useActiveNewTask } from "../../state/active-new-task";
import {
  pickableProjectGroups,
  resolveScratchProjectScopeKey,
} from "@t3tools/client-runtime/state/project-grouping";
import type {
  EnvironmentProject,
  EnvironmentThreadShell,
} from "@t3tools/client-runtime/state/shell";
import { threadSearchMatchKey } from "@t3tools/client-runtime/state/thread-search";
import { AnimatedLegendList } from "@legendapp/list/reanimated";
import { THREAD_SIDEBAR_ROW_LAYOUT } from "./threadListAnimations";
import { useAtomValue } from "@effect/atom-react";
import { useMemo, useRef } from "react";
import { StyleSheet, View } from "react-native";
import { GestureDetector, useNativeGesture } from "react-native-gesture-handler";
import type { SwipeableMethods } from "react-native-gesture-handler/ReanimatedSwipeable";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import type { SearchBarCommands } from "react-native-screens";

import { AppText as Text } from "../../components/ui/AppText";
import { NATIVE_LIQUID_GLASS_SUPPORTED } from "../../native/native-glass";
import { NativeStackScreenOptions } from "../../native/StackHeader";
import { useNowMinuteMs } from "../../lib/clock";
import { scopedProjectKey, scopedThreadKey } from "../../lib/scopedEntities";
import { useProjects, useNavigationThreadShells } from "../../state/entities";
import { useThreadSearch } from "../../state/queries";
import { useThreadListV2ShelfPreferences } from "./use-thread-list-v2-shelf-preferences";
import { threadListEnvironmentsAtom } from "../../state/presentation";
import { usePendingNewTasks, type PendingNewTask } from "../../state/use-pending-new-tasks";
import { useQueuedThreadKeys } from "../../state/use-thread-outbox";
import { useWorkspaceState } from "../../state/workspace";
import { useSavedRemoteConnections } from "../../state/use-remote-environment-registry";
import { useHardwareKeyboardCommand } from "../keyboard/hardwareKeyboardCommands";
import { useThreadListV2 } from "./useThreadListV2";
import { useThreadJumpShortcuts } from "../keyboard/threadKeyboardShortcuts";
import { requestThreadMessageJump } from "./threadMessageJump";
import { useThreadListSearchMatches } from "./useThreadListSearchMatches";
import { useHomeListOptions } from "../home/home-list-options";
import { buildHomeListFilterMenu } from "../home/home-list-filter-menu";
import { buildHomeProjectScopes } from "../home/homeThreadList";
import { SwipeableScrollGateProvider, useSwipeableScrollGate } from "../home/thread-swipe-actions";
import { usePendingTaskListActions } from "../home/usePendingTaskListActions";
import { useThreadListActions } from "../home/useThreadListActions";
import { getConnectionAwareBrandHeaderOptions } from "../home/WorkspaceConnectionTitle";
import { createSidebarHeaderItems } from "./sidebar-native-header-items";
import { SidebarNavigationShell } from "./sidebar-navigation-shell";
import {
  ThreadListV2PendingRow,
  ThreadListV2Row,
  ThreadListV2WorkingShelfHeader,
} from "./thread-list-v2-items";
import { useThreadRowProviderInstanceResolver } from "./thread-provider-instance";
import { threadListV2ListItemsAreEqual, type ThreadListV2ListItem } from "./threadListV2";
import { useSelectWorkspaceDetail } from "../workspace/useSelectWorkspaceDetail";
import { NativeScrollView } from "../../native/NativeScrollViewMarker";

interface ThreadNavigationSidebarProps {
  readonly width: number;
  readonly visible: boolean;
  readonly selectedThreadKey: string | null;
  readonly onOpenSettings: () => void;
  readonly onOpenEnvironmentSettings: () => void;
  readonly onNewThreadInProject: (project: EnvironmentProject) => void;
  readonly onOpenPendingTask: (pendingTask: PendingNewTask) => void;
  readonly onSearchQueryChange: (query: string) => void;
  readonly onSelectThread: (thread: EnvironmentThreadShell) => void;
  readonly onRequestVisibility: () => void;
  readonly searchQuery: string;
}

export function ThreadNavigationSidebar(props: ThreadNavigationSidebarProps) {
  return (
    <View
      testID="thread-navigation-sidebar"
      className="flex-1 border-border bg-drawer"
      style={{ borderRightWidth: StyleSheet.hairlineWidth, width: props.width }}
    >
      <SidebarNavigationShell>
        <ThreadNavigationSidebarPane {...props} />
      </SidebarNavigationShell>
    </View>
  );
}

function ThreadNavigationSidebarPane(props: ThreadNavigationSidebarProps) {
  const insets = useSafeAreaInsets();
  const projects = useProjects();
  const threads = useNavigationThreadShells();
  const { environments: workspaceEnvironments, state: catalogState } = useWorkspaceState();
  const { savedConnectionsById } = useSavedRemoteConnections();
  const searchBarRef = useRef<SearchBarCommands>(null);
  const openSwipeableRef = useRef<SwipeableMethods | null>(null);
  const sidebarScrollGesture = useNativeGesture();
  const { archiveThread, confirmDeleteThread, renameThread, regenerateThreadTitle } =
    useThreadListActions();
  const pendingTasks = usePendingNewTasks();
  const activeNewTask = useActiveNewTask();
  const queuedThreadKeys = useQueuedThreadKeys();
  const selectDetail = useSelectWorkspaceDetail();
  const { confirmDeletePendingTask } = usePendingTaskListActions((params) =>
    selectDetail({ name: "Thread", params }),
  );
  const environments = Object.values(savedConnectionsById)
    .map((connection) => ({
      environmentId: connection.environmentId,
      label: connection.environmentLabel,
    }))
    .sort((left, right) => left.label.localeCompare(right.label));
  const availableEnvironmentIds = new Set(
    environments.map((environment) => environment.environmentId),
  );
  const { options, setSelectedEnvironmentId, setSelectedProjectKey } =
    useHomeListOptions(availableEnvironmentIds);
  const searchEnvironmentIds =
    options.selectedEnvironmentId === null
      ? workspaceEnvironments
          .filter((environment) => environment.connectionState === "connected")
          .map((environment) => environment.environmentId)
      : workspaceEnvironments.some(
            (environment) =>
              environment.environmentId === options.selectedEnvironmentId &&
              environment.connectionState === "connected",
          )
        ? [options.selectedEnvironmentId]
        : [];
  // A legacy per-environment scratch key resolves here on every render; the stored
  // value is only rewritten when the user picks a filter.
  const resolvedProjectKey = resolveScratchProjectScopeKey(options.selectedProjectKey, projects, {
    sidebarProjectGroupingMode: options.projectGroupingMode,
    sidebarProjectGroupingOverrides: {},
  });
  const projectScopes = useMemo(
    () =>
      buildHomeProjectScopes({
        projects,
        environmentId: options.selectedEnvironmentId,
        projectGroupingMode: options.projectGroupingMode,
      }),
    [projects, options.selectedEnvironmentId, options.projectGroupingMode],
  );
  const projectFilterOptions = pickableProjectGroups(projectScopes).map((scope) => ({
    key: scope.key,
    label: scope.title,
  }));
  const projectTitleByProjectKey = useMemo(
    () =>
      new Map(
        projectScopes.flatMap((scope) =>
          scope.projectRefs.map(
            (projectRef) =>
              [
                scopedProjectKey(projectRef.environmentId, projectRef.projectId),
                scope.title,
              ] as const,
          ),
        ),
      ),
    [projectScopes],
  );
  // An archived project's filter reads as all projects, matching the filter menu.
  const selectedProjectScope =
    pickableProjectGroups(projectScopes).find((scope) => scope.key === resolvedProjectKey) ?? null;
  const threadSearch = useThreadSearch(
    searchEnvironmentIds,
    props.searchQuery,
    selectedProjectScope === null
      ? {}
      : { projectIds: selectedProjectScope.projectRefs.map((ref) => ref.projectId) },
  );
  // This map reaches LegendList's extraData, which re-renders every row whenever it changes
  // identity; the hook keeps it stable.
  const threadSearchMatchByKey = useThreadListSearchMatches(threads, threadSearch);
  const selectedProjectKey = selectedProjectScope?.key ?? null;
  const projectByKey = useMemo(() => {
    const map = new Map<string, EnvironmentProject>();
    for (const project of projects) {
      map.set(scopedProjectKey(project.environmentId, project.id), project);
    }
    return map;
  }, [projects]);

  const {
    loaded: shelfPreferencesLoaded,
    workingShelfEnabled,
    workingShelfExpanded,
    toggleWorkingShelf,
  } = useThreadListV2ShelfPreferences();
  const now = new Date(useNowMinuteMs()).toISOString();
  const listEnvironments = useAtomValue(threadListEnvironmentsAtom);
  const { providersByEnvironmentId, machineByEnvironmentId, titleRegenerationEnvironmentIds } =
    listEnvironments;
  const resolveProviderInstance = useThreadRowProviderInstanceResolver(providersByEnvironmentId);
  const {
    items: listItems,
    contentOnlyThreadKeys,
  } = useThreadListV2({
    threads,
    pendingTasks,
    environmentId: options.selectedEnvironmentId,
    projectRefs: selectedProjectScope === null ? null : selectedProjectScope.projectRefs,
    searchQuery: props.searchQuery,
    searchMatches: threadSearchMatchByKey,
    queuedThreadKeys,
    now,
    shelfPreferencesLoaded,
    workingShelfEnabled,
    workingShelfExpanded,
    selectedThreadKey: props.selectedThreadKey ?? null,
  });
  const handleSwipeableWillOpen = (methods: SwipeableMethods) => {
    if (openSwipeableRef.current !== methods) {
      openSwipeableRef.current?.close();
      openSwipeableRef.current = methods;
    }
  };
  const handleSwipeableClose = (methods: SwipeableMethods) => {
    if (openSwipeableRef.current === methods) {
      openSwipeableRef.current = null;
    }
  };
  // Destructure callback props; capturing all props would invalidate these row-closing handlers on unrelated changes.
  const { onSelectThread } = props;
  // A row listed for a message match opens the thread at that message, by
  // tap or by shortcut.
  const handleSelectThread = (thread: EnvironmentThreadShell) => {
    const key = threadSearchMatchKey({ environmentId: thread.environmentId, threadId: thread.id });
    const messageId = contentOnlyThreadKeys.has(key)
      ? threadSearchMatchByKey.get(key)?.messageId
      : undefined;
    if (messageId) {
      requestThreadMessageJump(scopedThreadKey(thread.environmentId, thread.id), messageId);
    }
    onSelectThread(thread);
    openSwipeableRef.current?.close();
  };
  const handleScrollBeginDrag = () => {
    openSwipeableRef.current?.close();
  };
  const { swipeEnabled, scrollGateHandlers } = useSwipeableScrollGate({
    onScrollBeginDrag: handleScrollBeginDrag,
  });
  // Recycle equality needs project/search data for titles and favicons; keep the minute clock on items to limit updates.
  const listExtraData = {
    activeNewTask,
    selectedThreadKey: props.selectedThreadKey ?? "",
    projectByKey,
    projectTitleByProjectKey,
    savedConnectionsById,
    listEnvironments,
    threadSearchMatchByKey,
    workingShelfEnabled,
  };
  useThreadJumpShortcuts(listItems, handleSelectThread);
  const focusSearch = () => {
    const focus = () => searchBarRef.current?.focus();
    if (!props.visible) {
      props.onRequestVisibility();
      setTimeout(focus, 240);
    } else {
      focus();
    }
    return true;
  };
  useHardwareKeyboardCommand("focusSearch", focusSearch);
  const renderListItem = ({ item }: { readonly item: ThreadListV2ListItem }) => {
    switch (item.type) {
      case "v2-pending": {
        const pendingScopeKey = scopedProjectKey(
          item.pendingTask.environmentId,
          item.pendingTask.projectId,
        );
        return (
          <ThreadListV2PendingRow
            pendingTask={item.pendingTask}
            selected={
              activeNewTask?.pendingKey === item.pendingTask.key ||
              (item.pendingTask.kind === "pending" &&
                scopedThreadKey(
                  item.pendingTask.environmentId,
                  item.pendingTask.message.threadId,
                ) === props.selectedThreadKey)
            }
            project={projectByKey.get(pendingScopeKey) ?? null}
            projectTitle={projectTitleByProjectKey.get(pendingScopeKey)}
            environmentLabel={
              Object.keys(savedConnectionsById).length > 1
                ? (savedConnectionsById[item.pendingTask.environmentId]?.environmentLabel ?? null)
                : null
            }
            environmentMachine={machineByEnvironmentId.get(item.pendingTask.environmentId)}
            pane="sidebar"
            showPendingDivider={item.showPendingDivider}
            onSelectPendingTask={props.onOpenPendingTask}
            onDeletePendingTask={confirmDeletePendingTask}
            fullSwipeWidth={props.width - 20}
            onSwipeableClose={handleSwipeableClose}
            onSwipeableWillOpen={handleSwipeableWillOpen}
            simultaneousSwipeGesture={sidebarScrollGesture}
          />
        );
      }
      case "v2-thread": {
        const thread = item.item.thread;
        const scopeKey = scopedProjectKey(thread.environmentId, thread.projectId);
        return (
          <ThreadListV2Row
            thread={thread}
            hasQueuedMessages={item.hasQueuedMessages}
            timeLabel={item.timeLabel}
            project={projectByKey.get(scopeKey) ?? null}
            providerInstance={resolveProviderInstance(thread)}
            providers={providersByEnvironmentId.get(thread.environmentId)}
            environmentLabel={
              Object.keys(savedConnectionsById).length > 1
                ? (savedConnectionsById[thread.environmentId]?.environmentLabel ?? null)
                : null
            }
            environmentMachine={machineByEnvironmentId.get(thread.environmentId)}
            searchMatch={
              // Only rows listed for their messages show one and open at it.
              contentOnlyThreadKeys.has(
                threadSearchMatchKey({ environmentId: thread.environmentId, threadId: thread.id }),
              )
                ? threadSearchMatchByKey.get(
                    threadSearchMatchKey({
                      environmentId: thread.environmentId,
                      threadId: thread.id,
                    }),
                  )
                : undefined
            }
            searchQuery={threadSearch.query}
            pane="sidebar"
            selected={
              !activeNewTask &&
              scopedThreadKey(thread.environmentId, thread.id) === props.selectedThreadKey
            }
            fullSwipeWidth={props.width - 20}
            onSelectThread={handleSelectThread}
            onDeleteThread={confirmDeleteThread}
            onArchiveThread={archiveThread}
            onRenameThread={renameThread}
            onRegenerateThreadTitle={regenerateThreadTitle}
            titleRegenerationSupported={titleRegenerationEnvironmentIds.has(thread.environmentId)}
            onSwipeableClose={handleSwipeableClose}
            onSwipeableWillOpen={handleSwipeableWillOpen}
            simultaneousSwipeGesture={sidebarScrollGesture}
          />
        );
      }
      case "v2-working-shelf":
        return (
          <ThreadListV2WorkingShelfHeader
            count={item.count}
            disabled={item.disabled}
            expanded={item.expanded}
            onToggle={toggleWorkingShelf}
            pane="sidebar"
          />
        );
    }
  };
  const filterCustomized = options.selectedEnvironmentId !== null || selectedProjectKey !== null;
  const filterIcon = filterCustomized
    ? "line.3.horizontal.decrease.circle.fill"
    : "line.3.horizontal.decrease.circle";
  const filterMenu = buildHomeListFilterMenu({
    environments,
    projects: projectFilterOptions,
    selectedEnvironmentId: options.selectedEnvironmentId,
    selectedProjectKey,
    onEnvironmentChange: setSelectedEnvironmentId,
    onProjectChange: setSelectedProjectKey,
  });
  const nativeHeaderItems = createSidebarHeaderItems({
    filterIcon,
    filterMenu,
    onOpenSettings: props.onOpenSettings,
  });
  const listEmpty = (
    <Text className="px-2 py-4 text-sm text-drawer-foreground-muted">
      {catalogState.isLoadingConnections
        ? "Loading threads…"
        : props.searchQuery.trim().length > 0
          ? threadSearch.isPending
            ? "Searching thread messages…"
            : "No matching threads"
          : selectedProjectScope !== null
            ? `No threads in ${selectedProjectScope.title}`
            : "No threads yet"}
    </Text>
  );

  return (
      <>
        <NativeStackScreenOptions
          optionsVersion={[nativeHeaderItems, props.width]}
          options={{
            ...getConnectionAwareBrandHeaderOptions({
              headerWidth: props.width,
              trailingItemCount: nativeHeaderItems.length,
              onOpenEnvironments: props.onOpenEnvironmentSettings,
              fallbackTitleStyle: { fontSize: 18, fontWeight: "800" },
            }),
            headerSearchBarOptions: {
              ref: searchBarRef,
              autoCapitalize: "none",
              hideNavigationBar: false,
              hideWhenScrolling: false,
              obscureBackground: false,
              placeholder: "Search",
              placement: "stacked",
              onCancelButtonPress: () => {
                props.onSearchQueryChange("");
              },
              onChangeText: (event) => {
                props.onSearchQueryChange(event.nativeEvent.text);
              },
            },
            unstable_headerRightItems: () => nativeHeaderItems,
          }}
        />
        <View className="flex-1">
          <SwipeableScrollGateProvider enabled={swipeEnabled}>
            <GestureDetector gesture={sidebarScrollGesture}>
              <AnimatedLegendList
                renderScrollComponent={NativeScrollView}
                itemLayoutAnimation={THREAD_SIDEBAR_ROW_LAYOUT}
                data={listItems}
                drawDistance={500}
                estimatedItemSize={64}
                extraData={listExtraData}
                getItemType={(item) => item.type}
                itemsAreEqual={threadListV2ListItemsAreEqual}
                keyExtractor={(item) => item.key}
                renderItem={renderListItem}
                automaticallyAdjustsScrollIndicatorInsets={NATIVE_LIQUID_GLASS_SUPPORTED}
                contentInsetAdjustmentBehavior={
                  NATIVE_LIQUID_GLASS_SUPPORTED ? "automatic" : "never"
                }
                contentContainerStyle={[
                  styles.threadListContent,
                  {
                    paddingBottom: Math.max(insets.bottom, 16) + 16,
                    paddingTop: 6,
                  },
                ]}
                keyboardDismissMode="on-drag"
                keyboardShouldPersistTaps="handled"
                {...scrollGateHandlers}
                recycleItems
                scrollEventThrottle={16}
                showsVerticalScrollIndicator={false}
                style={styles.threadList}
                ListEmptyComponent={listEmpty}
              />
            </GestureDetector>
          </SwipeableScrollGateProvider>
        </View>
      </>
  );
}

const styles = StyleSheet.create({
  threadList: {
    flex: 1,
  },
  threadListContent: {
    paddingHorizontal: 8,
  },
});
