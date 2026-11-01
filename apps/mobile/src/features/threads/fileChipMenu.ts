import { formatFilePathForClipboard } from "@t3tools/client-runtime/media-actions";
import { resolveMarkdownLinkPresentation } from "../../lib/markdownLinks";
import type { MarkdownFileContextMenu } from "../../native/SelectableMarkdownText.types";

import {
  isAbsolutePath,
  resolveFileLinkPath,
  resolveWorkspaceFilePath,
  resolveWorkspaceRelativeFilePath,
} from "../../lib/filePath";

export interface FileChipTarget {
  readonly fullPath?: string;
  readonly relativePath?: string;
}

export function fileChipClipboardPath(
  target: FileChipTarget,
  action: "copy-full-path" | "copy-relative-path",
): string | undefined {
  const path = action === "copy-full-path" ? target.fullPath : target.relativePath;
  return path === undefined ? undefined : formatFilePathForClipboard(path, target.fullPath);
}

export function resolveFileChipTarget(
  href: string,
  workspaceRoot: string | null | undefined,
): FileChipTarget | null {
  const presentation = resolveMarkdownLinkPresentation(href);
  if (presentation.kind !== "file") return null;
  const path = resolveFileLinkPath(workspaceRoot, presentation.path);
  if (path === null) return null;
  const relativePath = resolveWorkspaceRelativeFilePath(workspaceRoot, path);
  const fullPath = isAbsolutePath(path)
    ? path
    : workspaceRoot && relativePath
      ? resolveWorkspaceFilePath(workspaceRoot, relativePath)
      : undefined;
  if (!fullPath && !relativePath) return null;
  return {
    ...(fullPath ? { fullPath } : {}),
    ...(relativePath ? { relativePath } : {}),
  };
}

export function fileChipMenu(target: FileChipTarget): MarkdownFileContextMenu {
  return {
    title: target.fullPath ?? target.relativePath ?? "",
    actions: [
      ...(target.fullPath ? [{ id: "copy-full-path", title: "Copy full path" }] : []),
      ...(target.relativePath ? [{ id: "copy-relative-path", title: "Copy relative path" }] : []),
    ],
  };
}
