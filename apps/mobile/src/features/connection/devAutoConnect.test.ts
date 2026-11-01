import * as NodeModule from "node:module";
import type { NavigationState, PartialState } from "@react-navigation/native";
import { expect, it, vi } from "vite-plus/test";

const requireFromNative = NodeModule.createRequire(
  NodeModule.createRequire(import.meta.url).resolve("@react-navigation/native/package.json"),
);
const requireFromCore = NodeModule.createRequire(
  requireFromNative.resolve("@react-navigation/core/package.json"),
);
const { getActionFromState, getStateFromPath } = requireFromNative("@react-navigation/core") as {
  readonly getActionFromState: typeof import("@react-navigation/native").getActionFromState;
  readonly getStateFromPath: typeof import("@react-navigation/native").getStateFromPath;
};
const { StackRouter } = requireFromCore("@react-navigation/routers") as {
  readonly StackRouter: typeof import("@react-navigation/native").StackRouter;
};

const harness = vi.hoisted(() => ({
  connect: vi.fn(),
  goBack: vi.fn(),
  dispatch: vi.fn(),
  columns: false,
  // The root stack the form sits on when the connect succeeds.
  rootState: {
    key: "root",
    index: 2,
    routes: [
      { key: "home", name: "Home" },
      { key: "settings", name: "Settings" },
      { key: "new", name: "SettingsEnvironmentNew" },
    ],
  },
  // Navigation that is not ready yet holds callbacks here until the test flushes them.
  deferred: null as Array<() => void> | null,
}));

vi.mock("@react-navigation/native", () => ({
  StackActions: { replace: vi.fn() },
  CommonActions: {
    replaceParams: (params: object) => ({ type: "REPLACE_PARAMS", payload: { params } }),
  },
}));
vi.mock("../../state/use-remote-environment-registry", () => ({
  connectHost: harness.connect,
}));
vi.mock("../workspace/workspace-selection-store", () => ({
  workspaceHasColumns: () => harness.columns,
}));
vi.mock("../../lib/rootNavigation", () => ({
  rootNavigationRef: {
    getCurrentRoute: () => harness.rootState.routes[harness.rootState.index],
    getRootState: () => harness.rootState,
    getState: () => harness.rootState,
    getParent: () => undefined,
    canGoBack: () => true,
    goBack: harness.goBack,
    dispatch: harness.dispatch,
  },
  whenRootNavigationReady: (navigate: () => void) => {
    if (harness.deferred) harness.deferred.push(navigate);
    else navigate();
  },
}));

import { appAtomRegistry } from "../../state/atom-registry";
import {
  autoConnectReport,
  devAutoConnectAttemptAtom,
  hostFormLinkAction,
  startDevAutoConnect,
} from "./devAutoConnect";

const HOST = "http://192.168.1.10:3773";

function settingsLink(params: object): PartialState<NavigationState> {
  return {
    routes: [
      { name: "Home" },
      { name: "Settings", state: { routes: [{ name: "SettingsEnvironmentNew", params }] } },
    ],
  };
}

it("connects from a nested settings link once, reports failure, and closes the form on success", async () => {
  vi.stubGlobal("__DEV__", true);
  const failed = Promise.withResolvers<{ ok: boolean; error: string | null }>();
  harness.connect.mockReturnValueOnce(failed.promise);

  const state = settingsLink({ host: ` ${HOST} `, autoConnect: "1" });
  expect(startDevAutoConnect(state)).toBe(state);
  expect(harness.connect).toHaveBeenCalledExactlyOnceWith(HOST);
  expect(appAtomRegistry.get(devAutoConnectAttemptAtom)).toEqual({
    host: HOST,
    status: "connecting",
  });

  failed.resolve({ ok: false, error: "Connection refused." });
  await failed.promise;
  await Promise.resolve();
  expect(appAtomRegistry.get(devAutoConnectAttemptAtom)).toEqual({
    host: HOST,
    status: "failed",
    error: "Connection refused.",
  });
  expect(harness.goBack).not.toHaveBeenCalled();

  const succeeded = Promise.withResolvers<{ ok: boolean; error: string | null }>();
  harness.connect.mockReturnValueOnce(succeeded.promise);
  startDevAutoConnect(settingsLink({ host: HOST, autoConnect: "true" }));
  succeeded.resolve({ ok: true, error: null });
  await succeeded.promise;
  await Promise.resolve();
  expect(appAtomRegistry.get(devAutoConnectAttemptAtom)).toBeNull();
  expect(harness.goBack).toHaveBeenCalledOnce();
});

it("only prefills without the flag, and ignores the flag in production builds", () => {
  harness.connect.mockClear();
  vi.stubGlobal("__DEV__", true);
  startDevAutoConnect(settingsLink({ host: HOST }));
  vi.stubGlobal("__DEV__", false);
  startDevAutoConnect(settingsLink({ host: HOST, autoConnect: "1" }));
  expect(harness.connect).not.toHaveBeenCalled();
});

