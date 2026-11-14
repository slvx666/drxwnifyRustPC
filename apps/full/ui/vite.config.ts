import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// Tauri serves the built files; the dev server is fixed so tauri.conf.json can point at it.
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  build: { target: "es2022", outDir: "dist", emptyOutDir: true },
});
