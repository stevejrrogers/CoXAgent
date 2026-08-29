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

test('every sidebar view renders something and stays console-clean', async ({ page }) => {
  // The gate CXA-F233 needed: its new menu shipped with no script tag and a
  // made-up icon — clicking it threw and rendered nothing, and no spec
  // visited the view so nothing screamed. Now every sidebar entry is visited.
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);
  const views = await page.$$eval('.side a[data-v]', els => els.map(e => e.getAttribute('data-v')));
  expect(views.length).toBeGreaterThan(10);
  for (const v of views) {
    if (v === 'terminal') continue; // needs a PTY session — covered elsewhere
    await page.locator(`.side a[data-v="${v}"]`).click();
    await page.waitForTimeout(400);
    const visible = await page.$$eval('.view', els =>
      els.filter(e => (e as HTMLElement).offsetParent !== null)
        .map(e => (e as HTMLElement).innerText.trim().length));
    expect(visible.some(len => len > 0), `view "${v}" rendered empty`).toBeTruthy();
  }
  await assertNoConsoleErrors(errors);
});
