// Rewrites `import { SquarePen } from "lucide-react-native"` to one import per icon file, so a screen pulls in only
// the icons it draws instead of the whole 3,700-icon barrel (docs/paseo-ui.md "Loading"). The map from every export
// name (aliases included) to its file comes from the barrel itself. Type-only specifiers and names the barrel does not
// export stay on the original import, which TypeScript then drops when nothing but types use it.
const fs = require("fs");
const path = require("path");

const PACKAGE = "lucide-react-native";
let exportFiles = null;

function packageRoot() {
  // The package's exports map hides package.json and dist/, so find the root through its main entry
  const main = require.resolve(PACKAGE, { paths: [__dirname] });
  return main.slice(0, main.lastIndexOf(`${path.sep}dist${path.sep}`));
}

function loadExportFiles() {
  if (exportFiles) return exportFiles;
  const source = fs.readFileSync(path.join(packageRoot(), "dist", "esm", "lucide-react-native.mjs"), "utf8");
  exportFiles = new Map();
  const statement = /export\s*\{([^}]*)\}\s*from\s*'\.\/([^']+)'/g;
  for (const match of source.matchAll(statement)) {
    const file = `${PACKAGE}/dist/esm/${match[2]}`;
    for (const part of match[1].split(",")) {
      const [local, exported] = part.trim().split(/\s+as\s+/);
      if (!local) continue;
      exportFiles.set(exported ?? local, { file, imported: local });
    }
  }
  return exportFiles;
}

module.exports = function lucidePerIcon({ types: t }) {
  return {
    name: "lucide-per-icon",
    visitor: {
      ImportDeclaration(nodePath) {
        const { node } = nodePath;
        if (node.source.value !== PACKAGE || node.importKind === "type") return;
        const files = loadExportFiles();
        const kept = [];
        const rewritten = [];
        for (const specifier of node.specifiers) {
          const entry =
            t.isImportSpecifier(specifier) && specifier.importKind !== "type"
              ? files.get(t.isIdentifier(specifier.imported) ? specifier.imported.name : specifier.imported.value)
              : undefined;
          if (!entry) {
            kept.push(specifier);
            continue;
          }
          const local = t.identifier(specifier.local.name);
          const imported =
            entry.imported === "default"
              ? t.importDefaultSpecifier(local)
              : t.importSpecifier(local, t.identifier(entry.imported));
          rewritten.push(t.importDeclaration([imported], t.stringLiteral(entry.file)));
        }
        if (rewritten.length === 0) return;
        if (kept.length > 0) {
          node.specifiers = kept;
          nodePath.insertAfter(rewritten);
        } else {
          nodePath.replaceWithMultiple(rewritten);
        }
      },
    },
  };
};

module.exports.resolveLucideFile = function resolveLucideFile(moduleName) {
  if (!moduleName.startsWith(`${PACKAGE}/dist/esm/`)) return null;
  return path.join(packageRoot(), moduleName.slice(PACKAGE.length + 1));
};
