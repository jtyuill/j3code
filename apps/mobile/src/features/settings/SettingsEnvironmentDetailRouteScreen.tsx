import { useAtomValue } from "@effect/atom-react";
import { useNavigation, type StaticScreenProps } from "@react-navigation/native";
import { AuthOrchestrationReadScope, type EnvironmentId } from "@t3tools/contracts";
import { settlePromise, squashAtomCommandFailure } from "@t3tools/client-runtime/state/runtime";
import { AsyncResult } from "effect/reactivity";
import { useRef, useState } from "react";
import { View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";

import { AppText as Text } from "../../components/ui/AppText";
import { ProviderIcon } from "../../components/ProviderIcon";
import { ScreenScrollView } from "../../components/ui/ScreenScrollView";
import { serverEnvironment } from "../../state/server";
import { useEnvironmentScope, readEnvironmentScope } from "../../state/session";
import { useAtomCommand } from "../../state/use-atom-command";
import { useRemoteConnections } from "../../state/use-remote-environment-registry";
import { ConnectionEnvironmentRow } from "../connection/ConnectionEnvironmentRow";
import { EnvironmentRoutesSection } from "./EnvironmentRoutesSection";
import { ListActionRow as SettingsActionRow } from "../../components/ui/ListActionRow";
import { SettingsScreen } from "./components/SettingsScreen";
import { SettingsSection } from "./components/SettingsSection";

export function SettingsEnvironmentDetailRouteScreen({
  route,
}: StaticScreenProps<{
  readonly environmentId: EnvironmentId;
}>) {
  return (
    <EnvironmentDetail
      key={route.params.environmentId}
      environmentId={route.params.environmentId}
    />
  );
}

function EnvironmentDetail({ environmentId }: { readonly environmentId: EnvironmentId }) {
  const insets = useSafeAreaInsets();
  const navigation = useNavigation();
  const connections = useRemoteConnections();
  const environment = connections.connectedEnvironments.find(
    (entry) => entry.environmentId === environmentId,
  );
  const config = useAtomValue(serverEnvironment.configValueAtom(environmentId));
  const refreshProviders = useAtomCommand(serverEnvironment.refreshProviders);
  const [connectionExpanded, setConnectionExpanded] = useState(false);
  const [pending, setPending] = useState(false);
  const pendingRef = useRef(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const connected = environment?.isEnabled === true && environment.connectionState === "connected";
  const canRead = useEnvironmentScope(environmentId, AuthOrchestrationReadScope);
  const busy = !connected || pending;

  async function refresh() {
    if (busy || !canRead || pendingRef.current) return;
    if (!readEnvironmentScope(environmentId, AuthOrchestrationReadScope)) return;
    pendingRef.current = true;
    setPending(true);
    setError(null);
    setNotice(null);
    const result = await settlePromise(async () => {
      const refreshed = await refreshProviders({ environmentId, input: { reloadSecrets: true } });
      if (AsyncResult.isFailure(refreshed)) throw squashAtomCommandFailure(refreshed);
    });
    pendingRef.current = false;
    setPending(false);
    if (AsyncResult.isFailure(result)) {
      const cause = squashAtomCommandFailure(result);
      setError(
        cause instanceof Error ? cause.message : "The action could not be completed. Try again.",
      );
      return;
    }
    setNotice("Provider status refreshed.");
  }

  return (
    <SettingsScreen title={environment?.environmentLabel ?? "Environment"}>
      <ScreenScrollView
        contentInsetAdjustmentBehavior="automatic"
        className="flex-1"
        contentContainerClassName="gap-6 px-5 pt-4"
        contentContainerStyle={{ paddingBottom: Math.max(insets.bottom, 18) + 18 }}
      >
        {!environment ? (
          <Text className="text-base text-foreground-muted">
            This environment is no longer saved on this device.
          </Text>
        ) : (
          <>
            <SettingsSection title="Connection">
              <ConnectionEnvironmentRow
                environment={environment}
                expanded={connectionExpanded}
                onToggle={() => setConnectionExpanded((value) => !value)}
                onReconnect={connections.onReconnectEnvironment}
                onRemove={connections.onRemoveEnvironmentPress}
                onSetEnabled={connections.onSetEnvironmentEnabled}
                onUpdate={connections.onUpdateEnvironment}
              />
            </SettingsSection>
            <EnvironmentRoutesSection
              environmentId={environmentId}
              connected={connected}
              onAddRoute={() => navigation.navigate("SettingsEnvironmentNew")}
            />
            {!connected ? (
              <Text className="px-2 text-sm text-foreground-muted">
                Connect this environment to see its server and models.
              </Text>
            ) : null}
            {error ? (
              <Text selectable className="px-2 text-sm text-danger-foreground">
                {error}
              </Text>
            ) : null}
            {notice ? <Text className="px-2 text-sm text-foreground-muted">{notice}</Text> : null}
            {config ? (
              <>
                <SettingsSection title="Server">
                  <View className="gap-1 p-4">
                    <Text className="text-base text-foreground">
                      Version {config.environment.serverVersion}
                    </Text>
                  </View>
                </SettingsSection>
                <SettingsSection title="Harness">
                  <SettingsActionRow
                    icon="arrow.clockwise"
                    label="Refresh models"
                    disabled={busy || !canRead}
                    loading={pending}
                    onPress={() => void refresh()}
                  />
                  {config.providers
                    .filter((provider) => provider.enabled)
                    .map((provider) => (
                      <View key={provider.instanceId} className="gap-1 p-4">
                        <View className="flex-row items-center gap-2">
                          <ProviderIcon provider={provider.driver} size={18} />
                          <Text className="min-w-0 flex-1 text-base font-t3-medium text-foreground">
                            {provider.displayName ?? provider.driver}
                          </Text>
                        </View>
                        <Text className="text-sm text-foreground-muted">
                          {provider.installed
                            ? (provider.version ?? "Version unknown")
                            : "Not installed"}
                          {` · ${provider.models.length} models`}
                        </Text>
                        {provider.unavailableReason || provider.message ? (
                          <Text selectable className="text-sm text-foreground-muted">
                            {provider.unavailableReason ?? provider.message}
                          </Text>
                        ) : null}
                      </View>
                    ))}
                </SettingsSection>
              </>
            ) : null}
          </>
        )}
      </ScreenScrollView>
    </SettingsScreen>
  );
}
