module.exports = function (api) {
  api.cache(true);

  const expoPreset = [
    "babel-preset-expo",
    {
      // Transform `import.meta` for ALL platforms (web + native)
      // Required for modern ESM deps like Zustand 5 that use import.meta.env
      unstable_transformImportMeta: true,
    },
  ];

  return {
    presets: [expoPreset],
    plugins: [
      // One import per Lucide icon instead of the whole barrel (docs/paseo-ui.md "Loading")
      "./babel/lucide-per-icon.js",
      [
        "react-native-unistyles/plugin",
        {
          root: "src",
        },
      ],
    ],
  };
};
