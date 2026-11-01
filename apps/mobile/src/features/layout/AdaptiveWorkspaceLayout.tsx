import {
  WorkspaceColumnsRendererContext,
  WorkspacePresentationHostContext,
} from "./workspace-presentation-context";
import { AdaptiveWorkspaceContext, useAdaptiveWorkspaceLayout } from "./adaptive-workspace-context";
import { threadSelectionDeferral } from "./deferred-navigation";
import { uuidv4 } from "../../lib/uuid";
import type {
  EnvironmentProject,
  EnvironmentThreadShell,
} from "@t3tools/client-runtime/state/shell";
import { EnvironmentId, ThreadId, type SidebarProjectGroupingMode } from "@t3tools/contracts";
import { useAtomValue } from "@effect/atom-react";
import { useFocusEffect } from "@react-navigation/native";
import { NavigationContext, NavigationRouteContext, useNavigation } from "@react-navigation/native";
import { use, useEffect, useEffectEvent, useMemo, useRef, useState, type ReactNode } from "react";
import { useWindowDimensions, View } from "react-native";
import Animated, {
  useAnimatedStyle,
  useDerivedValue,
  useSharedValue,
  withTiming,
} from "react-native-reanimated";
import { AsyncResult } from "effect/reactivity";

import {
  deriveFileInspectorPaneLayout,
  deriveLayout,
  deriveWorkspacePaneLayout,
  isPrimarySidebarPinned,
  isWorkspaceListPath,
  type WorkspaceAuxiliaryPaneRole,
} from "../../lib/layout";
import { scopedThreadKey } from "../../lib/scopedEntities";
import type { ThreadListFilters } from "../../persistence/mobile-preferences";
import { appAtomRegistry } from "../../state/atom-registry";
import { useProjects } from "../../state/entities";
import { environmentThreadShells } from "../../state/threads";
import { mobilePreferencesAtom } from "../../state/preferences";
import {
  DEFAULT_MOBILE_PROJECT_GROUPING_SETTINGS,
  resolveMobileProjectGroupingSettings,
} from "../../state/project-grouping";
import {
  parseActiveThreadPath,
  useHardwareKeyboardCommand,
} from "../keyboard/hardwareKeyboardCommands";
import {
  selectWorkspaceDetail,
  type WorkspaceDetail,
} from "../workspace/workspace-detail-selection";
import { startOpeningDetail } from "../workspace/useSelectWorkspaceDetail";
import { useWorkspaceTabs } from "../workspace/WorkspaceTabsContext";
import { HomeListOptionsProvider } from "../home/home-list-options";
import { newTaskDestination } from "../threads/new-task-project-selection";
import { usePendingTaskListActions } from "../home/usePendingTaskListActions";
import { ThreadNavigationSidebar } from "../threads/ThreadNavigationSidebar";
import { RenderErrorBoundary, RenderFailureView } from "../../components/RenderErrorBoundary";
import { WORKSPACE_PANE_TIMING } from "./workspace-pane-animation";
import { WorkspaceInspectorPane } from "./workspace-inspector-pane";
import { WorkspaceContentWidthContext } from "./workspace-content-width";
import {
  NativeWorkspaceModeContext,
  NativeWorkspaceInspectorContext,
} from "../../native/v5-workspace-context";

import { useNativeLayoutMetrics } from "../../native/native-layout-metrics";
import { openSettings } from "../settings/settings-navigation";
import { dispatchRootStackReset, rootStackState } from "../../lib/root-stack-reset";
import {
  currentWorkspaceSelections,
  workspaceSelectionCheck,
} from "../workspace/workspace-selection-store";
import { workspaceTabNavigationState } from "../workspace/workspace-tab-navigation";
import type { WorkspaceTab } from "../workspace/workspace-tabs-state";
import { workspaceRoutesOf } from "./workspace-overlay-routes";

export { useAdaptiveWorkspaceLayout };

