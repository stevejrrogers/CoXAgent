// Cross-project duplicate radar (CXA-F253): the view is the human's decision
// surface for tickets that two DIFFERENT projects filed for the same ask.
//
// The hub fixture holds ONE project, so a real second project cannot exist
// here and the live endpoint can only ever answer with an empty radar — that
// empty state is asserted against the REAL server below (it pins the route
// mount, auth pass-through and the fetch/render wiring). The populated
// render/action contracts run over route mocks carrying the exact payload
// shape `pair_json` emits (pinned by the handler's Rust tests) — the same
// determinism idiom helpers.openApp already uses for /api/engines and
// preflight. No server state is mutated, so the golden-screenshot specs see
// pristine fixture data.
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

/// Mirrors presentation/src/server/duplicate_radar.rs::pair_json: one
/// collapsed entry (identical normalized titles) — a home ticket plus two
/// duplicates, the identical title (score 1.0) and a near-paraphrase (0.67),
/// each side carrying its id, project, scope and the computed similarity.
const ONE_PAIR = {
  homeProjectId: 'dupe-p1',
  homeProjectName: 'Alpha',
  homeTicketId: 'CXC-F101',
  homeTicketTitle: 'Fix flaky login',
  homeTicketScope: 'session tokens expire mid-run',
  normalizedTitle: 'fix flaky login',
  dupTitle: 'fix flaky login',
  duplicates: [
    {
      projectId: 'dupe-p2',
      projectName: 'Beta',
      ticketId: 'CXC-F201',
      title: 'fix flaky login',
      scope: 'login drops after every refresh',
      score: 1.0,
    },
    {
      projectId: 'dupe-p3',
      projectName: 'Gamma',
      ticketId: 'CXC-F301',
      title: 'Fix the flaky login flow',
      scope: 'auth flaps under load',
      score: 0.67,
    },
  ],
};

/// Serve the radar GET from a mutable holder, so an action test can flip the
/// payload to [] and watch the view re-run WITHOUT the resolved pair — the
/// persisted-allowlist exclusion (AC3), one round-trip at a time.
async function serveRadar(page: import('@playwright/test').Page, state: { pairs: unknown[] }) {
  await page.route('**/api/workspace/duplicates', (route) =>
    route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ crossProjectDuplicates: state.pairs }),
    }),
  );
}

test('the dupes radar renders its empty state from the live endpoint', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);
  await page.evaluate(() => (window as unknown as { nav: (v: string) => void }).nav('dupes'));
  // One registered project in the fixture ⇒ zero cross-project pairs, and
  // the empty state — not an error panel — must say so.
  await expect(page.locator('.dupe-empty-t')).toHaveText('No cross-project duplicates');
  await assertNoConsoleErrors(errors);
});

test('a cross-project pair renders as duplicates with ids, projects, scopes and scores', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);
  await serveRadar(page, { pairs: [ONE_PAIR] });
  await page.evaluate(() => (window as unknown as { nav: (v: string) => void }).nav('dupes'));

  await expect(page.locator('.dupe-card')).toHaveCount(1);
  await expect(page.locator('#dupes-body')).toContainText('1 duplicated ask across projects');

  // AC2 — the home side: badge, title, project, id, scope.
  const head = page.locator('.dupe-head');
  await expect(head).toContainText('duplicate');
  await expect(head).toContainText('Fix flaky login');
  await expect(head).toContainText('Alpha');
  await expect(head).toContainText('CXC-F101');
  await expect(head).toContainText('session tokens expire mid-run');

  // AC2 — each duplicate row: title, project, id, scope, computed score.
  const rows = page.locator('.dupe-row');
  await expect(rows).toHaveCount(2);
  await expect(rows.nth(0)).toContainText('fix flaky login');
  await expect(rows.nth(0)).toContainText('Beta');
  await expect(rows.nth(0)).toContainText('CXC-F201');
  await expect(rows.nth(0)).toContainText('login drops after every refresh');
  await expect(rows.nth(0)).toContainText('similarity 1.00');
  await expect(rows.nth(1)).toContainText('Fix the flaky login flow');
  await expect(rows.nth(1)).toContainText('Gamma');
  await expect(rows.nth(1)).toContainText('CXC-F301');
  await expect(rows.nth(1)).toContainText('similarity 0.67');

  // Every row hands the human all three verdicts — nothing auto-suppressed.
  for (const verdict of ['Redirect', 'Reject', 'Allow both']) {
    await expect(rows.nth(0).getByRole('button', { name: verdict })).toBeVisible();
  }
  await assertNoConsoleErrors(errors);
});

test('allow-and-keep posts the pair verdict and the radar drops the pair', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);
  const state = { pairs: [ONE_PAIR] };
  await serveRadar(page, state);
  let actionBody: Record<string, unknown> | null = null;
  await page.route('**/api/workspace/duplicates/action', async (route) => {
    actionBody = route.request().postDataJSON() as Record<string, unknown>;
    state.pairs = []; // the allow verdict excludes the pair from future runs
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ ok: true }),
    });
  });
  await page.evaluate(() => (window as unknown as { nav: (v: string) => void }).nav('dupes'));
  const rows = page.locator('.dupe-row');
  await expect(rows).toHaveCount(2);

  await rows.nth(0).getByRole('button', { name: 'Allow both' }).click();
  expect(actionBody).toEqual({
    action: 'allow',
    home_project_id: 'dupe-p1',
    home_ticket_id: 'CXC-F101',
    dup_project_id: 'dupe-p2',
    dup_ticket_id: 'CXC-F201',
  });
  // The radar re-runs over the persisted verdict: the allowed pair is gone
  // while the view stays console-clean.
  await expect(page.locator('.dupe-empty-t')).toHaveText('No cross-project duplicates');
  await assertNoConsoleErrors(errors);
});

test('redirect confirms through the modal and retires the duplicate into the home ticket', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);
  const state = { pairs: [ONE_PAIR] };
  await serveRadar(page, state);
  let actionBody: Record<string, unknown> | null = null;
  await page.route('**/api/workspace/duplicates/action', async (route) => {
    actionBody = route.request().postDataJSON() as Record<string, unknown>;
    state.pairs = []; // the retire verdict takes the dup off the board
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ ok: true, action: 'redirect' }),
    });
  });
  await page.evaluate(() => (window as unknown as { nav: (v: string) => void }).nav('dupes'));
  const rows = page.locator('.dupe-row');
  await expect(rows).toHaveCount(2);

  // A board-changing verdict is confirmed, never one-click silent.
  await rows.nth(1).getByRole('button', { name: 'Redirect' }).click();
  await expect(page.locator('#cm-title')).toHaveText('Redirect the duplicate');
  await page.locator('#cm-ok').click();
  expect(actionBody).toEqual({
    action: 'redirect',
    home_project_id: 'dupe-p1',
    home_ticket_id: 'CXC-F101',
    dup_project_id: 'dupe-p3',
    dup_ticket_id: 'CXC-F301',
  });
  await expect(page.locator('.dupe-empty-t')).toHaveText('No cross-project duplicates');
  await assertNoConsoleErrors(errors);
});
