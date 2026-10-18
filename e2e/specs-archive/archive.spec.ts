// The populated archive surface (CXA-F274): the seeded cold store shows up on
// the board (chip, column, closed summary) and archived tickets open as
// read-only dialogs. Console gate armed on every test.
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from '../specs/helpers.mjs';

test('the archive api serves the seeded tickets with the board shape', async ({ request }) => {
  const list = await request.get('/api/projects/default/tickets/archive');
  expect(list.status()).toBe(200);
  const doc = await list.json();
  expect(doc.total).toBe(3);
  expect(doc.tickets).toHaveLength(3);
  // Id-descending: newest ids first (lexicographic over the id string).
  expect(doc.tickets.map((t) => t.id)).toEqual(['CXC-F090', 'CXC-F013', 'CXC-B044']);
  for (const t of doc.tickets) {
    expect(t.archived).toBe(true);
    expect(t.design).toBeUndefined();
  }

  const detail = await request.get('/api/projects/default/ticket/CXC-B044');
  expect(detail.status()).toBe(200);
  const t = await detail.json();
  expect(t.archived).toBe(true);
  expect(t.title).toBe('Publish assigned host port in compose');
  expect(t.acceptance_criteria).toHaveLength(2);
  expect(t.test_cases[0].status).toBe('passed');
});

test('the board shows the Archived chip, the Archive column and the closed summary', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);

  await page.locator('a[data-v="board"]').click();
  await expect(page.locator('#board-filters')).toContainText('Archived · 3');
  await expect(page.locator('#board-cols .col h3', { hasText: 'Archive' })).toBeVisible();
  const summary = page.locator('#board-filters span', { hasText: 'hot +' });
  await expect(summary).toContainText('closed:');
  await expect(summary).toContainText('3 archived');
  // Seeded cards render with their saved status chips.
  await expect(page.locator('#board-cols')).toContainText('Publish assigned host port in compose');
  await expect(page.locator('#board-cols')).toContainText('Vendor lock-in audit');
  await assertNoConsoleErrors(errors);
  await expect(page).toHaveScreenshot('board-archive.png');
});

test('an archived ticket opens as a read-only dialog', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);

  await page.locator('a[data-v="board"]').click();
  await expect(page.locator('#board-cols')).toContainText('Publish assigned host port in compose');
  await page.locator('.card-t', { hasText: 'CXC-B044' }).click();
  const dialog = page.locator('#ov-ticket');
  await expect(dialog).toContainText('ARCHIVED');
  await expect(dialog).toContainText('served from the cold store');
  // The saved fields render: checklist + test-case verdicts.
  await expect(dialog).toContainText('compose publishes the assigned host port');
  // The verdict badge renders "Passed" in the DOM; CSS uppercases it visually.
  await expect(dialog).toContainText('Passed');
  // Every mutation affordance is suppressed.
  await expect(dialog.locator('button', { hasText: 'Edit' })).toHaveCount(0);
  await expect(dialog.locator('button', { hasText: 'Reject' })).toHaveCount(0);
  await expect(dialog.locator('button', { hasText: 'Attach file' })).toHaveCount(0);
  await expect(dialog.locator('.att-del')).toHaveCount(0);
  await expect(dialog.locator('#tkc-input')).toHaveCount(0);
  await expect(dialog).toContainText('This ticket is archived — history is read-only.');
  await assertNoConsoleErrors(errors);
  await expect(dialog).toHaveScreenshot('ticket-archived.png');
});

test('toggling the chip off returns the board to the hot columns only', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);

  await page.locator('a[data-v="board"]').click();
  await expect(page.locator('#board-filters')).toContainText('Archived · 3');
  await page.locator('#board-filters .fchip', { hasText: 'Archived' }).click();
  await expect(page.locator('#board-cols')).not.toContainText('Vendor lock-in audit');
  await expect(page.locator('#board-filters span', { hasText: 'hot +' })).toHaveCount(0);
  // And back on.
  await page.locator('#board-filters .fchip', { hasText: 'Archived' }).click();
  await expect(page.locator('#board-cols')).toContainText('Vendor lock-in audit');
  await assertNoConsoleErrors(errors);
});
