const path = require("node:path");
const { getDefaultConfig } = require("expo/metro-config");
const { withUniwindConfig } = require("uniwind/metro");
const extraThemes = require("./generated-uniwind-theme-names.json");

/** @type {import("expo/metro-config").MetroConfig} */
const config = getDefaultConfig(__dirname);
const workspaceRoot = path.resolve(__dirname, "../..");

config.watchFolders = [...new Set([...(config.watchFolders ?? []), workspaceRoot])];

module.exports = withUniwindConfig(config, {
  cssEntryFile: "./global.css",
  extraThemes,
  polyfills: { rem: 14 },
});
