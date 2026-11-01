import { ScreenScrollView as ScrollView } from "../../components/ui/ScreenScrollView";
import { useNavigation } from "@react-navigation/native";
import type { EnvironmentId } from "@t3tools/contracts";
import { useSafeAreaInsets } from "react-native-safe-area-context";

import { SettingsScreen } from "./components/SettingsScreen";
import { LocalEnvironmentList } from "../connection/LocalEnvironmentList";
import { useUniwindTheme } from "../../lib/useUniwindTheme";
import { useRemoteConnections } from "../../state/use-remote-environment-registry";

export function SettingsEnvironmentsRouteScreen() {
  const {
    connectedEnvironments,
    onReconnectEnvironment,
    onRemoveEnvironmentPress,
    onSetEnvironmentEnabled,
    onUpdateEnvironment,
  } = useRemoteConnections();
  const navigation = useNavigation();
  const insets = useSafeAreaInsets();
  const headerIconColor = useUniwindTheme()["--color-icon"];

  const openEnvironment = (environmentId: EnvironmentId) => {
    navigation.navigate("SettingsEnvironmentDetail", { environmentId });
  };

  return (
    <SettingsScreen
      title="Environments"
      actions={[
        {
          accessibilityLabel: "Add environment",
          icon: "plus",
          tintColor: headerIconColor,
          onPress: () => navigation.navigate("SettingsEnvironmentNew"),
        },
      ]}
    >
      <ScrollView
        alwaysBounceVertical
        contentInsetAdjustmentBehavior="automatic"
        showsVerticalScrollIndicator={false}
        className="flex-1"
        contentContainerClassName="px-5 pt-4"
        contentContainerStyle={{
          paddingBottom: Math.max(insets.bottom, 18) + 18,
        }}
      >
        <LocalEnvironmentList
          environments={connectedEnvironments}
          expandedId={null}
          onToggle={openEnvironment}
          opensDetails
          onReconnect={onReconnectEnvironment}
          onRemove={onRemoveEnvironmentPress}
          onSetEnabled={onSetEnvironmentEnabled}
          onUpdate={onUpdateEnvironment}
        />
      </ScrollView>
    </SettingsScreen>
  );
}
