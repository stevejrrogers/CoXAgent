// Shared helpers: console-error gate + navigation that waits for the app.
import { expect } from '@playwright/test';

/// Fail the test on any console error or page crash — a refactor that keeps
/// pixels but breaks a module load shows up here, not in the screenshot.
export function armConsoleGate(page, errors) {
  page.on('console', (msg) => {
    if (msg.type() === 'error') errors.push(msg.text());
  });
  page.on('pageerror', (err) => errors.push(String(err)));
}

export async function assertNoConsoleErrors(errors) {
  expect(errors, `console errors:\n${errors.join('\n')}`).toEqual([]);
}

/// Open the app and wait until it has painted real content.
export async function openApp(page) {
  // Pin a runnable engine before first paint. On load, CoXAgent asks
  // /api/engines what agent CLIs this machine has; a bare runner (Linux CI,
  // this repo's docker images) has none, which pops the "Connect a coding
  // agent" modal over every click and blocks the whole suite. Screenshots must
  // not depend on which host renders them, so report one engine: wizard
  // suppressed, pixels identical on darwin and linux alike.
  await page.route('**/api/engines', (route) =>
    route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify([
        { name: 'opencode', path: '/usr/local/bin/opencode', where: 'hub' },
      ]),
    }),
  );
  await page.goto('/');
  await page.waitForLoadState('networkidle');
}
