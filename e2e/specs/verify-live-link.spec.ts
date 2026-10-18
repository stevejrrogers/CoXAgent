// CXA-F245 (CXA-F242-C) + CXA-F247: the verify gate's live reproduction link.
//
// The inbox verify card renders the "Open live preview" anchor (CXA-F247:
// target=_blank rel=noopener on the element, https?-whitelisted href) and the
// reviewer's ticket modal the "Open live instance" control (F245), exactly
// when the payload carries a reproduce_url (CXA-F244 ships the field;
// null/absent ⇒ no control at all). The card's anchor opens the instance in a
// NEW TAB, and the send-back dialog cites the URL that was shown.
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
  // Context-level route: the anchor's rel="noopener" popup is its own page,
  // which page.route does not cover — the mock must sit above both.
  await page.context().route(`${LIVE_URL}**`, (route) =>
    route.fulfill({ status: 200, contentType: 'text/html', body: '<html><body>live app</body></html>' }),
  );
  await openApp(page);

  await page.locator('a[data-v="inbox"]').click();
  await expect(page.locator('#inbox-body')).toContainText('CXC-V001');

  // Show axis: only the card carrying reproduce_url gets the control.
  const linked = page.locator('#inbox-body .panel', { hasText: 'CXC-V001' });
  const link = linked.getByRole('link', { name: 'Open live preview' });
  await expect(link).toHaveCount(1);
  const unlinked = page.locator('#inbox-body .panel', { hasText: 'CXC-V002' });
  await expect(unlinked.getByRole('link', { name: 'Open live preview' })).toHaveCount(0);

  // CXA-F247: the control is a real anchor — the open-in-new-tab contract
  // lives on the element itself, with the opener severed.
  await expect(link).toHaveAttribute('target', '_blank');
  await expect(link).toHaveAttribute('rel', /(^|\s)noopener/);
  await expect(link).toHaveAttribute('href', LIVE_URL);

  // New tab: the anchor opens the instance itself, not the dashboard.
  const popupPromise = page.waitForEvent('popup');
  await link.click();
  const popup = await popupPromise;
  expect(popup.url()).toBe(LIVE_URL);
  await popup.close();

  // Send back (CXA-F247 plan step 2): the refusal dialog cites the instance
  // that was shown, so the reason can reference what was seen. Cancelled —
  // nothing is sent.
  await linked.getByRole('button', { name: 'Send back' }).click();
  await expect(page.locator('#cm-msg')).toContainText(LIVE_URL);
  await page.locator('#cm-cancel').click();

  await assertNoConsoleErrors(errors);
  await expect(page).toHaveScreenshot('inbox-verify-live-link.png');
});

test('a hostile reproduction url never renders the anchor nor a send-back citation', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  // CXA-F247 AC2: only an https?:// URL may become the anchor's href — a
  // javascript: scheme or a protocol-relative //host inheriting the
  // dashboard's origin must render no link at all, and the send-back dialog
  // must cite nothing for it.
  await page.route('**/api/projects/*/inbox', (route) =>
    route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        items: [
          verifyCard('CXC-V005', 'Script scheme — no link', 'javascript:alert(1)'),
          verifyCard('CXC-V006', 'Protocol-relative — no link', '//evil.example'),
        ],
      }),
    }),
  );
  await openApp(page);

  await page.locator('a[data-v="inbox"]').click();
  for (const ticket of ['CXC-V005', 'CXC-V006']) {
    const card = page.locator('#inbox-body .panel', { hasText: ticket });
    await expect(card.getByRole('link', { name: 'Open live preview' })).toHaveCount(0);
    await card.getByRole('button', { name: 'Send back' }).click();
    await expect(page.locator('#cm-msg')).not.toContainText('Live instance reviewed');
    await page.locator('#cm-cancel').click();
  }

  await assertNoConsoleErrors(errors);
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
