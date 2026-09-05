// Roadmap milestone strip depth (CXA-F361): per-milestone progress bars fed by
// the snapshot's derived read model (the same figures the projection endpoint
// serves), goal-complete rows collapsed with a check, and click-to-filter
// drill-in over the buckets with a second click clearing it. The snapshot is
// served by route-mocking the events stream the app actually consumes, so the
// real SSE -> render pipeline runs over deterministic data.
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

// created_at values are hard-coded (not derived from the clock) so the
// days-since figures stay deterministic forever — the persisted model carries
// no milestone date, the strip derives ages from the oldest linked ticket.
const FIXTURE_STATE = {
  current_version: '0.5.2',
  tickets: [
    { id: 'FEAT-OLD', type: 'feature', title: 'Shipped rollout wave', priority: 'medium', status: 'documented', created_at: '2026-06-07T12:00:00Z' },
    { id: 'FEAT-DONE', type: 'feature', title: 'Beta console gate hardening', priority: 'high', status: 'done', created_at: '2026-08-06T12:00:00Z' },
    { id: 'FEAT-PROG', type: 'feature', title: 'Beta progress bar rendering', priority: 'high', status: 'in_progress', created_at: '2026-08-16T12:00:00Z' },
    { id: 'FEAT-READY', type: 'feature', title: 'Beta drill-in strip', priority: 'medium', status: 'ready', created_at: '2026-08-26T12:00:00Z', design: { technical: 'plan' } },
    { id: 'FEAT-GA', type: 'feature', title: 'GA federation sync engine', priority: 'low', status: 'pending', created_at: '2026-08-27T12:00:00Z' },
  ],
  sprint: { number: 3, goal: 'Beta depth', started_cycle: 0, length_cycles: 10, committed: ['FEAT-DONE', 'FEAT-PROG', 'FEAT-READY'], started_at: '', bug_burn_floor: null },
  milestones: [
    { name: 'Shipped', goal: 'first shippable wave', target_version: '0.5.0', goal_complete: true, fulfilled: true },
    { name: 'Beta', goal: 'console gate, bars and drill-in', target_version: '0.9.0', goal_complete: false, fulfilled: false },
    { name: 'GA', goal: 'federation for every team', target_version: '1.0.0', goal_complete: false, fulfilled: false },
  ],
  // The milestone read model exactly as the backend serializes it (the Rust
  // sibling pins this shape): released -> 100 by the pipeline's own record,
  // the active target's closed share of its committed scope, no scope -> 0.
  derived: {
    milestones: [
      { id: 'Shipped', name: 'Shipped', target_version: '0.5.0', goal_complete: true, released: true, reached: true, committed: [], open: [], progress: 100 },
      { id: 'Beta', name: 'Beta', target_version: '0.9.0', goal_complete: false, released: false, reached: false, committed: ['FEAT-DONE', 'FEAT-PROG', 'FEAT-READY'], open: ['FEAT-PROG', 'FEAT-READY'], progress: 33 },
      { id: 'GA', name: 'GA', target_version: '1.0.0', goal_complete: false, released: false, reached: false, committed: [], open: [], progress: 0 },
    ],
  },
  history: [
    { version: '0.5.0', ticket: 'FEAT-OLD', title: 'rollout wave', at: '2026-06-07T12:00:00Z' },
    { version: '0.5.1', ticket: 'FEAT-OLD', title: 'patch wave', at: '2026-06-20T12:00:00Z' },
    { version: '0.5.2', ticket: 'FEAT-OLD', title: 'stability wave', at: '2026-07-04T12:00:00Z' },
  ],
};

const SSE_BODY = `data: ${JSON.stringify({ state: FIXTURE_STATE })}\n\n`;

test('milestone strip: progress bars, done collapse, drill-in toggle, console-clean', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  // The strip charts ages and forecast dates off Date.now while its data is
  // frozen — pin the clock (the space.spec pattern) or the golden rots daily.
  await page.addInitScript(() => {
    const fixed = Date.parse('2026-09-05T12:00:00Z');
    Date.now = () => fixed;
  });
  await page.route('**/api/projects/*/events', (route) =>
    route.fulfill({ status: 200, contentType: 'text/event-stream', body: SSE_BODY }),
  );
  await page.route('**/api/projects/*/state', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(FIXTURE_STATE) }),
  );
  await openApp(page);
  await page.evaluate(() => { (window as any).nav('roadmap'); });
  const body = page.locator('#roadmap-body');
  await expect(body).toContainText('Milestone roadmap');

  // AC1: one progress figure per milestone row, derived from ticket
  // completion — released row 100 by record, active row its committed
  // scope's closed share (1 of 3 -> 33%), unattributed row 0 with a
  // scope-not-committed hint instead of a bare zero.
  const bars = await page.$$eval('.msprog > div', els => els.map(el => (el as HTMLElement).style.width));
  expect(bars).toEqual(['100%', '33%', '0%']);
  await expect(body).toContainText('33%');
  await expect(body).toContainText('scope not committed');

  // AC1: the goal-complete row renders collapsed with a check, and each row
  // carries its derived days-since figure (oldest linked ticket's created_at).
  await expect(page.locator('.mscard.collapsed')).toHaveCount(1);
  await expect(page.locator('.mscard.collapsed')).toContainText('Shipped');
  await expect(page.locator('.mscard.collapsed')).toContainText('✓ complete');
  await expect(body).toContainText('90d');
  await expect(body).toContainText('30d');

  // The strip is the drill-in's selector — pin it before any interaction.
  await expect(page).toHaveScreenshot('roadmap-milestones.png', { fullPage: true });

  // AC2: clicking a milestone filters the buckets to its committed tickets —
  // open work names itself in Now/Next, delivered work in Shipped. The banner
  // sizes the lens honestly: 3 committed shown, the 2 uncommitted project
  // tickets named as hidden, not folded into the denominator.
  await page.locator('.mscard', { hasText: 'Beta' }).click();
  await expect(body).toContainText('Milestone Beta');
  await expect(body).toContainText('3 of 3 committed tickets shown');
  await expect(body).toContainText('2 other project tickets hidden');
  await expect(body).toContainText('Beta console gate hardening');
  await expect(body).toContainText('Beta drill-in strip');
  await expect(body).not.toContainText('GA federation sync engine');

  // AC2: a second click on the same milestone clears the filter — the banner
  // (and its Clear affordance) goes with it.
  await page.locator('.mscard', { hasText: 'Beta' }).click();
  await expect(body).toContainText('GA federation sync engine');
  await expect(body).not.toContainText('Clear filter');

  await assertNoConsoleErrors(errors);
});
