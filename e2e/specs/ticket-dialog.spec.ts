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

// CXA-F251: the per-criterion verdict-trajectory churn view at the verify
// surface. The fixture's B002 rode two real send-back cycles (the ledger
// holds both verify_send_back decisions), so its test cases carry an honest
// history: one stable criterion, one that flipped PASS->FAIL->PASS, and one
// abandoned after round 1 — the gap case.
test('the verify surface shows each criterion verdict trajectory and churn', async ({ page }) => {
  const errors = [];
  armConsoleGate(page, errors);
  await openApp(page);

  const dlg = page.locator('#ov-ticket.open');
  await page.evaluate(() => window.showTicket('B002'));
  await expect(dlg).toContainText('Search drops the final query while typing fast');
  const rows = dlg.locator('.tcitem');

  // Stable criterion: three PASS rounds, no churn badge. Its round-3
  // evidence_refresh record (screenshot attached, verdict unchanged) must
  // NOT read as a fourth verdict transition — the design's named hazard.
  const stable = rows.filter({ hasText: 'Search returns results for the full typed term' });
  await expect(stable.locator('.tctraj-v.pass')).toHaveCount(3);
  await expect(stable.locator('.tctraj-churn')).toHaveCount(0);
  await expect(stable.locator('.tctraj-gap')).toHaveCount(0);

  // Oscillating criterion (AC2): PASS->FAIL->PASS renders the amber churn
  // badge with the flip count — a warning, never a block on the decision.
  const churned = rows.filter({ hasText: 'Rapid typing never drops the final query' });
  await expect(churned.locator('.tctraj-v.pass')).toHaveCount(2);
  await expect(churned.locator('.tctraj-v.fail')).toHaveCount(1);
  await expect(churned.locator('.tctraj-churn')).toContainText('2 flips');

  // Gap criterion (AC4): failed in round 1, never re-judged — rounds 2 and 3
  // render explicit gaps, distinct from PASS and FAIL, never implied passes.
  const gapped = rows.filter({ hasText: 'Clearing the box resets the result list' });
  await expect(gapped.locator('.tctraj-v.fail')).toHaveCount(1);
  await expect(gapped.locator('.tctraj-gap')).toHaveCount(2);
  await expect(gapped.locator('.tctraj-gap').first()).toContainText('no verdict');

  // AC3: a criterion verified for the first time says so instead of
  // fabricating an empty history. F002 has a pending case and no send-backs.
  await page.evaluate(() => window.showTicket('F002'));
  await expect(dlg).toContainText('Sub-channels under a parent channel');
  const first = dlg.locator('.tcitem');
  await expect(first.locator('.tctraj-cy', { hasText: 'first cycle' })).toHaveCount(1);
  await expect(first.locator('.tctraj-v')).toHaveCount(0);

  await expect(page).toHaveScreenshot('ticket-churn.png');

  await assertNoConsoleErrors(errors);
});
