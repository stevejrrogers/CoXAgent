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

  // CXA-F024: the Test Coverage tab maps each acceptance criterion to its
  // coverage status. The seeded ticket has criteria but no verdicts yet, so
  // every row must read NOT TESTED — the gap made visible. (Folded into this
  // load on purpose: the auth rate limiter is a shared 20-req/60s bucket and
  // every extra page load in the suite spends it.)
  await dlg.locator('.tk-tab[data-tk-tab="coverage"]').click();
  const pane = dlg.locator('#tk-pane-coverage');
  await expect(pane).toBeVisible();
  await expect(dlg.locator('#tk-pane-details')).toBeHidden();
  await expect(pane.locator('.cov-row')).toHaveCount(2);
  await expect(pane).toContainText('Switching tabs switches the search scope');
  await expect(pane).toContainText('Cmd+K focuses the box');
  await expect(pane.locator('.cov-badge', { hasText: 'NOT TESTED' })).toHaveCount(2);

  // Edge case: criteria deliberately left empty after clarification — no rows
  // to cover, and the matrix says so instead of rendering nothing. Reuses the
  // same page session (no new page load).
  await page.evaluate(() => window.showTicket('B001'));
  // The modal re-renders in place; anchor on B001's own title so the click
  // below cannot land on the still-mounted F001 modal.
  await expect(dlg).toContainText('Delivery timeline connector line breaks');
  await page.locator('.tk-tab[data-tk-tab="coverage"]').click();
  await expect(page.locator('#tk-pane-coverage')).toContainText('No acceptance criteria to cover');

  // CXA-F241: the Forensics tab — per-gate evidence inspection. The seeded
  // tickets carry no gate decisions and no evidence (DoD gates need a human
  // or agent pass, which HTTP seeding cannot honestly produce), so the pane
  // must render the explicit empty state: absence distinguishable from
  // omission, never a blank pane.
  await page.evaluate(() => window.showTicket('F001'));
  await expect(dlg).toContainText('Search box scopes per tab');
  await dlg.locator('.tk-tab[data-tk-tab="forensics"]').click();
  const fg = dlg.locator('#tk-pane-forensics');
  await expect(fg).toBeVisible();
  await expect(dlg.locator('#tk-pane-details')).toBeHidden();
  await expect(fg).toContainText('No DoD evidence captured for this ticket');

  await assertNoConsoleErrors(errors);
});
