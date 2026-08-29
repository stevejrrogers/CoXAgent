// Hybrid-team surfaces: the Inbox view and the ticket dialog's assignee row.
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

test('the inbox view renders (empty state) and the nav badge stays hidden', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);

  await page.locator('a[data-v="inbox"]').click();
  await expect(page.locator('#inbox-body')).toContainText('Nothing waits on you');
  await expect(page.locator('#inbox-badge')).toBeHidden();

  await assertNoConsoleErrors(errors);
  await expect(page).toHaveScreenshot('inbox-empty.png');
});

test('the ticket dialog offers assignment to a person and back to the agents', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);

  await page.locator('a[data-v="board"]').click();
  await page.getByText('Search box scopes per tab').first().click();
  const dialog = page.locator('#ticket-body');
  await expect(dialog).toContainText('Assignee');
  await expect(dialog.locator('#tk-assign-sel')).toBeVisible();

  // Assign → the chip appears; return → back to the pool. Round-trips so the
  // frozen fixture is left unchanged for the other specs.
  await page.evaluate(async () => {
    await (window as any).assignTicket('F001', 'luffy');
  });
  await expect(dialog).toContainText('@luffy');
  await page.evaluate(async () => {
    await (window as any).assignTicket('F001', '');
  });
  await expect(dialog).toContainText('agents (pool)');

  await assertNoConsoleErrors(errors);
});
