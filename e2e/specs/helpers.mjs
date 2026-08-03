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
  await page.goto('/');
  await page.waitForLoadState('networkidle');
}
