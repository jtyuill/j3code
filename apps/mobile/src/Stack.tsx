import { isDraftThreadRoute } from "./features/threads/thread-route-state";
import { createWorkspaceStackNavigator } from "./features/layout/createWorkspaceStackNavigator";
import {
  createPathConfigForStaticNavigation,
  getPathFromState,
  NavigationState,
  StackActions,
  useNavigation,
} from "@react-navigation/native";
import {
  createNativeStackScreen,
  type NativeStackNavigationOptions,
} from "@react-navigation/native-stack";
import type { ReactNode } from "react";
import { Platform, Pressable, ScrollView, StyleSheet, useWindowDimensions } from "react-native";
import { useResolveClassNames } from "uniwind";

import { AppText as Text } from "./components/ui/AppText";
import { getCompactBrandHeaderOptions } from "./components/CompactBrandTitle";
import { WorkspaceConnectionStatusProvider } from "./features/home/WorkspaceConnectionTitle";
import { WorkspaceRootScreen } from "./features/workspace/WorkspaceRootScreen";
import {
  recordHomeFocus,
  recordHomeTransitionEnd,
  recordHomeTransitionStart,
} from "./features/home/home-route-visibility";
import {
  RenderErrorBoundary,
  RenderFailureView,
  type RenderFailureProps,
} from "./components/RenderErrorBoundary";
import { ArchivedThreadsRouteScreen } from "./features/archive/ArchivedThreadsRouteScreen";
import { AdaptiveWorkspaceLayout } from "./features/layout/AdaptiveWorkspaceLayout";
import {
  HardwareKeyboardCommandOverlay,
  HardwareKeyboardCommandProvider,
} from "./features/keyboard/HardwareKeyboardCommandProvider";
import { ThreadAgentsSheet } from "./features/threads/ThreadAgentsSheet";
import { ThreadBackgroundWorkSheet } from "./features/threads/ThreadBackgroundWorkSheet";
import { ThreadQueueSheet } from "./features/threads/ThreadQueueControl";
import { ThreadRouteScreen } from "./features/threads/ThreadRouteScreen";
import { ConnectionsRouteScreen } from "./features/connection/ConnectionsRouteScreen";
import { ConnectionsNewRouteScreen } from "./features/connection/ConnectionsNewRouteScreen";
import { AddProjectRoute } from "./features/projects/AddProjectRoute";
import { NewTaskEnvironmentPickerRouteScreen } from "./features/threads/NewTaskContextPickerScreens";
import {
  ExistingThreadSettingsRouteScreen,
  NewTaskThreadSettingsRouteScreen,
} from "./features/threads/ThreadSettingsSheet";
import { NewTaskFlowProvider } from "./features/threads/new-task-flow-provider";
import { newTaskFlowKey } from "./features/threads/new-task-flow-state";
import { NewTaskRouteScreen } from "./features/threads/NewTaskRouteScreen";
import { SettingsAppearanceRouteScreen } from "./features/settings/SettingsAppearanceRouteScreen";
import { SettingsEnvironmentDetailRouteScreen } from "./features/settings/SettingsEnvironmentDetailRouteScreen";
import { SettingsEnvironmentsRouteScreen } from "./features/settings/SettingsEnvironmentsRouteScreen";
import { SettingsFollowUpRouteScreen } from "./features/settings/SettingsFollowUpRouteScreen";
import { SettingsKeyboardRouteScreen } from "./features/settings/SettingsKeyboardRouteScreen";
import { SettingsProjectGroupingRouteScreen } from "./features/settings/SettingsProjectGroupingRouteScreen";
import { SettingsProjectOverviewRouteScreen } from "./features/settings/SettingsProjectOverviewRouteScreen";
import { SettingsAboutRouteScreen } from "./features/settings/SettingsAboutRouteScreen";
import { SettingsRouteScreen } from "./features/settings/SettingsRouteScreen";
import { SettingsLabsRouteScreen } from "./features/settings/ThreadSettingsSections";
import { SettingsEnvironmentFilterProvider } from "./features/settings/settings-environment-filter";
import { openSettings, settingsOpenIn } from "./features/settings/settings-navigation";
import { DEFAULT_SETTINGS_PAGE } from "./features/workspace/workspace-tab-navigation";
import {
  isWorkspaceOverlayRoute,
  registerPresentedRoutes,
} from "./features/layout/workspace-overlay-routes";
import { NATIVE_LIQUID_GLASS_SUPPORTED } from "./native/native-glass";
import { deriveLayout } from "./lib/layout";
import { useNativeLayoutMetrics } from "./native/native-layout-metrics";
import { nativeHeaderScrollEdgeEffects } from "./native/StackHeader";
import { HORIZONTAL_FORM_SHEET_OPTIONS, formSheetDetents } from "./native/sheet-surface";
import { workspaceDetailRouter } from "./features/workspace/workspace-detail-selection";

