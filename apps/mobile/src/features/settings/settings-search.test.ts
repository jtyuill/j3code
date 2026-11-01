import { describe, expect, it } from "vite-plus/test";

import { filterSettingsSections } from "./settings-search";

const SECTIONS = [
  { rows: [{ id: "projects", label: "Projects" }] },
  {
    title: "Connections",
    rows: [{ id: "SettingsEnvironments", label: "Environments" }],
  },
  {
    title: "Interface",
    rows: [
      { id: "SettingsAppearance", label: "Appearance" },
      { id: "SettingsKeyboard", label: "Keyboard" },
    ],
  },
  {
    title: "Other",
    rows: [
      { id: "SettingsFollowUp", label: "Chat" },
      { id: "SettingsArchive", label: "Archive" },
    ],
  },
];

function visible(query: string) {
  return filterSettingsSections(SECTIONS, query).map((section) => [
    section.title ?? null,
    section.rows.map((row) => row.label),
  ]);
}

describe("filterSettingsSections", () => {
  it.each([
    {
      query: "",
      expected: SECTIONS.map((section) => [
        section.title ?? null,
        section.rows.map((row) => row.label),
      ]),
    },
    { query: "   ", expected: visible("") },
    { query: "appear", expected: [["Interface", ["Appearance"]]] },
    { query: "KEYBOARD", expected: [["Interface", ["Keyboard"]]] },
    { query: "dark mode", expected: [["Interface", ["Appearance"]]] },
    { query: "steer", expected: [["Other", ["Chat"]]] },
    {
      query: "other",
      expected: [["Other", ["Chat", "Archive"]]],
    },
    {
      query: "unarchive",
      expected: [["Other", ["Archive"]]],
    },
    { query: "projects", expected: [[null, ["Projects"]]] },
    { query: "zzzz", expected: [] },
  ])("$query", ({ query, expected }) => {
    expect(visible(query)).toEqual(expected);
  });

  it("drops sections that start empty", () => {
    expect(filterSettingsSections([{ rows: [] }, ...SECTIONS], "").length).toBe(SECTIONS.length);
  });
});
