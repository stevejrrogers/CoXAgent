import { test, expect } from '@playwright/test';
import { ADMIN_USER, ADMIN_PASSWORD } from './helpers-auth.mjs';

test('correct credentials authenticate', async ({ page }) => {
  const resp = await page.request.post('/api/auth/login', {
    data: { username: ADMIN_USER, password: ADMIN_PASSWORD },
  });
  expect(resp.status()).toBe(200);
  const body = await resp.json();
  expect(body.ok).toBe(true);
  expect(body.username).toBe(ADMIN_USER);
});

test('harvest minted bearer token', async ({ page }) => {
  // Logging in via request shares the context cookie store.
  const login = await page.request.post('/api/auth/login', {
    data: { username: ADMIN_USER, password: ADMIN_PASSWORD },
  });
  expect(login.status()).toBe(200);

  // Unique label so a previously-persisted minted token never collides (409).
  const label = `e2e-harvest-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
  const mint = await page.request.post('/api/auth/my/tokens', {
    data: { label },
  });
  expect(mint.status()).toBe(200);
  const minted = await mint.json();
  expect(minted.token).toBeTruthy();
  expect(minted.token.length).toBe(64);

  const me = await page.request.get('/api/auth/me', {
    headers: { Authorization: `Bearer ${minted.token}` },
    ignoreHTTPSErrors: true,
    failOnStatusCode: false,
  });
  const meBody = await me.json().catch(() => ({}));
  expect(meBody.auth).toBe(true);
});
