import { RegistryContext, useAtomValue } from "@effect/atom-react";
import type { EnvironmentProject } from "@t3tools/client-runtime/state/shell";
import { AuthFilesystemReadScope, EnvironmentId, ProjectId } from "@t3tools/contracts";
import { AsyncResult, Atom, AtomRegistry } from "effect/reactivity";
import { act, useLayoutEffect } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vite-plus/test";
import type { ComposerDraft } from "../../state/use-composer-drafts";

const mocks = vi.hoisted(() => ({
  projects: vi.fn(),
  readFile: vi.fn(),
  session: vi.fn(),
  presentation: vi.fn(),
  registry: vi.fn(),
  drafts: vi.fn(),
  snapshot: vi.fn(),
  createDraft: vi.fn(),
  retargetDraft: vi.fn(),
  writeSettings: vi.fn(),
  writeText: vi.fn(),
}));
vi.mock("../../state/atom-registry", () => ({
  appAtomRegistry: {
    get: (atom: Atom.Atom<unknown>) => mocks.registry().get(atom),
    set: (atom: Atom.Writable<unknown, unknown>, value: unknown) =>
      mocks.registry().set(atom, value),
  },
}));
vi.mock("../../state/entities", () => ({
  useProjects: mocks.projects,
  useThreadShells: () => [],
  useEnvironmentServerConfig: () => null,
  useServerConfigs: () => new Map(),
}));
vi.mock("../../state/projects", () => ({
  projectEnvironment: { readFile: mocks.readFile, openScratch: {} },
}));
vi.mock("../../state/session", () => ({
  environmentSession: { sessionStateAtom: mocks.session },
}));
vi.mock("../../state/presentation", () => ({
  useEnvironmentPresentation: mocks.presentation,
}));
vi.mock("../../state/orchestration", () => ({ orchestrationEnvironment: {} }));
vi.mock("../../state/threads", () => ({ olderSettledThreads: {}, threadFamilies: {} }));
vi.mock("../../state/pull-requests", () => ({ composerPullRequests: {} }));
vi.mock("../../state/project-grouping", () => ({
  useMobileProjectGroupingSettings: () => ({ sidebarProjectGroupingMode: "none" }),
}));
vi.mock("./use-legacy-plan-mode-enabled", () => ({
  useLegacyPlanModeState: () => ({ enabled: false, loaded: true }),
}));
vi.mock("../../components/ui/Dialog", () => ({ alert: vi.fn() }));
vi.mock("../../state/use-atom-command", () => ({ useAtomCommand: () => vi.fn() }));
vi.mock("../../state/use-remote-environment-registry", () => ({
  useSavedRemoteConnections: () => ({ savedConnectionsById: {} }),
  useRemoteConnectionStatus: () => ({ connectedEnvironments: [] }),
}));
vi.mock("../../state/use-model-option-memory", () => ({
  rememberModelOptions: vi.fn(),
  withRememberedModelOptions: (value: unknown) => value,
}));
vi.mock("../../state/use-composer-drafts", () => ({
  composerDraftsLoadedAtom: Atom.make(true),
  useComposerDraft: mocks.drafts,
  getComposerDraftSnapshot: mocks.snapshot,
  createNewTaskDraft: mocks.createDraft,
  retargetNewTaskDraft: mocks.retargetDraft,
  updateComposerDraftSettings: mocks.writeSettings,
  setComposerDraftText: mocks.writeText,
  useStickyComposerModelSelection: () => null,
  isNewTaskDraftKey: (key: string) => key.startsWith("new-task:"),
  clearComposerDraft: vi.fn(),
  appendComposerDraftAttachments: vi.fn(),
  removeComposerDraftAttachment: vi.fn(),
  replaceComposerDraftAttachments: vi.fn(),
  setStickyComposerModelSelection: vi.fn(),
  setComposerDraftContext: vi.fn(),
  scheduleUnusedComposerAttachmentCleanup: vi.fn(),
  isComposerDraftEmpty: (draft: ComposerDraft) =>
    draft.text === "" && draft.attachments.length === 0,
}));

vi.mock("../../state/workspace", () => ({
  useConnectionsReady: () => true,
  useWorkspaceEnvironments: () => [],
}));
vi.mock("../../state/shell", () => ({ useEnvironmentShellReadiness: () => ({}) }));
vi.mock("../../lib/clock", () => ({ useNowMinuteMs: () => 0 }));

