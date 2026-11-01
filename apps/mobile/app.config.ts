import type { ExpoConfig } from "expo/config";

// Icons are still the T3 Code production artwork, copied from t3code's root `assets/prod`.
const ICON_PNG = "./assets/brand/ios-icon-1024.png";
const ICON_COMPOSER_PROJECT = "./assets/brand/app-icon.icon";

const dmSansFonts = {
  regular: "@expo-google-fonts/dm-sans/400Regular/DMSans_400Regular.ttf",
  medium: "@expo-google-fonts/dm-sans/500Medium/DMSans_500Medium.ttf",
  bold: "@expo-google-fonts/dm-sans/700Bold/DMSans_700Bold.ttf",
} as const;

const config: ExpoConfig = {
  name: "j3",
  slug: "j3code",
  platforms: ["ios"],
  scheme: "j3code",
  version: "0.1.0",
  orientation: "portrait",
  icon: ICON_PNG,
  userInterfaceStyle: "automatic",
  ios: {
    icon: ICON_COMPOSER_PROJECT,
    supportsTablet: true,
    bundleIdentifier: "dev.jacobyuill.j3code",
    // Free personal team: no push, app groups, associated domains, or Sign in with Apple.
    appleTeamId: "8NY34B669X",
    infoPlist: {
      UISupportedInterfaceOrientations: [
        "UIInterfaceOrientationPortrait",
        "UIInterfaceOrientationLandscapeLeft",
        "UIInterfaceOrientationLandscapeRight",
      ],
      // j3 servers are reached over plain http:// on the tailnet or LAN.
      NSAppTransportSecurity: {
        NSAllowsArbitraryLoads: true,
      },
      NSLocalNetworkUsageDescription:
        "Allow j3 to connect to j3 servers on your local network or tailnet.",
      // The share sheet's "Save Image" action requires this string.
      NSPhotoLibraryAddUsageDescription: "Allow j3 to save images to your photo library.",
      ITSAppUsesNonExemptEncryption: false,
    },
  },
  plugins: [
    "expo-asset",
    [
      "expo-font",
      {
        ios: {
          fonts: [dmSansFonts.regular, dmSansFonts.medium, dmSansFonts.bold],
        },
      },
    ],
    // Secure-store items never require biometric authentication.
    ["expo-secure-store", { faceIDPermission: false }],
    "expo-sqlite",
    // Composer uses the system photo picker, which needs neither permission.
    [
      "expo-image-picker",
      { photosPermission: false, cameraPermission: false, microphonePermission: false },
    ],
    [
      "expo-splash-screen",
      {
        image: ICON_PNG,
        resizeMode: "contain",
        backgroundColor: "#ffffff",
        imageWidth: 220,
        dark: {
          image: ICON_PNG,
          backgroundColor: "#0a0a0a",
        },
      },
    ],
    [
      "expo-build-properties",
      {
        ios: {
          deploymentTarget: "18.0",
        },
      },
    ],
    "./plugins/withIosCocoaPodsUuidCache.cjs",
  ],
  experiments: {
    // Metro only runs babel-plugin-react-compiler when this is set; see src/reactCompilerCoverage.test.ts.
    reactCompiler: true,
  },
};

export default config;
