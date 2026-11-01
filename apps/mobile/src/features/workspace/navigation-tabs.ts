import { NAVIGATION_TAB_LABELS, type NavigationTab } from "./navigation-tab-order";
import type { AppSymbolName } from "../../components/ui/AppSymbol";

export const WORKSPACE_TAB_LABELS = { ...NAVIGATION_TAB_LABELS, threads: "Chats" };

export const NAVIGATION_TAB_ICONS = {
  threads: "text.bubble",
} as const satisfies Readonly<Record<NavigationTab, AppSymbolName>>;
