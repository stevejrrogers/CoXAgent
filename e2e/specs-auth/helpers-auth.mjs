// Shared helpers for auth-enabled e2e: deterministic admin login + an
// API-level login that returns the session cookie, so specs don't repeat the
// same credential plumbing in every file.
import { expect } from '@playwright/test';

export const ADMIN_USER = 'adminos';
export const ADMIN_PASSWORD = 'ChangeMe_12345';

/// Log in over HTTP and return { status, body, cookie }. `cookie` is the raw
/// `cox_session=<token>` value suitable for addCookies().
export async function apiLogin(request) {
  const resp = await request.post('/api/auth/login', {
    data: { username: ADMIN_USER, password: ADMIN_PASSWORD },
  });
  const body = await resp.json();
  const setCookie = resp.headers()['set-cookie'] || '';
  const cookieValue = (setCookie.match(/cox_session=([^;]+)/) || [])[1] || '';
  return { status: resp.status(), body, cookie: cookieValue };
}

/// Establish a logged-in browser context via the session cookie (same pattern
/// as StubAuth::token(role) on the Rust side — deterministic principal control).
export async function signInAs(page, username, password) {
  await page.context().clearCookies();
  const resp = await page.request.post('/api/auth/login', {
    data: { username, password },
  });
  expect(resp.status(), `${username} should log in`).toBe(200);
  const setCookie = resp.headers()['set-cookie'] || '';
  const token = (setCookie.match(/cox_session=([^;]+)/) || [])[1];
  expect(token, 'login should set a session cookie').toBeTruthy();
  await page.context().addCookies([
    { name: 'cox_session', value: token, domain: '127.0.0.1', path: '/' },
  ]);
}

/// Open / and wait until the app either paints content or shows the login form.
export async function openApp(page) {
  await page.goto('/');
}
