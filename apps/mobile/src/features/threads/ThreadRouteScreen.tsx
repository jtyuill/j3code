import * as Dialog from "../../components/ui/Dialog";
import { useRememberThreadProject } from "./useRememberThreadProject";
import {
  StackActions,
  useNavigation,
  useRoute,
  type StaticScreenProps,
} from "@react-navigation/native";
import { useEffect, useEffectEvent, useState } from "react";
import * as Option from "effect/Option";
import { AuthOrchestrationOperateScope, EnvironmentId, MessageId, ThreadId } from "@t3tools/contracts";
import { ScreenScrollView as ScrollView } from "../../components/ui/ScreenScrollView";
import { useConnectionsReady } from "../../state/workspace";
import { useEnvironmentShellReadiness } from "../../state/shell";
import { restoredNewTaskDraftKey } from "../../state/new-task-draft-key";
import { clearPendingThreadCreationOutcome } from "../../state/pending-thread-creation";
import { recoverFailedThreadDraft } from "../../state/recover-failed-thread-draft";
import { EmptyState } from "../../components/ui/EmptyState";
import { LoadingScreen } from "../../components/LoadingScreen";
import { scopedThreadKey } from "../../lib/scopedEntities";
import { scopeThreadRef } from "@t3tools/client-runtime/environment";
import { useThreadShell } from "../../state/entities";
import { NATIVE_LIQUID_GLASS_SUPPORTED } from "../../native/native-glass";
import { connectionTone } from "../connection/connectionTone";
import {
  useRemoteConnections,
  useRemoteConnectionStatus,
  useRemoteEnvironmentRuntime,
} from "../../state/use-remote-environment-registry";
import { uuidv4 } from "../../lib/uuid";
import {
  isAtomCommandInterrupted,
  squashAtomCommandFailure,
} from "@t3tools/client-runtime/state/runtime";
import { readEnvironmentScope, useEnvironmentScope } from "../../state/session";
import { useSelectedThreadDetailState } from "../../state/use-thread-detail";
import { useThreadSelection } from "../../state/use-thread-selection";
import type { ThreadDetailScreenProps } from "./ThreadDetailScreen";
import { ThreadScreenBinding, ThreadScreenFallback, ThreadScreenHost } from "./ThreadScreenHost";
import { draftRouteIdentity } from "./thread-route-state";
import { NewTaskDraftRouteScreen, type NewTaskDraftRouteParams } from "./NewTaskDraftRouteScreen";
import { useAtomCommand } from "../../state/use-atom-command";
import { useSelectedThreadRequests } from "../../state/use-selected-thread-requests";
import { useThreadComposerState } from "../../state/use-thread-composer-state";
import { threadEnvironment } from "../../state/threads";
import { EMPTY_ENVIRONMENT_THREAD_STATE } from "@t3tools/client-runtime/state/threads";
import { useProjects } from "../../state/entities";
import { projectThreadContentPresentation } from "./threadContentPresentation";
import { useAppearancePreferences } from "../settings/appearance/AppearancePreferencesProvider";
import { useAdaptiveWorkspaceLayout } from "../layout/AdaptiveWorkspaceLayout";
import {
  threadDetailErrorIsCurrent,
  threadRouteIsHydrating,
  threadRouteOpensDraft,
} from "./thread-route-hydration";
import { ThreadHeader } from "./ThreadHeader";
import { resumeThread } from "./resume-thread";
import { openSettings } from "../settings/settings-navigation";

function firstRouteParam(value: string | string[] | undefined): string | null {
  const first = Array.isArray(value) ? value[0] : value;
  return first === undefined || first.trim().length === 0 ? null : first;
}

function OpeningThreadLoadingScreen() {
  return (
    <ThreadScreenFallback>
      <LoadingScreen message="Opening thread…" messagePlacement="above-spinner" />
    </ThreadScreenFallback>
  );
}

type ThreadRouteScreenProps = StaticScreenProps<{
  readonly environmentId?: string;
  readonly threadId?: string;
  readonly draft?: NewTaskDraftRouteParams;
  readonly prompt?: string | string[];
}>;

