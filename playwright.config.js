const { defineConfig } = require('@playwright/test');

module.exports = defineConfig({
  testDir: './tests',
  timeout: 30000,
  retries: 0,
  use: {
    baseURL: 'http://localhost:4000',
    headless: true,
    viewport: { width: 1440, height: 900 },
    actionTimeout: 10000,
    trace: 'on-first-retry',
  },
});
