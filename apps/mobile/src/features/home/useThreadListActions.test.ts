import { makeThreadShellFixture } from "../../test-fixtures";
import type { EnvironmentThreadShell } from "@t3tools/client-runtime/state/shell";
import {
  AuthOrchestrationOperateScope,
  EnvironmentId,
  ProjectId,
  ProviderInstanceId,
  RunId,
  ThreadId,
} from "@t3tools/contracts";
import * as Cause from "effect/Cause";
import { AsyncResult } from "effect/reactivity";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

const state = vi.hoisted(() => ({
  failedAction: null as string | null,
  archivedRefreshes: [] as string[],
  scopes: new Map<string, Set<string>>(),
  requests: [] as {
    action: string;
    environmentId: string;
    input: { threadId: string };
  }[],
  dialogs: [] as { onConfirm: () => void }[],
  alerts: [] as { title: string }[],
}));

vi.mock("react", () => ({
  useCallback: (callback: unknown) => callback,
  useRef: (current: unknown) => ({ current }),
}));
vi.mock("expo-haptics", () => ({
  impactAsync: async () => {},
  ImpactFeedbackStyle: { Light: "light" },
}));
vi.mock("../../components/ui/Dialog", () => ({
  alert: (title: string) => state.alerts.push({ title }),
  confirm: (dialog: { onConfirm: () => void }) => state.dialogs.push(dialog),
}));
vi.mock("../archive/useArchivedThreadSnapshots", () => ({
  refreshArchivedThreadsForEnvironment: (environmentId: string) => {
    state.archivedRefreshes.push(environmentId);
  },
}));
vi.mock("../../state/session", () => ({
  readEnvironmentScope: (environmentId: string, scope: string) =>
    state.scopes.get(environmentId)?.has(scope) === true,
}));
vi.mock("../../state/server", () => ({
  environmentServerConfigsAtom: "server-configs",
}));
vi.mock("../../state/atom-registry", () => ({
  appAtomRegistry: {
    get: () => new Map([...state.scopes.keys()].map((environmentId) => [environmentId, {}])),
  },
}));
vi.mock("../../state/use-atom-command", () => ({
  useAtomCommand: (command: unknown) => command,
}));
vi.mock("../../state/threads", () => ({
  threadEnvironment: Object.fromEntries(
    ["archive", "unarchive", "delete", "updateMetadata"].map((action) => [
      action,
      async (request: { environmentId: string; input: { threadId: string } }) => {
        state.requests.push({ action, ...request });
        if (!state.scopes.get(request.environmentId)?.has(AuthOrchestrationOperateScope)) {
          return AsyncResult.failure(Cause.fail(new Error("Thread operation denied")));
        }
        if (action === state.failedAction)
          return AsyncResult.failure(Cause.fail(new Error("Server rejected action")));
        return AsyncResult.success(undefined);
      },
    ]),
  ),
}));

import { useThreadListActions } from "./useThreadListActions";

const primaryEnvironmentId = EnvironmentId.make("primary");
const otherEnvironmentId = EnvironmentId.make("other");

function makeThread(input: Partial<EnvironmentThreadShell> = {}): EnvironmentThreadShell {
  return makeThreadShellFixture({
    id: ThreadId.make("thread"),
    title: "Thread",
    environmentId: primaryEnvironmentId,
    projectId: ProjectId.make("project"),
    modelSelection: { instanceId: ProviderInstanceId.make("codex"), model: "model" },
    ...input,
  });
}

const mutationCases = [
  ["archiveThread", "archive"],
  ["regenerateThreadTitle", "updateMetadata"],
] as const;

beforeEach(() => {
  state.failedAction = null;
  state.archivedRefreshes = [];
  state.scopes = new Map([
    [primaryEnvironmentId, new Set([AuthOrchestrationOperateScope])],
    [otherEnvironmentId, new Set<string>()],
  ]);
  state.requests = [];
  state.dialogs = [];
  state.alerts = [];
});

describe("thread list operation permissions", () => {
  it.each(mutationCases)(
    "%s rejects a retained callback after its environment loses permission",
    async (handler) => {
      const actions = useThreadListActions();
      state.scopes.get(primaryEnvironmentId)!.clear();

      expect(await actions[handler](makeThread())).toBe(false);

      expect(state.requests).toEqual([]);
    },
  );

  it.each(mutationCases)("%s requires the target environment's permission", async (handler) => {
    expect(
      await useThreadListActions()[handler](makeThread({ environmentId: otherEnvironmentId })),
    ).toBe(false);

    expect(state.requests).toEqual([]);
  });

  it.each(mutationCases)("%s sends its command", async (handler, action) => {
    expect(await useThreadListActions()[handler](makeThread())).toBe(true);

    expect(state.requests.map(({ action }) => action)).toEqual([action]);
  });

  it("rechecks permission after a retained delete confirmation", async () => {
    useThreadListActions().confirmDeleteThread(makeThread());
    const confirm = state.dialogs[0]?.onConfirm;
    expect(confirm).toBeTypeOf("function");
    state.scopes.get(primaryEnvironmentId)!.clear();

    await confirm!();

    expect(state.requests).toEqual([]);
  });
});

describe("thread archive", () => {
  it("rejects a concurrent archive request without sending another mutation", async () => {
    const actions = useThreadListActions();
    const thread = makeThread();
    const first = actions.archiveThread(thread);
    expect(await actions.archiveThread(thread)).toBe(false);
    expect(await first).toBe(true);
    expect(state.requests.map(({ action }) => action)).toEqual(["archive"]);
    expect(state.archivedRefreshes).toEqual([primaryEnvironmentId]);
  });

  it("reports a rejected archive without refreshing the archive", async () => {
    state.failedAction = "archive";
    expect(await useThreadListActions().archiveThread(makeThread())).toBe(false);
    expect(state.archivedRefreshes).toEqual([]);
    expect(state.alerts.at(-1)?.title).toBe("Could not archive thread");
  });

  it("keeps active provider runs out of the archive", async () => {
    const thread = makeThread({
      runtime: {
        status: "running",
        activeRunId: RunId.make("active-run"),
        providerInstanceId: ProviderInstanceId.make("codex"),
        providerName: "codex",
        lastError: null,
        updatedAt: "2026-09-02T00:00:00.000Z",
      },
    });
    expect(await useThreadListActions().archiveThread(thread)).toBe(false);
    expect(state.requests).toEqual([]);
    expect(state.archivedRefreshes).toEqual([]);
    expect(state.alerts.at(-1)?.title).toBe("Could not archive thread");
  });
});

describe("thread delete", () => {
  it("deletes after confirmation and refreshes the archive", async () => {
    useThreadListActions().confirmDeleteThread(makeThread());

    state.dialogs[0]!.onConfirm();

    await vi.waitFor(() => expect(state.archivedRefreshes).toEqual([primaryEnvironmentId]));
    expect(state.requests.map(({ action }) => action)).toEqual(["delete"]);
  });
});
