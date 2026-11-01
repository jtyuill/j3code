import * as Dialog from "../../components/ui/Dialog";
import type { ThreadRowProviderInstance } from "./thread-provider-instance";
import {
  THREAD_LIST_V2_ROW_CONTENT_CLASS_NAME,
  THREAD_LIST_V2_ROW_DIVIDERS,
  selectedThreadRowColors,
  getThreadListV2RowAppearance,
} from "./thread-list-v2-row-appearance";
import { RowPressable } from "../../components/ui/RowPressable";
import { useVisibleSecondClock } from "./use-visible-second-clock";
import type { EnvironmentProject, EnvironmentThreadShell } from "@t3tools/client-runtime/state/shell";
import type { EnvironmentThreadSearchMatch } from "@t3tools/client-runtime/state/thread-search";
import {
  formatWorkingDurationLabel,
  resolveThreadWorkingStartedAt,
} from "@t3tools/client-runtime/state/models";
import { AuthOrchestrationOperateScope, type EnvironmentMachineKind } from "@t3tools/contracts";
import { threadChipCopyTarget } from "@t3tools/client-runtime/thread-reference";
import type { MenuAction } from "@react-native-menu/menu";
import { memo, useCallback, useMemo, type ComponentProps } from "react";
import { Pressable, useWindowDimensions, View } from "react-native";
import type { SwipeableMethods } from "react-native-gesture-handler/ReanimatedSwipeable";

import type { ThreadListProvider } from "../../state/thread-list-environments";
import { SymbolView, type AppSymbolName } from "../../components/ui/AppSymbol";
import { AppText as Text } from "../../components/ui/AppText";
import { Menu } from "../../components/ui/Menu";
import { EnvironmentMachineSymbol } from "../../components/EnvironmentMachineSymbol";
import { ProjectFavicon } from "../../components/ProjectFavicon";
import { ProviderIcon, ProviderInstanceIcon } from "../../components/ProviderIcon";
import { cn } from "../../lib/cn";
import { copyTextWithHaptic } from "../../lib/copyTextWithHaptic";
import { copyThreadReference } from "../../lib/copyThreadReference";
import { useUniwindTheme } from "../../lib/useUniwindTheme";
import { useEnvironmentScope } from "../../state/session";
import type { PendingNewTask } from "../../state/use-pending-new-tasks";
import { useSwipeRowDormant } from "../home/swipe-row-activation";
import { ThreadSwipeable } from "../home/thread-swipe-actions";
import { buildThreadTitleRegenerationMenuItems } from "./thread-title-regeneration-menu";
import {
  threadHasUnseenCompletion,
  resolveThreadListV2Status,
  resolveThreadListV2ProviderDrivers,
  type ThreadListV2Status,
} from "./threadListV2";
import { QueuedMessageIcon } from "./queued-message-icon";
import { shouldRecedeThreadRow } from "./thread-row-emphasis";
import { ThreadSearchMatchExcerpt } from "./thread-search-match";
import { useScaledTextRole } from "../settings/appearance/useScaledTextRole";

interface StatusLabel {
  readonly label: string;
  readonly icon: AppSymbolName;
  readonly className: string;
  readonly iconTintClassName: string;
}
const STATUS_LABEL_BY_STATUS: Partial<Record<ThreadListV2Status, StatusLabel>> = {
  approval: {
    label: "Approval",
    icon: "exclamationmark.shield",
    className: "text-warning-foreground",
    iconTintClassName: "accent-warning-foreground",
  },
  input: {
    label: "Input",
    icon: "questionmark.bubble",
    className: "text-adaptive-indigo-600-300",
    iconTintClassName: "accent-adaptive-indigo-600-300",
  },
  working: {
    label: "Working",
    icon: "circle.dashed",
    className: "text-adaptive-sky-600-400",
    iconTintClassName: "accent-adaptive-sky-600-400",
  },
  waiting: {
    label: "Waiting",
    icon: "circle.dashed.inset.filled",
    className: "text-foreground-muted",
    iconTintClassName: "accent-foreground-muted",
  },
  failed: {
    label: "Failed",
    icon: "exclamationmark.circle",
    className: "text-danger-foreground",
    iconTintClassName: "accent-danger-foreground",
  },
  limited: {
    label: "Limited",
    icon: "exclamationmark.circle",
    className: "text-warning-foreground",
    iconTintClassName: "accent-warning-foreground",
  },
};
const DONE_STATUS_LABEL: StatusLabel = {
  label: "Done",
  icon: "checkmark.circle",
  className: "text-adaptive-emerald-700-300",
  iconTintClassName: "accent-adaptive-emerald-700-300",
};

