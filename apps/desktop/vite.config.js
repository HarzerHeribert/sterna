import { defineConfig } from "vite";
import { birds } from "./scripts/birds.mjs";

// The UI is one static page in ui/, built to dist/ for Tauri. The traced
// birds come from the repository's art/birds at build time, as the module
// `virtual:birds`, so the sprites are never copied by hand.
const BIRDS = "virtual:birds";
function birdArt() {
  return {
    name: "sterna-birds",
    resolveId: (id) => (id === BIRDS ? "\0" + BIRDS : null),
    load: (id) => (id === "\0" + BIRDS ? `export default ${JSON.stringify(birds())};` : null),
  };
}

export default defineConfig({
  root: "ui",
  base: "./",
  clearScreen: false,
  plugins: [birdArt()],
  server: { host: "127.0.0.1", port: 5199, strictPort: true },
  build: { outDir: "../dist", emptyOutDir: true, target: "es2022" },
});
