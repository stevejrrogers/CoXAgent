// Cost/metrics view: every panel renders even with zero spend - deterministic.
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

test('the cost view draws all four panels plus KPI labels', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);

  await page.locator('a[data-v="insights"]').click();

  for (const kpi of ['Total spend', 'Tokens', 'Runs']) {
    await expect(page.locator('#cost-kpis')).toContainText(kpi);
  }
  await expect(page.locator('#cost-roles')).toContainText(
    'no spend yet',
  );
  await expect(page.locator('#cost-operators')).toContainText(
    'no per-user spend yet',
  );
  // Token-saver shows actual seeded compression stats in this fixture.
  await expect(page.locator('#cost-tokensaver')).toContainText('compressed');
  await expect(page.locator('#cost-tokensaver')).toContainText('saved');

  await assertNoConsoleErrors(errors);
});