const HEADER_SCROLL_EDGE_EFFECTS = nativeHeaderScrollEdgeEffects(Platform.OS, Platform.Version);

type AppScreenOptions = NativeStackNavigationOptions & {
  readonly unstable_navigationItemStyle?: "editor";
  readonly unstable_headerMinimizationBehavior?: "onScrollUp" | "never";
};

const GLASS_HEADER_OPTIONS: AppScreenOptions = {
  headerBackButtonDisplayMode: "minimal",
  headerBackTitle: "",
  headerLargeTitle: false,
  headerShadowVisible: false,
  headerShown: true,
  headerStyle: NATIVE_LIQUID_GLASS_SUPPORTED ? { backgroundColor: "transparent" } : undefined,
  headerTitleStyle: { fontSize: 18, fontWeight: "800" },
  headerTransparent: NATIVE_LIQUID_GLASS_SUPPORTED,
  scrollEdgeEffects: NATIVE_LIQUID_GLASS_SUPPORTED ? HEADER_SCROLL_EDGE_EFFECTS : undefined,
  unstable_navigationItemStyle: NATIVE_LIQUID_GLASS_SUPPORTED ? "editor" : undefined,
};

// Hiding Android's native header after mount can leave detail panes measured short.
// These screens render their own Material toolbar, so hide it from the first layout.
const DETAIL_HEADER_OPTIONS: AppScreenOptions = {
  ...GLASS_HEADER_OPTIONS,
  headerShown: Platform.OS !== "android",
};

// Like Messages on the Duo: reading back through history (scrolling up) minimizes the
// header, and reversing toward the latest message restores it. Other devices ignore it.
const THREAD_HEADER_OPTIONS: AppScreenOptions = {
  ...DETAIL_HEADER_OPTIONS,
  unstable_headerMinimizationBehavior: "onScrollUp",
};

// The Duo minimizes headers on scroll down by default. Home and the pull request lists
// stutter against it on hardware (not reproducible in the simulator), so they keep theirs.
const FIXED_DUO_HEADER: AppScreenOptions = { unstable_headerMinimizationBehavior: "never" };

const SHEET_GLASS_HEADER_OPTIONS: AppScreenOptions = {
  ...GLASS_HEADER_OPTIONS,
  unstable_navigationItemStyle: undefined,
};

// Thread settings keep the Duo's vertical bar so cancel, search, filter and confirm sit in the
// rail. The picker stack spans the sheet so its header reaches the rail, and insets its content.
const THREAD_SETTINGS_SHEET_OPTIONS = {
  presentation: "formSheet" as const,
  sheetAllowedDetents: [1],
  sheetGrabberVisible: true,
  nativeContentInsetHorizontally: false,
};

// Settings pages are root routes so they share the workspace's columns and header. The
// `Settings` route marks the Settings tab as open (see settings-navigation.ts).
const SETTINGS_PAGE_OPTIONS = SHEET_GLASS_HEADER_OPTIONS;