function ThreadListV2Duration(props: { readonly startedAt: string | null }) {
  const startedAtMs = Date.parse(props.startedAt ?? "");
  const nowMs = useVisibleSecondClock(Number.isFinite(startedAtMs));
  if (!Number.isFinite(startedAtMs)) return null;
  return ` ${formatWorkingDurationLabel(nowMs - startedAtMs)}`;
}

const MENU_ACTIONS_TAIL: MenuAction[] = [
  { id: "archive", title: "Archive", image: "archivebox" },
  { id: "delete", title: "Delete", image: "trash", attributes: { destructive: true } },
];

function ThreadListV2Section(props: {
  readonly label: string;
  readonly pane?: "screen" | "sidebar";
  readonly disclosure?: {
    readonly expanded: boolean;
    readonly disabled?: boolean;
    readonly onToggle: () => void;
    readonly accessibilityLabel: string;
    readonly accessibilityHint: string;
  };
}) {
  const sidebarPane = props.pane === "sidebar";
  const className = cn(
    "mb-1.5 mt-4 flex-row items-center gap-2.5",
    props.pane === "sidebar" ? "px-3" : "px-5",
  );
  const content = (
    <>
      <Text
        className={cn(
          "text-xs font-t3-medium",
          sidebarPane ? "text-drawer-foreground-muted" : "text-foreground-tertiary",
        )}
      >
        {props.label}
      </Text>
      <View className={cn("h-px flex-1", sidebarPane ? "bg-drawer-border" : "bg-border")} />
      {props.disclosure ? (
        <SymbolView
          name="chevron.down"
          size={10}
          tintColorClassName={
            sidebarPane ? "accent-drawer-foreground-muted" : "accent-foreground-muted"
          }
          type="monochrome"
          style={{ transform: [{ rotate: props.disclosure.expanded ? "180deg" : "0deg" }] }}
        />
      ) : null}
    </>
  );

  return props.disclosure ? (
    <Pressable
      accessibilityHint={props.disclosure.accessibilityHint}
      accessibilityLabel={props.disclosure.accessibilityLabel}
      accessibilityRole="button"
      accessibilityState={{
        disabled: props.disclosure.disabled,
        expanded: props.disclosure.expanded,
      }}
      className={className}
      disabled={props.disclosure.disabled}
      onPress={props.disclosure.onToggle}
      style={({ pressed }) => ({ opacity: pressed ? 0.6 : 1 })}
    >
      {content}
    </Pressable>
  ) : (
    <View className={className}>{content}</View>
  );
}

const ThreadListV2SectionDivider = memo(function ThreadListV2SectionDivider(props: {
  readonly label: string;
  readonly pane?: "screen" | "sidebar";
}) {
  return <ThreadListV2Section {...props} />;
});

export const ThreadListV2WorkingShelfHeader = memo(function ThreadListV2WorkingShelfHeader(props: {
  readonly count: number;
  readonly disabled?: boolean;
  readonly expanded: boolean;
  readonly onToggle: () => void;
  readonly pane?: "screen" | "sidebar";
}) {
  return (
    <ThreadListV2Section
      label={props.expanded ? "Working" : `Working (${props.count})`}
      pane={props.pane}
      disclosure={{
        expanded: props.expanded,
        disabled: props.disabled,
        onToggle: props.onToggle,
        accessibilityLabel: `${props.count} working ${props.count === 1 ? "thread" : "threads"}`,
        accessibilityHint: `${props.expanded ? "Collapses" : "Expands"} the working threads.`,
      }}
    />
  );
});

