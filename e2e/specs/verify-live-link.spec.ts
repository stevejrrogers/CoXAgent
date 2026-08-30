// CXA-F245 (CXA-F242-C): the verify gate's live reproduction link.
//
// The inbox verify card and the reviewer's ticket modal must render an
// "Open live instance" control exactly when the payload carries a
// reproduce_url (CXA-F244 ships the field; null/absent ⇒ no control at all),
// and the card's control must open the instance in a NEW TAB.
//
// The frozen fixture seeds no verify-gate state, so these tests serve the
// verify payloads through route mocks (helpers.mjs keeps the rest of the app
// deterministic); the assertions run against the REAL view code —
// web/js/inbox.js renderInbox and web/js/chat.js showTicket.
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

// The suite never starts a real deploy, so the live instance is served by a
// route mock too — the click must still open a tab that lands somewhere.
const LIVE_URL = 'http://127.0.0.1:8101/';

const verifyCard = (ticket: string, title: string, url: string | null) => ({
  kind: 'verify',
  ticket,
  title,
  role: 'QA',
  can_act: true,
  reproduce_url: url,
});

// Minimal ticket-detail payload in the shape showTicket renders; the field
// under test rides alongside exactly as server/work.rs injects it.
const detail = (id: string, url: string | null) => ({
  id,
  title: 'Timeline fix needs a human verdict',
  type: 'bug',
  status: 'fixed',
  priority: 'high',
  complexity: 'medium',
  has_ui: true,
  description: 'Fixed and awaiting verification.',
  acceptance_criteria: ['The timeline renders without gaps'],
  reproduce_url: url,
});

test('the verify card links its live instance in a new tab and hides the link without one', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await page.route('**/api/projects/*/inbox', (route) =>
    route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        items: [
          verifyCard('CXC-V001', 'Deploy resolves — link shows', LIVE_URL),
          verifyCard('CXC-V002', 'No deploy — no link', null),
        ],
      }),
    }),
  );
  await page.route(`${LIVE_URL}**`, (route) =>
    route.fulfill({ status: 200, contentType: 'text/html', body: '<html><body>live app</body></html>' }),
  );
  await openApp(page);

  await page.locator('a[data-v="inbox"]').click();
  await expect(page.locator('#inbox-body')).toContainText('CXC-V001');

  // Show axis: only the card carrying reproduce_url gets the control.
  const linked = page.locator('#inbox-body .panel', { hasText: 'CXC-V001' });
  await expect(linked.getByRole('button', { name: 'Open live instance' })).toHaveCount(1);
  const unlinked = page.locator('#inbox-body .panel', { hasText: 'CXC-V002' });
  await expect(unlinked.getByRole('button', { name: 'Open live instance' })).toHaveCount(0);

  // New tab: the control opens the instance itself, not the dashboard.
  const popupPromise = page.waitForEvent('popup');
  await linked.getByRole('button', { name: 'Open live instance' }).click();
  const popup = await popupPromise;
  expect(popup.url()).toBe(LIVE_URL);
  await popup.close();

  await assertNoConsoleErrors(errors);
  await expect(page).toHaveScreenshot('inbox-verify-live-link.png');
});

test('the ticket modal shows the same live link driven by reproduce_url presence', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await page.route(/\/api\/projects\/[^/]+\/ticket\/[^/]+$/, (route) => {
    const id = new URL(route.request().url()).pathname.split('/').pop() ?? '';
    return route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify(detail(id, id === 'CXC-V003' ? LIVE_URL : null)),
    });
  });
  await openApp(page);

  const dialog = page.locator('#ticket-body');
  await page.evaluate(() => (window as any).showTicket('CXC-V003'));
  await expect(dialog).toContainText('Mark Verified');
  await expect(dialog.getByRole('button', { name: 'Open live instance' })).toBeVisible();

  // Same surface, null field (the F244 wire shape when no deploy resolves):
  // no control at all — never a dead button.
  await page.evaluate(() => (window as any).showTicket('CXC-V004'));
  await expect(dialog.getByRole('button', { name: 'Open live instance' })).toHaveCount(0);

  await assertNoConsoleErrors(errors);
});