const SETTINGS_SCREENS = {
  Settings: createNativeStackScreen({
    screen: SettingsRouteScreen,
    linking: "settings",
    options: {
      ...SETTINGS_PAGE_OPTIONS,
      title: "Settings",
      headerBackVisible: false,
    },
  }),
  SettingsEnvironments: createNativeStackScreen({
    screen: SettingsEnvironmentsRouteScreen,
    linking: "settings/environments",
    options: {
      ...SETTINGS_PAGE_OPTIONS,
      title: "Environments",
    },
  }),
  SettingsEnvironmentDetail: createNativeStackScreen({
    screen: SettingsEnvironmentDetailRouteScreen,
    linking: "settings/environments/:environmentId",
    options: { ...SETTINGS_PAGE_OPTIONS, title: "Environment" },
  }),
  SettingsLabs: createNativeStackScreen({
    screen: SettingsLabsRouteScreen,
    linking: "settings/labs",
    options: { ...SETTINGS_PAGE_OPTIONS, title: "Labs" },
  }),
  SettingsAbout: createNativeStackScreen({
    screen: SettingsAboutRouteScreen,
    linking: "settings/about",
    options: { ...SETTINGS_PAGE_OPTIONS, title: "About" },
  }),
  SettingsEnvironmentNew: createNativeStackScreen({
    screen: ConnectionsNewRouteScreen,
    linking: "settings/environment-new",
    options: {
      ...SETTINGS_PAGE_OPTIONS,
      title: "Add Environment",
    },
  }),
  SettingsArchive: createNativeStackScreen({
    screen: ArchivedThreadsRouteScreen,
    linking: "settings/archive",
    options: {
      ...SETTINGS_PAGE_OPTIONS,
      title: "Archive",
    },
  }),
  SettingsAppearance: createNativeStackScreen({
    screen: SettingsAppearanceRouteScreen,
    linking: "settings/appearance",
    options: {
      ...SETTINGS_PAGE_OPTIONS,
      title: "Appearance",
    },
  }),
  SettingsProjectGrouping: createNativeStackScreen({
    screen: SettingsProjectGroupingRouteScreen,
    linking: "settings/project-grouping",
    options: {
      ...SETTINGS_PAGE_OPTIONS,
      title: "General",
    },
  }),
  SettingsOrganization: createNativeStackScreen({
    screen: SettingsProjectGroupingRouteScreen,
    linking: "settings/organization",
    options: {
      ...SETTINGS_PAGE_OPTIONS,
      title: "General",
    },
  }),
  SettingsProjectOverview: createNativeStackScreen({
    screen: SettingsProjectOverviewRouteScreen,
    linking: "settings/project",
    options: { ...SETTINGS_PAGE_OPTIONS, title: "Projects" },
  }),
  SettingsKeyboard: createNativeStackScreen({
    screen: SettingsKeyboardRouteScreen,
    linking: "settings/keyboard",
    options: {
      ...SETTINGS_PAGE_OPTIONS,
      title: "Keyboard",
    },
  }),
  SettingsFollowUp: createNativeStackScreen({
    screen: SettingsFollowUpRouteScreen,
    linking: "settings/follow-ups",
    options: {
      ...SETTINGS_PAGE_OPTIONS,
      title: "Chat",
    },
  }),
};

export function workspaceLocationFromState(state: NavigationState) {
  const routes = state.routes.filter((route) => !isWorkspaceOverlayRoute(route.name));
  const effectiveState =
    routes.length > 0 && routes.length !== state.routes.length
      ? ({ ...state, routes, index: routes.length - 1 } as NavigationState)
      : state;
  const path = getPathFromState(effectiveState, navigationPathConfig);
  const route = effectiveState.routes[effectiveState.index];
  return {
    pathname: path.startsWith("/") ? path : `/${path}`,
    routeKey: route?.key,
    routeName: route?.name,
    routeKeys: effectiveState.routes.map((stackRoute) => stackRoute.key),
  };
}

export function rootPathnameFromState(state: NavigationState): string {
  const path = getPathFromState(state, navigationPathConfig);
  return path.startsWith("/") ? path : `/${path}`;
}

function RootStackLayout(props: {
  readonly children: React.ReactNode;
  readonly state: NavigationState;
}) {
  const pathname = rootPathnameFromState(props.state);
  const workspaceLocation = workspaceLocationFromState(props.state);
  const settingsOpen = settingsOpenIn(
    props.state.routes
      .slice(0, props.state.index + 1)
      .filter((route) => !isWorkspaceOverlayRoute(route.name)),
  );
  // The flow follows the draft route even while a pushed Thread covers it, so
  // its state lives exactly as long as the route.
  const draftRoute = props.state.routes.findLast(isDraftThreadRoute);
  const draftRouteOnTop =
    draftRoute !== undefined &&
    props.state.routes.findLast((route) => route.name === "Thread") === draftRoute;
  const draftParams = (
    draftRoute?.params as { draft?: { draftId?: string | string[] } } | undefined
  )?.draft;
  const routeDraftId = Array.isArray(draftParams?.draftId)
    ? draftParams.draftId[0]
    : draftParams?.draftId;

  return (
    <NewTaskFlowProvider
      active={draftRouteOnTop}
      flowKey={draftRoute === undefined ? undefined : newTaskFlowKey(draftRoute)}
      routeDraftId={routeDraftId}
    >
      <HardwareKeyboardCommandProvider pathname={pathname}>
        <SettingsEnvironmentFilterProvider open={settingsOpen}>
          <WorkspaceConnectionStatusProvider>
              <AdaptiveWorkspaceLayout
                pathname={workspaceLocation.pathname}
                workspaceRouteKey={workspaceLocation.routeKey}
                workspaceRouteName={workspaceLocation.routeName}
                workspaceRouteKeys={workspaceLocation.routeKeys}
              >
                {props.children}
                <HardwareKeyboardCommandOverlay />
              </AdaptiveWorkspaceLayout>
          </WorkspaceConnectionStatusProvider>
        </SettingsEnvironmentFilterProvider>
      </HardwareKeyboardCommandProvider>
    </NewTaskFlowProvider>
  );
}

