export const NAVIGATION_TAB_ORDER = ["threads"] as const;
export type NavigationTab = (typeof NAVIGATION_TAB_ORDER)[number];

export const NAVIGATION_TAB_LABELS: Readonly<Record<NavigationTab, string>> = {
  threads: "Threads",
};
