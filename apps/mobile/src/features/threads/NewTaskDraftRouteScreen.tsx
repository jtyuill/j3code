import type { StaticScreenProps } from "@react-navigation/native";

import { NewTaskDraftScreen } from "./NewTaskDraftScreen";

import type { NewTaskDraftRouteParams } from "./thread-route-state";
export type { NewTaskDraftRouteParams } from "./thread-route-state";

const first = (value: string | string[] | undefined) => (Array.isArray(value) ? value[0] : value);

export function NewTaskDraftRouteScreen({ route }: StaticScreenProps<NewTaskDraftRouteParams>) {
  const params = route.params ?? {};
  const environmentId = first(params.environmentId);
  const projectId = first(params.projectId);
  // NewTaskDraftScreen re-applies the route project whenever this object's
  // identity changes; the compiler memoizes it on the two ids.
  const initialProjectRef = { environmentId, projectId };
  return (
    <NewTaskDraftScreen initialProjectRef={initialProjectRef} draftId={first(params.draftId)} />
  );
}
