// Cost/metrics view: every panel renders even with zero spend - deterministic.
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

test('the cost view draws all four panels plus KPI labels', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  // The token-saver panel reads /api/token-saver, which aggregates a
  // host-local savings.log (COXAGENT_SHIM_DIR) written only when agents have
  // run here — so it is empty on a clean runner and non-empty on a dev box
  // with leftover shim state. Pin one value so the panel renders identically
  // on darwin and linux instead of whichever host happened to run agents.
  await page.route('**/api/token-saver', (route) =>
    route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ samples: 3, before: 12000, after: 2000 }),
    }),
  );
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
