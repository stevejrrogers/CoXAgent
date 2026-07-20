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
  webServer: {
    command: 'PGPASSWORD=coxagent-local-postgres psql -h localhost -U coxagent -d coxagent -c "DELETE FROM auth_sessions;" > /dev/null 2>&1; /Users/luton/Projects/CoXAgent/desktop/build/CoXAgent.app/Contents/MacOS/cox-server hub --registry /Users/luton/CoXAgent/registry.json --port 4000',
    port: 4000,
    reuseExistingServer: true,
  },
});