function NotFoundScreen() {
  const navigation = useNavigation();
  const screenBgStyle = StyleSheet.flatten(useResolveClassNames("bg-screen"));
  const primaryBgStyle = StyleSheet.flatten(useResolveClassNames("bg-primary"));
  const returnHomeButtonStyle = StyleSheet.flatten([
    {
      borderRadius: 999,
      paddingHorizontal: 20,
      paddingVertical: 14,
    },
    primaryBgStyle,
  ]);

  return (
    <ScrollView
      contentInsetAdjustmentBehavior="automatic"
      contentContainerStyle={{
        flexGrow: 1,
        alignItems: "center",
        justifyContent: "center",
        gap: 16,
        paddingHorizontal: 24,
        paddingVertical: 32,
      }}
      style={[{ flex: 1 }, screenBgStyle]}
    >
      <Text className="text-3xl font-t3-bold text-foreground" selectable>
        Route not found
      </Text>
      <Pressable
        style={returnHomeButtonStyle}
        onPress={() => navigation.dispatch(StackActions.replace("Home"))}
      >
        <Text className="text-base font-t3-bold text-primary-foreground">Return home</Text>
      </Pressable>
    </ScrollView>
  );
}

const RootStackConfig = createWorkspaceStackNavigator({
  initialRouteName: "Home",
  UNSTABLE_router: workspaceDetailRouter,
  // React Navigation calls layouts inline; compiled components would share the navigator's memo cache.
  layout: (props) => <RootStackLayout {...props} />,
  screenOptions: {
    headerShown: false,
  },
  screens: {
    Home: createNativeStackScreen({
      screen: WorkspaceRootScreen,
      linking: "",
      listeners: {
        transitionStart: ({ data }) => recordHomeTransitionStart(data.closing),
        transitionEnd: ({ data }) => recordHomeTransitionEnd(data.closing),
        focus: recordHomeFocus,
      },
      options: {
        ...GLASS_HEADER_OPTIONS,
        ...FIXED_DUO_HEADER,
        contentStyle: { backgroundColor: "transparent" },
        headerBackVisible: false,
        ...getCompactBrandHeaderOptions(),
      },
    }),
    Thread: createNativeStackScreen({
      screen: ThreadRouteScreen,
      linking: { path: "threads/:environmentId?/:threadId?", alias: ["new/draft"] },
      options: THREAD_HEADER_OPTIONS,
    }),
    NewTaskProject: createNativeStackScreen({
      screen: NewTaskRouteScreen,
      linking: { path: "new", alias: ["new/project"] },
      options: { ...GLASS_HEADER_OPTIONS, title: "Choose project" },
    }),
    NewTaskEnvironment: createNativeStackScreen({
      linking: "new/draft/environment",
      screen: NewTaskEnvironmentPickerRouteScreen,
      options: { ...GLASS_HEADER_OPTIONS, title: "Environment" },
    }),
    AddProject: createNativeStackScreen({
      linking: "new/add-project",
      screen: AddProjectRoute,
      options: {
        ...GLASS_HEADER_OPTIONS,
        title: "Add project",
      },
    }),
    NewTaskThreadSettings: createNativeStackScreen({
      screen: NewTaskThreadSettingsRouteScreen,
      linking: "new/draft/settings",
      options: {
        gestureEnabled: true,
        headerShown: false,
        ...(Platform.OS === "android"
          ? { presentation: "card" as const }
          : THREAD_SETTINGS_SHEET_OPTIONS),
      },
    }),
    ThreadSettingsSheet: createNativeStackScreen({
      screen: ExistingThreadSettingsRouteScreen,
      options: {
        gestureEnabled: true,
        headerShown: false,
        ...(Platform.OS === "android"
          ? { presentation: "card" as const }
          : {
              ...THREAD_SETTINGS_SHEET_OPTIONS,
            }),
      },
    }),
    ThreadQueue: createNativeStackScreen({
      screen: ThreadQueueSheet,
      options: {
        ...HORIZONTAL_FORM_SHEET_OPTIONS,
        headerShown: false,
        sheetAllowedDetents: formSheetDetents(0.65, 0.95),
        sheetGrabberVisible: true,
      },
    }),
    ThreadBackgroundWork: createNativeStackScreen({
      screen: ThreadBackgroundWorkSheet,
      options: {
        ...HORIZONTAL_FORM_SHEET_OPTIONS,
        headerShown: false,
        sheetAllowedDetents: formSheetDetents(0.5, 0.9),
        sheetGrabberVisible: true,
      },
    }),
    ThreadAgents: createNativeStackScreen({
      screen: ThreadAgentsSheet,
      options: {
        ...HORIZONTAL_FORM_SHEET_OPTIONS,
        headerShown: false,
        sheetAllowedDetents: formSheetDetents(0.5, 0.9),
        sheetGrabberVisible: true,
      },
    }),
    ...SETTINGS_SCREENS,
    Connections: createNativeStackScreen({
      screen: ConnectionsRouteScreen,
      linking: "connections",
      options: {
        title: "Environments",
        ...(Platform.OS === "android"
          ? { presentation: "card" as const, headerShown: false }
          : {
              ...HORIZONTAL_FORM_SHEET_OPTIONS,
              sheetAllowedDetents: [0.55, 0.7],
              sheetGrabberVisible: true,
            }),
      },
    }),
    ConnectionsNew: createNativeStackScreen({
      screen: ConnectionsNewRouteScreen,
      linking: "connections/new",
      options: {
        // Mount the iOS header immediately to avoid remounting the pairing form.
        ...(Platform.OS === "ios" ? SHEET_GLASS_HEADER_OPTIONS : undefined),
        title: "Add Environment",
        ...HORIZONTAL_FORM_SHEET_OPTIONS,
        sheetAllowedDetents: [0.55, 0.7],
        sheetGrabberVisible: true,
        sheetCornerRadius: Platform.OS === "android" ? 28 : undefined,
      },
    }),
    NotFound: createNativeStackScreen({
      screen: NotFoundScreen,
      linking: "*",
    }),
  },
});

