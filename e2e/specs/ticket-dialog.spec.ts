// CXA-F004 golden baselines for the ticket dialogs: the create form and the
// full read view of a seeded ticket (assignee row, acceptance criteria,
// design attachments section).
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

test('the new-ticket form renders every field', async ({ page }) => {
  const errors = [];
  armConsoleGate(page, errors);
  await openApp(page);

  await page.evaluate(() => window.openNewTicket());
  const dlg = page.locator('#ov-newticket.open');
  await expect(dlg).toBeVisible();
  await expect(dlg.locator('#nt-type option', { hasText: 'feature' })).toHaveCount(1);
  await expect(dlg.locator('#nt-prio option', { hasText: 'medium' })).toHaveCount(1);
  await expect(dlg.locator('#nt-cx option', { hasText: 'medium' })).toHaveCount(1);
  await expect(dlg.locator('#nt-ac')).toBeVisible();

  await expect(page).toHaveScreenshot('new-ticket-form.png');
  await assertNoConsoleErrors(errors);
});

test('the read dialog shows a seeded ticket with its acceptance criteria', async ({ page }) => {
  const errors = [];
  armConsoleGate(page, errors);
  await openApp(page);

  await page.evaluate(() => window.showTicket('F001'));
  const dlg = page.locator('#ov-ticket.open');
  await expect(dlg).toBeVisible();
  await expect(dlg).toContainText('Search box scopes per tab');
  await expect(dlg).toContainText('Acceptance criteria');
  await expect(dlg.getByText('agents (pool)')).toBeVisible();
  await expect(dlg.locator('#tk-assign-sel')).toBeVisible();

  await expect(page).toHaveScreenshot('ticket-read.png');
  await assertNoConsoleErrors(errors);
});
