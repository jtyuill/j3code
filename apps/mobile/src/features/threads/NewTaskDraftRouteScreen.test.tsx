import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vite-plus/test";

const harness = vi.hoisted(() => ({
  draftProps: [] as Array<{
    initialProjectRef: { environmentId?: string; projectId?: string };
    draftId?: string;
  }>,
}));

vi.mock("./NewTaskDraftScreen", () => ({
  NewTaskDraftScreen: (props: (typeof harness.draftProps)[number]) => {
    harness.draftProps.push(props);
    return null;
  },
}));

import { NewTaskDraftRouteScreen } from "./NewTaskDraftRouteScreen";

it("hands the draft screen the same project ref across re-renders", () => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  const root = createRoot(document.createElement("div"));
  using _root = { [Symbol.dispose]: () => act(() => root.unmount()) };
  const render = () =>
    act(() =>
      root.render(
        <NewTaskDraftRouteScreen
          route={
            {
              key: "draft-route",
              name: "NewTaskDraft",
              params: { environmentId: ["env-1"], projectId: "project-1", draftId: "draft-1" },
            } as never
          }
        />,
      ),
    );

  render();
  // The draft screen re-applies its route project when this identity changes,
  // and that write re-renders the route; an unstable ref loops forever.
  render();

  expect(harness.draftProps.at(-1)).toEqual({
    initialProjectRef: { environmentId: "env-1", projectId: "project-1" },
    draftId: "draft-1",
  });
  expect(new Set(harness.draftProps.map((props) => props.initialProjectRef)).size).toBe(1);
});