export function useAdaptiveWorkspacePaneRole(role: WorkspaceAuxiliaryPaneRole) {
  const { activateAuxiliaryPaneRole } = useAdaptiveWorkspaceLayout();

  useFocusEffect(() => activateAuxiliaryPaneRole(role));
}

/** Capture inspector navigation contexts. Register on focus events because frozen blurred screens may never clean up. */
export function useRegisterWorkspaceInspector(render: (() => ReactNode) | undefined) {
  const { registerWorkspaceInspector } = useAdaptiveWorkspaceLayout();
  const navigation = use(NavigationContext);
  const route = use(NavigationRouteContext);

  const wrappedRender = (() => {
    if (render === undefined) {
      return undefined;
    }
    return () => (
      <NavigationContext.Provider value={navigation}>
        <NavigationRouteContext.Provider value={route}>{render()}</NavigationRouteContext.Provider>
      </NavigationContext.Provider>
    );
  })();

  const focusedRef = useRef(false);
  const deactivateRef = useRef<(() => void) | null>(null);

  const syncRegistration = useEffectEvent(() => {
    if (!focusedRef.current || wrappedRender === undefined) {
      deactivateRef.current?.();
      return;
    }
    deactivateRef.current = registerWorkspaceInspector(wrappedRender);
  });

  // oxlint-disable-next-line t3code/no-unreasoned-state-sync -- Navigation focus events own the inspector registration lifetime.
  useEffect(() => {
    if (navigation === undefined) return;
    const focus = () => {
      if (focusedRef.current) return;
      focusedRef.current = true;
      syncRegistration();
    };
    const blur = () => {
      focusedRef.current = false;
      syncRegistration();
    };
    if (navigation.isFocused()) focus();
    const unsubscribeFocus = navigation.addListener("focus", focus);
    const unsubscribeBlur = navigation.addListener("blur", blur);
    return () => {
      unsubscribeFocus();
      unsubscribeBlur();
      focusedRef.current = false;
      deactivateRef.current?.();
      deactivateRef.current = null;
    };
  }, [navigation]);

  // oxlint-disable-next-line t3code/no-unreasoned-state-sync -- Changed pane content replaces the focused inspector registration.
  useEffect(() => {
    if (focusedRef.current) {
      syncRegistration();
    }
    // oxlint-disable-next-line react/exhaustive-effect-dependencies -- Changed pane content replaces its existing native workspace registration.
  }, [wrappedRender]);
}

type AdaptiveWorkspaceLayoutProps = {
  readonly children: ReactNode;
  readonly pathname: string;
  readonly workspaceRouteKey: string | undefined;
  readonly workspaceRouteName: string | undefined;
  readonly workspaceRouteKeys: ReadonlyArray<string>;
};

export function AdaptiveWorkspaceLayout(props: AdaptiveWorkspaceLayoutProps) {
  const preferencesResult = useAtomValue(mobilePreferencesAtom);
  if (!AsyncResult.isSuccess(preferencesResult)) {
    return AsyncResult.isFailure(preferencesResult) ? (
      <AdaptiveWorkspaceLayoutContent
        {...props}
        projectGroupingMode={DEFAULT_MOBILE_PROJECT_GROUPING_SETTINGS.sidebarProjectGroupingMode}
      />
    ) : null;
  }
  const groupingSettings = resolveMobileProjectGroupingSettings(preferencesResult.value);
  return (
    <AdaptiveWorkspaceLayoutContent
      {...props}
      projectGroupingMode={groupingSettings.sidebarProjectGroupingMode}
      savedThreadListFilters={preferencesResult.value.threadListFilters}
    />
  );
}

