import { ScreenScrollView as ScrollView } from "../../components/ui/ScreenScrollView";
import * as Haptics from "expo-haptics";
import {
  StackActions,
  useNavigation,
  useRoute,
  type StaticScreenProps,
} from "@react-navigation/native";
import { useAtomValue } from "@effect/atom-react";
import { useState } from "react";
import { View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { cn } from "../../lib/cn";
import { useUniwindTheme } from "../../lib/useUniwindTheme";
import { SettingsScreen } from "../settings/components/SettingsScreen";
import { AppText as Text, AppTextInput } from "../../components/ui/AppText";
import { useRemoteConnections } from "../../state/use-remote-environment-registry";
import { autoConnectReport, devAutoConnectAttemptAtom } from "./devAutoConnect";
import { leaveSettingsPage } from "../settings/settings-navigation";
import { workspaceHasColumns } from "../workspace/workspace-selection-store";

/** Offered on first launch, never connected to without the user confirming it. */
export const DEFAULT_HOST = "http://shieldy:3780";

type ConnectionsNewRouteParams = {
  readonly host?: string;
  readonly autoConnect?: string;
};

export function ConnectionsNewRouteScreen({
  route,
}: StaticScreenProps<ConnectionsNewRouteParams | undefined>) {
  const routeHost = route.params?.host?.trim() ?? "";
  // A new prefill link replaces whatever the form held.
  return <HostForm key={routeHost} routeHost={routeHost} />;
}

function HostForm({ routeHost }: { readonly routeHost: string }) {
  const { connectionHost, connectedEnvironmentCount, onChangeConnectionHost, onConnectPress } =
    useRemoteConnections();
  const navigation = useNavigation();
  const routeName = useRoute().name;
  const insets = useSafeAreaInsets();
  const [hostInput, setHostInput] = useState(
    () => routeHost || connectionHost || (connectedEnvironmentCount === 0 ? DEFAULT_HOST : ""),
  );
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);
  // The user's own edit or submit dismisses the dev auto-connect report.
  const [ownAttempt, setOwnAttempt] = useState(false);
  const autoConnect = autoConnectReport(
    useAtomValue(devAutoConnectAttemptAtom),
    routeHost,
    ownAttempt,
  );
  const connecting = isSubmitting || autoConnect.connecting;
  const connectionError = submitError ?? autoConnect.error;
  const headerIconColor = useUniwindTheme()["--color-icon"];
  const connectDisabled = connecting || hostInput.trim().length === 0;

  const handleHostChange = (value: string) => {
    setHostInput(value);
    setSubmitError(null);
    setOwnAttempt(true);
  };

  const handleSubmit = async () => {
    const host = hostInput.trim();
    setOwnAttempt(true);
    setSubmitError(null);
    setIsSubmitting(true);
    onChangeConnectionHost(host);
    const outcome = await onConnectPress(host);
    setIsSubmitting(false);
    setSubmitError(outcome.error);
    void Haptics.notificationAsync(
      outcome.ok
        ? Haptics.NotificationFeedbackType.Success
        : Haptics.NotificationFeedbackType.Error,
    );
    if (outcome.ok) {
      // Replacing with Home would nest it inside any sheet below; return to the opener.
      if (!navigation.canGoBack()) {
        navigation.dispatch(StackActions.replace("Home"));
      } else {
        // Added from the Settings list, the new environment shows on Environments.
        leaveSettingsPage(navigation, workspaceHasColumns(), "SettingsEnvironments");
      }
    }
  };

  return (
    <SettingsScreen
      formSheet={routeName === "ConnectionsNew"}
      title="Add environment"
      actions={[
        {
          accessibilityLabel: connecting ? "Connecting" : "Add environment",
          loading: connecting,
          icon: "checkmark",
          tintColor: headerIconColor,
          disabled: connectDisabled,
          onPress: () => {
            void handleSubmit();
          },
        },
      ]}
    >
      <ScrollView
        nestedScrollEnabled
        contentInsetAdjustmentBehavior="automatic"
        keyboardShouldPersistTaps="handled"
        showsVerticalScrollIndicator={false}
        className="flex-1"
        contentInset={{ bottom: Math.max(insets.bottom, 18) + 18 }}
        contentContainerStyle={{
          paddingHorizontal: 16,
          paddingTop: 4,
        }}
      >
        <View collapsable={false} className="gap-2">
          <View
            collapsable={false}
            className={cn(
              "overflow-hidden rounded-[26px] border-continuous border bg-grouped-card",
              connectionError ? "border-danger-foreground" : "border-transparent",
            )}
          >
            <View collapsable={false} className="h-13 flex-row items-center gap-3 px-4">
              <Text
                accessibilityElementsHidden
                importantForAccessibility="no-hide-descendants"
                className="w-14 text-base leading-[23px] text-foreground-muted"
              >
                Host
              </Text>
              <AppTextInput
                accessibilityLabel="Host"
                autoCapitalize="none"
                autoCorrect={false}
                keyboardType="url"
                placeholder={DEFAULT_HOST}
                returnKeyType="go"
                value={hostInput}
                onChangeText={handleHostChange}
                onSubmitEditing={() => {
                  if (!connectDisabled) void handleSubmit();
                }}
                className="min-h-0 flex-1 rounded-none border-0 bg-transparent p-0 text-base leading-[23px]"
              />
            </View>
          </View>
          <Text
            accessibilityLiveRegion="polite"
            className={cn(
              "px-4 text-sm leading-normal",
              connectionError ? "text-danger-foreground" : "text-foreground-muted",
            )}
          >
            {connectionError ??
              "The address of a j3 server on your tailnet or local network, such as http://shieldy:3780. j3 servers take no pairing code: the tailnet is the boundary."}
          </Text>
        </View>
      </ScrollView>
    </SettingsScreen>
  );
}
