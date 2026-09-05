// CXA-F367 chat ergonomics under RBAC: the rules the open-mode suite cannot
// produce, because there every caller is both author and admin. Provisioning
// strategy mirrors rbac-viewer.spec.ts: the fresh boot knows only `adminos`,
// so this spec creates its member-tier user through the admin API at runtime
// (create_user is idempotent, so reruns never conflict).
//
// The rules under test, each server-side (auth_mw lets member-tier writes
// through to the handlers — /api/chat/send is carved out for any signed-in
// user and react/pin/edit/delete need only can_write, which every role but
// viewer holds):
//   edit  — author-only, NO admin override
//   delete— author or admin (the one carve-out)
//   pin   — channel owner or admin; a member (owner-less #general) is refused
import { test, expect } from '@playwright/test';
import { ADMIN_USER, ADMIN_PASSWORD, apiLogin } from './helpers-auth.mjs';

const MEMBER_USER = 'chatmember';
const MEMBER_PASSWORD = 'MemberPass_12345';

async function login(page: any, username: string, password: string) {
  const resp = await page.request.post('/api/auth/login', {
    data: { username, password },
  });
  expect(resp.status(), `${username} should log in`).toBe(200);
  const setCookie = resp.headers()['set-cookie'] || '';
  return (setCookie.match(/cox_session=([^;]+)/) || [])[1] || '';
}

test('chat ergonomics permissions: author-only edit, admin-delete carve-out, owner/admin pin', async ({
  page,
}) => {
  // Admin session: provision the member-tier user.
  const admin = await apiLogin(page.request);
  expect(admin.status).toBe(200);
  await page.context().addCookies([
    { name: 'cox_session', value: admin.cookie, domain: '127.0.0.1', path: '/' },
  ]);
  const created = await page.request.post('/api/auth/users', {
    data: { username: MEMBER_USER, password: MEMBER_PASSWORD, role: 'ba' },
  });
  expect(created.status()).toBe(200);

  // Switch to the member and post a message the admin does not own.
  await page.context().clearCookies();
  await page.context().addCookies([
    {
      name: 'cox_session',
      value: await login(page, MEMBER_USER, MEMBER_PASSWORD),
      domain: '127.0.0.1',
      path: '/',
    },
  ]);
  const sent = await page.request.post('/api/chat/send', {
    data: { body: 'CXA-F367 rbac drill message', channel: 'general' },
  });
  expect(sent.status()).toBe(200);
  const memberMsgs = (await (
    await page.request.get('/api/chat/messages?channel=general&limit=20')
  ).json()) as Array<{ id: string; user: string; body: string; deleted: boolean }>;
  const drill = memberMsgs.find((m) => m.user === MEMBER_USER && !m.deleted);
  expect(drill, 'the member drill message is persisted').toBeTruthy();
  const mid = (drill as { id: string }).id;

  // A member reacts within the set (can_write passes the write gate) — the
  // happy-path sanity for a non-admin persona.
  expect(
    (
      await page.request.post('/api/chat/react', { data: { id: mid, emoji: '👀' } })
    ).status(),
  ).toBe(200);

  // Pin in #general: computed channels have no stored owner, so the rule
  // degenerates to admin authority — the member is refused even on their own
  // message.
  expect(
    (await page.request.post(`/api/chat/messages/${mid}/pin`, { data: {} })).status(),
    'a member cannot pin in an owner-less channel',
  ).toBe(403);

  // Switch to the admin.
  await page.context().clearCookies();
  await page.context().addCookies([
    {
      name: 'cox_session',
      value: await login(page, ADMIN_USER, ADMIN_PASSWORD),
      domain: '127.0.0.1',
      path: '/',
    },
  ]);

  // Edit has NO admin override: even the admin may not rewrite another
  // user's message.
  expect(
    (
      await page.request.patch(`/api/chat/messages/${mid}`, {
        data: { body: 'admin rewrite attempt' },
      })
    ).status(),
    'the admin cannot edit another user’s message',
  ).toBe(403);

  // Delete is the one carve-out: the admin tombstones the member's message…
  expect(
    (await page.request.delete(`/api/chat/messages/${mid}`)).status(),
    'the admin can delete another user’s message',
  ).toBe(200);
  const after = (await (
    await page.request.get('/api/chat/messages?channel=general&limit=20')
  ).json()) as Array<{ id: string; user: string; deleted: boolean; body: string }>;
  const tomb = after.find((m) => m.id === mid);
  expect(tomb?.deleted, 'the message is a server-side tombstone').toBe(true);
  expect(tomb?.body, 'the tombstone clears the body').toBe('');

  // …and a tombstone is not pinnable even by the admin (invisible slots the
  // cap would count).
  expect(
    (await page.request.post(`/api/chat/messages/${mid}/pin`, { data: {} })).status(),
    'a deleted message is not pinnable',
  ).toBe(400);

  // The admin CAN pin in the owner-less channel — on a live message.
  const live = after.find((m) => !m.deleted && m.id);
  const liveId = (live as { id: string }).id;
  const pinResp = await page.request.post(`/api/chat/messages/${liveId}/pin`, {
    data: {},
  });
  expect(pinResp.status(), 'the admin pins in #general').toBe(200);
  expect((await pinResp.json()).pinned, 'the toggle reports the pinned state').toBe(true);
  const unpinResp = await page.request.post(`/api/chat/messages/${liveId}/pin`, {
    data: {},
  });
  expect((await unpinResp.json()).pinned, 'the same toggle unpins').toBe(false);
});
