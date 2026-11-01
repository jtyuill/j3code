import { WORKSPACE_TAB_LABELS } from "./navigation-tabs";
import { SettingsRouteScreen } from "../settings/SettingsRouteScreen";
import { useNavigation } from "@react-navigation/native";
import { use } from "react";
import { View, useWindowDimensions } from "react-native";
import { useKeyboardState } from "react-native-keyboard-controller";
import { SafeAreaInsetsContext, useSafeAreaInsets } from "react-native-safe-area-context";

import { CompactBrandTitle } from "../../components/CompactBrandTitle";
import { HeaderItemsWidthContext } from "../../native/header-items-width";
import { NativeStackScreenOptions } from "../../native/StackHeader";
import { NativePrimaryColumnContext } from "../../native/v5-workspace-context";
import { isDuoDevice } from "../../lib/device-type";
import { NATIVE_LIQUID_GLASS_SUPPORTED } from "../../native/native-glass";
import { useWorkspaceSurface } from "./useWorkspaceSurface";
import {
  useDeviceType,
  useNativeColumnLayoutMetrics,
  useNativeLayoutMetrics,
} from "../../native/native-layout-metrics";
import { HomeRouteScreen } from "../home/HomeRouteScreen";
import { WorkspaceConnectionTitle } from "../home/WorkspaceConnectionTitle";
import { useAdaptiveWorkspaceLayout } from "../layout/AdaptiveWorkspaceLayout";
import {
  WorkspaceTabsContext,
  WorkspaceTabsProvider,
  useWorkspaceTabs,
} from "./WorkspaceTabsContext";
import { WorkspaceToolbar } from "./WorkspaceToolbar";
import { WORKSPACE_DOCK, workspaceDockBottomClearance } from "./workspace-tabs-state";
import { WorkspaceSidebarTabs } from "./WorkspaceSidebarTabs";
import {
  useCompactWorkspaceTabs,
  useSidebarWorkspaceTabs,
  useSystemWorkspaceTabBar,
} from "./useSystemWorkspaceTabBar";
import { openSettings } from "../settings/settings-navigation";

function WorkspaceRootTitle(props: {
  readonly columnWidth: number;
  readonly onOpenEnvironments: () => void;
}) {
  const { state, settingsOpen } = useWorkspaceTabs()!;
  const maxWidth = Math.max(0, props.columnWidth - (use(HeaderItemsWidthContext) ?? 160));
  const tabTitle = settingsOpen ? "Settings" : WORKSPACE_TAB_LABELS[state.selectedTab];
  return (
    <View style={{ width: maxWidth, minHeight: 32, justifyContent: "center" }}>
      <WorkspaceConnectionTitle
        brand={<CompactBrandTitle accessibilityLabel={`j3, ${tabTitle}`} fit />}
        maxWidth={maxWidth}
        onPress={props.onOpenEnvironments}
      />
    </View>
  );
}

function WorkspaceRootContent() {
  const navigation = useNavigation();
  const { width: windowWidth } = useWindowDimensions();
  const { layout, startNewTask } = useAdaptiveWorkspaceLayout();
  const primaryColumn = use(NativePrimaryColumnContext);
  const columnMetrics = useNativeColumnLayoutMetrics();
  const nativeMetrics = useNativeLayoutMetrics();
  const deviceType = useDeviceType();
  const surface = useWorkspaceSurface();
  const systemTabBar = useSystemWorkspaceTabBar();
  const compactTabs = useCompactWorkspaceTabs();
  const sidebarTabs = useSidebarWorkspaceTabs();
  const sidebarPicker = sidebarTabs === "picker" || (deviceType === "pad" && layout.usesSplitView);
  const sidebarNavigation =
    primaryColumn !== null &&
    (layout.usesSplitView ||
      deviceType === "pad" ||
      (nativeMetrics !== null && nativeMetrics.verticalBarEdge !== "none"));
  const width = columnMetrics?.width ?? windowWidth;
  const insets = useSafeAreaInsets();
  const keyboardHeight = useKeyboardState((keyboard) => keyboard.height);
  const workspace = useWorkspaceTabs()!;
  const { state } = workspace;
  // Settings is a workspace tab too; its list takes the place of the selected tab's.
  const settingsOpen = workspace.settingsOpen;
  const renderHeaderTitle = () => (
    <WorkspaceRootTitle
      columnWidth={width}
      onOpenEnvironments={() => openSettings(navigation, "SettingsEnvironments")}
    />
  );
  const tab = state.tabs[settingsOpen ? "settings" : state.selectedTab];
  const bottomClearance = isDuoDevice(deviceType)
    ? // The column's safe area already holds the Duo's search toolbar and keyboard.
      Math.max(insets.bottom, keyboardHeight) + 16
    : compactTabs || deviceType === "pad"
      ? workspaceDockBottomClearance({ bottomInset: insets.bottom, keyboardHeight })
      : Math.max(insets.bottom, keyboardHeight) +
        (systemTabBar || sidebarTabs || tab.searchMode !== "resting"
          ? sidebarPicker
            ? 80
            : 16
          : WORKSPACE_DOCK.height + WORKSPACE_DOCK.bottomGap + 16);
  const context = { ...workspace, bottomClearance };

  return (
    <WorkspaceTabsContext value={context}>
      <View className={`flex-1 ${sidebarPicker ? "bg-drawer" : surface.className}`}>
        <NativeStackScreenOptions
          optionsVersion={[
            settingsOpen,
            state.selectedTab,
            state.selectionRevision,
            width,
            sidebarNavigation,
          ]}
          options={{
            title: settingsOpen ? "Settings" : WORKSPACE_TAB_LABELS[state.selectedTab],
            headerShown: true,
            headerTransparent: NATIVE_LIQUID_GLASS_SUPPORTED,
            headerStyle: NATIVE_LIQUID_GLASS_SUPPORTED ? { backgroundColor: "transparent" } : undefined,
            contentStyle: undefined,
            unstable_headerLeftItems: () => [],
            // Beside a detail column the tab bar names the tab and the detail header the page,
            // so the sidebar keeps the brand.
            headerTitle: sidebarNavigation || !settingsOpen ? renderHeaderTitle : undefined,
          }}
        />
        <SafeAreaInsetsContext value={compactTabs ? { ...insets, bottom: 0 } : insets}>
          {settingsOpen ? <SettingsRouteScreen /> : <HomeRouteScreen />}
        </SafeAreaInsetsContext>
        {deviceType !== "pad" && deviceType !== "phone" && sidebarPicker ? (
          <WorkspaceSidebarTabs />
        ) : null}
        <WorkspaceToolbar sidebar={sidebarNavigation} onNewTask={startNewTask} />
      </View>
    </WorkspaceTabsContext>
  );
}

export function WorkspaceRootScreen() {
  const workspace = useWorkspaceTabs();
  return workspace ? (
    <WorkspaceRootContent />
  ) : (
    <WorkspaceTabsProvider>
      <WorkspaceRootContent />
    </WorkspaceTabsProvider>
  );
}
