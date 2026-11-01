import { NATIVE_LIQUID_GLASS_SUPPORTED } from "../../native/native-glass";
import { useAppearancePreferences } from "../settings/appearance/AppearancePreferencesProvider";
import { ThreadHeader } from "./ThreadHeader";

export function DraftThreadHeader() {
  const { themeVariables } = useAppearancePreferences();
  return (
    <ThreadHeader
      draft
      title="New thread"
      subtitle=""
      headerColor={themeVariables["--color-header"]}
      usesNativeHeaderGlass={NATIVE_LIQUID_GLASS_SUPPORTED}
    />
  );
}
