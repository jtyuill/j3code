import { useWorkspaceSidebarHeaderItem } from "../layout/useWorkspaceSidebarHeaderItem";
import { StackActions, useNavigation } from "@react-navigation/native";
import { threadHeaderAppearance } from "./thread-header-appearance";
import { isDuoDevice } from "../../lib/device-type";
import type { AppNativeStackNavigationOptions } from "../../native/StackHeader";
import { useDeviceType } from "../../native/native-layout-metrics";
import { useNativeWorkspaceColumnsSupported } from "../../native/NativeWorkspaceColumns";
import { useAdaptiveWorkspaceLayout } from "../layout/AdaptiveWorkspaceLayout";
import { withNativeGlassHeaderItem } from "../layout/native-glass-header-items";

type NativeHeaderItems = ReadonlyArray<Record<string, unknown>>;

export function useThreadHeaderOptions(props: {
  readonly title: string;
  readonly subtitle: string;
  readonly headerColor: string;
  readonly usesNativeHeaderGlass: boolean;
  readonly draft?: boolean;
}) {
  const usesNativeWorkspaceColumns = useNativeWorkspaceColumnsSupported();
  const deviceType = useDeviceType();
  const usesDuoHeader = usesNativeWorkspaceColumns && isDuoDevice(deviceType);
  const navigation = useNavigation();
  const { layout, startNewTask } = useAdaptiveWorkspaceLayout();
  const sidebarHeaderItem = useWorkspaceSidebarHeaderItem();
  const splitLeftHeaderItems: NativeHeaderItems = [
    ...(!usesDuoHeader
      ? [
          {
            spacing: 18,
            type: "spacing" as const,
          },
        ]
      : []),
    sidebarHeaderItem,
    ...(!usesDuoHeader && !props.draft
      ? [
          withNativeGlassHeaderItem({
            accessibilityLabel: "New task",
            icon: { name: "square.and.pencil", type: "sfSymbol" as const },
            identifier: "thread-left-new-task",
            label: "New task",
            onPress: startNewTask,
            type: "button" as const,
          }),
        ]
      : []),
  ];
  const canGoBack = navigation.canGoBack();
  const compactHomeHeaderItems: NativeHeaderItems = [
    withNativeGlassHeaderItem({
      accessibilityLabel: "Go to threads list",
      icon: { name: "list.bullet", type: "sfSymbol" as const },
      identifier: "thread-left-home",
      onPress: () => navigation.dispatch(StackActions.replace("Home")),
      type: "button" as const,
    }),
  ];

  const options: AppNativeStackNavigationOptions = {
    ...threadHeaderAppearance(props.usesNativeHeaderGlass),
    headerTitle: props.title,
    title: props.title,
    headerBackVisible: !layout.usesSplitView,
    unstable_headerLeftItems: layout.usesSplitView
      ? () => splitLeftHeaderItems
      : canGoBack
        ? undefined
        : () => compactHomeHeaderItems,
    unstable_headerToolbarItems: () => [],
    unstable_headerSubtitle: props.usesNativeHeaderGlass ? props.subtitle : undefined,
    contentStyle: undefined,
  };
  return {
    options,
    // Factories re-read only on version changes; include displayed items.
    optionsVersion: [splitLeftHeaderItems, usesDuoHeader],
    sidebar: false,
  };
}