function AdaptiveWorkspaceLayoutContent(
  props: AdaptiveWorkspaceLayoutProps & {
    readonly projectGroupingMode: SidebarProjectGroupingMode;
    readonly savedThreadListFilters?: ThreadListFilters | undefined;
  },
) {
  const projectGroupingMode = props.projectGroupingMode;
  const nativeWorkspace = use(NativeWorkspaceModeContext);
  const usesPresentationHost = use(WorkspacePresentationHostContext);
  const windowDimensions = useWindowDimensions();
  const nativeMetrics = useNativeLayoutMetrics();
  const { width: viewportWidth, height } = nativeMetrics ?? windowDimensions;
  const pathname = props.pathname;
  const workspace = useWorkspaceTabs();
  const navigation = useNavigation();
  const activeRoleOwner = useRef<symbol | null>(null);
  const [primarySidebarPreferredVisible, setPrimarySidebarPreferredVisible] = useState(true);
  const [supplementaryPanePreferredVisible, setSupplementaryPanePreferredVisible] = useState(true);
  const [supplementaryPanePreferredWidth, setSupplementaryPanePreferredWidth] = useState<
    number | null
  >(null);
  const [fileInspectorPreferredVisible, setFileInspectorPreferredVisible] = useState(true);
  const [fileInspectorPreferredWidth, setFileInspectorPreferredWidth] = useState<number | null>(
    null,
  );
  const [primarySidebarSearchQuery, setPrimarySidebarSearchQuery] = useState("");
  const [focusedAuxiliaryPaneRole, setFocusedAuxiliaryPaneRole] =
    useState<WorkspaceAuxiliaryPaneRole | null>(null);
  // The compiler can't memoize these derivations: they stay live across later hook calls. Fresh
  // objects here change the workspace context on every render, so every workspace screen
  // re-renders on any layout state change, and the inspector registration then loops: each
  // re-register re-renders its screen, which hands over a new inspector renderer.
  const layout = useMemo(
    () => deriveLayout({ width: viewportWidth, height, nativeMetrics, nativeWorkspace }),
    [height, viewportWidth, nativeMetrics, nativeWorkspace],
  );
  // The overlay sidebar closes when a page opens from it, and stays open on a list or on the
  // page a tab switch opened (`keepOpenFor`, set by that switch).
  const [overlaySidebar, setOverlaySidebar] = useState({
    active: layout.primarySidebarOverlaysContent,
    pathname,
    visible: isWorkspaceListPath(pathname),
    keepOpenFor: null as string | null,
  });
  if (
    overlaySidebar.active !== layout.primarySidebarOverlaysContent ||
    (overlaySidebar.active && overlaySidebar.pathname !== pathname)
  ) {
    setOverlaySidebar({
      active: layout.primarySidebarOverlaysContent,
      pathname,
      visible:
        isWorkspaceListPath(pathname) ||
        (overlaySidebar.keepOpenFor !== null &&
          overlaySidebar.keepOpenFor === props.workspaceRouteKey),
      keepOpenFor: null,
    });
  }
  const preferredSidebarVisible = layout.primarySidebarOverlaysContent
    ? overlaySidebar.visible
    : primarySidebarPreferredVisible;
  const primarySidebarPinned = isPrimarySidebarPinned({
    pathname,
    nativeWorkspace,
    usesSplitView: layout.usesSplitView,
  });
  const [composerEditing, setComposerEditing] = useState(false);
  // iOS keeps the sidebar beside the soft keyboard.
  const composerKeyboardActive = false;
  const showPrimarySidebar = primarySidebarPinned || preferredSidebarVisible;
  const width = viewportWidth;
  const shouldRenderPrimarySidebar = layout.usesSplitView;
  const fileInspector = useMemo(
    () =>
      deriveFileInspectorPaneLayout({
        layout,
        viewportWidth: width,
        preferredWidth: fileInspectorPreferredWidth ?? undefined,
        reservedLeadingWidth:
          shouldRenderPrimarySidebar && showPrimarySidebar && !layout.primarySidebarOverlaysContent
            ? (layout.listPaneWidth ?? 0) + (layout.listPaneGap ?? 0)
            : 0,
      }),
    [fileInspectorPreferredWidth, layout, showPrimarySidebar, shouldRenderPrimarySidebar, width],
  );
  const auxiliaryPaneRole: WorkspaceAuxiliaryPaneRole = focusedAuxiliaryPaneRole ?? "supplementary";
  const auxiliaryPanePreferredVisible =
    auxiliaryPaneRole === "inspector"
      ? fileInspectorPreferredVisible
      : supplementaryPanePreferredVisible;
  const auxiliaryPanePreferredWidth =
    auxiliaryPaneRole === "inspector"
      ? fileInspectorPreferredWidth
      : supplementaryPanePreferredWidth;
  const panes = useMemo(
    () =>
      deriveWorkspacePaneLayout({
        layout,
        viewportWidth: width,
        nativeWorkspace,
        primarySidebarPreferredVisible: showPrimarySidebar,
        auxiliaryPanePreferredVisible,
        auxiliaryPaneRole,
        auxiliaryPanePreferredWidth: auxiliaryPanePreferredWidth ?? undefined,
      }),
    [
      auxiliaryPanePreferredVisible,
      auxiliaryPaneRole,
      auxiliaryPanePreferredWidth,
      layout,
      nativeWorkspace,
      showPrimarySidebar,
      width,
    ],
  );
  const activeThread = parseActiveThreadPath(pathname);
  const selectedThreadKey = (() => {
    if (activeThread === null) return null;
    try {
      return scopedThreadKey(
        EnvironmentId.make(activeThread.environmentId),
        ThreadId.make(activeThread.threadId),
      );
    } catch {
      return null;
    }
  })();
  const [workspaceInspector, setWorkspaceInspector] = useState<{
    readonly render: () => ReactNode;
    readonly active: boolean;
  } | null>(null);
  const workspaceInspectorOwner = useRef<symbol | null>(null);
  const registerWorkspaceInspector = (render: () => ReactNode) => {
    const owner = Symbol("workspace-inspector");
    workspaceInspectorOwner.current = owner;
    setWorkspaceInspector({ render, active: true });

    return () => {
      // Outgoing screens deactivate after incoming registration; only the current owner may deactivate.
      if (workspaceInspectorOwner.current !== owner) {
        return;
      }
      setWorkspaceInspector((current) => (current === null ? null : { ...current, active: false }));
    };
  };
  const handleWorkspaceInspectorClosed = () => {
    setWorkspaceInspector((current) => (current !== null && !current.active ? null : current));
  };
  const activateAuxiliaryPaneRole = (role: WorkspaceAuxiliaryPaneRole) => {
    const owner = Symbol(role);
    activeRoleOwner.current = owner;
    setFocusedAuxiliaryPaneRole(role);

    return () => {
      if (activeRoleOwner.current !== owner) {
        return;
      }
      activeRoleOwner.current = null;
      setFocusedAuxiliaryPaneRole(null);
    };
  };
  const setPrimarySidebarVisible = (visible: boolean) => {
    if (layout.primarySidebarOverlaysContent) {
      setOverlaySidebar((current) => ({ ...current, visible }));
    } else {
      setPrimarySidebarPreferredVisible(visible);
    }
  };
  const togglePrimarySidebar = () => {
    if (primarySidebarPinned) {
      return;
    }
    if (!panes.primarySidebarVisible && panes.primarySidebarSuppressedByAuxiliary) {
      setFileInspectorPreferredVisible(false);
      setPrimarySidebarPreferredVisible(true);
      return;
    }
    setPrimarySidebarVisible(!preferredSidebarVisible);
  };
  const revealPrimarySidebar = () => {
    if (panes.primarySidebarSuppressedByAuxiliary) {
      setFileInspectorPreferredVisible(false);
    }
    setPrimarySidebarVisible(true);
  };
  const handleToggleSidebarCommand = () => {
    togglePrimarySidebar();
    return true;
  };
  const sidebarCommands = primarySidebarPinned ? [] : (["toggleSidebar"] as const);
  useHardwareKeyboardCommand(sidebarCommands, handleToggleSidebarCommand);
  const showAuxiliaryPane = (role: WorkspaceAuxiliaryPaneRole) => {
    if (role === "inspector") {
      setFocusedAuxiliaryPaneRole("inspector");
      setFileInspectorPreferredVisible(true);
      return;
    }
    setFocusedAuxiliaryPaneRole("supplementary");
    setSupplementaryPanePreferredVisible(true);
  };
  const toggleAuxiliaryPane = () => {
    if (auxiliaryPaneRole === "inspector") {
      setFileInspectorPreferredVisible((current) => !current);
      return;
    }
    setSupplementaryPanePreferredVisible((current) => !current);
  };
  const setAuxiliaryPaneWidth = (nextWidth: number) => {
    if (auxiliaryPaneRole === "inspector") {
      setFileInspectorPreferredWidth(nextWidth);
      return;
    }
    setSupplementaryPanePreferredWidth(nextWidth);
  };
  /**
   * The one way to open a workspace tab: replaces the workspace with the tab's list and, when the
   * window has columns, the page it last showed, keeping any sheet on top. An overlay sidebar
   * stays open over that page.
   */
  const openWorkspaceTab = (
    tab: WorkspaceTab | "settings",
    options?: { readonly columns?: boolean },
  ) => {
    const state = rootStackState(navigation);
    if (!state) return;
    const { workspace, overlays } = workspaceRoutesOf(state.routes.slice(0, state.index + 1));
    const next = workspaceTabNavigationState(
      { ...state, routes: workspace },
      tab,
      currentWorkspaceSelections(),
      {
        columns: options?.columns ?? layout.usesSplitView,
        check: workspaceSelectionCheck,
        overlays,
      },
    );
    if (layout.primarySidebarOverlaysContent) {
      const opened = next.routes.at(-1 - overlays.length)?.key ?? null;
      setOverlaySidebar((current) => ({ ...current, visible: true, keepOpenFor: opened }));
    }
    dispatchRootStackReset(navigation, state, next);
  };
  const handleOpenSettings = () => openWorkspaceTab("settings");

  /** The one way to show a page in the detail column; see `selectWorkspaceDetail`. */
  function handleSelectDetail(detail: WorkspaceDetail) {
    threadSelectionDeferral.run(() => {
      setFileInspectorPreferredVisible(false);
      if (layout.primarySidebarOverlaysContent) setPrimarySidebarVisible(false);
      workspace?.dispatch({ type: "select", tab: "threads" });
      startOpeningDetail(detail);
      navigation.dispatch(selectWorkspaceDetail(detail));
    });
  }
  const handleSelectThreadRoute = (params: ReactNavigation.RootParamList["Thread"]) =>
    handleSelectDetail({ name: "Thread", params });

  const projects = useProjects();
  // Reads the open page when pressed, so the layout context doesn't change on every navigation.
  const handleStartNewTask = () => {
    const state = rootStackState(navigation);
    const currentRoute = workspaceRoutesOf(
      state?.routes.slice(0, state.index + 1) ?? [],
    ).workspace.at(-1);
    const params =
      currentRoute?.name === "Thread"
        ? (currentRoute.params as ReactNavigation.RootParamList["Thread"])
        : undefined;
    const first = (value: string | string[] | undefined) =>
      Array.isArray(value) ? value[0] : value;
    const environmentId = first(params?.environmentId);
    const threadId = first(params?.threadId);
    // The full index, not the sidebar list: subagent and archived threads are current threads too.
    const projectId =
      environmentId && threadId
        ? appAtomRegistry
            .get(
              environmentThreadShells.environmentThreadIndexAtom(EnvironmentId.make(environmentId)),
            )
            .get(ThreadId.make(threadId))?.projectId
        : undefined;
    const preferences = appAtomRegistry.get(mobilePreferencesAtom);
    const saved = AsyncResult.isSuccess(preferences) ? preferences.value : undefined;
    handleSelectThreadRoute(
      newTaskDestination({
        projects,
        requestId: uuidv4(),
        useLastProject: saved?.useLastProjectForNewThreads,
        currentProject: environmentId && projectId ? { environmentId, projectId } : undefined,
        lastProject: saved?.lastThreadProject,
      }),
    );
  };

  useHardwareKeyboardCommand("newTask", () => {
    handleStartNewTask();
    return true;
  });

  // Create at root: the iOS sidebar renders in an independent navigation tree.
  const handleOpenEnvironmentSettings = () => {
    openSettings(navigation, "SettingsEnvironments");
  };

  const { openPendingTask: handleOpenPendingTask } =
    usePendingTaskListActions(handleSelectThreadRoute);

  const handleNewThreadInProject = (project: EnvironmentProject) => {
    handleSelectThreadRoute({
      draft: {
        requestId: uuidv4(),
        environmentId: String(project.environmentId),
        projectId: String(project.id),
        title: project.title,
      },
    });
  };

  const inspectorColumnTargetWidth =
    workspaceInspector !== null && workspaceInspector.active && panes.auxiliaryPaneVisible
      ? (panes.auxiliaryPaneWidth ?? 0)
      : 0;
  const contentTargetWidth = layout.usesSplitView
    ? Math.max(0, panes.contentPaneWidth - inspectorColumnTargetWidth)
    : null;

  const renderedSidebarWidth = useSharedValue(
    panes.primarySidebarVisible ? (layout.listPaneWidth ?? 0) + (layout.listPaneGap ?? 0) : 0,
  );
  // oxlint-disable-next-line t3code/no-unreasoned-state-sync -- animates the Reanimated sidebar width shared value toward the layout's target
  useEffect(() => {
    if (nativeWorkspace) return;
    const targetWidth = panes.primarySidebarVisible
      ? (layout.listPaneWidth ?? 0) + (layout.listPaneGap ?? 0)
      : 0;
    renderedSidebarWidth.set(withTiming(targetWidth, WORKSPACE_PANE_TIMING));
  }, [
    nativeWorkspace,
    layout.listPaneWidth,
    layout.listPaneGap,
    panes.primarySidebarVisible,
    renderedSidebarWidth,
  ]);
  const sidebarAnimatedStyle = useAnimatedStyle(() => ({ width: renderedSidebarWidth.value }));

  // Clip the navigator at its settled width so native headers and markdown reflow only once per pane toggle.
  const renderedInspectorWidth = useSharedValue(inspectorColumnTargetWidth);
  const renderedContentWidth = useDerivedValue(() =>
    Math.max(0, viewportWidth - renderedSidebarWidth.value - renderedInspectorWidth.value),
  );

  const handleSelectThread = (thread: EnvironmentThreadShell) => {
    handleSelectThreadRoute({
      environmentId: String(thread.environmentId),
      threadId: String(thread.id),
    });
  };

  // Rotating flips the overlay; until the new size reaches JS, predict the reset above.
  const sidebarOverRotates =
    nativeWorkspace && nativeMetrics?.hasHinge === true && layout.usesSplitView;
  // The current orientation's panes already account for an inspector that needs the sidebar's room.
  const sidebarInLandscape = layout.primarySidebarOverlaysContent
    ? primarySidebarPinned || primarySidebarPreferredVisible
    : panes.primarySidebarVisible;
  const sidebarInPortrait =
    primarySidebarPinned ||
    (layout.primarySidebarOverlaysContent ? overlaySidebar.visible : isWorkspaceListPath(pathname));
  const sidebarByOrientation = useMemo(
    () =>
      sidebarOverRotates ? { landscape: sidebarInLandscape, portrait: sidebarInPortrait } : null,
    [sidebarInLandscape, sidebarInPortrait, sidebarOverRotates],
  );

  const contextValue = {
    layout,
    panes,
    fileInspector,
    primarySidebarSearchQuery,
    startNewTask: handleStartNewTask,
    selectDetail: handleSelectDetail,
    activateAuxiliaryPaneRole,
    registerWorkspaceInspector,
    setPrimarySidebarSearchQuery,
    showAuxiliaryPane,
    toggleAuxiliaryPane,
    togglePrimarySidebar,
    setPrimarySidebarVisible,
    openWorkspaceTab,
    setAuxiliaryPaneWidth,
    composerKeyboardActive,
    setComposerEditing,
    sidebarByOrientation,
  };

  if (nativeWorkspace) {
    return (
      <HomeListOptionsProvider
        projectGroupingMode={projectGroupingMode}
        savedFilters={props.savedThreadListFilters}
      >
        <AdaptiveWorkspaceContext value={contextValue}>
          <NativeWorkspaceInspectorContext
            value={{ render: workspaceInspector?.render, visible: inspectorColumnTargetWidth > 0 }}
          >
            {props.children}
          </NativeWorkspaceInspectorContext>
        </AdaptiveWorkspaceContext>
      </HomeListOptionsProvider>
    );
  }
  const renderColumns = (children: ReactNode) => (
    <View testID="adaptive-workspace-layout" className="flex-1 flex-row">
      {shouldRenderPrimarySidebar && layout.listPaneWidth !== null ? (
        <Animated.View
          className="self-stretch overflow-hidden"
          accessibilityElementsHidden={!panes.primarySidebarVisible}
          collapsable={false}
          pointerEvents={panes.primarySidebarVisible ? "auto" : "none"}
          style={sidebarAnimatedStyle}
        >
          <Animated.View className="flex-1" style={{ width: layout.listPaneWidth }}>
            <RenderErrorBoundary
              renderFallback={(fallback) => (
                <RenderFailureView
                  {...fallback}
                  title="The sidebar couldn't be displayed"
                  exit={{ label: "Open settings", onPress: handleOpenSettings }}
                />
              )}
            >
              <ThreadNavigationSidebar
                width={layout.listPaneWidth}
                visible={panes.primarySidebarVisible}
                onRequestVisibility={revealPrimarySidebar}
                selectedThreadKey={selectedThreadKey}
                onOpenSettings={handleOpenSettings}
                onOpenEnvironmentSettings={handleOpenEnvironmentSettings}
                onNewThreadInProject={handleNewThreadInProject}
                onOpenPendingTask={handleOpenPendingTask}
                onSelectThread={handleSelectThread}
                onSearchQueryChange={setPrimarySidebarSearchQuery}
                searchQuery={primarySidebarSearchQuery}
              />
            </RenderErrorBoundary>
          </Animated.View>
        </Animated.View>
      ) : null}
      <View className="flex-1 overflow-hidden bg-screen" collapsable={false}>
        <View
          collapsable={false}
          style={contentTargetWidth !== null ? { flex: 1, width: contentTargetWidth } : { flex: 1 }}
        >
          <WorkspaceContentWidthContext value={layout.usesSplitView ? renderedContentWidth : null}>
            <View className="flex-1">{children}</View>
          </WorkspaceContentWidthContext>
        </View>
      </View>
      <WorkspaceInspectorPane
        pathname={props.pathname}
        renderedInspectorWidth={renderedInspectorWidth}
        active={workspaceInspector?.active ?? false}
        panes={panes}
        renderInspector={workspaceInspector?.render}
        setAuxiliaryPaneWidth={setAuxiliaryPaneWidth}
        onClosed={handleWorkspaceInspectorClosed}
      />
    </View>
  );

  return (
    <HomeListOptionsProvider
      projectGroupingMode={projectGroupingMode}
      savedFilters={props.savedThreadListFilters}
    >
      <AdaptiveWorkspaceContext.Provider value={contextValue}>
        {usesPresentationHost ? (
          <WorkspaceColumnsRendererContext value={renderColumns}>
            {props.children}
          </WorkspaceColumnsRendererContext>
        ) : (
          renderColumns(props.children)
        )}
      </AdaptiveWorkspaceContext.Provider>
    </HomeListOptionsProvider>
  );
}
