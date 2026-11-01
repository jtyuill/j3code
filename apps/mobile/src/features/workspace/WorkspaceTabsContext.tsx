import { RegistryContext, useAtomValue } from "@effect/atom-react";
import { Atom } from "effect/reactivity";
import { createContext, use, useState, type Dispatch, type ReactNode } from "react";

import {
  initialWorkspaceTabsState,
  workspaceTabsReducer,
  type WorkspaceSearchTab,
  type WorkspaceTab,
  type WorkspaceTabsAction,
  type WorkspaceTabsState,
} from "./workspace-tabs-state";

export const WorkspaceTabsContext = createContext<{
  readonly state: WorkspaceTabsState;
  readonly dispatch: Dispatch<WorkspaceTabsAction>;
  readonly isTabActive: (tab: WorkspaceTab) => boolean;
  readonly bottomClearance: number;
  readonly settingsOpen: boolean;
  readonly getScrollOffset: (tab: WorkspaceSearchTab) => number;
  readonly recordScrollOffset: (tab: WorkspaceSearchTab, offset: number) => void;
} | null>(null);

export function useWorkspaceTabs() {
  return use(WorkspaceTabsContext);
}

// One model per provider. Handlers on frozen screens keep stale closures, so
// they read the current selection from the registry at call time.
function createWorkspaceTabsModel() {
  const scrollOffsets: Record<WorkspaceSearchTab, number> = {
    threads: 0,
    settings: 0,
  };
  return {
    stored: Atom.make(initialWorkspaceTabsState()),
    getScrollOffset: (tab: WorkspaceSearchTab) => scrollOffsets[tab],
    recordScrollOffset: (tab: WorkspaceSearchTab, offset: number) => {
      scrollOffsets[tab] = offset;
    },
  };
}

export function WorkspaceTabsProvider(props: {
  readonly children: ReactNode;
  readonly settingsOpen?: boolean;
}) {
  const registry = use(RegistryContext);
  const [model] = useState(createWorkspaceTabsModel);
  const state = useAtomValue(model.stored);
  const context = {
    state,
    dispatch: (action: WorkspaceTabsAction) =>
      registry.set(model.stored, workspaceTabsReducer(registry.get(model.stored), action)),
    isTabActive: (tab: WorkspaceTab) => registry.get(model.stored).selectedTab === tab,
    bottomClearance: 0,
    settingsOpen: props.settingsOpen ?? false,
    getScrollOffset: model.getScrollOffset,
    recordScrollOffset: model.recordScrollOffset,
  };
  return <WorkspaceTabsContext value={context}>{props.children}</WorkspaceTabsContext>;
}
