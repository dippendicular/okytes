import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  base: "/quick/",
  build: {
    outDir: "../daemon/web",
    emptyOutDir: true,
    rollupOptions: { input: "quick.html" },
  },
});
