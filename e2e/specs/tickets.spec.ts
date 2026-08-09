// New-ticket dialog: full field set renders and cancels without mutating.
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

test('the new-ticket dialog offers every field then cancels cleanly', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);

  await page.evaluate(() => (window as any).openNewTicket());
  const dlg = page.locator('#ov-newticket.open');
  await expect(dlg).toBeVisible();

  await expect(dlg.locator('#nt-title')).toHaveAttribute('placeholder', 'e.g. Add password reset');
  await expect(dlg.locator('#nt-desc')).toBeVisible();
  for (const opt of ['feature', 'bug', 'chore']) {
    await expect(dlg.locator('#nt-type option', { hasText: opt })).toHaveCount(1);
  }
  for (const sel of ['#nt-prio', '#nt-cx']) {
    await expect(dlg.locator(sel)).toBeVisible();
  }
  await expect(dlg.locator('#nt-ac')).toBeVisible();
  await expect(dlg.getByText('Save & Start Flow')).toBeVisible();
  await expect(dlg.getByText('Save', { exact: true })).toBeVisible();

  // Cancel closes; nothing committed.
  await dlg.getByText('Cancel').click();
  await expect(page.locator('#ov-newticket')).not.toBeVisible();

  await assertNoConsoleErrors(errors);
});