const PENDING_TASK_MENU_ACTIONS: MenuAction[] = [
  { id: "delete", title: "Delete", image: "trash", attributes: { destructive: true } },
];

const DRAFT_TASK_MENU_ACTIONS: MenuAction[] = [
  { id: "delete", title: "Discard", image: "trash", attributes: { destructive: true } },
];

export const ThreadListV2PendingRow = memo(function ThreadListV2PendingRow(props: {
  readonly pendingTask: PendingNewTask;
  readonly selected?: boolean;
  readonly project: EnvironmentProject | null;
  readonly projectTitle?: string;
  readonly environmentLabel: string | null;
  readonly environmentMachine?: EnvironmentMachineKind;
  readonly pane?: "screen" | "sidebar";
  readonly showPendingDivider: boolean;
  readonly showTrailingDivider?: boolean;
  readonly onSelectPendingTask: (pendingTask: PendingNewTask) => void;
  readonly onDeletePendingTask: (pendingTask: PendingNewTask) => void;
  readonly fullSwipeWidth?: number;
  readonly onSwipeableWillOpen: (methods: SwipeableMethods) => void;
  readonly onSwipeableClose: (methods: SwipeableMethods) => void;
  readonly activationKey?: string;
  readonly simultaneousSwipeGesture?: ComponentProps<typeof ThreadSwipeable>["simultaneousWith"];
}) {
  const { pendingTask, onSelectPendingTask, onDeletePendingTask } = props;
  const { width: windowWidth } = useWindowDimensions();
  const dormant = useSwipeRowDormant(props.activationKey);
  const theme = useUniwindTheme();
  const sidebarPane = props.pane === "sidebar";
  const rowAppearance = getThreadListV2RowAppearance(theme, sidebarPane, props.selected ?? false);
  const isDraft = pendingTask.kind === "draft";
  const deleteLabel = isDraft ? "Discard" : "Delete";
  const handleDelete = useCallback(
    () => onDeletePendingTask(pendingTask),
    [onDeletePendingTask, pendingTask],
  );
  const deleteAction = useMemo(
    () => ({
      accessibilityLabel: `${deleteLabel} ${pendingTask.title}`,
      destructive: true,
      icon: "trash" as const,
      label: deleteLabel,
      onPress: handleDelete,
    }),
    [deleteLabel, handleDelete, pendingTask.title],
  );
  const projectTitle = props.projectTitle ?? props.project?.title ?? pendingTask.projectTitle ?? "";

  const handleMenuAction = useCallback(
    ({ nativeEvent }: { readonly nativeEvent: { readonly event: string } }) => {
      if (nativeEvent.event === "delete") onDeletePendingTask(pendingTask);
    },
    [onDeletePendingTask, pendingTask],
  );

  const rowContent = (
    <>
      <View className="flex-row items-center gap-1.5">
        {props.project ? (
          <ProjectFavicon
            environmentId={pendingTask.environmentId}
            faviconPath={props.project.faviconPath}
            projectIcon={props.project.projectIcon}
            size={15}
            projectTitle={props.project.title}
            workspaceRoot={props.project.workspaceRoot}
          />
        ) : null}
        <Text
          className={cn(
            "flex-1 text-sm font-t3-medium text-foreground-muted",
            sidebarPane && "text-drawer-foreground-muted",
          )}
          numberOfLines={1}
        >
          {projectTitle}
        </Text>
        {isDraft ? (
          <View className="flex-row items-center gap-1">
            <SymbolView
              name="square.and.pencil"
              size={10}
              tintColorClassName="accent-adaptive-amber-700-300"
              type="monochrome"
            />
            <Text className="text-xs text-adaptive-amber-700-300">Draft</Text>
          </View>
        ) : (
          <Text
            className={cn(
              "text-xs text-foreground-tertiary",
              sidebarPane && "text-drawer-foreground-muted",
            )}
          >
            Sends on reconnect
          </Text>
        )}
      </View>
      <Text
        className={cn(
          "mt-1 text-base font-t3-medium text-foreground",
          sidebarPane && "text-drawer-foreground",
        )}
        numberOfLines={1}
      >
        {pendingTask.title}
      </Text>
      {props.environmentLabel ? (
        <View className="mt-1 flex-row items-center gap-1">
          <Text
            className={cn(
              "shrink text-xs text-foreground-tertiary",
              sidebarPane && "text-drawer-foreground-muted",
            )}
            numberOfLines={1}
          >
            {props.environmentLabel}
          </Text>
          {props.environmentMachine ? (
            <EnvironmentMachineSymbol
              kind={props.environmentMachine}
              size={11}
              tintColorClassName={
                sidebarPane ? "accent-drawer-foreground-muted" : "accent-foreground-tertiary"
              }
            />
          ) : null}
        </View>
      ) : null}
    </>
  );

  return (
    <>
      {props.showPendingDivider ? (
        <ThreadListV2SectionDivider label="Unsent" pane={props.pane} />
      ) : null}
      <ThreadSwipeable
        dormant={dormant}
        threadKey={pendingTask.key}
        backgroundColor={rowAppearance.swipeBackgroundColor}
        containerStyle={rowAppearance.swipeContainerStyle}
        enableTrackpadSwipe
        fullSwipeAction="primary"
        fullSwipeWidth={props.fullSwipeWidth ?? windowWidth - 32}
        onDelete={handleDelete}
        onSwipeableClose={props.onSwipeableClose}
        onSwipeableWillOpen={props.onSwipeableWillOpen}
        primaryAction={deleteAction}
        secondaryAction={null}
        resetKey={pendingTask.key}
        simultaneousWith={props.simultaneousSwipeGesture}
        threadTitle={pendingTask.title}
      >
        {() => (
          <Menu
            actions={isDraft ? DRAFT_TASK_MENU_ACTIONS : PENDING_TASK_MENU_ACTIONS}
            onPressAction={handleMenuAction}
            shouldOpenOnLongPress
          >
            <RowPressable
              accessibilityHint={
                isDraft
                  ? `Opens the draft in the new task composer. Swipe left to ${deleteLabel.toLowerCase()}.`
                  : `Sends when the environment reconnects. Opens the task for editing. Swipe left to ${deleteLabel.toLowerCase()}.`
              }
              accessibilityLabel={pendingTask.title}
              accessibilityRole="button"
              accessibilityState={{ selected: props.selected ?? false }}
              key={pendingTask.key}
              className={rowAppearance.className}
              interactionClassName={rowAppearance.interactionClassName}
              interactionOpacity={rowAppearance.interactionOpacity}
              onPress={() => onSelectPendingTask(pendingTask)}
              style={rowAppearance.cardStyle}
            >
              {sidebarPane ? (
                rowContent
              ) : (
                <View>
                  <View className="px-5 py-2.5">{rowContent}</View>
                  {props.showTrailingDivider !== false ? (
                    <View className="ml-5 h-px bg-border-subtle" />
                  ) : null}
                </View>
              )}
            </RowPressable>
          </Menu>
        )}
      </ThreadSwipeable>
    </>
  );
});

