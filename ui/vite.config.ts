import { defineConfig } from "vitest/config";

// `npm run dev` proxies the API to a local server (override with ETIO_API).
const api = process.env.ETIO_API ?? "http://127.0.0.1:7070";

export default defineConfig({
  server: {
    proxy: {
      "/api": { target: api, changeOrigin: true },
      "/metrics": { target: api, changeOrigin: true },
    },
  },
  build: { outDir: "dist", sourcemap: true, target: "es2022" },
  test: { environment: "node" },
});
