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
      '/healthz': { target: api },
    },
  },
  build: { outDir: 'dist', emptyOutDir: true, sourcemap: false },
})
