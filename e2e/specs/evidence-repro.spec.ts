// CXA-F248 golden baselines: a criterion's resolved live-reproduction URL —
// recorded on the test-case evidence by the TEST verdict flow — renders as a
// clickable link beside the screenshot/API proof in the reviewer's ticket
// modal, and ONLY when it is present: a case with no resolved route gets no
// link at all (the no-fabrication half of the contract).
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

test('evidence repro links render per test case and only when resolved', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);

  await page.evaluate(() => (window as any).showTicket('F248'));
  const dlg = page.locator('#ov-ticket.open');
  await expect(dlg).toBeVisible();
  await expect(dlg).toContainText('Settings autosave indicator');

  // Exactly the two cases whose evidence carries a resolved repro get a link;
  // the pending third case (no evidence at all) renders none.
  const links = dlg.locator('.tcitem .tcrepro');
  await expect(links).toHaveCount(2);
  await expect(links.nth(0)).toHaveAttribute('href', 'http://127.0.0.1:8101/settings');
  await expect(links.nth(1)).toHaveAttribute('href', 'http://127.0.0.1:8101/settings#autosave');
  const items = dlg.locator('.tcitem');
  await expect(items.nth(2).locator('.tcrepro')).toHaveCount(0);

  await expect(page).toHaveScreenshot('ticket-evidence-repro.png');
  await assertNoConsoleErrors(errors);
});
