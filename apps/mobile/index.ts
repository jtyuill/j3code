import { registerRootComponent } from "expo";
import "react-native-gesture-handler";

import { featureFlags } from "react-native-screens";

import App from "./src/App";

// Enables iOS FormSheet sizing for nested native stacks in non-fitToContents sheets.
featureFlags.experiment.synchronousScreenUpdatesEnabled = true;


registerRootComponent(App);