function ThreadUnavailableScreen(props: {
  readonly actionLabel: string;
  readonly onAction: () => void;
}) {
  return (
    <ThreadScreenFallback>
      <ScrollView
        contentInsetAdjustmentBehavior="automatic"
        contentContainerStyle={{
          flexGrow: 1,
          justifyContent: "center",
          paddingHorizontal: 24,
          paddingVertical: 32,
        }}
        className="bg-screen flex-1"
      >
        <EmptyState
          title="Thread unavailable"
          detail="This thread is not available in the current mobile snapshot."
          actionLabel={props.actionLabel}
          onAction={props.onAction}
        />
      </ScrollView>
    </ThreadScreenFallback>
  );
}

/** A thread that can't load opens a new task in its environment instead of a dead end. */
function UnavailableThreadDraftRedirect(props: { readonly environmentId: EnvironmentId }) {
  const navigation = useNavigation();
  const projects = useProjects();
  const openDraft = useEffectEvent(() => {
    const project = projects.find((candidate) => candidate.environmentId === props.environmentId);
    navigation.dispatch(
      StackActions.replace("Thread", {
        draft: project
          ? {
              requestId: uuidv4(),
              environmentId: String(project.environmentId),
              projectId: String(project.id),
            }
          : { requestId: uuidv4() },
      }),
    );
  });
  // oxlint-disable-next-line t3code/no-unreasoned-state-sync -- replaces this route with a new draft thread route for the environment's project on mount
  useEffect(() => {
    openDraft();
  }, []);
  return <OpeningThreadLoadingScreen />;
}

export function ThreadRouteScreen(props: ThreadRouteScreenProps) {
  return (
    <ThreadScreenHost>
      {props.route.params?.draft ? (
        <NewTaskDraftRouteScreen
          key={draftRouteIdentity(props.route.params.draft)}
          route={{ ...props.route, params: props.route.params.draft }}
        />
      ) : (
        <LiveThreadRouteController {...props} />
      )}
    </ThreadScreenHost>
  );
}

function LiveThreadRouteController(props: ThreadRouteScreenProps) {
  const connectionsReady = useConnectionsReady();
  const { connectionState } = useRemoteConnectionStatus();
  const { selectedThread } = useThreadSelection();
  const params = props.route.params ?? {};
  const environmentIdRaw = firstRouteParam(params.environmentId);
  const threadIdRaw = firstRouteParam(params.threadId);
  const environmentId = environmentIdRaw ? EnvironmentId.make(environmentIdRaw) : null;
  const routeEnvironmentRuntime = useRemoteEnvironmentRuntime(environmentId);
  const routeEnvironmentShellState = useEnvironmentShellReadiness(environmentId);
  const { onReconnectEnvironment } = useRemoteConnections();
  const navigation = useNavigation();
  const routeName = useRoute().name;
  const routeConnectionState =
    routeEnvironmentRuntime?.connectionState ?? (environmentId ? "available" : connectionState);
  const routeThreadKey =
    environmentId !== null && threadIdRaw !== null
      ? scopedThreadKey(environmentId, ThreadId.make(threadIdRaw))
      : null;
  const selectedThreadKey =
    selectedThread === null
      ? null
      : scopedThreadKey(selectedThread.environmentId, selectedThread.id);
  const selectedThreadDetailState = useSelectedThreadDetailState();
  const detailError = selectedThreadDetailState.error;
  const [errorBeforeConnected, setErrorBeforeConnected] = useState(() =>
    routeConnectionState === "connected" ? Option.none<string>() : detailError,
  );
  if (routeConnectionState !== "connected" && errorBeforeConnected !== detailError) {
    setErrorBeforeConnected(detailError);
  }

  if (environmentId === null || threadIdRaw === null) {
    return <OpeningThreadLoadingScreen />;
  }

  if (selectedThread !== null && selectedThreadKey === routeThreadKey) {
    return <ThreadRouteContent {...props} selectedThreadDetailState={selectedThreadDetailState} />;
  }

  const stillHydrating = threadRouteIsHydrating({
    isLoadingConnections: !connectionsReady,
    environmentRegistered: routeEnvironmentRuntime !== null,
    localCacheRead:
      routeEnvironmentShellState.cacheRead &&
      selectedThreadDetailState !== EMPTY_ENVIRONMENT_THREAD_STATE,
    connectionState: routeConnectionState,
    shellStatus: routeEnvironmentShellState.status,
    shellHasError: routeEnvironmentShellState.hasError,
    detailStatus: selectedThreadDetailState.status,
    detailHasError: threadDetailErrorIsCurrent({
      connectionState: routeConnectionState,
      error: detailError,
      errorBeforeConnected,
    }),
  });

  if (stillHydrating) {
    return <OpeningThreadLoadingScreen />;
  }

  if (
    routeName === "Thread" &&
    threadRouteOpensDraft({
      connectionState: routeConnectionState,
      detailStatus: selectedThreadDetailState.status,
    })
  ) {
    return <UnavailableThreadDraftRedirect environmentId={environmentId} />;
  }

  return (
    <ThreadUnavailableScreen
      actionLabel={
        routeEnvironmentRuntime === null ? "Manage environments" : "Reconnect environment"
      }
      onAction={() => {
        if (routeEnvironmentRuntime !== null) {
          onReconnectEnvironment(environmentId);
          return;
        }
        openSettings(navigation, "SettingsEnvironments");
      }}
    />
  );
}

