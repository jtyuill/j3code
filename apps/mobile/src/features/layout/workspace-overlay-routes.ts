/** Root routes listed as presented over the workspace. */
const LISTED_OVERLAY_ROUTES: ReadonlySet<string> = new Set([
  "Connections",
  "ConnectionsNew",
  "NewTaskProject",
  "NewTaskAttachment",
  "NewTaskThreadSettings",
  "AddProject",
  "NewTaskEnvironment",
  "SettingsLegal",
  "ThreadAgents",
  "ThreadBackgroundWork",
  "ThreadQueue",
  "ThreadSettingsSheet",
]);

// Every route whose `presentation` is a sheet or modal, read from the root stack's config once.
let presentedRoutes: ReadonlySet<string> = new Set();

export function registerPresentedRoutes(names: Iterable<string>) {
  presentedRoutes = new Set(names);
}

/** Whether a root route is presented over the workspace (a sheet or modal). */
export function isWorkspaceOverlayRoute(name: string) {
  return LISTED_OVERLAY_ROUTES.has(name) || presentedRoutes.has(name);
}

/** The routes that make up the workspace, up to the first sheet or modal above it. */
export function workspaceRoutesOf<Route extends { readonly name: string }>(
  routes: readonly Route[],
) {
  const firstOverlay = routes.findIndex((route) => isWorkspaceOverlayRoute(route.name));
  return {
    workspace: firstOverlay < 0 ? routes : routes.slice(0, firstOverlay),
    overlays: firstOverlay < 0 ? [] : routes.slice(firstOverlay),
  };
}
