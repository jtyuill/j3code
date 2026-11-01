import { NAVIGATION_TAB_ORDER } from "./navigation-tab-order";
import { requireNativeView } from "expo";
import type { NativeSyntheticEvent, ViewProps } from "react-native";
import { useControlTheme } from "../../components/ui/useControlTheme";
import { useWorkspaceTabs } from "./WorkspaceTabsContext";
import { useSelectWorkspaceTab } from "./useSelectWorkspaceTab";

const TABS = [...NAVIGATION_TAB_ORDER, "settings"] as const;

const NativeSidebarTabBar = requireNativeView<
  ViewProps & {
    tabs: readonly string[];
    selectedTab: string;
    selectedColor: string;
    appearance: string;
    showsCompose: boolean;
    compact?: boolean;
    onSelectionChange: (event: NativeSyntheticEvent<{ tab: string }>) => void;
    onCompose: () => void;
  }
>("T3NativeControls", "SidebarTabBar");

export function WorkspaceSidebarTabs(props: {
  readonly onNewTask?: () => void;
  readonly compact?: boolean;
  readonly reserveComposeSpace?: boolean;
}) {
  const workspace = useWorkspaceTabs()!;
  const selectTab = useSelectWorkspaceTab();
  const { colors, colorScheme } = useControlTheme();
  return (
    <NativeSidebarTabBar
      tabs={TABS}
      selectedTab={workspace.settingsOpen ? "settings" : workspace.state.selectedTab}
      selectedColor={colors["--color-primary"]}
      appearance={colorScheme}
      showsCompose={Boolean(props.onNewTask)}
      compact={props.compact}
      onCompose={() => props.onNewTask?.()}
      onSelectionChange={({ nativeEvent }) => {
        const tab = TABS.find((candidate) => candidate === nativeEvent.tab);
        if (tab) selectTab(tab);
      }}
      // Keep the host stable while UIKit updates the bars' intrinsic size and safe area together.
      style={{
        position: "absolute",
        bottom: 0,
        left: props.compact ? 8 : 0,
        // Keep the tab icons in place when Compose disappears on other destinations.
        right:
          (props.compact ? 12 : 0) +
          ((props.reserveComposeSpace ?? props.compact) && !props.onNewTask
            ? props.compact
              ? 64
              : 76
            : 0),
        height: 128,
      }}
    />
  );
}
