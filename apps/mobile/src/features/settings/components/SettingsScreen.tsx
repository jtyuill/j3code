import { useNavigation } from "@react-navigation/native";
import type { ReactNode } from "react";
import { View } from "react-native";

import type { AppBarProps } from "../../../components/ui/AppBar.types";
import { AppBar } from "../../../components/ui/AppBar";
import { ScreenContent as SettingsScreenContent } from "../../../components/ui/ScreenContent";
import { useDeviceType } from "../../../native/native-layout-metrics";
import { useCompactWorkspaceTabs } from "../../workspace/useSystemWorkspaceTabBar";
import {
  ScreenScrollViewBottomInset,
  ScreenScrollViewSurface,
} from "../../../components/ui/ScreenScrollView";

export { SettingsScreenContent };

export function SettingsScreen(
  props: Pick<AppBarProps, "title" | "actions"> & {
    readonly children: ReactNode;
    readonly formSheet?: boolean;
  },
) {
  const navigation = useNavigation();
  const deviceType = useDeviceType();
  const compactTabs = useCompactWorkspaceTabs();
  // Settings' grouped cards only lift off the screen surface, so the folded Duo doesn't take
  // the workspace's drawer surface here.
  const surfaceClassName = deviceType === "duo-folded" ? "bg-screen" : "bg-sheet";

  return (
    <View collapsable={false} className={`flex-1 ${surfaceClassName}`}>
      <AppBar
        title={props.title}
        actions={props.actions}
        sidebar={false}
        onBack={() => navigation.goBack()}
        hideBottomBorder={!props.formSheet}
      />
      <ScreenScrollViewBottomInset value={deviceType === "pad" || compactTabs ? 80 : 0}>
        <ScreenScrollViewSurface value={surfaceClassName}>{props.children}</ScreenScrollViewSurface>
      </ScreenScrollViewBottomInset>
    </View>
  );
}