function ThreadRouteContent(
  props: ThreadRouteScreenProps & {
    readonly selectedThreadDetailState: ReturnType<typeof useSelectedThreadDetailState>;
  },
) {
  const { themeVariables } = useAppearancePreferences();
  const headerColor = themeVariables["--color-header"];
  const { layout } = useAdaptiveWorkspaceLayout();
  const { connectionState } = useRemoteConnectionStatus();
  const { onReconnectEnvironment } = useRemoteConnections();
  const {
    selectedThread,
    selectedThreadCreation,
    selectedThreadDetailRef,
    selectedThreadProject,
    selectedEnvironmentConnection,
  } = useThreadSelection();
  useRememberThreadProject(selectedThreadProject);
  const canOperateThread = useEnvironmentScope(
    selectedThread?.environmentId ?? null,
    AuthOrchestrationOperateScope,
  );
  const selectedThreadDetailState = props.selectedThreadDetailState;
  const selectedThreadDetail = Option.getOrNull(selectedThreadDetailState.data);
  const composer = useThreadComposerState();
  const requests = useSelectedThreadRequests();
  const interruptThreadTurn = useAtomCommand(threadEnvironment.interruptTurn, "thread interrupt");
  const resumeThreadQueue = useAtomCommand(threadEnvironment.resumeThreadQueue, {
    label: "resume queue",
    reportFailure: false,
  });
  const continueThreadRun = useAtomCommand(threadEnvironment.startTurn, {
    label: "continue thread",
    reportFailure: false,
  });
  const setThreadRuntimeMode = useAtomCommand(threadEnvironment.setRuntimeMode, {
    label: "set thread runtime mode",
    reportFailure: false,
  });
  const setThreadInteractionMode = useAtomCommand(threadEnvironment.setInteractionMode, {
    label: "set thread interaction mode",
    reportFailure: false,
  });
  const loadEarlierHistory = useAtomCommand(threadEnvironment.loadEarlierHistory, {
    label: "load earlier thread history",
    reportFailure: false,
  });
  const historyControls = (() => {
    const history = selectedThreadDetailState.history;
    if (!selectedThread) {
      return undefined;
    }
    if (!history.hasMoreHistory && history.error === null) {
      return undefined;
    }
    return {
      hasMoreHistory: history.hasMoreHistory,
      loading: history.loading,
      error: history.error,
      onLoadEarlier: () => {
        void loadEarlierHistory({
          environmentId: selectedThread.environmentId,
          input: { threadId: selectedThread.id },
        });
      },
      onLoadThrough: (throughEntryId: string) => {
        void loadEarlierHistory({
          environmentId: selectedThread.environmentId,
          input: { threadId: selectedThread.id, throughEntryId },
        });
      },
    };
  })();
  const navigation = useNavigation();
  const params = props.route.params ?? {};
  const environmentIdRaw = firstRouteParam(params.environmentId);
  const environmentId = environmentIdRaw ? EnvironmentId.make(environmentIdRaw) : null;
  const threadId = firstRouteParam(params.threadId);
  const routeThreadIdentity =
    environmentIdRaw !== null && threadId !== null ? `${environmentIdRaw}:${threadId}` : null;
  const routeEnvironmentRuntime = useRemoteEnvironmentRuntime(environmentId);
  const routeConnectionState =
    routeEnvironmentRuntime?.connectionState ?? (environmentId ? "available" : connectionState);
  const routeConnectionError = routeEnvironmentRuntime?.connectionError ?? null;
  const selectedThreadWithDraftSettings = selectedThread
    ? {
        ...selectedThread,
        modelSelection: composer.modelSelection ?? selectedThread.modelSelection,
        runtimeMode: composer.runtimeMode ?? selectedThread.runtimeMode,
        interactionMode: composer.interactionMode ?? selectedThread.interactionMode,
      }
    : null;

  const usesNativeHeaderGlass = NATIVE_LIQUID_GLASS_SUPPORTED;
  const delegatedFrom = selectedThread?.delegatedFrom ?? null;
  const delegatedFromRef =
    delegatedFrom === null
      ? null
      : scopeThreadRef(delegatedFrom.environmentId, delegatedFrom.threadId);
  // The title it had at delegation, until this client sees the parent itself.
  const delegatedFromThread = useThreadShell(delegatedFromRef);
  const headerSubtitle = [
    selectedThreadProject?.title ?? null,
    selectedEnvironmentConnection?.environmentLabel ?? null,
    // A linked environment's delegated task names its parent; other linked work its origin.
    delegatedFrom !== null
      ? `Subagent of ${delegatedFromThread?.title ?? delegatedFrom.title}`
      : selectedThread?.linkOrigin
        ? `From ${selectedThread.linkOrigin.label}`
        : null,
  ]
    .filter(Boolean)
    .join(" · ");
  const handleReconnectEnvironment = () => {
    if (!environmentId) {
      return;
    }
    onReconnectEnvironment(environmentId);
  };

  const handleOpenConnectionEditor = () => {
    void navigation.navigate("Connections");
  };
  const handleStopThread = () => {
    if (
      !selectedThread ||
      !readEnvironmentScope(selectedThread.environmentId, AuthOrchestrationOperateScope) ||
      composer.interruptibleRunId === null
    ) {
      return;
    }
    return interruptThreadTurn({
      environmentId: selectedThread.environmentId,
      input: {
        threadId: selectedThread.id,
        runId: composer.interruptibleRunId,
      },
    });
  };

  const handleResumeQueue = async () => {
    if (!selectedThread) return;
    const result = await resumeThread({
      thread: selectedThread,
      resumableRunId: composer.resumableRunId,
      queueHeld: composer.queueHeld,
      runtimeMode: composer.runtimeMode ?? selectedThread.runtimeMode,
      interactionMode: composer.interactionMode ?? selectedThread.interactionMode,
      modelSelection: composer.modelSelection,
      messageId: MessageId.make(uuidv4()),
      createdAt: new Date().toISOString(),
      setRuntimeMode: setThreadRuntimeMode,
      setInteractionMode: setThreadInteractionMode,
      startTurn: continueThreadRun,
      resumeQueue: resumeThreadQueue,
    });
    if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
      const error = squashAtomCommandFailure(result);
      Dialog.alert(
        "Could not resume thread",
        error instanceof Error ? error.message : "Try again.",
      );
    }
  };

  const handleEditFailedCreation = async () => {
    const creation = selectedThreadCreation?.message;
    if (!creation?.creation || routeThreadIdentity === null) {
      return;
    }
    // Open recovery by draft id or the sheet creates an empty draft and strands restored content.
    try {
      await recoverFailedThreadDraft(creation);
    } catch (error) {
      Dialog.alert(
        "Could not restore draft",
        error instanceof Error ? error.message : String(error),
      );
      return;
    }
    clearPendingThreadCreationOutcome(routeThreadIdentity);
    navigation.dispatch(
      StackActions.replace("Thread", {
        draft: {
          draftId: restoredNewTaskDraftKey(creation.messageId),
          environmentId: String(creation.environmentId),
          projectId: String(creation.creation.projectId),
          ...(selectedThreadProject ? { title: selectedThreadProject.title } : {}),
        },
      }),
    );
  };
  const awaitingBootstrapTurn =
    selectedThreadDetail?.runs.some((run) => run.status === "preparing") ?? false;
  const creationState = ((): ThreadDetailScreenProps["creationState"] => {
    if (selectedThreadCreation === null) {
      return awaitingBootstrapTurn ? { kind: "preparing", serverOwned: true } : null;
    }
    if (selectedThreadCreation.outcome?.kind === "failed") {
      return {
        kind: "failed",
        reason: selectedThreadCreation.outcome.reason,
        onEditTask: handleEditFailedCreation,
      };
    }
    return { kind: "preparing", serverOwned: false };
  })();
  if (!environmentId || !threadId) {
    return <OpeningThreadLoadingScreen />;
  }

  if (!selectedThread) {
    return <OpeningThreadLoadingScreen />;
  }

  // Queued creation displays its prompt before server creation; temporary not-found is expected.
  const contentPresentation =
    creationState !== null
      ? { kind: "ready" as const }
      : projectThreadContentPresentation({
          hasDetail: selectedThreadDetail !== null,
          detailError: Option.getOrNull(selectedThreadDetailState.error),
          detailDeleted: selectedThreadDetailState.status === "deleted",
          connectionState: routeConnectionState,
        });
  const serverConfig = routeEnvironmentRuntime?.serverConfig ?? null;
  return (
    <ThreadScreenBinding
      header={
        <ThreadHeader
          title={selectedThread.title}
          subtitle={headerSubtitle}
          headerColor={headerColor}
          usesNativeHeaderGlass={usesNativeHeaderGlass}
        />
      }
      canOperateThread={canOperateThread}
      selectedThread={selectedThreadWithDraftSettings ?? selectedThread}
      selectedThreadDetailRef={selectedThreadDetailRef}
      contentPresentation={contentPresentation}
      screenTone={connectionTone(routeConnectionState)}
      connectionError={routeConnectionError}
      environmentLabel={selectedEnvironmentConnection?.environmentLabel ?? null}
      selectedThreadFeed={composer.selectedThreadFeed}
      activityRun={composer.selectedThreadActivityRun}
      activeWorkStartedAt={
        creationState?.kind === "preparing" ? null : composer.activeWorkStartedAt
      }
      isCompacting={composer.isCompacting}
      runlessWorkActive={composer.runlessWorkActive}
      providerSubagentStatus={composer.providerSubagentStatus}
      creationState={creationState}
      activePendingApproval={requests.activePendingApproval}
      activePendingApprovals={requests.activePendingApprovals}
      respondingApprovalId={requests.respondingApprovalId}
      activePendingUserInput={requests.activePendingUserInput}
      activePendingUserInputDrafts={requests.activePendingUserInputDrafts}
      activePendingUserInputAnswers={requests.activePendingUserInputAnswers}
      respondingUserInputId={requests.respondingUserInputId}
      connectionStateLabel={routeConnectionState}
      threadSyncStatus={selectedThreadDetailState.status}
      historyControls={historyControls}
      activeThreadBusy={composer.activeThreadBusy}
      canStopThread={composer.interruptibleRunId !== null}
      queuedRunEdit={composer.queuedRunEdit}
      composerDraftKey={composer.composerDraftKey}
      followUpBehavior={composer.followUpBehavior}
      canSteerActiveTurn={composer.canSteerActiveTurn}
      isSavingQueuedEdit={composer.isSavingQueuedEdit}
      onCancelQueuedRunEdit={composer.cancelQueuedRunEdit}
      onRemoveQueuedEditAttachment={composer.onRemoveQueuedEditAttachment}
      environmentId={selectedThread.environmentId}
      projectWorkspaceRoot={selectedThreadProject?.workspaceRoot ?? null}
      selectedThreadQueueCount={composer.selectedThreadQueueCount}
      queuedMessages={
        selectedThreadCreation && selectedThreadCreation.outcome?.kind !== "delivered"
          ? [selectedThreadCreation.message, ...composer.selectedThreadQueuedMessages]
          : composer.selectedThreadQueuedMessages
      }
      dispatchingMessageId={composer.dispatchingQueuedMessageId}
      layoutVariant={layout.variant}
      usesAutomaticContentInsets={usesNativeHeaderGlass}
      onOpenConnectionEditor={handleOpenConnectionEditor}
      onChangeDraftMessage={composer.onChangeDraftMessage}
      onPickDraftMedia={composer.onPickDraftMedia}
      onNativePasteImages={composer.onNativePasteImages}
      onRemoveDraftImage={composer.onRemoveDraftImage}
      serverConfig={serverConfig}
      onStopThread={handleStopThread}
      onResumeQueue={
        composer.queueHeld || composer.resumableRunId !== null ? handleResumeQueue : null
      }
      onSendMessage={composer.onSendMessage}
      onReconnectEnvironment={handleReconnectEnvironment}
      onUpdateThreadInteractionMode={composer.onUpdateInteractionMode}
      onRespondToApproval={requests.onRespondToApproval}
      onSelectUserInputOption={requests.onSelectUserInputOption}
      onChangeUserInputCustomAnswer={requests.onChangeUserInputCustomAnswer}
      onSubmitUserInput={requests.onSubmitUserInput}
      onDismissUserInput={requests.onDismissUserInput}
    />
  );
}
