// Space (workspace) view: overview + the work board with the seeded backlog.
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

test('overview renders the seeded project and stays console-clean', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  // The KPI tiles (CXA-F360) chart a 28-day UTC window that slides with the
  // real clock, while the fixture state is frozen — without a pinned clock the
  // golden would rot the day the fixture's spend days slide out of the window.
  // Freezing Date.now near the fixture's own dates keeps relative-time strings
  // and the tile windows deterministic forever.
  await page.addInitScript(() => {
    const fixed = Date.parse('2026-09-05T12:00:00Z');
    Date.now = () => fixed;
  });
  await openApp(page);

  await expect(page.locator('body')).toContainText('Ticket status');
  await assertNoConsoleErrors(errors);
  await expect(page).toHaveScreenshot('overview.png');
});

test('the work board shows the seeded tickets', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);

  await page.locator('a[data-v="board"]').click();
  await expect(page.locator('body')).toContainText('Search box scopes per tab');
  await expect(page.locator('body')).toContainText('Delivery timeline connector line breaks');
  await assertNoConsoleErrors(errors);
  await expect(page).toHaveScreenshot('board.png');
});
