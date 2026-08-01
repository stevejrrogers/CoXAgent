// Space (workspace) view: overview + the work board with the seeded backlog.
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

test('overview renders the seeded project and stays console-clean', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
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
