import { Pressable } from "react-native";

import { SymbolView } from "../../components/ui/AppSymbol";

export function ComposerAttachmentButton(props: {
  readonly disabled?: boolean;
  readonly size?: "regular" | "large";
  readonly onPickMedia: () => Promise<void>;
}) {
  return (
    <Pressable
      accessibilityLabel="Add attachment"
      accessibilityRole="button"
      accessibilityState={{ disabled: props.disabled }}
      className="size-[44px] shrink-0 items-center justify-center rounded-full active:opacity-70 disabled:opacity-50"
      disabled={props.disabled}
      onPress={() => void props.onPickMedia()}
    >
      <SymbolView
        name="plus"
        size={props.size === "large" ? 26 : 20}
        weight="regular"
        tintColorClassName="accent-icon"
        type="monochrome"
      />
    </Pressable>
  );
}
