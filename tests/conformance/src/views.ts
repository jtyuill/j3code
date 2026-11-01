import type {
  OrchestrationProjectShell,
  OrchestrationV2AppThread,
  OrchestrationV2ConversationMessage,
  OrchestrationV2Run,
  OrchestrationV2RuntimeRequest,
  OrchestrationV2ShellStreamItem,
  OrchestrationV2ThreadProjection,
  OrchestrationV2ThreadShell,
  OrchestrationV2ThreadStreamItem,
  OrchestrationV2TurnItem,
} from "@t3tools/contracts";

/** Run statuses the client treats as active (`CR/state/threadWorkflows.ts`). */
export const ACTIVE_RUN_STATUSES: Readonly<Record<string, true>> = {
  preparing: true,
  starting: true,
  running: true,
  waiting: true,
};

/**
 * Sequence bookkeeping shared by both streams. The client drops items at or below its cursor
 * (`CR/state/threads.ts`, `CR/state/shellReducer.ts`), so overlap is only counted; a sequence at
 * or below the previous item of the same subscription is a violation.
 */
class SequenceTracker {
  /** Cursor: snapshot sequence, then the last applied item. */
  cursor: number | undefined;
  private previous: number | undefined;
  overlap = 0;
  readonly violations: string[] = [];

  private readonly label: string;

  constructor(label: string, afterSequence: number | undefined) {
    this.label = label;
    this.cursor = afterSequence;
  }

  snapshot(sequence: number): void {
    if (this.cursor !== undefined && sequence < this.cursor) {
      this.violations.push(
        `${this.label}: snapshot at sequence ${sequence} is behind the cursor ${this.cursor}`,
      );
    }
    this.cursor = sequence;
  }

  /** Returns whether the client would apply the item. */
  advance(sequence: number, kind: string): boolean {
    if (this.previous !== undefined && sequence <= this.previous) {
      this.violations.push(
        `${this.label}: ${kind} at sequence ${sequence} after ${this.previous} (sequences must increase)`,
      );
    }
    this.previous = sequence;
    if (this.cursor !== undefined && sequence <= this.cursor) {
      this.overlap += 1;
      return false;
    }
    this.cursor = sequence;
    return true;
  }
}

/** The thread state a client derives from `orchestration.subscribeThread`. */
export class ThreadView {
  readonly sequences: SequenceTracker;
  snapshots = 0;
  synchronized = false;
  firstKind: string | undefined;
  thread: OrchestrationV2AppThread | undefined;
  deleted = false;
  /** Type of the last `thread.*` event applied. */
  lastThreadEvent: string | undefined;
  readonly runs = new Map<string, OrchestrationV2Run>();
  /** Every status each run has been seen in, in order. */
  readonly runStatusHistory = new Map<string, string[]>();
  readonly runtimeRequests = new Map<string, OrchestrationV2RuntimeRequest>();
  readonly messages = new Map<string, OrchestrationV2ConversationMessage>();
  readonly turnItems = new Map<string, OrchestrationV2TurnItem>();
  readonly unknownEventTypes = new Set<string>();
  events = 0;

  readonly threadId: string;

  constructor(threadId: string, afterSequence: number | undefined) {
    this.threadId = threadId;
    this.sequences = new SequenceTracker("subscribeThread", afterSequence);
  }

  apply(item: OrchestrationV2ThreadStreamItem): void {
    this.firstKind ??= item.kind;
    switch (item.kind) {
      case "synchronized":
        this.synchronized = true;
        return;
      case "snapshot":
        this.snapshots += 1;
        this.sequences.snapshot(item.snapshotSequence);
        this.applyProjection(item.projection);
        return;
      case "unknown-event":
        this.unknownEventTypes.add(item.eventType);
        this.sequences.advance(item.sequence, `unknown event ${item.eventType}`);
        return;
      case "event": {
        this.events += 1;
        if (!this.sequences.advance(item.sequence, `event ${item.event.type}`)) return;
        // The client ignores events for other threads (`CR/state/orchestrationV2Projection.ts`).
        if (item.event.threadId !== this.threadId) return;
        const event = item.event;
        switch (event.type) {
          case "thread.created":
          case "thread.archived":
          case "thread.unarchived":
          case "thread.settled":
          case "thread.unsettled":
          case "thread.snoozed":
          case "thread.unsnoozed":
          case "thread.pinned":
          case "thread.auto-settle-set":
          case "thread.unpinned":
          case "thread.pin-reordered":
          case "thread.active-reordered":
          case "thread.visited":
          case "thread.marked-unread":
          case "thread.metadata-updated":
          case "thread.pull-request-synced":
          case "thread.runtime-mode-updated":
          case "thread.interaction-mode-updated":
          case "thread.model-selection-updated":
          case "thread.provider-switched":
          case "thread.guest-share-updated":
            this.thread = event.payload;
            this.lastThreadEvent = event.type;
            return;
          case "thread.deleted":
            this.thread = event.payload;
            this.lastThreadEvent = event.type;
            this.deleted = true;
            return;
          case "run.created":
          case "run.updated":
            this.setRun(event.payload);
            return;
          case "runtime-request.updated":
            this.runtimeRequests.set(event.payload.id, event.payload);
            return;
          case "message.updated":
            this.messages.set(event.payload.id, event.payload);
            return;
          case "turn-item.updated":
            this.turnItems.set(event.payload.id, event.payload);
            return;
          default:
            return;
        }
      }
    }
  }

