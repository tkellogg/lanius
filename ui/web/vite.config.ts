import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

const backend = process.env.ELANUS_WEB_BACKEND ?? 'http://127.0.0.1:7180';

export default defineConfig({
  plugins: [react()],
  publicDir: false,
  server: {
    host: '127.0.0.1',
    port: Number(process.env.ELANUS_VITE_PORT ?? 5173),
    strictPort: false,
    proxy: {
      '/api': {
        target: backend,
        changeOrigin: true,
      },
    },
  },
});
