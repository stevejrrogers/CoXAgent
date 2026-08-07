import { defineConfig } from '@playwright/test';

const PORT = 4518;

export default defineConfig({
  testDir: './specs-auth',
  fullyParallel: false,
  workers: 1,
  retries: 0,
  timeout: 40_000,
  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
    viewport: { width: 1280, height: 900 },
    colorScheme: 'dark',
    reducedMotion: 'reduce',
    trace: 'retain-on-failure',
  },
  webServer: {
    command: `sh ./run-server-auth.sh ${PORT}`,
    url: `http://127.0.0.1:${PORT}/api/health`,
    reuseExistingServer: false,
    timeout: 60_000,
  },
});
