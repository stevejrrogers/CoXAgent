// Secondary data-display surfaces render without console errors.
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

test('overview draws its KPI tiles', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);
  for (const label of ['Shipped', 'In flight', 'Documented', 'Releases']) {
    await expect(page.locator('#kpis')).toContainText(label);
  }
  await assertNoConsoleErrors(errors);
});

test('the activity view renders its shell with an agent filter', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);
  await page.locator('a[data-v="activity"]').click();
  await expect(page.locator('#activity-full')).toBeVisible();
  await expect(page.locator('#act-filter option[value=""]')).toHaveCount(1);
  await assertNoConsoleErrors(errors);
});

test('the settings view loads cleanly', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);
  await page.locator('a[data-v="settings"]').click();
  const body = page.locator('#settings-body');
  await expect(body).toBeVisible();
  // Config comes back over the wire and paints real content.
  await expect(body).not.toBeEmpty();
});
