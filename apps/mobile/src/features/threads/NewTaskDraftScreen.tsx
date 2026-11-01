import { DraftEnvironmentControl } from "./DraftEnvironmentControl";
import { NativeScrollEdgeEffectsContext } from "../../native/NativeScrollViewMarker";
import { DraftThreadHeader } from "./DraftThreadHeader";
import { threadHeaderAppearance } from "./thread-header-appearance";
import { NATIVE_LIQUID_GLASS_SUPPORTED } from "../../native/native-glass";
import { useComposerAttachmentActions } from "../../state/use-composer-attachment-actions";
import { submittedThreadParams } from "./thread-route-state";
import { ThreadScreenBinding, ThreadScreenFallback } from "./ThreadScreenHost";
import { draftThreadShell } from "./draft-thread-shell";
import { useRememberThreadProject } from "./useRememberThreadProject";
import { scopedProjectKey } from "../../lib/scopedEntities";
import { connectionTone } from "../connection/connectionTone";
import { AppBar } from "../../components/ui/AppBar";
import { ThreadId } from "@t3tools/contracts";

import * as Dialog from "../../components/ui/Dialog";
import { useAtomValue } from "@effect/atom-react";
import { AsyncResult } from "effect/reactivity";
import { settlePromise, squashAtomCommandFailure } from "@t3tools/client-runtime/state/runtime";
import {
  CommonActions,
  StackActions,
  useIsFocused,
  useNavigation,
  usePreventRemove,
} from "@react-navigation/native";
import { useEffect, useRef, useState } from "react";
import { Pressable, View } from "react-native";

import { ScreenScrollView as ScrollView } from "../../components/ui/ScreenScrollView";
import { KeyboardController } from "react-native-keyboard-controller";

import {
  AuthOrchestrationOperateScope,
  PROVIDER_SEND_TURN_MAX_ATTACHMENTS,
  resolveEnvironmentMachineKind,
} from "@t3tools/contracts";

import { ComposerEditor, type ComposerEditorHandle } from "../../components/ComposerEditor";
import { composerContextImportsAtom } from "../../state/use-composer-drafts";
import { abortComposerDraftOperations } from "../../state/composerDraftOperations";
import { composerContextSendBlockReason } from "../../lib/composerContext";
import { ComposerInlineControl } from "../../components/ComposerToolbar";

import {
  composerAttachmentUploadBlockReason,
  composerAttachmentUploadsAtom,
} from "../../state/composer-attachment-uploads";

import { AppText as Text } from "../../components/ui/AppText";
import { threadContextsOutsideEnvironment } from "@t3tools/shared/composerContextReferences";

import { makeTurnCommandMetadata } from "../../lib/commandMetadata";

import {
  clearComposerDraftContent,
  getComposerDraftSnapshot,
  scheduleUnusedComposerAttachmentCleanup,
} from "../../state/use-composer-drafts";
import { useEnvironmentServerConfig, useProjects } from "../../state/entities";
import {
  describeUnavailableModelSelection,
  isModelSelectionUnavailable,
  resolveSelectableModelSelection,
} from "../../lib/modelOptions";
import { enqueueThreadOutboxMessage } from "../../state/thread-outbox";
import {
  useRemoteConnections,
  useRemoteEnvironmentRuntime,
} from "../../state/use-remote-environment-registry";
import { useNewTaskFlow } from "./new-task-flow-provider";
import {
  nextEnvironmentId,
  useHardwareKeyboardCommand,
} from "../keyboard/hardwareKeyboardCommands";
import { resolveDraftProjectSelection } from "./new-task-project-selection";
import { appAtomRegistry } from "../../state/atom-registry";

import { readEnvironmentScope, useEnvironmentScope } from "../../state/session";

