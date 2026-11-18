import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// The API (and the SSE log stream) proxy to the Rust server in dev; in
// production the same bundle is embedded in the binary and served from the
// same origin, so the app never needs to know an API base URL.
const api = `http://127.0.0.1:${process.env.HUNTWELL_API_PORT || 8611}`

export default defineConfig({
  plugins: [react()],
  server: {
    port: Number(process.env.HUNTWELL_UI_PORT || 5611),
    strictPort: true,
    proxy: {
      '/api': { target: api, changeOrigin: false },
      '/dl': { target: api, changeOrigin: false },
      // Claude's MCP connector (web/mcp_api.rs), so the URL the API access
      // page shows works against the dev server too.
      '/mcp': { target: api, changeOrigin: false },
      // Huntwell for Chrome's own endpoints (web/extension.rs), so the extension
      // can sign in against the dev server on this port.
      '/ext': { target: api, changeOrigin: false },
      // Slack's Events API (support chat replies), for a tunnel to this port.
      '/slack': { target: api, changeOrigin: false },
      '/healthz': { target: api },
    },
  },
  build: { outDir: 'dist', emptyOutDir: true, sourcemap: false },
})
