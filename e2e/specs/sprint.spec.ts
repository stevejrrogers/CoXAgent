// Sprint scope controls: pull a backlog ticket into the running sprint, drop
// it again, and reject nonsense. The scope round-trips so the frozen fixture is
// left exactly as the other specs expect it.
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

/// The project the fixture serves. `PID` is a top-level `const` in a classic
/// script, so it is NOT reachable as `window.PID` from an evaluate block.
async function projectId(page): Promise<string> {
  return page.evaluate(async () => {
    const list = await (await fetch('/api/projects')).json();
    return list[0].id as string;
  });
}

test('the backlog tab offers a sprint-scope control on each row', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);

  await page.locator('a[data-v="board"]').click();
  await page.locator('#work-seg button[data-w="backlog"]').click();
  const row = page.locator('#backlog-body .act').first();
  await expect(row).toBeVisible();
  await expect(row.locator('.sp-scope')).toHaveCount(1);

  await assertNoConsoleErrors(errors);
});

test('a backlog ticket can be pulled into the sprint and dropped again', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);
  const pid = await projectId(page);

  await page.locator('a[data-v="board"]').click();
  await page.locator('#work-seg button[data-w="backlog"]').click();
  const id = (await page.locator('#backlog-body .act .tk').first().innerText()).trim();

  const call = (action: string) =>
    page.evaluate(
      async ([p, a, t]) => {
        const r = await fetch(`/api/projects/${p}/sprint/${a}`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ tickets: [t] }),
        });
        return { status: r.status, body: await r.text() };
      },
      [pid, action, id],
    );

  expect((await call('commit')).status).toBe(200);
  expect((await call('drop')).status).toBe(200);

  await assertNoConsoleErrors(errors);
});

// No console gate here: the whole point is a deliberate 400, which the browser
// logs as a failed resource. Gating on it would fail the test that proves the
// server rejects nonsense.
test('a ticket id that is not one is rejected rather than silently ignored', async ({ page }) => {
  await openApp(page);
  const pid = await projectId(page);

  const status = await page.evaluate(async (p) => {
    const r = await fetch(`/api/projects/${p}/sprint/commit`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ tickets: ['not a ticket id'] }),
    });
    return r.status;
  }, pid);
  expect(status).toBe(400);

});

test('a sprint can be queued from the backlog tab and dropped again', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);

  await page.locator('a[data-v="board"]').click();
  await page.locator('#work-seg button[data-w="backlog"]').click();

  // Queue a sprint through the modal.
  await page.getByRole('button', { name: /New sprint/ }).click();
  await page.locator('#cm-input').fill('harden the queue e2e');
  await page.locator('#cm-ok').click();
  const card = page.locator('#backlog-body .spq-card', { hasText: 'harden the queue e2e' });
  await expect(card).toBeVisible();
  await expect(card).toContainText('up next');

  // Scope a ticket onto the plan through the API the drag uses, then see it.
  const pid = await projectId(page);
  const qid = await page.evaluate(async (pid) => {
    const s = await (await fetch(`/api/projects/${pid}/state`)).json();
    return s.sprint_queue[0].id as number;
  }, pid);
  await page.evaluate(async ({ pid, qid }) => {
    await fetch(`/api/projects/${pid}/sprint-queue/${qid}/scope`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ add: ['F001'], remove: [] }),
    });
  }, { pid, qid });
  await page.locator('#work-seg button[data-w="board"]').click();
  await page.locator('#work-seg button[data-w="backlog"]').click();
  await expect(page.locator('#backlog-body .spq-chip', { hasText: 'F001' })).toBeVisible();

  // Drop the plan — the fixture leaves exactly as the other specs expect.
  await card.locator('button', { hasText: 'plan' }).click();
  await page.locator('#cm-ok').click();
  await expect(page.locator('#backlog-body .spq-card', { hasText: 'harden the queue e2e' })).toHaveCount(0);

  await assertNoConsoleErrors(errors);
});

test('a ticket can be put on hold, filtered by status, and resumed', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);
  const pid = await projectId(page);

  // Hold F002 through the API the dialog button uses.
  await page.evaluate(async (pid) => {
    await fetch(`/api/projects/${pid}/ticket/F002/status/hold`, { method: 'POST' });
  }, pid);
  await page.locator('a[data-v="board"]').click();
  await page.locator('#work-seg button[data-w="board"]').click();
  await expect(page.locator('#board-cols .card-t', { hasText: 'F002' })).toContainText('on hold');

  // The status filter isolates held tickets.
  await page.locator('#board-filters .fchip', { hasText: 'on hold' }).click();
  await expect(page.locator('#board-cols .card-t')).toHaveCount(1);
  await page.locator('#board-filters .fchip', { hasText: 'all statuses' }).click();

  // The backlog row is marked and offers no sprint pull while held.
  await page.locator('#work-seg button[data-w="backlog"]').click();
  const held = page.locator('#backlog-body .act', { hasText: 'F002' });
  await expect(held).toContainText('on hold');
  await expect(held.locator('.sp-scope')).toHaveCount(0);

  // Resume — the fixture leaves exactly as the other specs expect.
  await page.evaluate(async (pid) => {
    await fetch(`/api/projects/${pid}/ticket/F002/status/resume`, { method: 'POST' });
  }, pid);
  await page.locator('#work-seg button[data-w="board"]').click();
  await page.locator('#work-seg button[data-w="backlog"]').click();
  await expect(page.locator('#backlog-body .act', { hasText: 'F002' }).locator('.sp-scope')).toHaveCount(1);

  await assertNoConsoleErrors(errors);
});
