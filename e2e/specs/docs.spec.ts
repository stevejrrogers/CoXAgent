// Wiki/docs (CXA-F364 depth): the seeded page opens with its auto table of
// contents (3+ headings) and its backlinks footer; the sidebar search filters
// titles+bodies with highlighted matches and clearing restores the tree.
// Runs against the frozen fixture: "Deploy health gate" (4 sections) and the
// sibling "Rollout runbook" whose body references it by title.
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

const openWiki = async (page) => {
  await page.locator("a[onclick*=\"nav('docs')\"]").first().click();
};

test('the seeded wiki page renders with its auto TOC and backlinks footer', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);
  await openWiki(page);

  await page.getByText('Deploy health gate', { exact: false }).first().click();
  await expect(page.locator('body')).toContainText('health endpoint answers');

  // 4 markdown sections → the sticky TOC renders, one link per heading,
  // each targeting the anchor id the rendered heading actually carries.
  const toc = page.locator('#doc-toc');
  await expect(toc).toBeVisible();
  await expect(toc.locator('.doc-toc-l')).toHaveCount(4);
  const lastLink = toc.locator('.doc-toc-l').last();
  await expect(lastLink).toHaveAttribute('href', /#h-/);
  // Reading-position tracking: following a TOC link marks it active.
  await lastLink.click();
  await expect(toc.locator('.doc-toc-l.on')).toHaveCount(1);

  // Backlinks footer: the sibling runbook references this page by title.
  const foot = page.locator('#doc-backlinks');
  await expect(foot).toBeVisible();
  await expect(foot).toContainText('Referenced by (1)');
  await expect(foot).toContainText('Rollout runbook');

  await assertNoConsoleErrors(errors);
  await expect(page).toHaveScreenshot('docs.png', { fullPage: false });
});

test('the sidebar search filters titles and bodies and clearing restores the tree', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);
  await openWiki(page);

  const list = page.locator('#docs-list');
  await list.locator('.docfolder', { hasText: 'guides' }).click(); // expand the folder
  await expect(list.locator('.docitem')).toHaveCount(2); // the unfiltered tree

  const box = page.locator('#doc-search');
  await box.fill('rollout');
  const hits = list.locator('.docsearch-hit');
  // Body search reaches BOTH pages: the runbook by title, the gate page by a
  // body mention — title matches rank first, matches carry <mark>.
  await expect(hits).toHaveCount(2);
  await expect(hits.first()).toContainText('Rollout runbook');
  await expect(hits.first().locator('mark').first()).toBeVisible();

  // Zero matches: the explicit empty state, never a blank panel.
  await box.fill('zzqx');
  await expect(list).toContainText('No pages match');

  // Clearing the box redraws the FULL tree — no filtered stub left behind.
  await box.fill('');
  await expect(list.locator('.docitem')).toHaveCount(2);
  await expect(list).toContainText('Deploy health gate');

  await assertNoConsoleErrors(errors);
});
