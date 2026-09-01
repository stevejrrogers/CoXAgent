// Global search (CXA-F275): one box across tickets, wiki and chat threads.
// Cmd/Ctrl+K opens the palette; a query returns labeled hits grouped by kind,
// each deep-linking to its surface (ticket dialog, wiki page, anchored chat
// message). Runs against the frozen fixture state: tickets F001/B002 carry
// "search" in their titles, the standup chat message says "search scoping",
// and the wiki page is "Deploy health gate" (body: "health endpoint answers").
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

const openPalette = async (page) => {
  await page.keyboard.press('ControlOrMeta+k');
  await expect(page.locator('#ov-chatsearch.open')).toBeVisible();
};

test('global search returns labeled tickets, wiki and chat hits and deep-links each', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);

  await openPalette(page);
  await page.locator('#chatsearch-input').fill('search');
  // Server-backed kinds render in labeled groups; the ticket and the chat
  // message both match the fixture, each carrying its kind chip.
  await expect(page.locator('#chatsearch-list .cs-item[data-kind="ticket"]').first()).toBeVisible();
  await expect(page.locator('#chatsearch-list .cs-item[data-kind="message"]').first()).toBeVisible();
  await expect(page.locator('#chatsearch-list .cmdk-cat', { hasText: 'TICKETS' })).toBeVisible();
  await expect(page.locator('#chatsearch-list .cmdk-cat', { hasText: 'CHAT' })).toBeVisible();

  await expect(page).toHaveScreenshot('search.png', { fullPage: false });

  // A ticket hit deep-links into the ticket dialog — whichever ticket the
  // server ranked first (both fixture tickets start with "Search").
  const ticketRow = page.locator('#chatsearch-list .cs-item[data-kind="ticket"]').first();
  const ticketId = await ticketRow.getAttribute('data-id');
  await ticketRow.click();
  await expect(page.locator('#ov-ticket.open')).toBeVisible();
  await expect(page.locator('#ticket-body')).toContainText(` ${ticketId} · `);
  await page.locator('#ov-ticket.open .x').first().click(); // close the dialog

  // A chat hit lands on the anchored message in the chat mode (chat is a
  // mode, not a hash view — the message row is the proof it got there).
  await openPalette(page);
  await page.locator('#chatsearch-input').fill('search');
  await expect(page.locator('#chatsearch-list .cs-item[data-kind="message"]').first()).toBeVisible();
  await page.locator('#chatsearch-list .cs-item[data-kind="message"]').first().click();
  await expect(page.locator('#msg-18c77b8aa5c446500')).toBeVisible();

  // A wiki hit opens the page itself (DOC_CUR set), not just the docs view.
  await openPalette(page);
  await page.locator('#chatsearch-input').fill('health');
  await expect(page.locator('#chatsearch-list .cs-item[data-kind="page"]').first()).toBeVisible();
  await page.locator('#chatsearch-list .cs-item[data-kind="page"]').first().click();
  await expect(page.locator('#view-docs.on')).toBeVisible();
  await expect(page.locator('#docs-main')).toContainText('Deploy health gate');
  // DOC_CUR is a classic-script global (not a window property) — read it via a
  // bare identifier inside the page, the same way helpers.mjs reads STATE.
  await expect
    .poll(() =>
      page.evaluate(() => {
        try {
          // eslint-disable-next-line no-undef
          return typeof DOC_CUR !== 'undefined' ? String(DOC_CUR) : '';
        } catch {
          return '';
        }
      }),
    )
    .toBe('deploy-health-gate');

  await assertNoConsoleErrors(errors);
});

test('short and zero-match queries show the explicit empty state', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);

  await openPalette(page);
  // Shorter than 2 characters: the explicit minimum-length hint, no fetch.
  await page.locator('#chatsearch-input').fill('s');
  await expect(page.locator('#chatsearch-list')).toContainText('Type at least 2 characters');

  // Zero matches: "No results for …", never a blank panel.
  await page.locator('#chatsearch-input').fill('zzqx');
  await expect(page.locator('#chatsearch-list')).toContainText('No results for');

  await assertNoConsoleErrors(errors);
});