export const ThreadListV2Row = memo(function ThreadListV2Row(props: {
  readonly thread: EnvironmentThreadShell;
  readonly hasQueuedMessages?: boolean;
  readonly timeLabel: string;
  readonly project: EnvironmentProject | null;
  readonly providers: ReadonlyArray<ThreadListProvider> | undefined;
  readonly providerInstance: ThreadRowProviderInstance | null;
  readonly environmentLabel: string | null;
  readonly environmentMachine?: EnvironmentMachineKind;
  readonly pane?: "screen" | "sidebar";
  readonly showTrailingDivider?: boolean;
  readonly selected?: boolean;
  readonly fullSwipeWidth?: number;
  readonly onSelectThread: (thread: EnvironmentThreadShell) => void;
  readonly onDeleteThread: (thread: EnvironmentThreadShell) => void;
  readonly onRenameThread: (thread: EnvironmentThreadShell) => void;
  readonly onRegenerateThreadTitle: (thread: EnvironmentThreadShell) => void;
  readonly onArchiveThread: (thread: EnvironmentThreadShell) => void;
  readonly titleRegenerationSupported: boolean;
  readonly onSwipeableWillOpen: (methods: SwipeableMethods) => void;
  readonly onSwipeableClose: (methods: SwipeableMethods) => void;
  readonly activationKey?: string;
  readonly searchMatch?: EnvironmentThreadSearchMatch;
  readonly searchQuery?: string;
  readonly simultaneousSwipeGesture?: ComponentProps<typeof ThreadSwipeable>["simultaneousWith"];
}) {
  const { width: windowWidth, fontScale } = useWindowDimensions();
  const titleTypography = useScaledTextRole("body");
  const metadataTypography = useScaledTextRole("label");
  const projectIconSize = titleTypography.fontSize * 1.25 * fontScale;
  const machineIconSize = metadataTypography.fontSize * fontScale;
  const titleLineHeight = titleTypography.lineHeight * fontScale;
  const {
    thread,
    onSelectThread,
    onDeleteThread,
    onRenameThread,
    onRegenerateThreadTitle,
    onArchiveThread,
  } = props;
  const dormant = useSwipeRowDormant(props.activationKey);

  const selectThread = () => onSelectThread(thread);
  const { providerDrivers, providerIconUrl } = useMemo(() => {
    const provider = props.providers?.find(
      (candidate) =>
        candidate.instanceId ===
        (thread.runtime?.providerInstanceId ?? thread.modelSelection.instanceId),
    );
    return {
      providerDrivers: resolveThreadListV2ProviderDrivers(thread, props.providers),
      providerIconUrl: provider?.iconUrl,
    };
  }, [thread, props.providers]);

  const providerInstance = props.providerInstance;
  const theme = useUniwindTheme();
  const sidebarPane = props.pane === "sidebar";
  const selected = props.selected === true;
  const rowAppearance = getThreadListV2RowAppearance(theme, sidebarPane, selected);

  const status = resolveThreadListV2Status(thread);
  const isUnread = status === "ready" && threadHasUnseenCompletion(thread);
  const workingLabel = STATUS_LABEL_BY_STATUS[status];
  const statusLabel =
    (status === "working" && workingLabel !== undefined && thread.goal?.status === "active"
      ? { ...workingLabel, label: "Goal" }
      : workingLabel) ?? (isUnread ? DONE_STATUS_LABEL : undefined);
  const recede = shouldRecedeThreadRow({ status, selected });
  const isRegeneratingTitle = thread.titleRegeneration != null;
  const rowAccessibilityLabel = `${thread.title}${props.hasQueuedMessages ? ", messages queued to send" : ""}${isRegeneratingTitle ? ", regenerating title" : ""}`;
  const timeLabel = props.timeLabel;

  const handleDelete = useCallback(() => onDeleteThread(thread), [onDeleteThread, thread]);
  const handleRename = useCallback(() => onRenameThread(thread), [onRenameThread, thread]);
  const canOperateThread = useEnvironmentScope(thread.environmentId, AuthOrchestrationOperateScope);
  const handleRegenerateTitle = useCallback(
    () => onRegenerateThreadTitle(thread),
    [onRegenerateThreadTitle, thread],
  );
  const handleArchive = useCallback(() => onArchiveThread(thread), [onArchiveThread, thread]);

  const menuActions = useMemo<MenuAction[]>(
    () => [
      { id: "copy-thread", title: "Copy thread", image: "text.bubble" },
      { id: "copy-thread-id", title: "Copy thread ID", image: "doc.on.doc" },
      { id: "rename", title: "Rename", image: "square.and.pencil" },
      ...buildThreadTitleRegenerationMenuItems({
        supported: props.titleRegenerationSupported,
        isRegenerating: isRegeneratingTitle,
      }),
      ...MENU_ACTIONS_TAIL,
    ],
    [props.titleRegenerationSupported, isRegeneratingTitle],
  );
  const handleMenuAction = useCallback(
    ({ nativeEvent }: { readonly nativeEvent: { readonly event: string } }) => {
      if (nativeEvent.event === "archive") handleArchive();
      if (nativeEvent.event === "rename") handleRename();
      if (nativeEvent.event === "regenerate-title") handleRegenerateTitle();
      if (nativeEvent.event === "copy-thread") {
        const target = threadChipCopyTarget(
          { environmentId: thread.environmentId, threadId: thread.id },
          thread.title,
        );
        void copyThreadReference(target).then((didCopy) => {
          if (!didCopy) Dialog.alert(target.failureTitle, "Try copying this thread again.");
        });
      }
      if (nativeEvent.event === "copy-thread-id") {
        copyTextWithHaptic(thread.id, { target: "thread-id" });
      }
      if (nativeEvent.event === "delete") handleDelete();
    },
    [thread, handleArchive, handleDelete, handleRegenerateTitle, handleRename],
  );
  const archiveAction = useMemo(
    () => ({
      accessibilityLabel: `Archive ${thread.title}`,
      icon: "archivebox" as const,
      label: "Archive",
      onPress: handleArchive,
    }),
    [handleArchive, thread.title],
  );
  const swipeAccessibilityHint = canOperateThread
    ? "Opens the thread. Swipe left to archive."
    : "Opens the thread";

  const titleTextInset = props.project ? projectIconSize + 8 : 0;
  const cardContent = (
    <>
      <View className="flex-row items-start gap-2">
        {props.project ? (
          <View className="justify-center" style={{ height: titleLineHeight }}>
            <ProjectFavicon
              environmentId={thread.environmentId}
              faviconPath={props.project.faviconPath}
              projectIcon={props.project.projectIcon}
              size={projectIconSize}
              projectTitle={props.project.title}
              workspaceRoot={props.project.workspaceRoot}
            />
          </View>
        ) : null}
        <Text
          className={cn(
            "flex-1 text-base",
            !recede && "font-t3-medium",
            selected
              ? selectedThreadRowColors.foregroundClassName
              : rowAppearance.foregroundClassName,
            isRegeneratingTitle && "opacity-55",
          )}
          numberOfLines={2}
        >
          {thread.title}
        </Text>
        <View className="flex-row items-center gap-1.5" style={{ minHeight: titleLineHeight }}>
          {props.hasQueuedMessages ? <QueuedMessageIcon selected={selected} /> : null}
          {statusLabel ? (
            <View className="flex-row items-center gap-1">
              <SymbolView
                name={statusLabel.icon}
                size={13}
                tintColorClassName={
                  selected
                    ? selectedThreadRowColors.iconTintClassName
                    : statusLabel.iconTintClassName
                }
                type="monochrome"
                weight="semibold"
              />
              <Text
                className={cn(
                  "text-xs font-t3-bold",
                  (status === "working" || status === "waiting") && "tabular-nums",
                  selected ? selectedThreadRowColors.foregroundClassName : statusLabel.className,
                )}
              >
                {statusLabel.label}
                {status === "working" ? (
                  <ThreadListV2Duration startedAt={resolveThreadWorkingStartedAt(thread)} />
                ) : status === "waiting" ? (
                  <ThreadListV2Duration startedAt={thread.latestRun?.completedAt ?? null} />
                ) : null}
              </Text>
            </View>
          ) : (
            <Text
              className={cn(
                "text-xs tabular-nums",
                selected
                  ? selectedThreadRowColors.foregroundClassName
                  : rowAppearance.tertiaryForegroundClassName,
              )}
            >
              {timeLabel}
            </Text>
          )}
        </View>
      </View>
      {props.searchMatch ? (
        <View className="mt-1" style={{ paddingLeft: titleTextInset }}>
          <ThreadSearchMatchExcerpt
            sidebar={sidebarPane}
            match={props.searchMatch}
            query={props.searchQuery ?? ""}
            selected={selected}
          />
        </View>
      ) : null}
      <View className="mt-1 flex-row items-center gap-2" style={{ paddingLeft: titleTextInset }}>
        {(status === "failed" || status === "limited") && thread.runtime?.lastError ? (
          <Text
            className={cn(
              "flex-1 text-xs",
              selected
                ? selectedThreadRowColors.mutedForegroundClassName
                : status === "limited"
                  ? "text-warning-foreground"
                  : "text-danger-foreground",
            )}
            numberOfLines={1}
          >
            {thread.runtime.lastError}
          </Text>
        ) : props.environmentLabel ? (
          <View className="min-w-0 flex-1 flex-row items-center gap-1">
            {props.environmentMachine ? (
              <EnvironmentMachineSymbol
                kind={props.environmentMachine}
                size={machineIconSize}
                tintColorClassName={
                  selected
                    ? selectedThreadRowColors.mutedIconTintClassName
                    : rowAppearance.tertiaryIconTintClassName
                }
              />
            ) : null}
            <Text
              className={cn(
                "shrink text-xs",
                selected
                  ? selectedThreadRowColors.mutedForegroundClassName
                  : rowAppearance.tertiaryForegroundClassName,
              )}
              numberOfLines={1}
            >
              {props.environmentLabel}
            </Text>
          </View>
        ) : (
          <View className="flex-1" />
        )}
        {providerInstance ? (
          <View className="flex-row items-center">
            {providerDrivers.slice(0, -1).map(
              (driver, index) => (
                /* oxlint-disable react/no-array-index-key -- Provider history can repeat a driver; these decorative icons have no local state. */ <View
                  key={`${driver}:${index}`}
                  className="-mr-1 opacity-30"
                >
                  <ProviderIcon provider={driver} size={12} />
                </View>
              ) /* oxlint-enable react/no-array-index-key */,
            )}
            <ProviderInstanceIcon
              iconUrl={providerIconUrl}
              provider={providerInstance.driverKind}
              size={14}
              displayName={providerInstance.displayName}
              accentColor={providerInstance.accentColor}
              showBadge={providerInstance.showBadge}
              surfaceColor={rowAppearance.providerIconSurfaceColor}
            />
          </View>
        ) : null}
      </View>
    </>
  );

  const fadedCardContent = recede ? (
    <View style={{ opacity: 0.55 }}>{cardContent}</View>
  ) : (
    cardContent
  );

  const rowContent = (close: () => void) => (
    <RowPressable
      key={`${thread.environmentId}:${thread.id}`}
      interactionClassName={rowAppearance.interactionClassName}
      interactionOpacity={rowAppearance.interactionOpacity}
      className={rowAppearance.className}
      accessibilityHint={swipeAccessibilityHint}
      accessibilityLabel={rowAccessibilityLabel}
      accessibilityRole="button"
      accessibilityState={{ selected }}
      onPress={() => {
        close();
        selectThread();
      }}
      style={rowAppearance.cardStyle}
    >
      {sidebarPane ? (
        fadedCardContent
      ) : (
        <View>
          <View className={THREAD_LIST_V2_ROW_CONTENT_CLASS_NAME}>{fadedCardContent}</View>
          {THREAD_LIST_V2_ROW_DIVIDERS && props.showTrailingDivider !== false ? (
            <View className="ml-5 h-px bg-border-subtle" />
          ) : null}
        </View>
      )}
    </RowPressable>
  );

  if (!canOperateThread)
    return <View style={rowAppearance.swipeContainerStyle}>{rowContent(() => {})}</View>;

  return (
    <View collapsable={false}>
      <ThreadSwipeable
        dormant={dormant}
        threadKey={`${thread.environmentId}:${thread.id}`}
        backgroundColor={rowAppearance.swipeBackgroundColor}
        containerStyle={rowAppearance.swipeContainerStyle}
        enableTrackpadSwipe
        fullSwipeAction="primary"
        fullSwipeWidth={props.fullSwipeWidth ?? windowWidth - 32}
        onDelete={handleDelete}
        onSwipeableClose={props.onSwipeableClose}
        onSwipeableWillOpen={props.onSwipeableWillOpen}
        primaryAction={archiveAction}
        secondaryAction={null}
        resetKey={`${thread.environmentId}:${thread.id}`}
        simultaneousWith={props.simultaneousSwipeGesture}
        threadTitle={thread.title}
      >
        {(close) => (
          <Menu actions={menuActions} onPressAction={handleMenuAction} shouldOpenOnLongPress>
            {rowContent(close)}
          </Menu>
        )}
      </ThreadSwipeable>
    </View>
  );
});
