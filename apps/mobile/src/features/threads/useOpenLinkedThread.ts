import { StackActions, useNavigation, type NavigationState } from "@react-navigation/native";

type ThreadTarget = { readonly environmentId: string; readonly threadId: string };

function routeIsThread(route: NavigationState["routes"][number] | undefined, target: ThreadTarget) {
  const params = route?.params as ReactNavigation.RootParamList["Thread"] | undefined;
  return (
    route?.name === "Thread" &&
    params?.environmentId === target.environmentId &&
    params?.threadId === target.threadId
  );
}

/**
 * How a thread link opened from inside a thread reaches its target. `navigate` would reuse the
 * current Thread route, so Back would skip the thread the link was in and land on the list.
 */
export function linkedThreadStep(
  state: NavigationState,
  target: ThreadTarget,
): "stay" | "pop" | "push" {
  if (routeIsThread(state.routes[state.index], target)) return "stay";
  return routeIsThread(state.routes[state.index - 1], target) ? "pop" : "push";
}

export function useOpenLinkedThread() {
  const navigation = useNavigation();
  return (target: ThreadTarget) => {
    const state = navigation.getState();
    const step = state ? linkedThreadStep(state, target) : "push";
    if (step === "pop") navigation.dispatch(StackActions.pop());
    else if (step === "push") navigation.dispatch(StackActions.push("Thread", target));
  };
}
