// Shared helpers for auth-enabled e2e: deterministic admin login + an
// API-level login that returns the session cookie, so specs don't repeat the
// same credential plumbing in every file.
import { expect } from '@playwright/test';
// The SSE snapshot wait is shared with the open suite: both apps hydrate
// STATE over the same 1 Hz stream, so the hydration gate is defined once.
import { awaitStateSnapshot } from '../specs/helpers.mjs';

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

/// Open the app signed-in and wait until it has hydrated the fixture state.
/// Login-wall paths keep their bare page.goto — behind the wall no app shell
/// (and no STATE) ever renders, so they must not wait for one.
export async function openApp(page) {
  await page.goto('/');
  await awaitStateSnapshot(page);
}