function GuardedScreenLayout(props: {
  readonly children: ReactNode;
  readonly route: { readonly name: string; readonly params?: object | undefined };
}) {
  return (
    <RenderErrorBoundary
      resetKeys={[props.route.params]}
      renderFallback={(fallback) => (
        <ScreenRenderFallback {...fallback} routeName={props.route.name} />
      )}
    >
      {props.children}
    </RenderErrorBoundary>
  );
}

function ScreenRenderFallback(props: RenderFailureProps & { readonly routeName: string }) {
  const navigation = useNavigation();
  const exit = navigation.canGoBack()
    ? { label: "Go back", onPress: () => navigation.goBack() }
    : props.routeName === "Home"
      ? // Home itself failed, and the Settings list renders inside it; open a Settings page.
        {
          label: "Open settings",
          onPress: () => openSettings(navigation, DEFAULT_SETTINGS_PAGE),
        }
      : { label: "Return home", onPress: () => navigation.dispatch(StackActions.replace("Home")) };

  return <RenderFailureView {...props} exit={exit} />;
}

registerPresentedRoutes(
  Object.entries(RootStackConfig.config.screens ?? {}).flatMap(([name, screen]) => {
    const options = (screen as { readonly options?: unknown }).options;
    const presentation =
      options && typeof options === "object"
        ? (options as { readonly presentation?: string }).presentation
        : undefined;
    return presentation !== undefined && presentation !== "card" ? [name] : [];
  }),
);

export const RootStack = RootStackConfig.with(function AdaptiveRootStack({ Navigator }) {
  // oxlint-disable-next-line react/no-unstable-nested-components -- React Navigation invokes screenLayout as a render callback.
  return <Navigator screenLayout={(props) => <GuardedScreenLayout {...props} />} />;
});

type RootStackType = typeof RootStack;

const navigationPathConfig = {
  screens: createPathConfigForStaticNavigation(RootStack) ?? {},
};

declare module "@react-navigation/native" {
  interface RootNavigator extends RootStackType {}
}
