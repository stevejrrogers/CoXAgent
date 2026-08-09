import { test, expect } from '@playwright/test';
import { ADMIN_USER, ADMIN_PASSWORD, apiLogin } from './helpers-auth.mjs';

const VIEWER_USER = 'viewere2e';
const VIEWER_PASSWORD = 'ViewerPass_12345';

// AC5: role 'viewer' is read-only server-side.
//
// Provisioning strategy: the auth file only ever contains adminos on a fresh
// boot (build_auth provisions just the admin from env; state is wiped per
// run). This spec creates its own isolated viewer through the admin API at
// runtime instead of depending on state left by another file or run.
// create_user on the backend is idempotent (existing username -> hash/role
// updated), so re-running never conflicts with a previously-persisted record.
test('viewer can read but writes are forbidden', async ({ page }) => {
  // Admin session cookie for provisioning.
  const admin = await apiLogin(page.request);
  expect(admin.status).toBe(200);
  await page.context().addCookies([
    { name: 'cox_session', value: admin.cookie, domain: '127.0.0.1', path: '/' },
  ]);

  const created = await page.request.post('/api/auth/users', {
    data: { username: VIEWER_USER, password: VIEWER_PASSWORD, role: 'viewer' },
  });
  expect(created.status()).toBe(200);

  // Log in as the fresh viewer over HTTP (separate request context).
  const vlogin = await page.request.post('/api/auth/login', {
    data: { username: VIEWER_USER, password: VIEWER_PASSWORD },
  });
  expect(vlogin.status()).toBe(200);

  // Read works for a viewer.
  const me = await page.request.get('/api/auth/me');
  expect(me.status()).toBe(200);
  const meBody = await me.json();
  expect(meBody.auth).toBe(true);
  expect(meBody.username).toBe(VIEWER_USER);
  expect(meBody.role).toBe('viewer');

  // A write on a management surface is forbidden for viewers (403).
  const forbidden = await page.request.post('/api/auth/users', {
    data: { username: 'nope-e2e', password: 'NopePass_00000', role: 'super' },
    failOnStatusCode: false,
    ignoreHTTPSErrors: true,
  });
  expect(forbidden.status()).toBe(403);
});
