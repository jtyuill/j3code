import { installComposerAttachmentUploads } from "./state/composer-attachment-uploads";
import { installThreadOutboxDelivery } from "./state/threadOutboxDelivery";
import * as Linking from "expo-linking";
import * as SplashScreen from "expo-splash-screen";
import { useEffect } from "react";
import { StatusBar } from "react-native";
import { GestureHandlerRootView } from "react-native-gesture-handler";
import { KeyboardProvider } from "react-native-keyboard-controller";
import { SafeAreaProvider } from "react-native-safe-area-context";
import {
  createStaticNavigation,
  getActionFromState,
  getStateFromPath,
  type NavigationState,
} from "@react-navigation/native";
import { pipe } from "effect/Function";

import { RegistryContext } from "@effect/atom-react";
import { ProjectChipIconRasterizer } from "./native/ProjectChipIconImages";
import * as Dialog from "./components/ui/Dialog";
import {
  AppearancePreferencesProvider,
  installAppearanceRuntime,
  useAppearancePreferences,
} from "./features/settings/appearance/AppearancePreferencesProvider";
import { RootStack } from "./Stack";
import { appAtomRegistry } from "./state/atom-registry";
import { subscribeComposerDraftModelReconciliation } from "./state/use-composer-drafts";
import { environmentThreadShells } from "./state/threads";
import { environmentShell } from "./state/shell";
import { OverlayPortalHost } from "./components/ui/OverlayPortal";
import { shouldHandleAppLink, restoreDraftLinkState } from "./lib/appLinking";
import { hostFormLinkAction, startDevAutoConnect } from "./features/connection/devAutoConnect";
import {
  disposeOnFoundationReplace,
  type FoundationHotModule,
} from "./lib/foundation-fast-refresh";
import { useMobileNavigationTheme } from "./lib/useMobileNavigationTheme";
import { installUiRuntimeMemoryWarningGc } from "./lib/useUiRuntimeMemoryWarningGc";
import { restoreSettingsLinkState } from "./features/settings/settings-navigation";
import {
  hydrateWorkspaceSelections,
  launchWorkspaceURL,
  recordWorkspaceSelections,
  settingsLinkPage,
} from "./features/workspace/workspace-selection-store";
import { workspaceDetailLinkAction } from "./features/workspace/workspace-detail-selection";
import { NativeLayoutMetricsProvider } from "./native/native-layout-metrics";
import {
  attachRootNavigation,
  flushRootNavigationQueue,
  rootNavigationRef,
} from "./lib/rootNavigation";
import { pruneNewTaskFlowStates } from "./features/threads/new-task-flow-state";

import "../global.css";

void SplashScreen.preventAutoHideAsync().catch(() => {});
void hydrateWorkspaceSelections();

function getAppStateFromPath(path: string, options: Parameters<typeof getStateFromPath>[1]) {
  return pipe(
    getStateFromPath(path, options),
    (state) => restoreDraftLinkState(state, path),
    (state) => restoreSettingsLinkState(state, settingsLinkPage()),
    startDevAutoConnect,
  );
}

const appLinking = {
  prefixes: [Linking.createURL("/"), "j3code://"],
  config: { initialRouteName: "Home" },
  filter: shouldHandleAppLink,
  getInitialURL: async () => (await Linking.getInitialURL()) ?? (await launchWorkspaceURL()),
  getActionFromState: (
    state: Parameters<typeof getActionFromState>[0],
    options: Parameters<typeof getActionFromState>[1],
  ): ReturnType<typeof getActionFromState> =>
    // Our router handles the selection; React Navigation's type only lists its own actions.
    (workspaceDetailLinkAction(state) as ReturnType<typeof getActionFromState>) ??
    hostFormLinkAction(
      state,
      rootNavigationRef.isReady() ? rootNavigationRef.getRootState() : undefined,
    ) ??
    getActionFromState(state, options),
  getStateFromPath: getAppStateFromPath,
};

const Navigation = createStaticNavigation(RootStack);

function handleNavigationStateChange(state: NavigationState | undefined): void {
  recordWorkspaceSelections(state);
  if (!state) return;
  pruneNewTaskFlowStates(state);
}

function handleNavigationReady(): void {
  handleNavigationStateChange(rootNavigationRef.getRootState());
  flushRootNavigationQueue();
}

declare const module: { readonly hot?: FoundationHotModule } | undefined;

const appServiceDisposers = [
  installUiRuntimeMemoryWarningGc(),
  subscribeComposerDraftModelReconciliation({
    registry: appAtomRegistry,
    threadShellAtom: environmentThreadShells.threadShellAtom,
    threadRefsAtom: environmentThreadShells.threadRefsAtom,
    shellStateAtom: environmentShell.stateAtom,
  }),
  installAppearanceRuntime(),
  installThreadOutboxDelivery(),
  installComposerAttachmentUploads(),
];
disposeOnFoundationReplace(typeof module === "undefined" ? undefined : module.hot, () => {
  for (const dispose of appServiceDisposers) dispose();
});

function SplashScreenCoordinator() {
  const { isReady } = useAppearancePreferences();

  // oxlint-disable-next-line t3code/no-unreasoned-state-sync -- hides the native splash screen once appearance preferences have loaded
  useEffect(() => {
    if (isReady) void SplashScreen.hide();
  }, [isReady]);

  return null;
}

export default function App() {
  return (
    <RegistryContext.Provider value={appAtomRegistry}>
      <AppearancePreferencesProvider>
        <AppContent />
      </AppearancePreferencesProvider>
    </RegistryContext.Provider>
  );
}

function AppContent() {
  const { themeAppearance } = useAppearancePreferences();
  const navigationTheme = useMobileNavigationTheme();

  return (
    <>
      <SplashScreenCoordinator />
      <GestureHandlerRootView className="flex-1">
        <KeyboardProvider statusBarTranslucent>
          <SafeAreaProvider>
            <StatusBar barStyle={themeAppearance === "dark" ? "light-content" : "dark-content"} />
            {/* native-stack needs the theme's dark flag to select dark native headers. */}
            <NativeLayoutMetricsProvider>
              <Navigation
                ref={attachRootNavigation}
                linking={appLinking}
                onReady={handleNavigationReady}
                theme={navigationTheme}
                onStateChange={handleNavigationStateChange}
              />
              <Dialog.Host />
              <ProjectChipIconRasterizer />
            </NativeLayoutMetricsProvider>
            <OverlayPortalHost />
          </SafeAreaProvider>
        </KeyboardProvider>
      </GestureHandlerRootView>
    </>
  );
}
