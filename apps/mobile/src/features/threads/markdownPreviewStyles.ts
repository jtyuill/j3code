import { FONT_FAMILIES } from "../../lib/fontFamilies";
import { resolveNativeMarkdownTypography } from "../../lib/appearancePreferences";
import { useUniwindTheme } from "../../lib/useUniwindTheme";
import { useAppearancePreferences } from "../settings/appearance/AppearancePreferencesProvider";
import type { NativeMarkdownTextStyle } from "../../native/SelectableMarkdownText";

export interface MarkdownPreviewStyles {
  readonly nativeTextStyle: NativeMarkdownTextStyle;
}

export function useMarkdownPreviewStyles(): MarkdownPreviewStyles {
  const { appearance } = useAppearancePreferences();
  const nativeMarkdownTypography = resolveNativeMarkdownTypography(appearance.baseFontSize);
  const theme = useUniwindTheme();
  const body = theme["--color-md-body"];
  const strong = theme["--color-md-strong"];
  const link = theme["--color-md-link"];
  const blockquoteBorder = theme["--color-md-blockquote-border"];
  const codeBackground = theme["--color-md-code-bg"];
  const codeText = theme["--color-md-code-text"];
  const horizontalRule = theme["--color-md-hr"];
  const regularFontFamily = FONT_FAMILIES.regular;
  const boldFontFamily = FONT_FAMILIES.bold;

  return {
    nativeTextStyle: {
      color: body,
      strongColor: strong,
      mutedColor: body,
      linkColor: link,
      inlineCodeColor: codeText,
      codeColor: codeText,
      codeBackgroundColor: codeBackground,
      codeBlockBackgroundColor: codeBackground,
      fileTextColor: codeText,
      quoteMarkerColor: blockquoteBorder,
      dividerColor: horizontalRule,
      fontSize: nativeMarkdownTypography.fontSize,
      lineHeight: nativeMarkdownTypography.lineHeight,
      headingFontSizes: nativeMarkdownTypography.headingFontSizes,
      fontFamily: regularFontFamily,
      headingFontFamily: boldFontFamily,
      boldFontFamily,
    },
  };
}