it("forgets a failed auto-connect once the form is opened again without the flag", async () => {
  vi.stubGlobal("__DEV__", true);
  const failed = Promise.withResolvers<{ ok: boolean; error: string | null }>();
  harness.connect.mockReturnValueOnce(failed.promise);
  startDevAutoConnect(settingsLink({ host: HOST, autoConnect: "1" }));
  failed.resolve({ ok: false, error: "Connection refused." });
  await failed.promise;
  await Promise.resolve();
  expect(appAtomRegistry.get(devAutoConnectAttemptAtom)).toMatchObject({ status: "failed" });

  startDevAutoConnect(settingsLink({ host: HOST }));
  expect(appAtomRegistry.get(devAutoConnectAttemptAtom)).toBeNull();
});

it("drops the result of a connect that a later link superseded", async () => {
  vi.stubGlobal("__DEV__", true);
  const stale = Promise.withResolvers<{ ok: boolean; error: string | null }>();
  harness.connect.mockReturnValueOnce(stale.promise);
  startDevAutoConnect(settingsLink({ host: HOST, autoConnect: "1" }));
  startDevAutoConnect(settingsLink({ host: HOST }));

  stale.resolve({ ok: false, error: "Connection refused." });
  await stale.promise;
  await Promise.resolve();
  expect(appAtomRegistry.get(devAutoConnectAttemptAtom)).toBeNull();
});

it("keeps Connect busy while the auto-connect runs, even after the user edits the form", () => {
  const connecting = { host: HOST, status: "connecting" } as const;
  expect(autoConnectReport(connecting, HOST, true)).toEqual({
    connecting: true,
    error: null,
  });
  const failed = { host: HOST, status: "failed", error: "Expired." } as const;
  expect(autoConnectReport(failed, HOST, false)).toEqual({
    connecting: false,
    error: "Expired.",
  });
  expect(autoConnectReport(failed, HOST, true).error).toBeNull();
  expect(autoConnectReport(connecting, "http://other:3773", false).connecting).toBe(false);
});

async function connectSuccessfully(link: PartialState<NavigationState>) {
  const succeeded = Promise.withResolvers<{ ok: boolean; error: string | null }>();
  harness.connect.mockReturnValueOnce(succeeded.promise);
  startDevAutoConnect(link);
  succeeded.resolve({ ok: true, error: null });
  await succeeded.promise;
  await Promise.resolve();
}

it("on columns, a successful settings link shows Environments instead of an empty detail", async () => {
  vi.stubGlobal("__DEV__", true);
  harness.goBack.mockClear();
  harness.dispatch.mockClear();
  harness.columns = true;
  await connectSuccessfully(settingsLink({ host: HOST, autoConnect: "1" }));
  harness.columns = false;

  expect(harness.goBack).not.toHaveBeenCalled();
  expect(harness.dispatch).toHaveBeenCalledExactlyOnceWith({
    type: "RESET",
    target: "root",
    payload: expect.objectContaining({
      index: 2,
      routes: [
        { key: "home", name: "Home" },
        { key: "settings", name: "Settings" },
        { name: "SettingsEnvironments", params: undefined },
      ],
    }),
  });
});

it("a success that waited for navigation leaves a newer link's form open", async () => {
  vi.stubGlobal("__DEV__", true);
  harness.goBack.mockClear();
  harness.dispatch.mockClear();
  harness.deferred = [];
  await connectSuccessfully(settingsLink({ host: HOST, autoConnect: "1" }));
  startDevAutoConnect(settingsLink({ host: "http://192.168.1.11:3773" }));

  const deferred = harness.deferred;
  harness.deferred = null;
  for (const navigate of deferred) navigate();
  expect(harness.goBack).not.toHaveBeenCalled();
  expect(harness.dispatch).not.toHaveBeenCalled();
});

it("a host link fills the host form already on screen instead of stacking a second one", () => {
  const linking = {
    initialRouteName: "Home",
    screens: {
      Home: "",
      Settings: "settings",
      SettingsEnvironmentNew: "settings/environment-new",
      ConnectionsNew: "connections/new",
    },
  };
  const router = StackRouter({});
  const options = {
    routeNames: Object.keys(linking.screens),
    routeParamList: {},
    routeGetIdList: {},
  };
  // Settings > Environments > Add environment, as the user opened it.
  const open = router.getRehydratedState(
    {
      index: 2,
      routes: [{ name: "Home" }, { name: "Settings" }, { name: "SettingsEnvironmentNew" }],
    },
    options,
  );
  const link = getStateFromPath(`connections/new?host=${encodeURIComponent(HOST)}`, linking)!;
  const action = hostFormLinkAction(link, open) ?? getActionFromState(link, linking)!;
  const next = router.getStateForAction(open, { target: open.key, ...action }, options);

  expect(next?.routes.map((route) => route.name)).toEqual([
    "Home",
    "Settings",
    "SettingsEnvironmentNew",
  ]);
  expect(next?.routes.at(-1)).toMatchObject({
    key: open.routes[2]!.key,
    params: { host: HOST },
  });
});
