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

// CXA-F367 — message ergonomics, driven through the real hover toolbar and
// pickers so the goldens and the console gate actually gate them:
//   react:  React → openEmoji → pickEmoji → toggleReact → POST /api/chat/react
//   edit:   Edit → startEditMsg → saveEditInline → PATCH /api/chat/messages/:id
//   pin:    Pin → togglePin → POST /api/chat/messages/:id/pin (toggle = unpin)
//   delete: Delete → deleteMsg → DELETE /api/chat/messages/:id (coxModal confirm)
// All four rules are enforced server-side; the seeded messages belong to the
// same open-mode user ("user") as the browser, so the author-only paths are
// exercisable for real. The drill message is fresh, so the mode golden above
// never sees mutated seeds.
test('message ergonomics: edit, delete, react and pin under the console gate', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);

  await page.locator('#mode-chat').click();
  await expect(page.locator('#chat-msgs')).toBeVisible();
  await expect(page.locator('body')).toContainText('Standup: timeline fix is in review');

  const DRILL = 'CXA-F367 ergonomics drill message';
  await page.locator('#chat-input').fill(DRILL);
  await page.locator('#chat-input').press('Enter');
  const row = page.locator('.smsg', { hasText: DRILL }).last();
  await expect(row).toBeVisible();
  const mid = ((await row.getAttribute('id')) || '').replace(/^msg-/, '');
  expect(mid.length, 'the posted message carries a server id').toBeGreaterThan(5);
  const msg = page.locator(`#msg-${mid}`);

  // React through the real picker (the Gestures tab holds 👍): the pill must
  // render with this user's count.
  await msg.hover();
  await msg.locator('.sacts button[title="React"]').click();
  await expect(page.locator('#emoji-pop')).toBeVisible();
  await page.locator('#emoji-pop .etab[title="Gestures"]').click();
  await page.locator('#emoji-pop .ecell', { hasText: '👍' }).first().click();
  await expect(msg.locator('.tcre', { hasText: '👍' })).toContainText('1');

  // Edit in place; the live update marks the message "(edited)".
  await msg.hover();
  await msg.locator('.sacts button[title="Edit"]').click();
  const box = page.locator(`#edit-${mid}`);
  await expect(box).toBeVisible();
  await box.fill(`${DRILL} — edited in place`);
  await box.press('Enter');
  await expect(msg.locator('.tcedit')).toHaveText('(edited)');

  // Pin: the bar under the channel header lists the message, the chip scrolls
  // to it, and the same toggle unpins.
  await msg.hover();
  await msg.locator('.sacts button[title="Pin"]').click();
  const bar = page.locator('#pins-bar');
  await expect(bar).toBeVisible();
  await expect(bar.locator('.pin-chip', { hasText: DRILL })).toBeVisible();
  await bar.locator('.pin-chip', { hasText: DRILL }).click();
  await msg.hover();
  await msg.locator('.sacts button[title="Pin"]').click();
  await expect(bar).toBeHidden();

  // Pin once more so the goldens capture the full ergonomics state (reaction
  // pill + "(edited)" + pin bar), then let the "Pinned" toast fade out first
  // so no overlay leaks into the pixels. The row's wall-clock time is masked:
  // the golden pins behaviour, not the minute of the run.
  await msg.hover();
  await msg.locator('.sacts button[title="Pin"]').click();
  await expect(bar).toBeVisible();
  await expect(page.locator('#toasts .toast')).toHaveCount(0);
  await page.mouse.move(4, 4);
  await expect(msg).toHaveScreenshot('chat-msg-ergonomics.png', {
    mask: [msg.locator('.stm')],
  });
  await expect(bar).toHaveScreenshot('chat-pin-bar.png');

  // Delete with its confirm modal → the tombstone replaces the body, and the
  // pin bar drops the message (pins list live messages only).
  await msg.hover();
  await msg.locator('.sacts button[title="Delete"]').click();
  await page.locator('#cm-ok').click();
  await expect(msg).toContainText('This message was deleted');
  await expect(bar).toBeHidden();

  await assertNoConsoleErrors(errors);
});

// The ergonomics rules are server-side (CXA-F367): crafted REST calls hit the
// same walls the buttons do. Open mode makes the browser an admin ("user"),
// which is exactly what lets this test pin, delete and cap pins directly.
test('reaction set and pin cap are enforced server-side, not just in the UI', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);

  const call = (path: string, init?: RequestInit) =>
    page.evaluate(
      async ([p, o]) => {
        const r = await fetch(p as string, o as RequestInit | undefined);
        return { status: r.status, body: await r.json().catch(() => null) };
      },
      [path, init] as const,
    );

  // A hand-crafted emoji outside the declared set is a 400, picker or not.
  const bad = await call('/api/chat/react', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ id: 'no-such-message', emoji: '🦄' }),
  });
  expect(bad.status, 'out-of-set emoji is refused before the message lookup').toBe(400);

  // A clean pin slate: unpin whatever earlier tests left in #general, so the
  // cap arithmetic below is exact.
  const pins = await page.evaluate(async () =>
    (await fetch('/api/chat/pins?channel=general')).json(),
  );
  for (const m of pins as Array<{ id: string }>) {
    await call(`/api/chat/messages/${m.id}/pin`, { method: 'POST' });
  }

  // Six fresh messages, then pin five: the sixth pin is a 409, and after one
  // unpin the same message pins again — the cap is a live constraint, not a
  // one-way wall.
  const drilled: string[] = [];
  for (let i = 0; i < 6; i++) {
    await call('/api/chat/send', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ body: `CXA-F367 cap drill ${i}`, channel: 'general' }),
    });
    const msgs = (await page.evaluate(async () =>
      (await fetch('/api/chat/messages?channel=general&limit=20')).json(),
    )) as Array<{ id: string; body: string; deleted: boolean }>;
    const mine = msgs.find((m) => m.body === `CXA-F367 cap drill ${i}` && !m.deleted);
    expect(mine, `drill message ${i} came back from the server`).toBeTruthy();
    drilled.push((mine as { id: string }).id);
  }
  for (const id of drilled.slice(0, 5)) {
    const r = await call(`/api/chat/messages/${id}/pin`, { method: 'POST' });
    expect(r.status, `pin ${id}`).toBe(200);
  }
  const sixth = await call(`/api/chat/messages/${drilled[5]}/pin`, { method: 'POST' });
  expect(sixth.status, 'the 6th pin is refused').toBe(409);
  expect(
    (await call(`/api/chat/messages/${drilled[0]}/pin`, { method: 'POST' })).status,
    'unpin frees a slot',
  ).toBe(200);
  expect(
    (await call(`/api/chat/messages/${drilled[5]}/pin`, { method: 'POST' })).status,
    'the freed slot accepts a pin',
  ).toBe(200);

  // A deleted message is not pinnable: its tombstone is invisible in the pin
  // bar, so accepting the pin would mint a slot the cap counts but nobody
  // can see to unpin. (The delete itself auto-unpins for the same reason.)
  await call(`/api/chat/messages/${drilled[1]}/pin`, { method: 'POST' });
  expect(
    (await call(`/api/chat/messages/${drilled[1]}`, { method: 'DELETE' })).status,
    'the author deletes their own message',
  ).toBe(200);
  expect(
    (await call(`/api/chat/messages/${drilled[1]}/pin`, { method: 'POST' })).status,
    'a tombstone is not pinnable',
  ).toBe(400);

  await assertNoConsoleErrors(errors);
});
