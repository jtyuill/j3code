import { isDraftThreadRoute } from "./thread-route-state";
import * as Dialog from "../../components/ui/Dialog";
import { AppActivityIndicator } from "../../components/ui/AppActivityIndicator";
import { AppBar } from "../../components/ui/AppBar";
import { StackActions, useNavigation, type StaticScreenProps } from "@react-navigation/native";
import { SymbolView } from "../../components/ui/AppSymbol";
import { pickableProjectGroups } from "@t3tools/client-runtime/state/project-grouping";
import { isScratchProject } from "@t3tools/client-runtime/state/projects";
import {
  isAtomCommandInterrupted,
  squashAtomCommandFailure,
} from "@t3tools/client-runtime/state/runtime";
import type { EnvironmentProject } from "@t3tools/client-runtime/state/shell";
import { useRef, useState } from "react";
import { Pressable, View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { cn } from "../../lib/cn";
import { ScreenContent } from "../../components/ui/ScreenContent";
import { Button } from "../../components/ui/Button";
import { ScreenScrollView as ScrollView } from "../../components/ui/ScreenScrollView";
import { AppText as Text } from "../../components/ui/AppText";
import { ProjectFavicon } from "../../components/ProjectFavicon";
import { useServerConfigs } from "../../state/entities";
import { projectEnvironment } from "../../state/projects";
import { useAtomCommand } from "../../state/use-atom-command";
import { useRemoteConnectionStatus } from "../../state/use-remote-environment-registry";
import type { WorkspaceState } from "../../state/workspaceModel";
import { useWorkspaceState } from "../../state/workspace";

import { useNewTaskFlow } from "./new-task-flow-provider";
import {
  filterProjectScopes,
  getProjectScopeSelectionTarget,
  resolveNewTaskScratchOption,
} from "./new-task-project-selection";
import { useSelectWorkspaceDetail } from "../workspace/useSelectWorkspaceDetail";

type NewTaskRouteParams = {
  readonly prompt?: string | string[];
  readonly repo?: string | string[];
  readonly provider?: string | string[];
  readonly model?: string | string[];
};

function deriveProjectEmptyState(catalogState: WorkspaceState): {
  readonly title: string;
  readonly detail: string;
  readonly loading: boolean;
} {
  if (catalogState.isLoadingConnections) {
    return {
      title: "Loading environments",
      detail: "Checking saved environments on this device.",
      loading: true,
    };
  }

  if (!catalogState.hasConnections) {
    return {
      title: "No environments connected",
      detail: "Add an environment before creating a task.",
      loading: false,
    };
  }

  if (
    (catalogState.connectionState === "available" ||
      catalogState.connectionState === "offline" ||
      catalogState.connectionState === "error" ||
      catalogState.connectionState === "unsupported") &&
    !catalogState.hasLoadedShellSnapshot
  ) {
    return {
      title:
        catalogState.connectionState === "unsupported"
          ? "Client not supported"
          : "Environment unavailable",
      detail:
        catalogState.connectionError ??
        "The saved environment is offline. Check the URL or start the environment, then retry.",
      loading: false,
    };
  }

  if (
    catalogState.hasConnectingEnvironment &&
    !catalogState.hasLoadedShellSnapshot &&
    catalogState.connectionError === null
  ) {
    return {
      title: "Connecting to environment",
      detail: "Loading projects from the saved environment.",
      loading: true,
    };
  }

  return {
    title: "No projects found",
    detail: "The connected environment did not report any projects.",
    loading: false,
  };
}

function NewTaskHeader(props: {
  readonly title: string;
  readonly canAddProject: boolean;
  readonly searchText: string;
  readonly onSearchTextChange: (text: string) => void;
}) {
  const navigation = useNavigation();
  const goBack = () => {
    if (navigation.canGoBack()) navigation.goBack();
    else navigation.getParent()?.goBack();
  };
  return (
    <>
      <AppBar
        title={props.title}
        sidebar={false}
        options={{ headerBackVisible: true }}
        hideBottomBorder
        onBack={goBack}
        actions={
          props.canAddProject
            ? [
                {
                  accessibilityLabel: "Add project",
                  icon: "plus",
                  onPress: () => navigation.dispatch(StackActions.push("AddProject")),
                },
              ]
            : []
        }
        search={{
          value: props.searchText,
          onChangeText: props.onSearchTextChange,
          placeholder: "Search projects",
        }}
      />
    </>
  );
}

export function NewTaskRouteScreen(_props: StaticScreenProps<NewTaskRouteParams | undefined>) {
  const [searchText, setSearchText] = useState("");
  const { projectScopes, selectedEnvironmentId, setProject } = useNewTaskFlow();
  const { state: catalogState } = useWorkspaceState();
  const navigation = useNavigation();
  const selectDetail = useSelectWorkspaceDetail();
  const insets = useSafeAreaInsets();
  const projectEmptyState = deriveProjectEmptyState(catalogState);
  const serverConfigs = useServerConfigs();
  const listScopes = pickableProjectGroups(projectScopes).filter(
    (scope) =>
      !scope.projects.every((project) =>
        isScratchProject(project, serverConfigs.get(project.environmentId)?.scratchWorkspaceRoot),
      ),
  );
  const visibleScopes = filterProjectScopes(listScopes, searchText);
  const { connectedEnvironments } = useRemoteConnectionStatus();
  const connectedEnvironmentIds = new Set(
    connectedEnvironments
      .filter((environment) => environment.connectionState === "connected")
      .map((environment) => environment.environmentId),
  );
  const openScratch = useAtomCommand(projectEnvironment.openScratch, {
    reportFailure: false,
  });
  const scratchOption = resolveNewTaskScratchOption({
    environments: connectedEnvironments,
    serverConfigs,
    selectedEnvironmentId,
  });
  const canStartScratch = scratchOption?.ready === true;
  const scratchStartInFlightRef = useRef(false);

  function selectProject(project: EnvironmentProject): void {
    const root = navigation.getState()?.routeNames.includes("NewTaskProject")
      ? navigation
      : navigation.getParent();
    const rootState = root?.getState();
    const threadRoute = rootState?.routes[rootState.index - 1];
    if (isDraftThreadRoute(threadRoute)) {
      setProject(project);
      root?.goBack();
      return;
    }
    selectDetail({
      name: "Thread",
      params: {
        draft: {
          environmentId: String(project.environmentId),
          projectId: String(project.id),
        },
      },
    });
  }

  async function startScratch(): Promise<void> {
    if (!canStartScratch || !scratchOption || scratchStartInFlightRef.current) return;
    const environmentId = scratchOption.environmentId;
    scratchStartInFlightRef.current = true;
    const result = await openScratch({ environmentId, input: {} });
    if (result._tag === "Failure") {
      scratchStartInFlightRef.current = false;
      if (!isAtomCommandInterrupted(result)) {
        const error = squashAtomCommandFailure(result);
        Dialog.alert(
          "Could not start without a project",
          error instanceof Error
            ? error.message
            : "The folder for threads without a project could not be created.",
        );
      }
      return;
    }
    selectProject(result.value);
    scratchStartInFlightRef.current = false;
  }

  return (
    <View collapsable={false} className="flex-1 bg-sheet">
      <NewTaskHeader
        title="Choose project"
        canAddProject={catalogState.hasReadyEnvironment}
        searchText={searchText}
        onSearchTextChange={setSearchText}
      />

      <ScreenContent>
        <ScrollView
          contentInsetAdjustmentBehavior="automatic"
          showsVerticalScrollIndicator={false}
          keyboardShouldPersistTaps="handled"
          keyboardDismissMode="on-drag"
          className="flex-1"
          contentContainerStyle={{
            gap: 12,
            paddingBottom: Math.max(insets.bottom, 18) + 18,
            paddingHorizontal: 20,
            paddingTop: 8,
          }}
        >
          {scratchOption !== null && listScopes.length > 0 ? (
            <View collapsable={false} className="overflow-hidden rounded-[24px] bg-grouped-card">
              <Pressable
                accessibilityRole="button"
                accessibilityLabel="No project"
                disabled={!canStartScratch}
                onPress={() => void startScratch()}
                className={cn(
                  "flex-row items-center gap-3 bg-grouped-card px-4 py-3.5",
                  !canStartScratch && "opacity-45",
                )}
              >
                <View className="h-7 w-7 items-center justify-center">
                  <SymbolView
                    name="text.bubble"
                    size={18}
                    tintColorClassName="accent-icon-muted"
                    type="monochrome"
                  />
                </View>
                <View className="min-w-0 flex-1">
                  <Text className="text-base font-t3-bold leading-snug">No project</Text>
                  <Text className="text-xs leading-snug text-foreground-muted" numberOfLines={1}>
                    Start a task without a project
                  </Text>
                </View>
                <SymbolView
                  name="chevron.right"
                  size={14}
                  tintColorClassName="accent-chevron"
                  type="monochrome"
                />
              </Pressable>
            </View>
          ) : null}
          {listScopes.length === 0 ? (
            <View
              collapsable={false}
              className="items-center gap-3 rounded-[24px] bg-grouped-card px-6 py-8"
            >
              {projectEmptyState.loading ? (
                <AppActivityIndicator colorClassName="accent-icon-muted" />
              ) : null}
              <Text className="text-center text-lg font-t3-bold text-foreground">
                {projectEmptyState.title}
              </Text>
              <Text className="text-center text-sm leading-normal text-foreground-muted">
                {projectEmptyState.detail}
              </Text>
              {!catalogState.hasReadyEnvironment ? (
                <View className="mt-1">
                  <Button
                    label="Add environment"
                    tone="primary"
                    onPress={() => navigation.navigate("ConnectionsNew")}
                  />
                </View>
              ) : (
                <>
                  <View className="mt-1">
                    <Button
                      label="Add new project"
                      tone="primary"
                      onPress={() => navigation.dispatch(StackActions.push("AddProject"))}
                    />
                  </View>
                  {canStartScratch ? (
                    <Button
                      label="Start without a project"
                      tone="secondary"
                      onPress={() => void startScratch()}
                    />
                  ) : null}
                </>
              )}
            </View>
          ) : visibleScopes.length === 0 ? (
            <View className="items-center gap-2 px-6 py-8">
              <Text className="text-center text-lg font-t3-bold text-foreground">
                No matching projects
              </Text>
              <Text className="text-center text-sm leading-normal text-foreground-muted">
                Try a different project name or workspace path.
              </Text>
            </View>
          ) : (
            <View collapsable={false} className="overflow-hidden rounded-[24px] bg-grouped-card">
              {visibleScopes.map((scope, scopeIndex) => {
                const hasMultipleProjects = scope.projects.length > 1;
                const selectionTarget = getProjectScopeSelectionTarget(
                  scope,
                  selectedEnvironmentId,
                  connectedEnvironmentIds,
                );
                return (
                  <View
                    key={scope.key}
                    className={cn(scopeIndex > 0 && "border-t border-border-subtle")}
                  >
                    <Pressable
                      accessibilityRole="button"
                      accessibilityLabel={scope.title}
                      onPress={() => selectProject(selectionTarget)}
                      className="flex-row items-center gap-3 bg-grouped-card px-4 py-3.5"
                    >
                      <View className="h-7 w-7 items-center justify-center">
                        <ProjectFavicon
                          environmentId={scope.representative.environmentId}
                          faviconPath={scope.representative.faviconPath}
                          projectIcon={scope.representative.projectIcon}
                          size={20}
                          projectTitle={scope.title}
                          workspaceRoot={scope.representative.workspaceRoot}
                        />
                      </View>
                      <View className="min-w-0 flex-1">
                        <Text className={cn("text-base leading-snug", "font-t3-bold")}>
                          {scope.title}
                        </Text>
                        <Text
                          className="text-xs leading-snug text-foreground-muted"
                          ellipsizeMode="middle"
                          numberOfLines={1}
                        >
                          {hasMultipleProjects
                            ? `${scope.projects.length} workspaces`
                            : selectionTarget.workspaceRoot}
                        </Text>
                      </View>
                      <SymbolView
                        name="chevron.right"
                        size={14}
                        tintColorClassName="accent-chevron"
                        type="monochrome"
                      />
                    </Pressable>
                  </View>
                );
              })}
            </View>
          )}
        </ScrollView>
      </ScreenContent>
    </View>
  );
}
