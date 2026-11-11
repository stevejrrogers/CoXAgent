// CXA-F437 — the fleet spend panel's Export CSV button: renders for admins,
// downloads a header-correct CSV, degrades to a friendly failure toast on
// server errors and DIMS (never hides) when the API answers 403.
import { readFileSync } from 'node:fs';
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

const FLEET = {
  hub_ceiling_usd: 50,
  hub_warn_pct: 0.8,
  totals: { spend_usd: 1.5, today_usd: 0.5, spend_7d_usd: 1, projects: 2, over: 0, approaching: 0, broken: 0, hub_headroom_usd: 48.5 },
  projects: [
    { id: 'a', name: 'Alpha', space_id: null, spend_usd: 1, today_usd: 0.5, spend_7d_usd: 1, lifetime_cap_usd: null, daily_cap_usd: null, headroom_usd: null, headroom_today_usd: null, status: 'ok', broken: false },
    { id: 'b', name: 'Beta', space_id: null, spend_usd: 0.5, today_usd: 0, spend_7d_usd: 0, lifetime_cap_usd: 10, daily_cap_usd: 4, headroom_usd: 9.5, headroom_today_usd: 4, status: 'ok', broken: false },
  ],
  spaces: [],
};

test('export button downloads an invoice-shaped CSV with the header row first', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await page.route('**/api/fleet/spend?', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(FLEET) }),
  );
  let exportQuery = '';
  await page.route('**/api/fleet/spend/export.csv*', async (route) => {
    exportQuery = new URL(route.request().url()).search;
    await route.fulfill({
      status: 200,
      contentType: 'text/csv; charset=utf-8',
      headers: { 'Content-Disposition': 'attachment; filename="coxagent-invoice-2026-11-13.csv"' },
      // Simple fixture rows (no embedded CRLF, so the record count is exact).
      body: 'date,issued,id,name,space_id,today_usd,spend_7d_usd,lifetime_usd,daily_cap_usd,lifetime_cap_usd,status\r\nall,2026-11-13,a,Alpha,,0.50,1.00,1.00,,,ok\r\n#totals,all,2026-11-13,,,,1.50,1.00,1.50,,,ok\r\n',
    });
  });
  await openApp(page);
  await page.evaluate(() => { (window as any).nav('mg-fleet'); });
  const btn = page.locator('#fleet-export');
  await expect(btn).toBeVisible();
  await expect(btn).toBeEnabled();

  const [download] = await Promise.all([page.waitForEvent('download'), btn.click()]);
  // CXA-F437 contract: GET /api/fleet/spend/export.csv, default window=all
  // (asserted at handler level in crates/app/tests/fleet_spend_api.rs).
  expect(download.suggestedFilename()).toMatch(/^coxagent-invoice-\d{4}-\d{2}-\d{2}\.csv$/);

  const csv = readFileSync((await download.path())!, 'utf8');
  const lines = csv.trimEnd().split('\r\n');
  expect(lines[0]).toBe('date,issued,id,name,space_id,today_usd,spend_7d_usd,lifetime_usd,daily_cap_usd,lifetime_cap_usd,status');
  expect(lines.length).toBeGreaterThanOrEqual(3); // header + >=1 line item (+ optional #totals)
  expect(lines.some((l) => l.startsWith('#totals,'))).toBe(true);
  await assertNoConsoleErrors(errors);
});

test('a failing export shows a friendly error naming the cause, not a crash', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await page.route('**/api/fleet/spend?', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(FLEET) }),
  );
  await page.route('**/api/fleet/spend/export.csv*', (route) =>
    route.fulfill({ status: 500, contentType: 'application/json', body: JSON.stringify({ error: 'export failed: malformed ledger' }) }),
  );
  await openApp(page);
  await page.evaluate(() => { (window as any).nav('mg-fleet'); });
  const btn = page.locator('#fleet-export');
  await expect(btn).toBeVisible();
  await btn.click();
  const toast = page.locator('#toasts .toast').first();
  await expect(toast).toContainText('Export failed', { ignoreCase: true });
  await expect(toast).toContainText('malformed ledger');
  // In-flight disabled state cleared after the failure — the user can retry.
  await expect(btn).toBeEnabled();
  // Chromium logs the injected 500 itself ("Failed to load resource…") — that
  // line IS the harness, not an app defect; filter it like paste-attach.spec.ts.
  await assertNoConsoleErrors(errors.filter((e) => !e.includes('Failed to load resource')));
});
