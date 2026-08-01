// Wiki/docs: the seeded page opens and renders its body.
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

test('the seeded wiki page is reachable and renders', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);

  // Open the Docs view, then the seeded page.
  await page.locator("a[onclick*=\"nav('docs')\"]").first().click();
  await page.getByText('Deploy health gate', { exact: false }).first().click();
  await expect(page.locator('body')).toContainText('health endpoint answers');

  await assertNoConsoleErrors(errors);
  await expect(page).toHaveScreenshot('docs.png', { fullPage: false });
});
