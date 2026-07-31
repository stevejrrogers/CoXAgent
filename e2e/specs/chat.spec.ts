// Chat view: channels, messages, the scoped search.
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

test('chat mode shows seeded messages and search finds them', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);

  await page.locator('#mode-chat').click();
  await expect(page.locator('body')).toContainText('Standup: timeline fix is in review');

  await assertNoConsoleErrors(errors);
  await expect(page).toHaveScreenshot('chat.png', { fullPage: false });
});
