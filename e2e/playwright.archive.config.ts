import { defineConfig } from '@playwright/test';

// The archive suite (CXA-F274): boots the fixture server with the in-memory
// ticket archive wired and seeded (run-server-archive.sh), so the board's
// archive surface and the read-only dialog render against a POPULATED cold
// store. Separate config so the main suite's server — and its golden
// screenshots — stay on the empty-archive path, which must remain
// byte-identical to the pre-archive board.
const PORT = 4527;

export default defineConfig({
  testDir: './specs-archive',
  fullyParallel: false,
  workers: 1,
  retries: 0,
  timeout: 30_000,
  expect: {
    toHaveScreenshot: {
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
    command: `sh ./run-server-archive.sh ${PORT}`,
    url: `http://127.0.0.1:${PORT}/api/health`,
    reuseExistingServer: false,
    timeout: 60_000,
  },
});
