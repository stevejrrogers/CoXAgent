import { defineConfig } from '@playwright/test';

// Boots an ephemeral CoXAgent server on its own port (never the hub's 4000)
// with a frozen state fixture, so screenshots and flows are deterministic.
const PORT = 4517;

export default defineConfig({
  testDir: './specs',
  fullyParallel: false,
  workers: 1,
  retries: 0,
  timeout: 30_000,
  expect: {
    toHaveScreenshot: {
      // Frozen state still ages: relative timestamps drift a few pixels.
      maxDiffPixelRatio: 0.02,
      animations: 'disabled',
    },
  },
  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
    viewport: { width: 1280, height: 900 },
    colorScheme: 'dark',
    reducedMotion: 'reduce',
  },
  webServer: {
    command: `sh ./run-server.sh ${PORT}`,
    url: `http://127.0.0.1:${PORT}/api/health`,
    reuseExistingServer: false,
    timeout: 60_000,
  },
});
