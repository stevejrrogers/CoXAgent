// Slot collision radar (CXA-F329): the board warns when two tickets running
// in parallel slots declare the same files — and marks radar-blind tickets
// (no declared files) as low-confidence instead of silently OK.
//
// The hub fixture holds no running slots, so the live endpoint can only ever
// answer with an empty radar — that absence is asserted against the REAL
// server below (it pins the render wiring and the omission rule: no
// `derived.collisions` key, no badge, console-clean). The populated render
// contract runs over a mocked snapshot carrying the exact payload shape
// `lite_state_value` emits (pinned by the handler's Rust tests in
// server/status.rs) — the same determinism idiom helpers.openApp already uses
// for /api/engines and preflight. No server state is mutated, so the
// golden-screenshot specs see pristine fixture data.
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

/// One SSE tick + the poll-fallback GET, both serving the same crafted
/// snapshot. `handle(m)` renders `m.state` through the same `render()` the
/// real 1 Hz stream drives; `runner: null` is a no-op in renderRunner.
async function serveSnapshot(page, state) {
  const payload = JSON.stringify({ state, runner: null, viewers: 1, online: [] });
  const sse = `data: ${payload}\n\n`;
  await page.route('**/api/projects/*/events', (route) =>
    route.fulfill({ status: 200, contentType: 'text/event-stream', body: sse }),
  );
  await page.route('**/api/projects/*/state', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', body: payload }),
  );
}

/// A minimal snapshot shaped like lite_state_value output: the tickets the
/// board renders plus the additive derived.collisions key.
function snapshotWith(tickets, derived) {
  return {
    tickets,
    sprint: null,
    sprint_queue: [],
    chat: [],
    comments: [],
    questions: [],
    activity: [],
    history: [],
    sprints: [],
    ...(derived ? { derived } : {}),
  };
}

const SHARED = 'crates/app/src/main.rs';

test('the board renders running slots with no collision badge on the live fixture', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);
  await page.locator('a[data-v="board"]').click();
  // The fixture has no InProgress tickets at all: no badge anywhere, and the
  // absence of derived.collisions must not read as an error anywhere.
  await expect(page.locator('.card-t:has-text("COLLISION")')).toHaveCount(0);
  await assertNoConsoleErrors(errors);
});

test('two running slots sharing a file show the COLLISION badge with the partner id', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  // The exact payload shape the Rust wire tests pin: pairs carry both ids
  // (a < b) and the sorted shared files.
  await serveSnapshot(
    page,
    snapshotWith(
      [
        { id: 'CXC-F001', type: 'feature', title: 'first slot', status: 'in_progress', priority: 'medium' },
        { id: 'CXC-F002', type: 'feature', title: 'second slot', status: 'in_progress', priority: 'medium' },
        { id: 'CXC-B003', type: 'bug', title: 'blind slot', status: 'in_progress', priority: 'high' },
      ],
      {
        collisions: {
          pairs: [{ a: 'CXC-F001', b: 'CXC-F002', files: [SHARED] }],
          unknown_files: ['CXC-B003'],
        },
      },
    ),
  );
  await openApp(page);
  await page.locator('a[data-v="board"]').click();

  // AC1 — the colliding slot shows the advisory with its partner named, and
  // the shared files ride the tooltip so meaning never rides on color alone.
  // (Filter on the card's own .cid: the partner id also appears inside the
  // other card's COLLISION badge text.)
  const cardOf = (id: string) =>
    page.locator('.card-t', { has: page.locator(`.cid:text-is("${id}")`) });
  const first = cardOf('CXC-F001');
  await expect(first.locator('.badges').getByText('COLLISION CXC-F002')).toBeVisible();
  await expect(first.locator('[title*="same files"]')).toHaveAttribute('title', new RegExp(SHARED));
  const second = cardOf('CXC-F002');
  await expect(second.locator('.badges').getByText('COLLISION CXC-F001')).toBeVisible();

  // AC3 — the no-hint slot is visibly marked radar-blind, not silently OK.
  const blind = cardOf('CXC-B003');
  await expect(blind.locator('.badges').getByText('UNMAPPED')).toBeVisible();
  await assertNoConsoleErrors(errors);
});

test('running slots on disjoint files stay silent — no badge, console-clean', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  // AC2 — disjoint module families: no derived key at all (the omission rule
  // the Rust tests pin), so the board renders bare running tickets.
  await serveSnapshot(
    page,
    snapshotWith([
      { id: 'CXC-F001', type: 'feature', title: 'rust slot', status: 'in_progress', priority: 'medium' },
      { id: 'CXC-F002', type: 'feature', title: 'web slot', status: 'in_progress', priority: 'medium' },
    ]),
  );
  await openApp(page);
  await page.locator('a[data-v="board"]').click();
  await expect(page.locator('.card-t:has-text("CXC-F001")')).toBeVisible();
  await expect(page.locator('.card-t:has-text("COLLISION")')).toHaveCount(0);
  await expect(page.locator('.card-t:has-text("UNMAPPED")')).toHaveCount(0);
  await assertNoConsoleErrors(errors);
});
