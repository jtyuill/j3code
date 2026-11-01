import * as NodeModule from "node:module";
import type { EnvironmentConnectionPhase } from "@t3tools/client-runtime/connection";
import { workspaceDetailRouter } from "../workspace/workspace-detail-selection";
import type { NavigationState } from "@react-navigation/native";
import { AuthOrchestrationOperateScope } from "@t3tools/contracts";
import * as Cause from "effect/Cause";
import { AsyncResult } from "effect/reactivity";
import { act, createElement, isValidElement, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";

const state = vi.hoisted(() => ({
  scopes: new Set<string>(),
  baseDirectory: "",
  projects: [] as string[],
  existingProjects: [] as {
    environmentId: string;
    id: string;
    title: string;
    workspaceRoot: string;
  }[],
  createResult: null as Promise<unknown> | null,
  connectionStates: {} as Record<string, EnvironmentConnectionPhase>,
  waitForProject: vi.fn(),
  dispatch: vi.fn(),
  alert: vi.fn(),
  navigationState: undefined as NavigationState | undefined,
  environments: [{ environmentId: "environment", environmentLabel: "Environment" }],
}));

function loadRouters() {
  const requireNavigation = NodeModule.createRequire(
    NodeModule.createRequire(import.meta.url).resolve("@react-navigation/native/package.json"),
  );
  return requireNavigation("@react-navigation/routers") as {
    StackRouter: typeof import("@react-navigation/native").StackRouter;
    StackActions: typeof import("@react-navigation/native").StackActions;
    CommonActions: typeof import("@react-navigation/native").CommonActions;
  };
}

vi.mock("../workspace/useSelectWorkspaceDetail", async () => {
  const { selectWorkspaceDetail } = await import("../workspace/workspace-detail-selection");
  return {
    useSelectWorkspaceDetail: () => (detail: Parameters<typeof selectWorkspaceDetail>[0]) =>
      state.dispatch(selectWorkspaceDetail(detail)),
  };
});
vi.mock("../../native/StackHeader", () => ({
  nativeHeaderScrollEdgeEffects: () => undefined,
}));
vi.mock("../../components/ui/Button", () => ({ Button: "Button" }));
vi.mock("../../components/ui/ScreenScrollView", () => ({ ScreenScrollView: "ScrollView" }));
vi.mock("../settings/components/SettingsScreen", () => ({ SettingsScreen: "SettingsScreen" }));
vi.mock("react-native", () => ({
  Platform: { OS: "ios" },
  ActivityIndicator: "ActivityIndicator",
  Alert: { alert: state.alert },
  Pressable: "Pressable",
  ScrollView: "ScrollView",
  View: "View",
}));
vi.mock("@react-navigation/native", () => ({
  useNavigation: () => ({
    dispatch: state.dispatch,
    getState: () =>
      state.navigationState ?? {
        index: 3,
        routes: [
          { key: "home", name: "Home" },
          { key: "draft", name: "Thread", params: { draft: {} } },
          { key: "picker", name: "NewTaskProject" },
          { key: "add", name: "AddProject" },
        ],
      },
  }),
  CommonActions: { reset: (input: unknown) => input },
  StackActions: {
    push: (name: string, params: unknown) => ({ type: "PUSH", payload: { name, params } }),
  },
}));
vi.mock("react-native-safe-area-context", () => ({
  useSafeAreaInsets: () => ({ bottom: 0 }),
}));
vi.mock("../../components/ui/AppSymbol", () => ({ SymbolView: "SymbolView" }));
vi.mock("../../components/ui/AppText", () => ({ AppText: "Text", AppTextInput: "TextInput" }));
vi.mock("../../components/EnvironmentMachineSymbol", () => ({
  EnvironmentMachineSymbol: "EnvironmentMachineSymbol",
}));
vi.mock("../../components/ui/ErrorBanner", () => ({ ErrorBanner: "ErrorBanner" }));
vi.mock("../../lib/uuid", () => ({ uuidv4: () => "project" }));
vi.mock("../../state/session", () => ({
  useEnvironmentScope: (_environmentId: string, scope: string) => state.scopes.has(scope),
  readEnvironmentScope: (_environmentId: string, scope: string) => state.scopes.has(scope),
}));
vi.mock("../../state/entities", () => ({
  useProjects: () => state.existingProjects,
  waitForProject: state.waitForProject,
  useServerConfigs: () =>
    new Map([
      [
        "environment",
        {
          environment: { platform: { os: "linux" }, capabilities: {} },
          settings: { addProjectBaseDirectory: state.baseDirectory },
        },
      ],
    ]),
}));
vi.mock("../../state/use-remote-environment-registry", () => ({
  useRemoteEnvironmentRuntime: () => ({ connectionState: "connected" }),
  useRemoteConnectionStatus: () => ({
    connectedEnvironments: state.environments.map((environment) => ({
      environmentId: environment.environmentId,
      connectionState: state.connectionStates[environment.environmentId] ?? "connected",
    })),
  }),
  useSavedRemoteConnections: () => ({
    savedConnectionsById: Object.fromEntries(
      state.environments.map((environment) => [environment.environmentId, environment]),
    ),
  }),
}));
vi.mock("../../state/use-atom-command", () => ({ useAtomCommand: (command: unknown) => command }));
vi.mock("../../state/use-atom-query-runner", () => ({ useAtomQueryRunner: () => () => {} }));
vi.mock("../../state/query", () => ({ useEnvironmentQuery: () => ({ data: null }) }));
vi.mock("../../state/presentation", () => ({
  useEnvironmentPresentation: () => ({ isReady: true, presentation: null }),
}));
vi.mock("../../state/filesystem", () => ({ filesystemEnvironment: {} }));
vi.mock("../../state/projects", () => ({
  projectEnvironment: {
    create: ({ input }: { input: { workspaceRoot: string } }) => {
      if (!state.scopes.has(AuthOrchestrationOperateScope)) {
        return Promise.resolve(
          AsyncResult.failure(Cause.fail(new Error("Project creation denied"))),
        );
      }
      state.projects.push(input.workspaceRoot);
      return state.createResult ?? Promise.resolve(AsyncResult.success(undefined));
    },
  },
}));

import { AddProjectScreen } from "./AddProjectScreen";
import { AddProjectRoute } from "./AddProjectRoute";

function findAction(
  node: ReactNode,
  label: string,
  includeDisabled = false,
): (() => unknown) | null {
  if (Array.isArray(node)) {
    for (const child of node) {
      const action = findAction(child, label, includeDisabled);
      if (action) return action;
    }
    return null;
  }
  if (
    !isValidElement<{
      label?: string;
      title?: string;
      onPress?: () => unknown;
      disabled?: boolean;
      children?: ReactNode;
    }>(node)
  ) {
    return null;
  }
  if ((node.props.label ?? node.props.title) === label && node.props.onPress) {
    return node.props.disabled && !includeDisabled ? null : node.props.onPress;
  }
  return findAction(node.props.children, label, includeDisabled);
}

function findSelectedTitle(node: ReactNode): string | null {
  if (Array.isArray(node)) {
    for (const child of node) {
      const title = findSelectedTitle(child);
      if (title) return title;
    }
    return null;
  }
  if (!isValidElement<{ title?: string; selected?: boolean; children?: ReactNode }>(node)) {
    return null;
  }
  if (node.props.selected === true && node.props.title) return node.props.title;
  return findSelectedTitle(node.props.children);
}

const mountedRoots: ReturnType<typeof createRoot>[] = [];
afterEach(() => {
  for (const root of mountedRoots.splice(0)) act(() => root.unmount());
});
function renderScreen(render: () => ReactNode) {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  const root = createRoot(document.createElement("div"));
  mountedRoots.push(root);
  let screen: ReactNode;
  function Observer() {
    screen = render();
    return null;
  }
  act(() => root.render(createElement(Observer)));
  return screen;
}

function mountProjectFlow(withDraft: boolean, withChooser = true) {
  const { StackRouter, StackActions, CommonActions } = loadRouters();
  const base = StackRouter({});
  const router = { ...base, ...workspaceDetailRouter(base) };
  const options = {
    routeNames: ["Home", "Thread", "NewTaskProject", "AddProject"],
    routeParamList: {},
    routeGetIdList: {},
  };
  let current = router.getInitialState(options);
  const home = current.routes[0]!;
  const apply = (action: Parameters<typeof router.getStateForAction>[1]) => {
    const next = router.getStateForAction(current, action, options);
    if (!next || next.stale !== false) throw new Error("Navigation action rejected");
    current = next;
    state.navigationState = current;
  };
  if (withDraft) {
    apply(StackActions.push("Thread", { draft: { projectId: "previous-project" } }));
  }
  for (const name of [...(withChooser ? ["NewTaskProject"] : []), "AddProject"]) {
    apply(StackActions.push(name));
  }
  state.navigationState = current;
  state.dispatch.mockImplementation((action) =>
    apply(
      action.type === "PUSH" || action.type === "SELECT_WORKSPACE_DETAIL"
        ? action
        : CommonActions.reset(action),
    ),
  );
  return {
    home,
    getState: () => current,
    back: () => apply(StackActions.pop()),
    setParams: (params: object) => apply(CommonActions.setParams(params)),
    push: (name: string) => apply(StackActions.push(name)),
  };
}

const TWO_ENVIRONMENTS = [
  { environmentId: "environment", environmentLabel: "Environment" },
  { environmentId: "another-environment", environmentLabel: "Another environment" },
];

function resetEnvironments() {
  state.connectionStates = {};
  state.environments = [{ environmentId: "environment", environmentLabel: "Environment" }];
}

describe("add project environment", () => {
  it("blocks adding on a requested unavailable host until reconnect or manual choice", async () => {
    state.scopes = new Set([AuthOrchestrationOperateScope]);
    state.environments = TWO_ENVIRONMENTS;
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    const root = createRoot(document.createElement("div"));
    let screen: ReactNode;
    function Observer() {
      screen = AddProjectScreen({ environmentId: "environment" });
      return null;
    }
    try {
      for (const phase of ["available", "offline", "error"] as const) {
        state.connectionStates = { environment: phase };
        await act(() => root.render(createElement(Observer)));
        expect(findAction(screen, "Add project")).toBeNull();
        expect(findSelectedTitle(screen)).toBe("Environment");
      }
      state.connectionStates = {};
      await act(() => root.render(createElement(Observer)));
      expect(findAction(screen, "Add project")).not.toBeNull();
      state.connectionStates = { environment: "offline" };
      await act(() => root.render(createElement(Observer)));
      expect(findAction(screen, "Add project")).toBeNull();
      const select = findAction(screen, "Another environment");
      if (!select) throw new Error("Connected environment selection missing");
      await act(() => select());
      expect(findAction(screen, "Add project")).not.toBeNull();
      // The offline host drops out once deselected, leaving one host and no picker.
      expect(findAction(screen, "Environment", true)).toBeNull();
    } finally {
      await act(() => root.unmount());
      resetEnvironments();
    }
  });

  it("keeps a manual host selection when the requested host finishes connecting", async () => {
    state.scopes = new Set([AuthOrchestrationOperateScope]);
    state.environments = TWO_ENVIRONMENTS;
    state.connectionStates = { environment: "connecting" };
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    const root = createRoot(document.createElement("div"));
    let screen: ReactNode;
    function Observer() {
      screen = AddProjectScreen({ environmentId: "environment" });
      return null;
    }
    try {
      await act(() => root.render(createElement(Observer)));
      expect(findAction(screen, "Add project")).toBeNull();
      const select = findAction(screen, "Another environment");
      if (!select) throw new Error("Connected environment selection missing");
      await act(() => select());
      state.connectionStates = {};
      await act(() => root.render(createElement(Observer)));
      expect(findAction(screen, "Add project")).not.toBeNull();
      expect(findSelectedTitle(screen)).toBe("Another environment");
    } finally {
      await act(() => root.unmount());
      resetEnvironments();
    }
  });

  it("follows warm environment requests while preserving manual selection between requests", async () => {
    state.scopes = new Set([AuthOrchestrationOperateScope]);
    state.environments = TWO_ENVIRONMENTS;
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    const root = createRoot(document.createElement("div"));
    let screen: ReactNode;
    function RouteObserver(props: { environmentId?: string | string[] }) {
      const route = AddProjectRoute({ route: { params: props } });
      screen = AddProjectScreen(route.props);
      return null;
    }
    const renderRequest = (environmentId: string | string[]) =>
      act(() => root.render(createElement(RouteObserver, { environmentId })));
    try {
      await renderRequest("environment");
      expect(findSelectedTitle(screen)).toBe("Environment");
      await renderRequest("another-environment");
      expect(findSelectedTitle(screen)).toBe("Another environment");
      const select = findAction(screen, "Environment");
      if (!select) throw new Error("Environment selection missing");
      await act(() => select());
      await renderRequest(["another-environment"]);
      expect(findSelectedTitle(screen)).toBe("Environment");
      await renderRequest("environment");
      await renderRequest("another-environment");
      expect(findSelectedTitle(screen)).toBe("Another environment");
    } finally {
      await act(() => root.unmount());
      resetEnvironments();
    }
  });
});

describe("add project navigation", () => {
  function setup() {
    state.navigationState = undefined;
    state.scopes = new Set([AuthOrchestrationOperateScope]);
    state.baseDirectory = "/tmp/t3-mobile-audit-fixture";
    state.projects = [];
    state.dispatch.mockReset();
    state.alert.mockReset();
    state.waitForProject.mockReset();
    const created = Promise.withResolvers<ReturnType<typeof AsyncResult.success<void>>>();
    const projected = Promise.withResolvers<unknown>();
    state.createResult = created.promise;
    state.waitForProject.mockReturnValue(projected.promise);
    const submit = findAction(
      renderScreen(() => AddProjectScreen({ environmentId: "environment" })),
      "Add project",
    );
    if (!submit) throw new Error("Add project action missing");
    return { created, projected, submit };
  }

  it("does not create a project when the connection cannot operate", async () => {
    const { submit } = setup();
    state.scopes = new Set();
    await submit();

    expect(state.projects).toEqual([]);
    expect(state.alert).toHaveBeenCalledOnce();
  });

  it("keeps navigation in place and explains a project projection timeout", async () => {
    const { created, projected, submit } = setup();
    const submission = submit();
    created.resolve(AsyncResult.success(undefined));
    projected.resolve(null);
    await submission;

    expect(state.projects).toEqual(["/tmp/t3-mobile-audit-fixture"]);
    expect(state.dispatch).not.toHaveBeenCalled();
    expect(state.alert).toHaveBeenCalledExactlyOnceWith(
      "Project added",
      "The project was created but has not reached this device yet. It will appear in the project list once the connection catches up.",
    );
  });

  it("does not replace a reopened flow when the previous submission finishes", async () => {
    const { created, projected, submit } = setup();
    const navigation = mountProjectFlow(false);
    const submission = submit();
    created.resolve(AsyncResult.success(undefined));
    await created.promise;
    navigation.back();
    navigation.push("AddProject");
    const remaining = navigation.getState();
    projected.resolve({ environmentId: "environment", id: "project" });
    await submission;

    expect(state.projects).toEqual(["/tmp/t3-mobile-audit-fixture"]);
    expect(navigation.getState()).toEqual(remaining);
    expect(state.dispatch).not.toHaveBeenCalled();
  });

  it("does not replace a warm-linked flow sharing the original route key", async () => {
    const { created, projected, submit } = setup();
    const navigation = mountProjectFlow(false);
    const submission = submit();
    created.resolve(AsyncResult.success(undefined));
    await created.promise;
    const originalKey = navigation.getState().routes.at(-1)!.key;
    navigation.setParams({ environmentId: "other-environment" });
    const remaining = navigation.getState();
    expect(remaining.routes.at(-1)!.key).toBe(originalKey);
    projected.resolve({ environmentId: "environment", id: "project" });
    await submission;

    expect(navigation.getState()).toEqual(remaining);
    expect(state.dispatch).not.toHaveBeenCalled();
  });

  it.each([
    { origin: "Thread", withDraft: true, withChooser: true },
    { origin: "NewTaskProject", withDraft: false, withChooser: true },
    { origin: "Home", withDraft: false, withChooser: false },
  ])(
    "waits for projection then selects the draft from $origin",
    async ({ withDraft, withChooser }) => {
      const { created, projected, submit } = setup();
      const navigation = mountProjectFlow(withDraft, withChooser);
      const before = navigation.getState();
      const submission = submit();
      created.resolve(AsyncResult.success(undefined));
      await created.promise;
      expect(state.projects).toEqual(["/tmp/t3-mobile-audit-fixture"]);
      expect(navigation.getState()).toBe(before);
      projected.resolve({ environmentId: "environment", id: "project" });
      await submission;
      expect(state.dispatch).toHaveBeenCalledTimes(1);
      expect(navigation.getState().routes.map((route) => route.name)).toEqual(["Home", "Thread"]);
      expect(navigation.getState().routes[1]?.params).toEqual({
        draft: {
          environmentId: "environment",
          projectId: "project",
          title: "t3-mobile-audit-fixture",
        },
      });
      navigation.back();
      expect(navigation.getState().routes).toEqual([navigation.home]);
    },
  );
});

describe("local folder path", () => {
  function findPathInput(
    node: ReactNode,
  ): { value: string; onChangeText: (value: string) => void } | null {
    if (Array.isArray(node)) {
      for (const child of node) {
        const input = findPathInput(child);
        if (input) return input;
      }
      return null;
    }
    if (
      !isValidElement<{
        value?: string;
        onChangeText?: (value: string) => void;
        onSubmit?: () => void;
        children?: ReactNode;
      }>(node)
    ) {
      return null;
    }
    const { value, onChangeText, onSubmit } = node.props;
    if (typeof value === "string" && onChangeText && onSubmit) return { value, onChangeText };
    return findPathInput(node.props.children);
  }

  function mountLocalFolder() {
    state.scopes = new Set([AuthOrchestrationOperateScope]);
    state.baseDirectory = "/work";
    state.connectionStates = {};
    state.environments = [
      { environmentId: "environment", environmentLabel: "Environment" },
      { environmentId: "another-environment", environmentLabel: "Another environment" },
    ];
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    const root = createRoot(document.createElement("div"));
    const renderedPaths: string[] = [];
    let input: ReturnType<typeof findPathInput> = null;
    function LocalFolderObserver(props: { environmentId?: string }) {
      input = findPathInput(AddProjectScreen(props));
      if (input) renderedPaths.push(input.value);
      return null;
    }
    return {
      renderedPaths,
      get path() {
        return input?.value;
      },
      type: (value: string) => act(() => input!.onChangeText(value)),
      render: (environmentId?: string) =>
        act(() => root.render(createElement(LocalFolderObserver, { environmentId }))),
      [Symbol.asyncDispose]: async () => {
        await act(() => root.unmount());
        state.connectionStates = {};
        state.environments = [{ environmentId: "environment", environmentLabel: "Environment" }];
      },
    };
  }

  it("shows the new host's default path on the first render after the host changes", async () => {
    await using screen = mountLocalFolder();
    await screen.render("environment");
    await screen.type("/work/edited");
    screen.renderedPaths.length = 0;

    await screen.render("another-environment");

    expect(screen.renderedPaths).not.toContain("/work/edited");
    expect(screen.path).toBe("~/");
  });

  it("keeps each host's typed path when switching between hosts", async () => {
    await using screen = mountLocalFolder();
    await screen.render("environment");
    await screen.type("/work/a");
    await screen.render("another-environment");
    await screen.type("~/b");

    await screen.render("environment");
    expect(screen.path).toBe("/work/a");
    await screen.render("another-environment");
    expect(screen.path).toBe("~/b");
  });

  it("keeps a typed path when the default host drops out and comes back", async () => {
    await using screen = mountLocalFolder();
    await screen.render();
    expect(screen.path).toBe("~/");
    await screen.type("~/code/app");

    state.connectionStates = { "another-environment": "offline" };
    await screen.render();
    expect(screen.path).toBe("/work/");

    state.connectionStates = {};
    await screen.render();
    expect(screen.path).toBe("~/code/app");
  });
});
