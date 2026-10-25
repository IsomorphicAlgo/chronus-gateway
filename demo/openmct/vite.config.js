// Serves Open MCT (prebuilt, from node_modules/openmct/dist) plus the Chronus plugin, and proxies
// the gateway's API and WebSocket so the page talks to it same-origin (the gateway sends no CORS
// headers by design). Point it at another gateway with CHRONUS_GATEWAY=http://host:port.
import { defineConfig } from 'vite';

const gateway = process.env.CHRONUS_GATEWAY ?? 'http://127.0.0.1:8080';

export default defineConfig({
  // Open MCT's dist (openmct.js, its CSS, fonts, workers) is served from the site root as-is.
  publicDir: 'node_modules/openmct/dist',
  server: {
    host: '127.0.0.1',
    port: 5174,
    proxy: {
      '/api': { target: gateway, changeOrigin: true },
      '/telemetry': { target: gateway.replace(/^http/, 'ws'), ws: true, changeOrigin: true }
    }
  },
  preview: {
    host: '127.0.0.1',
    port: 4174,
    proxy: {
      '/api': { target: gateway, changeOrigin: true },
      '/telemetry': { target: gateway.replace(/^http/, 'ws'), ws: true, changeOrigin: true }
    }
  }
});
