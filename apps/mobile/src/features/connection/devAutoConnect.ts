import type { NavigationState, PartialState } from "@react-navigation/native";
import { CommonActions, StackActions } from "@react-navigation/native";
import { Atom } from "effect/reactivity";

import { findLinkedRoute, updateLinkedRoute } from "../../lib/appLinking";
import { rootNavigationRef, whenRootNavigationReady } from "../../lib/rootNavigation";
import { appAtomRegistry } from "../../state/atom-registry";
import { leaveSettingsPage } from "../settings/settings-navigation";
import { workspaceHasColumns } from "../workspace/workspace-selection-store";
import { connectHost } from "../../state/use-remote-environment-registry";

export type DevAutoConnectAttempt =
  | { readonly host: string; readonly status: "connecting" }
  | { readonly host: string; readonly status: "failed"; readonly error: string | null };

/** The connect the dev deep link started, so the form it opens can show its progress. */
export const devAutoConnectAttemptAtom = Atom.make<DevAutoConnectAttempt | null>(null).pipe(
  Atom.keepAlive,
  Atom.withLabel("mobile:dev-auto-connect-attempt"),
);

/**
 * What the form opened by `routeHost` shows of the dev auto-connect. Editing or submitting
 * (`dismissed`) hides a failure, but a connect still running keeps Add busy, so the user
 * cannot start a second connect beside it.
 */
export function autoConnectReport(
  attempt: DevAutoConnectAttempt | null,
  routeHost: string,
  dismissed: boolean,
): { readonly connecting: boolean; readonly error: string | null } {
  if (attempt === null || routeHost.length === 0 || attempt.host !== routeHost) {
    return { connecting: false, error: null };
  }
  if (attempt.status === "connecting") return { connecting: true, error: null };
  return { connecting: false, error: dismissed ? null : attempt.error };
}

// The host form is mounted both as a root sheet and inside the settings sheet.
const HOST_FORM_ROUTES = ["ConnectionsNew", "SettingsEnvironmentNew"];

function isAutoConnectFlag(value: unknown): boolean {
  return value === "1" || value === "true";
}

async function connect(host: string) {
  const attempt: DevAutoConnectAttempt = { host, status: "connecting" };
  appAtomRegistry.set(devAutoConnectAttemptAtom, attempt);
  const outcome = await connectHost(host);
  // A later link replaced or cleared this attempt; its result belongs to no open form.
  if (appAtomRegistry.get(devAutoConnectAttemptAtom) !== attempt) return;
  if (!outcome.ok) {
    appAtomRegistry.set(devAutoConnectAttemptAtom, {
      host,
      status: "failed",
      error: outcome.error,
    });
    return;
  }
  whenRootNavigationReady(() => {
    // A newer link (with or without the flag) owns the form now.
    if (appAtomRegistry.get(devAutoConnectAttemptAtom) !== attempt) return;
    appAtomRegistry.set(devAutoConnectAttemptAtom, null);
    const current = rootNavigationRef.getCurrentRoute()?.name;
    if (current === undefined || !HOST_FORM_ROUTES.includes(current)) return;
    // Same exit as the form's own Connect: return to the opener, and on columns show the
    // new environment on Environments instead of an empty detail.
    if (!rootNavigationRef.canGoBack()) rootNavigationRef.dispatch(StackActions.replace("Home"));
    else leaveSettingsPage(rootNavigationRef, workspaceHasColumns(), "SettingsEnvironments");
  });
}

/**
 * `connections/new?host=…&autoConnect=1` (or `settings/environment-new?…`) connects from the
 * link itself, so a remount of the form never connects twice. Production builds ignore the flag
 * so a link can never save a host without the user confirming it.
 */
export function startDevAutoConnect(
  state: PartialState<NavigationState> | undefined,
): PartialState<NavigationState> | undefined {
  if (!__DEV__) return state;
  const route = findLinkedRoute(state, HOST_FORM_ROUTES);
  if (route === undefined) return state;
  const params = (route.params ?? {}) as {
    readonly autoConnect?: unknown;
    readonly host?: unknown;
  };
  const host = typeof params.host === "string" ? params.host.trim() : "";
  if (isAutoConnectFlag(params.autoConnect) && host.length > 0) {
    void connect(host);
  } else {
    // Only links put a host on the form, so a form opened by any other link starts
    // clean instead of showing an earlier auto-connect's failure for the same host.
    appAtomRegistry.set(devAutoConnectAttemptAtom, null);
  }
  return state;
}

/**
 * A host link that lands on an open host form fills that form. React Navigation would push
 * `ConnectionsNew` over the Settings page's `SettingsEnvironmentNew`, stacking two copies of the
 * same form. The form keys on the link's host, so new params remount it with the prefill.
 */
export function hostFormLinkAction(
  link: PartialState<NavigationState>,
  current: NavigationState | undefined,
) {
  const linked = findLinkedRoute(link, HOST_FORM_ROUTES);
  const open = current?.routes[current.index];
  if (!linked || !current || !open || !HOST_FORM_ROUTES.includes(open.name)) return undefined;
  return {
    ...CommonActions.replaceParams(linked.params ?? {}),
    source: open.key,
    target: current.key,
  };
}
