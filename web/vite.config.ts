import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// Built by CI, unpacked by the VM's puller into /var/www/site. Hash routing,
// so Caddy needs no SPA fallback. Dev proxies the gated data paths and Perses
// to the live box - sign in there once in the same browser and the cookie
// rides along.
export default defineConfig({
  plugins: [react()],
  base: '/',
  build: {
    outDir: 'dist',
    sourcemap: false,
    chunkSizeWarningLimit: 3000,
    // One entry, fixed names: the bot links /assets/island.js and
    // /assets/island.css from every page it renders.
    rollupOptions: {
      input: 'src/island.tsx',
      output: { entryFileNames: 'assets/island.js', chunkFileNames: 'assets/[name].js', assetFileNames: 'assets/[name][extname]' },
    },
  },
  server: {
    proxy: {
      '/data': { target: 'https://dps.nocturnal-guild.de', changeOrigin: true },
      '/prom': { target: 'https://dps.nocturnal-guild.de', changeOrigin: true },
      '/perses': { target: 'https://dps.nocturnal-guild.de', changeOrigin: true },
    },
  },
});
