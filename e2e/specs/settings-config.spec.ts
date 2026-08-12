// COX-B050: a coxagent.json the hub cannot read must stop the Settings screen,
// not be papered over with defaults. The screen is not read-only — Save PUTs
// back the whole document it was handed — so rendering the form from blanks
// would write defaults over every unrendered setting on disk (governance
// policy included) the next time an operator pressed Save.
import { test, expect } from '@playwright/test';
import { openApp } from './helpers.mjs';

const BROKEN = {
  error: 'deploy.host_port: 99999 is not a valid TCP port',
  field: 'deploy.host_port',
  detail: 'invalid value: integer `99999`, expected u16',
};

/// Answer the config GET with the hub's 422, and count every write attempt.
async function serveBrokenConfig(page: import('@playwright/test').Page) {
  const writes: string[] = [];
  await page.route('**/api/projects/*/config', async (route) => {
    if (route.request().method() === 'GET') {
      return route.fulfill({
        status: 422,
        contentType: 'application/json',
        body: JSON.stringify(BROKEN),
      });
    }
    writes.push(route.request().postData() ?? '');
    return route.fulfill({ status: 200, contentType: 'application/json', body: '{}' });
  });
  return writes;
}

test('an unreadable coxagent.json shows the broken field instead of a form', async ({ page }) => {
  await serveBrokenConfig(page);
  await openApp(page);
  await page.locator('a[data-v="settings"]').click();

  const body = page.locator('#settings-body');
  await expect(body).toContainText('coxagent.json could not be read');
  // The one broken field is named, so the operator knows what to fix.
  await expect(body.locator('code')).toHaveText('deploy.host_port');
  // No form, therefore nothing to save over the file we could not parse.
  await expect(body.locator('button.save')).toHaveCount(0);
});

test('saving is refused while the config is unreadable', async ({ page }) => {
  const writes = await serveBrokenConfig(page);
  await openApp(page);
  await page.locator('a[data-v="settings"]').click();
  await expect(page.locator('#settings-body')).toContainText('coxagent.json could not be read');

  // Defence in depth: even called directly, the save must not PUT a config
  // assembled from blanks over a document the hub never parsed.
  await page.evaluate(() => (window as unknown as { saveSettings: () => Promise<void> }).saveSettings());

  expect(writes, `a config was written despite the load failing:\n${writes.join('\n')}`).toEqual([]);
});
