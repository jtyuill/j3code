import { NAVIGATION_TAB_ORDER } from "./navigation-tab-order";
import { Toolbar } from "../../components/ui/Toolbar";
import { useControlTheme } from "../../components/ui/useControlTheme";
import { useDeviceType } from "../../native/native-layout-metrics";
import { NAVIGATION_TAB_ICONS, WORKSPACE_TAB_LABELS } from "./navigation-tabs";
import { useWorkspaceTabs } from "./WorkspaceTabsContext";
import { useSelectWorkspaceTab } from "./useSelectWorkspaceTab";
import { useCompactWorkspaceTabs, useSidebarWorkspaceTabs } from "./useSystemWorkspaceTabBar";

export function WorkspaceTabToolbarButtons() {
  const workspace = useWorkspaceTabs()!;
  const selectTab = useSelectWorkspaceTab();
  const selected = workspace.settingsOpen ? "settings" : workspace.state.selectedTab;
  const selectedTint = useControlTheme().colors["--color-primary"];
  return (
    <>
      {NAVIGATION_TAB_ORDER.map((tab) => (
        <Toolbar.Button
          key={tab}
          identifier={`workspace-tab-${tab}`}
          accessibilityLabel={WORKSPACE_TAB_LABELS[tab]}
          icon={NAVIGATION_TAB_ICONS[tab]}
          tintColor={selected === tab ? selectedTint : undefined}
          onPress={() => selectTab(tab)}
        />
      ))}
      <Toolbar.Button
        identifier="workspace-tab-settings"
        accessibilityLabel="Settings"
        icon="gearshape"
        tintColor={selected === "settings" ? selectedTint : undefined}
        onPress={() => selectTab("settings")}
      />
    </>
  );
}

export function WorkspaceSectionToolbar({
  onNewTask,
  showCompose = true,
}: {
  readonly onNewTask: () => void;
  readonly showCompose?: boolean;
}) {
  const sidebarTabs = useSidebarWorkspaceTabs();
  const compactTabs = useCompactWorkspaceTabs();
  const deviceType = useDeviceType();
  return (
    <>
      {showCompose && sidebarTabs !== "picker" && deviceType !== "duo-unfolded" && !compactTabs ? (
        <Toolbar placement="right">
          <Toolbar.Button
            accessibilityLabel="New chat"
            icon="square.and.pencil"
            onPress={onNewTask}
          />
        </Toolbar>
      ) : null}
      {sidebarTabs === "toolbar" ? (
        <Toolbar placement="bottom">
          <WorkspaceTabToolbarButtons />
        </Toolbar>
      ) : null}
    </>
  );
}
