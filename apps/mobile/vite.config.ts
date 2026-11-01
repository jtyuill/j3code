import * as NodeModule from "node:module";
import * as NodeURL from "node:url";
import { defineConfig, normalizePath } from "vite-plus";
import "vite-plus/test/config";

const reactTests = [
  "src/features/threads/new-task-flow-provider.test.tsx",
  "src/features/projects/AddProjectScreen.test.ts",
  "src/lib/useMediaVideoPlayback.test.tsx",
  "src/lib/filePreview.test.ts",
  "src/components/ProjectFavicon.test.tsx",
  "src/features/settings/components/SettingsDragHandle.test.tsx",
  "src/features/threads/ThreadMarkdownImage.test.tsx",
  "src/features/threads/NewTaskDraftRouteScreen.test.tsx",
  "src/native/NativeHeaderToolbar.test.tsx",
  "src/native/V5StackHeader.ios.test.tsx",
  "src/native/useRetryingImageSource.test.tsx",
  "src/features/home/home-list-options.test.tsx",
  "src/features/workspace/useSearchScrollRestoration.test.tsx",
  "src/features/connection/ConnectionsNewRouteScreen.test.tsx",
];

const sourceRoot = `${normalizePath(NodeURL.fileURLToPath(new URL("./src", import.meta.url)))}/`;
const require = NodeModule.createRequire(import.meta.url);
const babel: {
  transformAsync: (
    code: string,
    options: Record<string, unknown>,
  ) => Promise<{ code?: string | null; map?: object | null } | null>;
} = require("@babel/core");
const babelConfig = require("./babel.config.js")({ cache: () => {} });

export default defineConfig({
  test: {
    hookTimeout: 60_000,
    testTimeout: 60_000,
    projects: [
      {
        extends: true,
        test: {
          name: "unit",
          include: ["**/*.test.{ts,tsx,mjs}"],
          exclude: ["**/node_modules/**", ...reactTests],
          setupFiles: ["../../packages/shared/src/testing/providerEnv.ts"],
        },
      },
      {
        extends: true,
        plugins: [
          {
            name: "mobile-release-react-compiler",
            enforce: "pre",
            async transform(code, id) {
              if (
                !normalizePath(id).startsWith(sourceRoot) ||
                !/\.tsx?$/.test(id) ||
                /\.test\.tsx?$/.test(id)
              ) {
                return;
              }
              const result = await babel.transformAsync(code, {
                ...babelConfig,
                presets: babelConfig.presets.map(([name, options]: [string, object]) => [
                  require.resolve(name),
                  // Vite runs in Node; inline Babel helpers instead of Metro runtime imports.
                  { ...options, enableBabelRuntime: false },
                ]),
                filename: id,
                babelrc: false,
                configFile: false,
                sourceMaps: true,
                caller: {
                  name: "metro",
                  bundler: "metro",
                  platform: "ios",
                  isDev: false,
                  isServer: false,
                  isNodeModule: false,
                  supportsStaticESM: true,
                  supportsReactCompiler: true,
                },
              });
              if (result?.code) return { code: result.code, map: result.map };
            },
          },
        ],
        test: {
          name: "react",
          include: reactTests,
          // Native components mock React Native and render through react-dom in these tests.
          environment: "jsdom",
          setupFiles: ["../../packages/shared/src/testing/providerEnv.ts"],
        },
      },
    ],
  },
});
