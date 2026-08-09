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
