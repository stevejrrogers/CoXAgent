// Outbound alert delivery history (CXA-F235): the Activity view lists the
// project's spooled webhook alerts with delivery status, console-clean.
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

test('the activity view renders the outbound alerts panel', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);

  await page.locator('a[data-v="activity"]').click();
  await expect(page.locator('#alerts-body')).toBeVisible();
  // No webhook configured in the fixture: the panel renders its empty state,
  // not an error.
  await expect(page.locator('#alerts-body')).toContainText('no outbound alerts yet');
  await assertNoConsoleErrors(errors);
});
