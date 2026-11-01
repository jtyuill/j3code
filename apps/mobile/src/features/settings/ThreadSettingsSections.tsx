import { ScreenScrollView as ScrollView } from "../../components/ui/ScreenScrollView";
import { useAtomSet, useAtomValue } from "@effect/atom-react";
import { AsyncResult } from "effect/reactivity";
import { View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";

import { AppText as Text } from "../../components/ui/AppText";
import { mobilePreferencesAtom, updateMobilePreferencesAtom } from "../../state/preferences";
import { SettingsSection } from "./components/SettingsSection";
import { SettingsSwitchRow } from "./components/SettingsSwitchRow";
import { SettingsScreen } from "./components/SettingsScreen";

export function SettingsLabsRouteScreen() {
  const insets = useSafeAreaInsets();

  return (
    <SettingsScreen title="Labs">
      <ScrollView
        contentInsetAdjustmentBehavior="automatic"
        showsVerticalScrollIndicator={false}
        className="flex-1"
        contentContainerClassName="gap-6 px-5 pt-4"
        contentContainerStyle={{ paddingBottom: Math.max(insets.bottom, 18) + 18 }}
      >
        <LegacySettingsSection />
      </ScrollView>
    </SettingsScreen>
  );
}

export function WorkingSectionSettings() {
  const savePreferences = useAtomSet(updateMobilePreferencesAtom);
  const preferences = useAtomValue(mobilePreferencesAtom);
  const workingShelfEnabled =
    AsyncResult.isSuccess(preferences) && preferences.value.workingShelfEnabled !== false;

  return (
    <View className="gap-3">
      <SettingsSection title="Sidebar">
        <SettingsSwitchRow
          icon="bolt.circle"
          label="Working section"
          value={workingShelfEnabled}
          onValueChange={(value) => savePreferences({ workingShelfEnabled: value })}
        />
      </SettingsSection>
      <Text className="px-2 text-sm text-foreground-muted">
        Fold working and monitoring threads into a Working section. They return to the top of the
        list when they need you. While this is on, active threads are ordered by time and cannot be
        moved.
      </Text>
    </View>
  );
}

export function NewThreadSettingsSection() {
  const savePreferences = useAtomSet(updateMobilePreferencesAtom);
  const preferences = useAtomValue(mobilePreferencesAtom);
  const useLastProject =
    AsyncResult.isSuccess(preferences) && preferences.value.useLastProjectForNewThreads !== false;

  return (
    <SettingsSection title="New threads">
      <SettingsSwitchRow
        icon="square.and.pencil"
        label="Use last project"
        subtitle="Start new threads in the last project you used on this device."
        value={useLastProject}
        onValueChange={(value) => savePreferences({ useLastProjectForNewThreads: value })}
      />
    </SettingsSection>
  );
}

function LegacySettingsSection() {
  const savePreferences = useAtomSet(updateMobilePreferencesAtom);
  const preferences = useAtomValue(mobilePreferencesAtom);
  const planModeEnabled =
    AsyncResult.isSuccess(preferences) && preferences.value.planModeEnabled === true;

  return (
    <View className="gap-3">
      <SettingsSection title="Legacy">
        <SettingsSwitchRow
          icon="hammer"
          label="Plan Mode"
          value={planModeEnabled}
          onValueChange={(value) => savePreferences({ planModeEnabled: value })}
        />
      </SettingsSection>
      <Text className="px-2 text-sm text-foreground-muted">
        Opt into retired interfaces kept for compatibility. Plan Mode restores the Build/Plan
        control; otherwise every task runs in Build mode.
      </Text>
    </View>
  );
}
