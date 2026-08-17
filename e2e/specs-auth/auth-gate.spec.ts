import { test, expect } from '@playwright/test';
import { ADMIN_USER, ADMIN_PASSWORD } from './helpers-auth.mjs';

// CXA-F001 auth gate: pipeline bootstrap + AC2 return-path redirection.
//
// These cover what is unique to the auth surface's front door:
//   - AC1: an authenticated server boots and rejects unauthenticated traffic.
//   - AC2: a guest who lands with ?next=<view> is gated first, then returned
//     to that view once they sign in (shell.js rememberDestination).
// Cross-cutting AC3/AC4/AC5 behaviours live in their own specs (login,
// validation, guest-protected, rbac-viewer) rather than being duplicated here.

test('AC1 pipeline bootstraps and gates unauthenticated requests', async ({ page }) => {
  const health = await page.request.get('/api/health');
  expect(health.status()).toBe(200);

  const noBody = await page.request.post('/api/auth/login', { data: {} });
  expect(noBody.status()).toBeGreaterThanOrEqual(400);
  expect(noBody.status()).toBeLessThan(500);
});

test('AC2 ?next returns a guest to their requested view after sign-in', async ({ page }) => {
  // Fresh context: no session cookie. Ask for /insights via ?next; the app
  // must gate first and only land there after a successful login.
  await page.goto('/?next=%23insights');

  await expect(page.locator('#ov-login')).toBeVisible();

  // Sign in through the form as an authenticated user would.
  await page.locator('#lg-user').fill(ADMIN_USER);
  await page.locator('#lg-pass').fill(ADMIN_PASSWORD);
  await page.locator('#ov-login button.pri').click();

  // The requested view renders once authenticated.
  await expect(page.locator('#view-insights')).toBeVisible();
});
