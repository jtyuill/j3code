import type { StaticScreenProps } from "@react-navigation/native";
import { AddProjectScreen } from "./AddProjectScreen";

type AddProjectRouteParams = {
  readonly environmentId?: string | string[];
};

export function AddProjectRoute({ route }: StaticScreenProps<AddProjectRouteParams | undefined>) {
  return <AddProjectScreen {...(route.params ?? {})} />;
}
