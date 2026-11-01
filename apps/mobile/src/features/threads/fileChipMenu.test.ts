import { describe, expect, it } from "vite-plus/test";

import {
  fileChipClipboardPath,
  fileChipMenu,
  resolveFileChipTarget,
} from "./fileChipMenu";

it("copies native paths from a relative file chip in a Windows workspace", () => {
  const target = resolveFileChipTarget("reports/Device Users.csv", "C:/repo");
  if (!target) throw new Error("File-chip target unavailable");
  expect(fileChipClipboardPath(target, "copy-full-path")).toBe(
    String.raw`C:\repo\reports\Device Users.csv`,
  );
  expect(fileChipClipboardPath(target, "copy-relative-path")).toBe(
    String.raw`reports\Device Users.csv`,
  );
});

describe("resolveFileChipTarget", () => {
  it("resolves a workspace-relative link to both paths", () => {
    expect(resolveFileChipTarget("src/app.ts:12", "/repo")).toEqual({
      fullPath: "/repo/src/app.ts",
      relativePath: "src/app.ts",
    });
  });

  it("resolves home links for navigation and menu actions", () => {
    expect(resolveFileChipTarget("~/.agents/AGENTS.md:314", "/home/maria/repo")).toEqual({
      fullPath: "/home/maria/.agents/AGENTS.md",
    });
    expect(resolveFileChipTarget("~/repo/src/main.ts:12", "/home/maria/repo")).toEqual({
      fullPath: "/home/maria/repo/src/main.ts",
      relativePath: "src/main.ts",
    });
    expect(resolveFileChipTarget("~/.agents/AGENTS.md", "C:\\Users\\maria\\repo")).toEqual({
      fullPath: "C:\\Users\\maria\\.agents\\AGENTS.md",
    });
  });

  it("ignores links that are not files or cannot be opened", () => {
    expect(resolveFileChipTarget("https://example.com/app.ts", "/repo")).toBeNull();
    expect(resolveFileChipTarget("~/report.md", "/repo")).toBeNull();
    expect(resolveFileChipTarget("../other/file.ts", "/repo")).toBeNull();
  });
});

describe("fileChipMenu", () => {
  it("offers only the copy actions the target has paths for", () => {
    expect(fileChipMenu(resolveFileChipTarget("src/app.ts", "/repo")!).actions).toEqual([
      { id: "copy-full-path", title: "Copy full path" },
      { id: "copy-relative-path", title: "Copy relative path" },
    ]);
    expect(fileChipMenu(resolveFileChipTarget("/tmp/report.pdf", "/repo")!).actions).toEqual([
      { id: "copy-full-path", title: "Copy full path" },
    ]);
  });
});
