import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import { execFileSync } from 'node:child_process';

// The commit a build is made from, shown in About (where its source is).
if (!process.env.VITE_RAWTONIC_COMMIT) {
  try {
    process.env.VITE_RAWTONIC_COMMIT = execFileSync('git', ['rev-parse', '--short=12', 'HEAD'], {
      encoding: 'utf8',
    }).trim();
  } catch {
    // Not a git checkout: About shows the repository only.
  }
}

const host = process.env.TAURI_DEV_HOST;

// https://vitejs.dev/config/
export default defineConfig(async () => ({
  plugins: [tailwindcss(), react()],

  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: 'ws',
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      ignored: ['**/src-tauri/**'],
    },
  },

  envPrefix: ['VITE_', 'TAURI_ENV_*'],
  // Profiling builds: react-dom's profiling build, so <Profiler> reports
  // commit durations (PerfProfiler in usePerformanceDiagnostics.ts).
  resolve: process.env.VITE_RAWTONIC_PERF === '1'
    ? { alias: [{ find: /^react-dom\/client$/, replacement: 'react-dom/profiling' }] }
    : undefined,
  build: {
    minify: !process.env.TAURI_ENV_DEBUG ? 'esbuild' : false,
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
  },
}));
