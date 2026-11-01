import { ScrollView, View } from "react-native";

import { AppText as Text } from "../../../../components/ui/AppText";
import {
  resolveMarkdownFontSizes,
  resolveMobileCodeSurface,
} from "../../../../lib/appearancePreferences";
import { useAppearancePreferences } from "../AppearancePreferencesProvider";

const CODE_FONT_FAMILY = "ui-monospace";

export function AppearancePreviewSeparator() {
  return <View className="h-px bg-separator" />;
}

export function TextAppearancePreview(props: { readonly fontSize: number }) {
  const sizes = resolveMarkdownFontSizes(props.fontSize);

  return (
    <View className="gap-1 p-4">
      <Text
        className="text-foreground"
        style={{ fontSize: sizes.m, lineHeight: sizes.bodyLineHeight }}
      >
        The quick brown fox jumps over the lazy dog.
      </Text>
      <Text
        className="text-foreground-muted"
        style={{ fontSize: sizes.s, lineHeight: Math.round(sizes.s * 1.4) }}
      >
        Messages, labels, and headings scale with this size.
      </Text>
    </View>
  );
}

interface CodePreviewToken {
  readonly text: string;
  readonly keyword?: boolean;
}

interface CodePreviewLine {
  readonly id: string;
  readonly tokens: ReadonlyArray<CodePreviewToken>;
}

const CODE_PREVIEW_LINES: ReadonlyArray<CodePreviewLine> = [
  {
    id: "signature",
    tokens: [{ text: "function", keyword: true }, { text: " formatUser(user) {" }],
  },
  {
    id: "body",
    tokens: [
      { text: "  " },
      { text: "return", keyword: true },
      { text: " `${user.name} <${user.email}>` // demonstrates how long lines behave" },
    ],
  },
  { id: "close", tokens: [{ text: "}" }] },
];

export function CodeAppearancePreview(props: {
  readonly fontSize: number;
  readonly wordBreak: boolean;
}) {
  const surface = resolveMobileCodeSurface(props.fontSize);

  const lineNumber = (line: CodePreviewLine, index: number) => (
    <Text
      className="text-right text-icon-subtle"
      key={line.id}
      style={{
        fontFamily: CODE_FONT_FAMILY,
        fontSize: surface.lineNumberFontSize,
        lineHeight: surface.rowHeight,
        width: 22,
      }}
    >
      {index + 1}
    </Text>
  );

  const codeLine = (line: CodePreviewLine, wrap: boolean) => (
    <Text
      className="text-foreground"
      key={line.id}
      numberOfLines={wrap ? undefined : 1}
      style={{
        fontFamily: CODE_FONT_FAMILY,
        fontSize: surface.fontSize,
        lineHeight: surface.rowHeight,
      }}
    >
      {line.tokens.map((token) => (
        <Text
          key={token.text}
          className={token.keyword ? "text-md-link" : undefined}
          style={{
            fontFamily: CODE_FONT_FAMILY,
            fontSize: surface.fontSize,
            lineHeight: surface.rowHeight,
          }}
        >
          {token.text}
        </Text>
      ))}
    </Text>
  );

  if (props.wordBreak) {
    return (
      <View className="p-4">
        {CODE_PREVIEW_LINES.map((line, index) => (
          <View className="flex-row" key={line.id}>
            {lineNumber(line, index)}
            <View className="flex-1 pl-3">{codeLine(line, true)}</View>
          </View>
        ))}
      </View>
    );
  }

  return (
    <View className="flex-row p-4">
      <View>{CODE_PREVIEW_LINES.map((line, index) => lineNumber(line, index))}</View>
      <ScrollView
        horizontal
        contentContainerStyle={{ paddingLeft: 12 }}
        showsHorizontalScrollIndicator={false}
      >
        <View>{CODE_PREVIEW_LINES.map((line) => codeLine(line, false))}</View>
      </ScrollView>
    </View>
  );
}
