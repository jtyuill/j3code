import { StackActions, useNavigation } from "@react-navigation/native";
import { threadChipCopyTarget } from "@t3tools/client-runtime/thread-reference";
import {
  createContext,
  use,
  useCallback,
  useMemo,
  useState,
  useSyncExternalStore,
  type PropsWithChildren,
  type ReactNode,
} from "react";

import * as Dialog from "../../components/ui/Dialog";
import { copyThreadReference } from "../../lib/copyThreadReference";
import {
  firstWorkspaceDetailOpen,
  workspaceHasColumns,
} from "../workspace/workspace-selection-store";
import { T3KeyboardCommands } from "../../native/T3KeyboardCommands";
import { useThreadShell } from "../../state/entities";
import { CommandPalette } from "./CommandPalette";
import {
  dispatchHardwareKeyboardCommand,
  getHardwareKeyboardCommandRegistrationVersion,
  getRegisteredHardwareKeyboardCommands,
  parseActiveThreadPath,
  subscribeToHardwareKeyboardCommandRegistrations,
  type HardwareKeyboardCommand,
} from "./hardwareKeyboardCommands";
import { selectWorkspaceDetail } from "../workspace/workspace-detail-selection";

const CommandPaletteContext = createContext<ReactNode>(null);

/** Render inside the workspace so palette actions share its navigation and pane state. */
export function HardwareKeyboardCommandOverlay() {
  return use(CommandPaletteContext);
}

export function HardwareKeyboardCommandProvider({
  children,
  pathname,
}: PropsWithChildren<{ readonly pathname: string }>) {
  const navigation = useNavigation();
  const [paletteOpen, setPaletteOpen] = useState(false);
  const closePalette = useCallback(() => setPaletteOpen(false), []);
  const activeThreadRef = useMemo(() => parseActiveThreadPath(pathname), [pathname]);
  const activeThread = useThreadShell(activeThreadRef);
  const activeThreadTitle = activeThread?.title ?? "";
  const registrationVersion = useSyncExternalStore(
    subscribeToHardwareKeyboardCommandRegistrations,
    getHardwareKeyboardCommandRegistrationVersion,
    getHardwareKeyboardCommandRegistrationVersion,
  );
  const enabledCommands = useMemo(() => {
    const commands = new Set<HardwareKeyboardCommand>(getRegisteredHardwareKeyboardCommands());
    commands.add("newTask");
    commands.add("commandPalette");
    if (pathname !== "/" && !pathname.startsWith("/threads/")) {
      for (const command of commands) {
        if (command.startsWith("thread.jump.")) commands.delete(command);
      }
    }
    if (pathname !== "/" || navigation.canGoBack()) commands.add("back");
    if (activeThreadRef !== null) commands.add("copyThreadReference");
    return [...commands];
    // oxlint-disable-next-line react/memo-dependencies -- registrationVersion invalidates the snapshot of the external keyboard-command registry.
  }, [activeThreadRef, pathname, registrationVersion, navigation]);

  const onCommand = useCallback(
    (command: HardwareKeyboardCommand) => {
      if (command === "commandPalette") {
        setPaletteOpen(true);
        return;
      }
      if (dispatchHardwareKeyboardCommand(command)) return;

      if (command === "copyThreadReference") {
        if (activeThreadRef === null) return;
        const target = threadChipCopyTarget(activeThreadRef, activeThreadTitle);
        void copyThreadReference(target).then((didCopy) => {
          if (!didCopy) Dialog.alert(target.failureTitle, "Try copying this thread again.");
        });
        return;
      }

      if (command === "newTask") {
        navigation.dispatch(selectWorkspaceDetail({ name: "Thread", params: { draft: {} } }));
        return;
      }
      if (command === "back") {
        // Beside a visible list, the first detail page has nothing to go back to.
        if (workspaceHasColumns() && firstWorkspaceDetailOpen(navigation)) return;
        if (navigation.canGoBack()) {
          navigation.goBack();
        } else {
          navigation.dispatch(StackActions.replace("Home"));
        }
        return;
      }
    },
    [activeThreadRef, activeThreadTitle, navigation],
  );

  const palette = useMemo(
    () =>
      paletteOpen ? (
        <CommandPalette pathname={pathname} onClose={closePalette} onCommand={onCommand} />
      ) : null,
    [closePalette, onCommand, paletteOpen, pathname],
  );

  return (
    <CommandPaletteContext value={palette}>
      <T3KeyboardCommands enabledCommands={enabledCommands} onCommand={onCommand}>
        {children}
      </T3KeyboardCommands>
    </CommandPaletteContext>
  );
}