import { NewTaskFlowProvider, useNewTaskFlow } from "./new-task-flow-provider";

function project(environment: string, id: string): EnvironmentProject {
  return {
    environmentId: EnvironmentId.make(environment),
    id: ProjectId.make(id),
    title: id,
    workspaceRoot: `/${id}`,
    repositoryIdentity: null,
    defaultModelSelection: null,
    defaultThreadEnvMode: "worktree",
    scripts: [],
    createdAt: "2026-10-26T00:00:00Z",
    updatedAt: "2026-10-26T00:00:00Z",
  };
}

function setup() {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.clearAllMocks();
  const registry = AtomRegistry.make();
  mocks.registry.mockReturnValue(registry);
  const projects = [project("one", "a"), project("one", "b"), project("two", "b")];
  mocks.projects.mockReturnValue(projects);
  mocks.presentation.mockReturnValue({
    isReady: true,
    presentation: { connection: { phase: "connected", error: null } },
  });
  const session = Atom.make(
    AsyncResult.success({ authenticated: true, scopes: [AuthFilesystemReadScope] }),
  );
  mocks.session.mockReturnValue(session);
  const file = Atom.make(AsyncResult.success({ exists: true, contents: "{}" }));
  mocks.readFile.mockReturnValue(file);

  const empty: ComposerDraft = { text: "", attachments: [] };
  const drafts = Atom.make<Record<string, ComposerDraft>>({}).pipe(Atom.keepAlive);
  const snapshot = (key: string | null): ComposerDraft =>
    (key === null ? undefined : registry.get(drafts)[key]) ?? empty;
  const write = (key: string, patch: Partial<ComposerDraft>) =>
    registry.set(drafts, { ...registry.get(drafts), [key]: { ...snapshot(key), ...patch } });
  mocks.drafts.mockImplementation((key: string | null) => {
    const all = useAtomValue(drafts);
    return (key === null ? undefined : all[key]) ?? empty;
  });
  mocks.snapshot.mockImplementation(snapshot);
  mocks.createDraft.mockImplementation((target) => {
    const key = `new-task:${Object.keys(registry.get(drafts)).length}`;
    write(key, { project: target });
    return key;
  });
  mocks.retargetDraft.mockImplementation((key, target) => write(key, { project: target }));
  mocks.writeSettings.mockImplementation(write);
  mocks.writeText.mockImplementation((key, text) => write(key, { text }));
  let current: ReturnType<typeof useNewTaskFlow>;
  function Probe() {
    const value = useNewTaskFlow();
    useLayoutEffect(() => {
      current = value;
    });
    return null;
  }
  const root = createRoot(document.createElement("div"));
  return {
    projects,
    snapshot,
    get current() {
      return current;
    },
    async render(active: boolean) {
      await act(async () =>
        root.render(
          <RegistryContext.Provider value={registry}>
            <NewTaskFlowProvider active={active} flowKey="test-flow">
              <Probe />
            </NewTaskFlowProvider>
          </RegistryContext.Provider>,
        ),
      );
    },
    [Symbol.dispose]() {
      act(() => root.unmount());
      registry.dispose();
    },
  };
}

it("creates a draft only while active and carries it across project and environment changes", async () => {
  using screen = setup();
  await screen.render(false);
  expect(screen.current.selectedProject).toEqual(screen.projects[0]);
  expect(mocks.createDraft).not.toHaveBeenCalled();

  await screen.render(true);
  const draftKey = screen.current.draftKey!;
  act(() => screen.current.setPrompt("keep this draft"));

  await act(async () => screen.current.setProject(screen.projects[1]!));
  expect(screen.current.prompt).toBe("keep this draft");
  expect(screen.current.draftKey).toBe(draftKey);
  await act(async () => screen.current.selectEnvironment(EnvironmentId.make("two")));
  expect(screen.current.selectedProject).toEqual(screen.projects[2]);
  expect(screen.snapshot(draftKey).project).toEqual({
    environmentId: "two",
    projectId: "b",
  });

  await screen.render(false);
  await screen.render(true);
  expect(screen.current.draftKey).toBe(draftKey);
  expect(screen.current.prompt).toBe("keep this draft");
});