  private applyProjection(projection: OrchestrationV2ThreadProjection): void {
    this.thread = projection.thread;
    this.runs.clear();
    for (const run of projection.runs) this.setRun(run);
    this.runtimeRequests.clear();
    for (const request of projection.runtimeRequests) this.runtimeRequests.set(request.id, request);
    this.messages.clear();
    for (const message of projection.messages) this.messages.set(message.id, message);
    this.turnItems.clear();
    for (const item of projection.turnItems) this.turnItems.set(item.id, item);
  }

  private setRun(run: OrchestrationV2Run): void {
    this.runs.set(run.id, run);
    const history = this.runStatusHistory.get(run.id) ?? [];
    if (history.at(-1) !== run.status) history.push(run.status);
    this.runStatusHistory.set(run.id, history);
  }

  runForMessage(messageId: string): OrchestrationV2Run | undefined {
    for (const run of this.runs.values()) if (run.userMessageId === messageId) return run;
    return undefined;
  }

  itemsOfRun(runId: string): OrchestrationV2TurnItem[] {
    return [...this.turnItems.values()].filter((item) => item.runId === runId);
  }

  /** Assistant text of a run, restoring deduped bodies (`textOmitted`) from messages. */
  assistantText(runId: string): string {
    const texts: string[] = [];
    const itemMessageIds = new Set<string>();
    for (const item of this.itemsOfRun(runId)) {
      if (item.type !== "assistant_message") continue;
      itemMessageIds.add(item.messageId);
      texts.push(
        item.textOmitted === true ? (this.messages.get(item.messageId)?.text ?? "") : item.text,
      );
    }
    // Messages without a feed item (none expected, but tolerated).
    for (const message of this.messages.values()) {
      if (message.runId === runId && message.role === "assistant" && !itemMessageIds.has(message.id)) {
        texts.push(message.text);
      }
    }
    return texts.join("\n");
  }

  describe(): string {
    const runs = [...this.runs.values()]
      .map((run) => `${run.id}[${(this.runStatusHistory.get(run.id) ?? []).join("→")}]`)
      .join(", ");
    const requests = [...this.runtimeRequests.values()]
      .map((request) => `${request.id}:${request.kind}:${request.status}`)
      .join(", ");
    return [
      `cursor=${this.sequences.cursor ?? "none"}`,
      `snapshots=${this.snapshots}`,
      `synchronized=${this.synchronized}`,
      `events=${this.events}`,
      `runs={${runs}}`,
      `runtimeRequests={${requests}}`,
      `turnItems=${this.turnItems.size}`,
    ].join(" ");
  }
}

/** The shell state a client derives from `orchestration.subscribeShell` (`CR/state/shellReducer.ts`). */
export class ShellView {
  readonly sequences: SequenceTracker;
  snapshots = 0;
  synchronized = false;
  firstKind: string | undefined;
  snapshotSequence: number | undefined;
  readonly projects = new Map<string, OrchestrationProjectShell>();
  readonly threads = new Map<string, OrchestrationV2ThreadShell>();
  readonly archivedThreads = new Map<string, OrchestrationV2ThreadShell>();
  readonly removedProjects = new Set<string>();
  readonly removedThreads = new Set<string>();

  constructor(afterSequence: number | undefined) {
    this.sequences = new SequenceTracker("subscribeShell", afterSequence);
  }

  apply(item: OrchestrationV2ShellStreamItem): void {
    this.firstKind ??= item.kind;
    switch (item.kind) {
      case "synchronized":
        this.synchronized = true;
        return;
      case "snapshot": {
        this.snapshots += 1;
        const snapshot = item.snapshot;
        this.snapshotSequence = snapshot.snapshotSequence;
        this.sequences.snapshot(snapshot.snapshotSequence);
        this.projects.clear();
        for (const project of snapshot.projects) this.projects.set(project.id, project);
        this.threads.clear();
        for (const thread of snapshot.threads) this.threads.set(thread.id, thread);
        this.archivedThreads.clear();
        for (const thread of snapshot.archivedThreads) this.archivedThreads.set(thread.id, thread);
        return;
      }
      case "project.updated":
        if (!this.sequences.advance(item.sequence, item.kind)) return;
        this.projects.set(item.project.id, item.project);
        return;
      case "project.removed":
        if (!this.sequences.advance(item.sequence, item.kind)) return;
        this.projects.delete(item.projectId);
        this.removedProjects.add(item.projectId);
        return;
      case "thread.updated":
        if (!this.sequences.advance(item.sequence, item.kind)) return;
        if (item.location === "active") {
          this.threads.set(item.thread.id, item.thread);
          this.archivedThreads.delete(item.thread.id);
        } else {
          this.threads.delete(item.thread.id);
          this.archivedThreads.set(item.thread.id, item.thread);
        }
        return;
      case "thread.removed":
        if (!this.sequences.advance(item.sequence, item.kind)) return;
        this.threads.delete(item.threadId);
        this.archivedThreads.delete(item.threadId);
        this.removedThreads.add(item.threadId);
        return;
    }
  }

  describe(): string {
    const threads = [...this.threads.values()]
      .map((thread) => `${thread.id}:${thread.activityRunStatus ?? thread.status}`)
      .join(", ");
    return [
      `cursor=${this.sequences.cursor ?? "none"}`,
      `snapshots=${this.snapshots}`,
      `synchronized=${this.synchronized}`,
      `projects=${this.projects.size}`,
      `threads={${threads}}`,
      `archived=${this.archivedThreads.size}`,
    ].join(" ");
  }
}
