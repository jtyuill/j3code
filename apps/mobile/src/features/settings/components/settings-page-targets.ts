/** Settings pages a Settings row opens. */
export const SETTINGS_PAGE_TARGETS = [
  "SettingsEnvironments",
  "SettingsAbout",
  "SettingsArchive",
  "SettingsAppearance",
  "SettingsOrganization",
  "SettingsProjectOverview",
  "SettingsLabs",
  "SettingsKeyboard",
  "SettingsFollowUp",
  "SettingsProjectGrouping",
] as const;

export type SettingsPageTarget = (typeof SETTINGS_PAGE_TARGETS)[number];
