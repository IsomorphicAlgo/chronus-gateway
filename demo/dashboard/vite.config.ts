import { defineConfig } from "vite";

// Showcase S2: static build to `dist/` for `npm run build`; `npm run dev` for local demos.
export default defineConfig({
  root: ".",
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
});
