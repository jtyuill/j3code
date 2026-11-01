import { NATIVE_WORKSPACE_COLUMNS_SUPPORTED } from "../native/NativeWorkspaceColumns";
import type { NativeStackNavigationOptions } from "@react-navigation/native-stack";
import { Platform, View } from "react-native";

import { AppText as Text } from "./ui/AppText";
import { IPAD_HOME_TITLE_OFFSET } from "../lib/layoutMetrics";

const BRAND_MAX_FONT_SCALE = 1.4;

export function brandTitleOffset(): number {
  return Platform.OS === "ios" && Platform.isPad && !NATIVE_WORKSPACE_COLUMNS_SUPPORTED ? IPAD_HOME_TITLE_OFFSET : 0;
}

export function CompactBrandTitle(
  props: {
    readonly allowFontScaling?: boolean;
    readonly accessibilityLabel?: string;
    /** Fills the row instead of sizing to the title. */
    readonly fit?: boolean;
  } = {},
) {
  const title = (
    <View
      aria-level={1}
      accessibilityLabel={props.accessibilityLabel ?? "j3, Threads"}
      accessible
      role="heading"
      className="flex-row items-center"
      style={[{ marginLeft: brandTitleOffset() }, props.fit && { flexShrink: 0 }]}
    >
      <Text
        allowFontScaling={props.allowFontScaling}
        maxFontSizeMultiplier={BRAND_MAX_FONT_SCALE}
        className="font-t3-bold text-foreground"
        style={{ fontSize: 21, letterSpacing: -0.5 }}
      >
        j3
      </Text>
    </View>
  );

  return props.fit ? <View className="min-w-0 flex-1 flex-row items-center">{title}</View> : title;
}

function renderCompactBrandTitle() {
  return <CompactBrandTitle allowFontScaling />;
}

export function getCompactBrandHeaderOptions(
  fallbackTitleStyle?: NativeStackNavigationOptions["headerTitleStyle"],
): NativeStackNavigationOptions {
  return {
    headerTitle: renderCompactBrandTitle,
    headerTitleStyle: fallbackTitleStyle,
    title: "Threads",
    unstable_headerLeftItems: undefined,
  };
}
