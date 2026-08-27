import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'

// 开发时前端跑在 5173，接口转发给本地的 lessord。
// 构建产物进 dist/，由 lessord 用 rust-embed 打进二进制。
const backend = process.env.LESSOR_API ?? 'http://127.0.0.1:8080'

export default defineConfig({
  plugins: [svelte()],
  build: { outDir: 'dist', emptyOutDir: true, target: 'es2022' },
  server: {
    proxy: {
      '/api/events': { target: backend.replace(/^http/, 'ws'), ws: true },
      '/api': backend,
      '/healthz': backend,
    },
  },
})
