// Fleet spend cockpit (CXA-F278): the Manage → Fleet spend view renders the
// hub totals, the per-project burn table and the soft-ceiling editor.
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

// Frozen payload so the panel is deterministic regardless of the fixture's
// real spend — the endpoint's own math is pinned by the Rust contract tests
// (crates/app/tests/fleet_spend_api.rs); this spec pins the VIEW.
const FLEET = {
  hub_ceiling_usd: 50,
  hub_warn_pct: 0.8,
  totals: {
    spend_usd: 21.5,
    today_usd: 5.5,
    spend_7d_usd: 8.0,
    projects: 4,
    over: 1,
    approaching: 1,
    broken: 1,
    hub_headroom_usd: 44.5,
  },
  projects: [
    {
      id: 'over', name: 'over', space_id: 's1',
      spend_usd: 12, today_usd: 3, spend_7d_usd: 5.5,
      lifetime_cap_usd: 10, daily_cap_usd: 4,
      headroom_usd: 0, headroom_today_usd: 1, status: 'over', broken: false,
    },
    {
      id: 'approaching', name: 'approaching', space_id: 's1',
      spend_usd: 8.5, today_usd: 2, spend_7d_usd: 2,
      lifetime_cap_usd: 10, daily_cap_usd: null,
      headroom_usd: 1.5, headroom_today_usd: null, status: 'approaching', broken: false,
    },
    {
      id: 'uncapped', name: 'uncapped', space_id: null,
      spend_usd: 1, today_usd: 0.5, spend_7d_usd: 0.5,
      lifetime_cap_usd: null, daily_cap_usd: null,
      headroom_usd: null, headroom_today_usd: null, status: 'ok', broken: false,
    },
    {
      id: 'broken', name: 'broken', space_id: null,
      spend_usd: 0, today_usd: 0, spend_7d_usd: 0,
      lifetime_cap_usd: null, daily_cap_usd: null,
      headroom_usd: null, headroom_today_usd: null, status: 'ok', broken: true,
    },
  ],
  spaces: [
    { id: 's1', name: 'Alpha', spend_usd: 20.5, today_usd: 5, spend_7d_usd: 7.5, budget_usd: 9, status: 'over' },
  ],
};

test('the fleet cockpit renders totals, sorted burn and the ceiling editor', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await page.route('**/api/fleet/spend', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(FLEET) }),
  );
  await openApp(page);

  // Direct nav — the manage sidebar is hidden in workspace mode (mode-manage
  // toggles it), exactly how the views sweep reaches every screen.
  await page.evaluate(() => { (window as any).nav('mg-fleet'); });
  const el = page.locator('#mg-fleet-body');
  await expect(el).toContainText('Fleet spend');
  await expect(el).toContainText('$5.50');
  await expect(el).toContainText('$8.00');
  await expect(el).toContainText('$21.50');

  // Projects are listed by burn (server-sorted): the over-cap project leads.
  const first = el.locator('.panel .wsrow').nth(1);
  await expect(first).toContainText('over');
  // Status is carried by words, not color alone.
  await expect(el).toContainText('OVER');
  await expect(el).toContainText('80%+');
  // Uncapped shows the word, never a fake headroom.
  await expect(el).toContainText('uncapped');
  // The broken registration is flagged, not silently dropped.
  await expect(el).toContainText('BROKEN');
  // Space rollup.
  await expect(el).toContainText('Alpha');
  // Soft-ceiling editor shows the persisted ceiling and headroom.
  await expect(page.locator('#fleet-ceiling')).toHaveValue('50');
  await expect(el).toContainText('$44.50');

  await assertNoConsoleErrors(errors);
});

test('saving the hub ceiling PUTs the entered value and repaints', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  // Stateful double: a saved ceiling shows up in the next GET, like the real
  // hub does (PUT persists to the workspace doc, GET reflects it).
  let ceiling = FLEET.hub_ceiling_usd;
  await page.route('**/api/fleet/spend', (route) =>
    route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ ...FLEET, hub_ceiling_usd: ceiling }),
    }),
  );
  let putBody: { ceiling_usd?: number | null } | null = null;
  await page.route('**/api/fleet/ceiling', async (route) => {
    putBody = (await route.request().postDataJSON()) as { ceiling_usd?: number | null };
    ceiling = (putBody?.ceiling_usd ?? 0) as number;
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ ceiling_usd: putBody?.ceiling_usd ?? 0 }),
    });
  });
  await openApp(page);
  await page.evaluate(() => { (window as any).nav('mg-fleet'); });
  await expect(page.locator('#fleet-ceiling')).toBeVisible();

  await page.locator('#fleet-ceiling').fill('75');
  await page.locator('button:has-text("Save")').first().click();
  await expect.poll(() => putBody).toEqual({ ceiling_usd: 75 });
  // The save-triggered refetch now reports the new ceiling.
  await expect(page.locator('#fleet-ceiling')).toHaveValue('75');

  await assertNoConsoleErrors(errors);
});
