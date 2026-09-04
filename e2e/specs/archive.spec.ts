// Empty-archive invariance (CXA-F274): the fixture server boots with NO
// archive store, so the board, the API and the detail endpoint must be
// indistinguishable from the pre-archive behavior — the one-time probe
// answers empty and nothing user-visible appears.
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

test('the archive api answers the pinned empty shape', async ({ request }) => {
  const list = await request.get('/api/projects/default/tickets/archive');
  expect(list.status()).toBe(200);
  expect(await list.json()).toEqual({ tickets: [], total: 0 });

  const missing = await request.get('/api/projects/nowhere/tickets/archive');
  expect(missing.status()).toBe(404);

  // The detail fallback on a miss is the unchanged 404, byte for byte.
  const noTicket = await request.get('/api/projects/default/ticket/CXC-NOPE');
  expect(noTicket.status()).toBe(404);
  expect(await noTicket.text()).toBe('no such project');
});

test('the board renders without any archive surface', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);

  await page.locator('a[data-v="board"]').click();
  await expect(page.locator('#board-cols .col h3')).toContainText('Backlog');
  await expect(page.locator('#board-filters')).not.toContainText('Archived');
  await expect(page.locator('#board-filters')).not.toContainText('closed:');
  await expect(page.locator('#board-cols')).not.toContainText('Archive');
  await assertNoConsoleErrors(errors);
});
