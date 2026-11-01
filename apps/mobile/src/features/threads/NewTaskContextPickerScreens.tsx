import * as Haptics from "expo-haptics";
import { useNavigation } from "@react-navigation/native";
import { useState, type ReactNode } from "react";
import { Pressable, ScrollView, View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { resolveEnvironmentMachineKind, type EnvironmentId } from "@t3tools/contracts";

import { Button } from "../../components/ui/Button";
import { ScreenContent } from "../../components/ui/ScreenContent";
import { SymbolView } from "../../components/ui/AppSymbol";
import { AppText as Text } from "../../components/ui/AppText";
import { EnvironmentMachineSymbol } from "../../components/EnvironmentMachineSymbol";
import { cn } from "../../lib/cn";
import { NativeStackScreenOptions } from "../../native/StackHeader";
import { useEnvironmentPresentation } from "../../state/presentation";
import { useRemoteConnections } from "../../state/use-remote-environment-registry";
import { ConnectionStatusDot } from "../connection/ConnectionStatusDot";
import {
  ConnectionDiagnosisDetails,
  useConnectionDiagnosis,
} from "../connection/ConnectionDiagnosisDetails";
import { environmentStatusText } from "../connection/environmentStatusText";
import { useNewTaskFlow } from "./new-task-flow-provider";

function EnvironmentPickerRow(props: {
  environmentId: EnvironmentId;
  title: string;
  selected: boolean;
  disabled: boolean;
  isLast: boolean;
  onPress: () => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const { presentation } = useEnvironmentPresentation(props.environmentId);
  const { diagnosis, state } = useConnectionDiagnosis(props.environmentId, presentation);
  const { onReconnectEnvironment } = useRemoteConnections();
  const connection = presentation?.connection;
  const status =
    diagnosis?.headline ??
    (connection ? environmentStatusText({ ...connection, error: null }) : "Offline");
  return (
    <View className={cn("bg-grouped-card", !props.isLast && "border-b border-border-subtle")}>
      <View className="flex-row items-center">
        <Pressable
          accessibilityRole="radio"
          accessibilityLabel={`${props.title}, ${status}`}
          accessibilityState={{ checked: props.selected }}
          disabled={props.disabled}
          onPress={props.onPress}
          className="min-h-16 flex-1 flex-row items-center gap-3 px-4 py-3 active:bg-subtle"
        >
          <EnvironmentMachineSymbol
            kind={resolveEnvironmentMachineKind(presentation?.serverConfig ?? null)}
            size={20}
            tintColorClassName="accent-icon-muted"
          />
          <View className="min-w-0 flex-1 gap-1">
            <Text className="text-base font-t3-medium text-foreground" numberOfLines={1}>
              {props.title}
            </Text>
            <View className="flex-row items-center gap-1.5">
              <ConnectionStatusDot state={connection?.phase ?? "offline"} pulse={false} size={6} />
              <Text className="shrink text-xs text-foreground-muted" numberOfLines={2}>
                {status}
              </Text>
            </View>
          </View>
          {props.selected ? (
            <SymbolView name="checkmark" size={16} tintColorClassName="accent-icon" />
          ) : null}
        </Pressable>
        <Pressable
          accessibilityRole="button"
          accessibilityLabel={`${expanded ? "Hide" : "Show"} connection details for ${props.title}`}
          accessibilityState={{ expanded }}
          onPress={() => setExpanded(!expanded)}
          className="min-h-11 min-w-11 items-center justify-center active:opacity-65"
        >
          <SymbolView
            name={expanded ? "chevron.up" : "chevron.down"}
            size={13}
            tintColorClassName="accent-icon-muted"
          />
        </Pressable>
      </View>
      {expanded ? (
        <View className="gap-3 px-4 pb-4">
          {diagnosis ? (
            <ConnectionDiagnosisDetails hostLabel={props.title} diagnosis={diagnosis} />
          ) : (
            <Text className="text-sm text-foreground-muted">{status}</Text>
          )}
          {state.retryAt ? (
            <Text className="text-sm text-foreground-muted">
              Next reconnect at {new Date(state.retryAt).toLocaleTimeString()}
            </Text>
          ) : null}
          {connection?.phase !== "connected" ? (
            <Button
              label="Reconnect now"
              onPress={() => onReconnectEnvironment(props.environmentId)}
            />
          ) : null}
        </View>
      ) : null}
    </View>
  );
}

function PickerSurface(props: { readonly children: ReactNode }) {
  return <View className="overflow-hidden rounded-2xl bg-grouped-card">{props.children}</View>;
}

export function NewTaskEnvironmentPickerRouteScreen() {
  const flow = useNewTaskFlow();
  const navigation = useNavigation();
  const insets = useSafeAreaInsets();
  return (
    <View className="flex-1 bg-sheet" collapsable={false}>
      <NativeStackScreenOptions options={{ headerShown: true, title: "Environment" }} />
      <ScreenContent>
        <ScrollView
          contentInsetAdjustmentBehavior="automatic"
          contentContainerStyle={{
            paddingBottom: Math.max(insets.bottom, 16) + 16,
            paddingHorizontal: 16,
            paddingTop: 16,
          }}
          showsVerticalScrollIndicator={false}
        >
          <PickerSurface>
            {flow.environments.map((environment, index) => (
              <EnvironmentPickerRow
                key={String(environment.environmentId)}
                environmentId={environment.environmentId}
                isLast={index === flow.environments.length - 1}
                disabled={flow.switchingToEnvironmentId !== null}
                onPress={() => {
                  void Haptics.selectionAsync();
                  void flow.switchEnvironment(environment.environmentId).then((switched) => {
                    if (switched) navigation.goBack();
                  });
                }}
                selected={flow.selectedEnvironmentId === environment.environmentId}
                title={environment.environmentLabel}
              />
            ))}
          </PickerSurface>
        </ScrollView>
      </ScreenContent>
    </View>
  );
}
