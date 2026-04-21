import { defineConfig } from "@tanstack/start/config";
import tsConfigPaths from "vite-tsconfig-paths";

export default defineConfig({
  server: {
    preset: "node-server",
    port: 3000,
  },
  vite: {
    plugins: [tsConfigPaths()],
  },
  routers: {
    client: {
      entry: "./app/client.tsx",
    },
    ssr: {
      entry: "./app/ssr.tsx",
    },
  },
});