export function NewTaskDraftScreen(props: {
  readonly initialProjectRef?: {
    readonly environmentId?: string;
    readonly projectId?: string;
  };
  readonly draftId?: string;
}) {
  const [creationMetadata] = useState(makeTurnCommandMetadata);
  const projects = useProjects();
  const flow = useNewTaskFlow();
  const navigation = useNavigation();
  const isFocused = useIsFocused();
  const goBack = () => {
    if (navigation.canGoBack()) navigation.goBack();
    else navigation.dispatch(StackActions.replace("Home"));
  };
  const { projectScopes, selectedProject, selectedProjectKey, setProject } = flow;
  // selectedProject falls back to the environment's first project; remember only a chosen one.
  useRememberThreadProject(
    selectedProject &&
      selectedProjectKey === scopedProjectKey(selectedProject.environmentId, selectedProject.id)
      ? selectedProject
      : null,
  );
  const selectedProjectLabel = (() => {
    if (!selectedProject) return null;
    const scope = projectScopes.find((candidate) =>
      candidate.projectRefs.some(
        (ref) =>
          ref.environmentId === selectedProject.environmentId &&
          ref.projectId === selectedProject.id,
      ),
    );
    return scope?.title ?? selectedProject.title;
  })();
  const { onReconnectEnvironment } = useRemoteConnections();
  const environmentRuntime = useRemoteEnvironmentRuntime(selectedProject?.environmentId ?? null);
  const connectionState = environmentRuntime?.connectionState ?? "offline";
  const selectedEnvironmentServerConfig = useEnvironmentServerConfig(
    selectedProject?.environmentId ?? null,
  );
  const environmentConnected = connectionState === "connected";
  const canOperate = useEnvironmentScope(
    selectedProject?.environmentId ?? null,
    AuthOrchestrationOperateScope,
  );
  const uploadStates = useAtomValue(composerAttachmentUploadsAtom);
  const attachmentBlockReason = selectedProject
    ? composerAttachmentUploadBlockReason({
        environmentId: selectedProject.environmentId,
        attachments: flow.attachments,
        connected: environmentConnected,
        serverConfig: selectedEnvironmentServerConfig,
        states: uploadStates,
      })
    : null;
  const promptInputRef = useRef<ComposerEditorHandle>(null);
  const [isReturningToProjectPicker, setIsReturningToProjectPicker] = useState(false);
  const requestedInitialProjectAvailable = Boolean(
    props.initialProjectRef?.environmentId &&
    props.initialProjectRef.projectId &&
    projects.some(
      (project) =>
        project.environmentId === props.initialProjectRef?.environmentId &&
        project.id === props.initialProjectRef?.projectId,
    ),
  );
  // A route naming a draft that is gone, or whose project is gone, also
  // returns to the picker.
  const isRouteDraftMissing =
    props.draftId !== undefined && flow.missingRouteDraftId === props.draftId;
  const isProjectPickerReturnActive =
    (isReturningToProjectPicker && !requestedInitialProjectAvailable) || isRouteDraftMissing;
  const contextImports = useAtomValue(composerContextImportsAtom);
  const isImportingContext = flow.draftKey ? contextImports[flow.draftKey] === true : false;
  const isComposerInteractionLocked = flow.submitting || isImportingContext;
  const { environments, selectedEnvironmentId, switchEnvironment, switchingToEnvironmentId } = flow;
  const cycleEnvironment = () => {
    if (isComposerInteractionLocked) return true;
    const next = nextEnvironmentId(environments, switchingToEnvironmentId ?? selectedEnvironmentId);
    if (next !== null) void switchEnvironment(next);
    return true;
  };
  const cycleEnvironmentCommands = environments.length > 1 ? (["cycleHost"] as const) : [];
  useHardwareKeyboardCommand(cycleEnvironmentCommands, cycleEnvironment);
  usePreventRemove(flow.submitting, () => undefined);
  const appliedInitialProjectKeyRef = useRef<string | null>(null);
  // oxlint-disable-next-line t3code/no-unreasoned-state-sync -- ends the picker return once the project arrives, else replaces the route with the picker next frame
  useEffect(() => {
    if (isReturningToProjectPicker && requestedInitialProjectAvailable) {
      // oxlint-disable-next-line react/set-state-in-effect -- Ends the return-to-picker flow once the requested project arrives from the catalog.
      setIsReturningToProjectPicker(false);
    }
    if (!isProjectPickerReturnActive) {
      return;
    }
    // Wait for usePreventRemove to disable before replacing, or the transfer guard swallows the fallback.
    const frame = requestAnimationFrame(() => {
      navigation.dispatch(StackActions.replace("NewTaskProject"));
    });
    return () => cancelAnimationFrame(frame);
  }, [
    isProjectPickerReturnActive,
    isReturningToProjectPicker,
    navigation,
    requestedInitialProjectAvailable,
  ]);

  const lastInitialProjectRefRef = useRef(props.initialProjectRef);
  const initialPickerRef = useRef<"open" | "dismissed" | null>(null);

  // oxlint-disable-next-line t3code/no-unreasoned-state-sync -- applies the route or catalog project to the draft once projects load, or opens the picker in the navigator
  useEffect(() => {
    if (props.draftId) {
      return;
    }
    if (lastInitialProjectRefRef.current !== props.initialProjectRef) {
      lastInitialProjectRefRef.current = props.initialProjectRef;
      appliedInitialProjectKeyRef.current = null;
    }
    const initialEnvironmentId = props.initialProjectRef?.environmentId;
    const initialProjectId = props.initialProjectRef?.projectId;
    if (initialEnvironmentId && initialProjectId) {
      const directProject =
        projects.find(
          (project) =>
            project.environmentId === initialEnvironmentId && project.id === initialProjectId,
        ) ?? null;

      if (directProject) {
        // Apply the route project once or picker changes would immediately revert.
        const directProjectKey = `${directProject.environmentId}:${directProject.id}`;
        if (appliedInitialProjectKeyRef.current === directProjectKey) {
          return;
        }
        appliedInitialProjectKeyRef.current = directProjectKey;
        if (
          selectedProject?.environmentId === directProject.environmentId &&
          selectedProject.id === directProject.id
        ) {
          return;
        }
        setProject(directProject);
        return;
      }

      if (projects.length > 0) {
        // oxlint-disable-next-line react/set-state-in-effect -- Route-driven project resolution starts the return-to-picker flow when no project matches.
        setIsReturningToProjectPicker(true);
      }
      return;
    }

    const selection = resolveDraftProjectSelection(selectedProjectKey, projects, {
      pickerClosed: initialPickerRef.current === "open" && isFocused,
    });
    if (selection.kind === "preserve") {
      initialPickerRef.current = null;
      return;
    }
    if (selection.kind === "dismiss") {
      initialPickerRef.current = "dismissed";
      goBack();
      return;
    }
    // Open the first picker once; catalog updates must not push it again over a cancelled draft.
    if (initialPickerRef.current !== null) {
      return;
    }
    initialPickerRef.current = "open";
    navigation.dispatch(CommonActions.navigate("NewTaskProject"));
  }, [
    goBack,
    isFocused,
    projects,
    props.initialProjectRef,
    props.draftId,
    navigation,
    selectedProject,
    selectedProjectKey,
    setProject,
  ]);

  const selectedEnvironmentLabel =
    flow.environments.find(
      (environment) => environment.environmentId === flow.selectedEnvironmentId,
    )?.environmentLabel ?? "Environment";
  const { onPickDraftMedia: handlePickMedia, onNativePasteImages: handleNativePasteImages } =
    useComposerAttachmentActions({
      draftKey: flow.draftKey,
      serverConfig: selectedEnvironmentServerConfig,
      locked: isComposerInteractionLocked,
    });

  async function handleStart(): Promise<import("@t3tools/contracts").MessageId | null> {
    const selectedProject = flow.selectedProject;
    const draftKey = flow.draftKey;
    if (!selectedProject || !draftKey || isComposerInteractionLocked) {
      return null;
    }
    if (
      environmentConnected &&
      !readEnvironmentScope(selectedProject.environmentId, AuthOrchestrationOperateScope)
    ) {
      return null;
    }
    const draft = getComposerDraftSnapshot(draftKey);
    if (appAtomRegistry.get(composerContextImportsAtom)[draftKey]) return null;
    const modelSelection =
      resolveSelectableModelSelection(
        selectedEnvironmentServerConfig,
        draft.modelSelection ?? null,
        selectedProject.id,
      ) ?? flow.selectedModel;
    const initialMessageText = draft.text.trim();

    if (
      attachmentBlockReason !== null ||
      !modelSelection ||
      (initialMessageText.length === 0 && draft.attachments.length === 0) ||
      flow.submitting
    ) {
      return null;
    }
    if (
      environmentConnected &&
      isModelSelectionUnavailable(
        selectedEnvironmentServerConfig,
        modelSelection,
        selectedProject.id,
      )
    ) {
      const { title, detail } = describeUnavailableModelSelection(
        selectedEnvironmentServerConfig,
        modelSelection,
        selectedProject.id,
      );
      Dialog.alert(title, detail);
      return null;
    }
    // Recovery may retain over-cap attachments; block send before uploading a turn the server would reject.
    if (draft.attachments.length > PROVIDER_SEND_TURN_MAX_ATTACHMENTS) {
      Dialog.alert(
        "Too many attachments",
        `Remove attachments until there are at most ${PROVIDER_SEND_TURN_MAX_ATTACHMENTS}.`,
      );
      return null;
    }

    const contextBlockReason = composerContextSendBlockReason(draft.context);
    if (contextBlockReason) {
      Dialog.alert("Too much context", contextBlockReason);
      return null;
    }
    const strandedThread = threadContextsOutsideEnvironment({
      text: draft.text,
      records: draft.context?.records ?? [],
      environmentId: selectedProject.environmentId,
      linkedEnvironments: new Map(),
    })[0];
    if (strandedThread) {
      const owner =
        flow.environments.find(
          (environment) => environment.environmentId === strandedThread.environmentId,
        )?.environmentLabel ?? "its machine";
      Dialog.alert(
        "Attached thread unavailable",
        `"${strandedThread.title}" is on another machine. Switch back to ${owner} or remove it from the draft.`,
      );
      return null;
    }

    const message = flow.buildPendingTaskMessage(creationMetadata);
    if (!message) {
      return null;
    }
    // Persist locally before clearing or leaving the draft; server setup runs on the thread.
    flow.setSubmitting(true);
    const queued = await settlePromise(() => enqueueThreadOutboxMessage(message));
    flow.setSubmitting(false);
    if (AsyncResult.isFailure(queued)) {
      const error = squashAtomCommandFailure(queued);
      Dialog.alert(
        "Could not queue task",
        error instanceof Error ? error.message : "The task could not be saved to the outbox.",
      );
      return null;
    }
    promptInputRef.current?.blur();
    navigation.dispatch(
      CommonActions.setParams(
        submittedThreadParams(String(message.environmentId), String(message.threadId)),
      ),
    );
    // The composer has moved to the submitted thread; a pick still converting
    // would otherwise resurrect this draft.
    abortComposerDraftOperations(draftKey);
    const draftSnapshot = getComposerDraftSnapshot(draftKey);
    // The queued message owns the attachments now, so the sweep is deferred
    // until the write confirms it.
    clearComposerDraftContent(draftKey, {
      clearModelSelection: true,
      deferAttachmentCleanup: true,
    });
    scheduleUnusedComposerAttachmentCleanup(draftSnapshot.attachments);
    return message.messageId;
  }

  // Keep the current surface until the local draft selection is ready. Publishing
  // an empty loading surface here flashes between two already-cached threads.
  if (!selectedProject) return null;

  const setupBlockReason = flow.submitting ? "Starting the thread…" : null;
  const chooseProject = () => {
    if (isComposerInteractionLocked) {
      return;
    }
    promptInputRef.current?.blur();
    void KeyboardController.dismiss({ animated: true });
    navigation.navigate("NewTaskProject");
  };
  const openEnvironmentPicker = () => {
    if (isComposerInteractionLocked) {
      return;
    }
    promptInputRef.current?.blur();
    void KeyboardController.dismiss({ animated: true });
    navigation.navigate("NewTaskEnvironment");
  };

  const environmentControl = (
    <DraftEnvironmentControl
      connectionState={connectionState}
      label={selectedEnvironmentLabel}
      machineKind={resolveEnvironmentMachineKind(selectedEnvironmentServerConfig)}
      disabled={isComposerInteractionLocked}
      onChooseEnvironment={openEnvironmentPicker}
    />
  );
  const hero = flow.isScratchDraft ? (
    <View className="items-center gap-2 px-6" testID="new-task-hero">
      <Text className="text-center text-2xl font-t3-medium tracking-tight text-foreground">
        What should we work on?
      </Text>
      <View className="flex-row flex-wrap items-center justify-center gap-x-1">
        <ComposerInlineControl
          accessibilityHint="Opens the project picker"
          accessibilityLabel="No project"
          chevronDirection="right"
          disabled={isComposerInteractionLocked}
          icon="folder"
          label="No project"
          onPress={chooseProject}
        />
        {environmentControl}
      </View>
    </View>
  ) : (
    <View className="items-center gap-6 px-6" testID="new-task-hero">
      <View className="w-full items-center gap-1.5">
        <Text className="text-center text-2xl font-t3-medium tracking-tight text-foreground">
          What should we build
        </Text>
        <View className="max-w-full flex-row items-center justify-center">
          <Text className="text-2xl font-t3-medium tracking-tight text-foreground">in </Text>
          <Pressable
            accessibilityHint="Opens the project picker"
            accessibilityLabel={selectedProjectLabel ?? selectedProject.title}
            accessibilityRole="button"
            disabled={isComposerInteractionLocked}
            onPress={chooseProject}
            className="min-w-0 max-w-[250px] border-b border-foreground-muted active:opacity-65"
          >
            <Text
              className="text-2xl font-t3-medium tracking-tight text-foreground"
              numberOfLines={1}
            >
              {selectedProjectLabel ?? selectedProject.title}
            </Text>
          </Pressable>
          <Text className="text-2xl font-t3-medium tracking-tight text-foreground">?</Text>
        </View>
      </View>

      {environmentControl}
    </View>
  );
  // The nested hero scroll view does not receive UIKit's automatic navigation inset in split columns.
  const heroViewport = (headerInset: number, bottomInset: number, availableHeight: number) =>
    availableHeight < 180 ? null : (
      <NativeScrollEdgeEffectsContext
        value={{ top: "hidden", bottom: "hidden", left: "hidden", right: "hidden" }}
      >
        <View
          collapsable={false}
          style={{
            position: "absolute",
            top: headerInset,
            bottom: bottomInset,
            left: 0,
            right: 0,
            overflow: "hidden",
          }}
        >
          <ScrollView
            alwaysBounceVertical={false}
            className="flex-1"
            contentInsetAdjustmentBehavior="never"
            contentContainerClassName="grow items-center"
            contentContainerStyle={{ paddingVertical: 24 }}
            keyboardDismissMode="interactive"
            keyboardShouldPersistTaps="handled"
            showsVerticalScrollIndicator={false}
            style={{ flex: 1 }}
            testID="new-task-hero-scroll"
          >
            <View style={{ flexGrow: 1 }} />
            {hero}
            <View style={{ flexGrow: 2 }} />
          </ScrollView>
        </View>
      </NativeScrollEdgeEffectsContext>
    );

  if (!flow.selectedModel) {
    return (
      <ThreadScreenFallback
        header={
          <AppBar
            title="New thread"
            sidebar={false}
            options={{
              ...threadHeaderAppearance(NATIVE_LIQUID_GLASS_SUPPORTED),
              unstable_headerSubtitle: undefined,
              unstable_headerRightItems: () => [],
              unstable_headerToolbarItems: () => [],
            }}
          />
        }
      >
        <View className="flex-1 justify-end gap-3 p-4">
          {environmentControl}
          <Text className="text-foreground-muted">
            Your draft is saved on this device. Connect to an environment to choose its first model.
          </Text>
          <ComposerEditor
            draftKey={flow.draftKey}
            value={flow.prompt}
            onChangeText={flow.setPrompt}
            placeholder="Ask anything"
            multiline
            style={{ minHeight: 64 }}
          />
        </View>
      </ThreadScreenFallback>
    );
  }
  const thread = draftThreadShell({
    environmentId: selectedProject.environmentId,
    projectId: selectedProject.id,
    threadId: ThreadId.make(creationMetadata.threadId),
    modelSelection: flow.selectedModel,
    runtimeMode: flow.runtimeMode,
    interactionMode: flow.interactionMode,
    createdAt: creationMetadata.createdAt,
  });
  return (
    <>
      <ThreadScreenBinding
        environmentId={selectedProject.environmentId}
        header={<DraftThreadHeader />}
        draftMode={{
          content: heroViewport,
          controls: null,
          editorRef: promptInputRef,
          sendBlockedReason: setupBlockReason,
          interactionLocked: isComposerInteractionLocked,
        }}
        canOperateThread={canOperate}
        selectedThread={thread}
        selectedThreadDetailRef={null}
        contentPresentation={{ kind: "ready" }}
        screenTone={connectionTone(connectionState)}
        connectionError={environmentRuntime?.connectionError ?? null}
        environmentLabel={selectedEnvironmentLabel}
        selectedThreadFeed={[]}
        activityRun={null}
        activeWorkStartedAt={null}
        isCompacting={false}
        creationState={null}
        activePendingApproval={null}
        activePendingApprovals={[]}
        respondingApprovalId={null}
        activePendingUserInput={null}
        activePendingUserInputDrafts={{}}
        activePendingUserInputAnswers={null}
        respondingUserInputId={null}
        connectionStateLabel={connectionState}
        activeThreadBusy={false}
        canStopThread={false}
        queuedRunEdit={null}
        composerDraftKey={flow.draftKey}
        followUpBehavior="queue"
        canSteerActiveTurn={false}
        isSavingQueuedEdit={false}
        onCancelQueuedRunEdit={() => {}}
        onRemoveQueuedEditAttachment={() => {}}
        projectWorkspaceRoot={selectedProject.workspaceRoot}
        selectedThreadQueueCount={0}
        queuedMessages={[]}
        dispatchingMessageId={null}
        serverConfig={selectedEnvironmentServerConfig}
        usesAutomaticContentInsets
        onOpenConnectionEditor={() => navigation.navigate("Connections")}
        onChangeDraftMessage={flow.setPrompt}
        onPickDraftMedia={handlePickMedia}
        onNativePasteImages={handleNativePasteImages}
        onRemoveDraftImage={flow.removeAttachment}
        onStopThread={() => {}}
        onResumeQueue={null}
        onSendMessage={handleStart}
        onReconnectEnvironment={() => onReconnectEnvironment(selectedProject.environmentId)}
        onUpdateThreadInteractionMode={flow.planModeEnabled ? flow.setInteractionMode : undefined}
        onRespondToApproval={async () => {}}
        onSelectUserInputOption={() => {}}
        onChangeUserInputCustomAnswer={() => {}}
        onSubmitUserInput={async () => {}}
        onDismissUserInput={async () => {}}
      />
    </>
  );
}
