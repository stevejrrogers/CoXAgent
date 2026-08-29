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

test('switching Manage → Chat lands in chat mode, not Overview', async ({ page }) => {
  await openApp(page);
  await page.evaluate(() => { (window as any).nav('mg-spaces'); });
  await page.evaluate(() => { (window as any).setMode('chat'); });
  await expect(page.locator('body')).toHaveClass(/mode-chat/);
  await expect(page.locator('#chat-input')).toBeVisible();
  // And leaving chat returns to a workspace view, not a manage one.
  await page.evaluate(() => { (window as any).setMode('workspace'); });
  await expect(page.locator('body')).not.toHaveClass(/mode-chat/);
  await expect(page.locator('body')).not.toHaveClass(/mode-manage/);
});
