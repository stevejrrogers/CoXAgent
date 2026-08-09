import { test, expect } from '@playwright/test';

// AC2: an unauthenticated browser must not reach protected pages, whatever
// route (including /#manage) it lands on. boot() in shell.js calls /api/auth/me
// first and opens only the login overlay (#ov-login) when there is no session,
// so no authenticated content ever renders for a guest.
test('a guest on /#manage gets only the login form', async ({ page }) => {
  // Fresh context: no session cookie.
  await page.goto('/#manage');

  // Login overlay is what a guest sees — nothing else.
  await expect(page.locator('#ov-login')).toBeVisible();

  // Server agrees: both the identity probe and the management surface are gated.
  const me = await page.request.get('/api/auth/me');
  expect(me.status()).toBe(401);

  const overview = await page.request.get('/api/manage/overview', {
    ignoreHTTPSErrors: true,
    failOnStatusCode: false,
  });
  expect(overview.status()).toBe(401);
});
