// CXA-F311: pasting an image (Ctrl+V / Cmd+V) into either composer must ride
// the exact same attachment path the attach button and drag-drop already use —
// same handleFiles, same upload API, same chip, same toasty error paths. Image
// pastes are simulated the way a real one reaches the composer: a
// ClipboardEvent carrying DataTransfer items, dispatched on the focused
// composer input; the text-entry no-hijack case uses a real clipboard paste
// (a synthetic paste never performs the native insertion it must not break).
import { test, expect, type Page } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

// A real 1x1 PNG: uploads store verbatim bytes, chips preview by mime.
const PNG_BYTES = [
  0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d,
  0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
  0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00,
  0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x62, 0x00, 0x01, 0x00, 0x00,
  0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49,
  0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

// `bytes` is the literal file content; `size` allocates that many zero bytes
// (used only to push a file past the 25MB client guard without a huge fixture).
type PasteFile = { name: string; mime?: string; bytes?: number[]; size?: number };

// ATT is a top-level `let` in chat.js — a global lexical binding, NOT a window
// property — so in-page evaluates must reference it bare. Entries are the
// server's Attachment while uploaded, or an uploading placeholder (no url/mime).
declare const ATT: { chat: Array<{ name: string; url?: string; mime?: string }>; disc: Array<{ name: string; url?: string; mime?: string }> };

// Dispatch a paste on a composer with clipboard items built from the given
// files (and optionally leading text). Returns whether the app intercepted it.
// Chromium drops the clipboardData passed to the ClipboardEvent constructor,
// so the DataTransfer is shadowed onto the instance with defineProperty —
// the app code then reads it exactly as it reads a real paste's.
function pasteInto(page: Page, inputId: string, files: PasteFile[], text?: string): Promise<{ defaultPrevented: boolean }> {
  return page.evaluate(([inputId, files, text]) => {
    const target = document.getElementById(inputId) as HTMLInputElement;
    target.focus();
    const dt = new DataTransfer();
    if (text !== undefined) dt.items.add(text, 'text/plain');
    for (const f of files) {
      const data = new Uint8Array(f.bytes ?? f.size ?? 0);
      dt.items.add(new File([data], f.name, { type: f.mime || 'application/octet-stream' }));
    }
    const ev = new ClipboardEvent('paste', { bubbles: true, cancelable: true });
    Object.defineProperty(ev, 'clipboardData', { value: dt });
    const delivered = target.dispatchEvent(ev);
    return { defaultPrevented: !delivered };
  }, [inputId, files, text] as const);
}

function attChips(page: Page, surface: 'chat' | 'disc') {
  return page.locator(`#${surface}-att .att-chip`);
}

test('pasting an image into the chat composer attaches it through the shared upload path', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);
  await page.evaluate(() => (window as any).setMode('chat'));
  await expect(page.locator('#chat-input')).toBeVisible();

  const uploadUrls: string[] = [];
  page.on('request', (req) => {
    if (req.method() === 'POST' && req.url().endsWith('/upload')) uploadUrls.push(req.url());
  });

  await pasteInto(page, 'chat-input', [{ name: 'image.png', mime: 'image/png', bytes: PNG_BYTES }]);

  await expect(attChips(page, 'chat')).toHaveCount(1);
  await expect(attChips(page, 'chat').locator('.att-nm')).toHaveText(['image.png']);
  await expect(attChips(page, 'chat').locator('img')).toHaveCount(1);
  const names = await page.evaluate(() => ATT.chat.map((a) => a.name));
  expect(names).toEqual(['image.png']);
  // Same API the attach button and drag-drop already call — no new upload path.
  expect(uploadUrls).toHaveLength(1);
  expect(new URL(uploadUrls[0]).pathname).toBe('/api/chat/upload');
  await assertNoConsoleErrors(errors);
});

test('paste behaves identically to the attach button and drag-drop', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);
  await page.evaluate(() => (window as any).setMode('chat'));
  await expect(page.locator('#chat-input')).toBeVisible();

  // Attach button path first: the baseline every other entry must match.
  await page.locator('#chat-file').setInputFiles({ name: 'image.png', mimeType: 'image/png', buffer: Buffer.from(PNG_BYTES) });
  await expect(attChips(page, 'chat').locator('.att-nm')).toHaveText(['image.png']);
  await expect(attChips(page, 'chat').locator('img')).toHaveCount(1);

  // Paste must land the identical chip — same name, same image preview.
  await pasteInto(page, 'chat-input', [{ name: 'image.png', mime: 'image/png', bytes: PNG_BYTES }]);
  await expect(attChips(page, 'chat').locator('.att-nm')).toHaveText(['image.png', 'image.png']);
  await expect(attChips(page, 'chat').locator('img')).toHaveCount(2);

  // Drag-drop must land the identical chip too. PNG_BYTES is passed as an
  // argument — page.evaluate bodies run in the page, outside this module's scope.
  await page.evaluate((png) => {
    const dt = new DataTransfer();
    dt.items.add(new File([new Uint8Array(png)], 'image.png', { type: 'image/png' }));
    const ev = new DragEvent('drop', { dataTransfer: dt, bubbles: true, cancelable: true });
    document.getElementById('chat-msgs')!.dispatchEvent(ev);
  }, PNG_BYTES);
  await expect(attChips(page, 'chat').locator('.att-nm')).toHaveText(['image.png', 'image.png', 'image.png']);
  await expect(attChips(page, 'chat').locator('img')).toHaveCount(3);

  // All three entries carry the same attachment shape in the same state.
  const atts = await page.evaluate(() => ATT.chat);
  expect(atts.map((a) => ({ name: a.name, hasUrl: !!a.url, mime: a.mime }))).toEqual([
    { name: 'image.png', hasUrl: true, mime: 'image/png' },
    { name: 'image.png', hasUrl: true, mime: 'image/png' },
    { name: 'image.png', hasUrl: true, mime: 'image/png' },
  ]);
  await assertNoConsoleErrors(errors);
});

test('a pasted screenshot rides along when the message sends', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);
  await page.evaluate(() => (window as any).setMode('chat'));
  await expect(page.locator('#chat-input')).toBeVisible();

  await pasteInto(page, 'chat-input', [{ name: 'image.png', mime: 'image/png', bytes: PNG_BYTES }]);
  await expect(attChips(page, 'chat').locator('.att-nm')).toHaveText(['image.png']);

  await page.locator('#chat-input').fill('check this screenshot');
  await page.locator('#chat-input').press('Enter');

  await expect(page.locator('#chat-msgs')).toContainText('check this screenshot', { timeout: 10_000 });
  // The pasted attachment went with it: rendered on the message, strip cleared.
  await expect(page.locator('#chat-msgs .msg-att img')).toHaveCount(1, { timeout: 10_000 });
  await expect(attChips(page, 'chat')).toHaveCount(0);
  await assertNoConsoleErrors(errors);
});

test('pasting several images at once attaches them like a multi-file drop', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);
  await page.evaluate(() => (window as any).setMode('chat'));
  await expect(page.locator('#chat-input')).toBeVisible();

  await pasteInto(page, 'chat-input', [
    { name: 'image.png', mime: 'image/png', bytes: PNG_BYTES },
    { name: 'image 2.png', mime: 'image/png', bytes: PNG_BYTES },
  ]);

  await expect(attChips(page, 'chat')).toHaveCount(2);
  // Screenshot names render verbatim; every chip carries a name — no empty chip.
  await expect(attChips(page, 'chat').locator('.att-nm')).toHaveText(['image.png', 'image 2.png']);
  await expect(attChips(page, 'chat').locator('img')).toHaveCount(2);
  const names = await page.evaluate(() => ATT.chat.map((a) => a.name));
  expect(names).toEqual(['image.png', 'image 2.png']);
  await assertNoConsoleErrors(errors);
});

test('a nameless pasted screenshot is renamed pasted-image.png and renders', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);
  await page.evaluate(() => (window as any).setMode('chat'));
  await expect(page.locator('#chat-input')).toBeVisible();

  // OS clipboards hand over screenshots with an empty filename; the media
  // server derives the serving Content-Type from the stored extension, so the
  // app must give the blob a name pre-upload or it never renders as an image.
  await pasteInto(page, 'chat-input', [{ name: '', mime: 'image/png', bytes: PNG_BYTES }]);

  await expect(attChips(page, 'chat')).toHaveCount(1);
  await expect(attChips(page, 'chat').locator('.att-nm')).toHaveText(['pasted-image.png']);
  await expect(attChips(page, 'chat').locator('img')).toHaveCount(1);
  // The stored blob serves back as a real image, not octet-stream.
  const served = await page.evaluate(async () => {
    const url = ATT.chat[0] && ATT.chat[0].url;
    const r = await fetch(url as string);
    return { status: r.status, type: r.headers.get('content-type') };
  });
  expect(served.status).toBe(200);
  expect(served.type).toContain('image/png');
  // The extension is derived from the mime subtype, not hardcoded to png —
  // a nameless JPEG must not masquerade as a .png (or fall back to octet-stream).
  await pasteInto(page, 'chat-input', [{ name: '', mime: 'image/jpeg', bytes: PNG_BYTES }]);
  await expect(attChips(page, 'chat')).toHaveCount(2);
  await expect(attChips(page, 'chat').locator('.att-nm')).toHaveText(['pasted-image.png', 'pasted-image.jpeg']);
  await expect(attChips(page, 'chat').locator('img')).toHaveCount(2);
  await assertNoConsoleErrors(errors);
});

test('pasting into the discussion composer attaches to the discussion, never to chat', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);
  await page.evaluate(() => (window as any).nav('discuss'));
  await expect(page.locator('#disc-input')).toBeVisible();

  const uploadUrls: string[] = [];
  page.on('request', (req) => {
    if (req.method() === 'POST' && /\/api\/projects\/[^/]+\/upload$/.test(req.url())) uploadUrls.push(req.url());
  });

  await pasteInto(page, 'disc-input', [{ name: 'image.png', mime: 'image/png', bytes: PNG_BYTES }]);

  await expect(attChips(page, 'disc')).toHaveCount(1);
  await expect(attChips(page, 'disc').locator('.att-nm')).toHaveText(['image.png']);
  const att = await page.evaluate(() => ({ disc: ATT.disc.map((a) => a.name), chat: ATT.chat }));
  expect(att.disc).toEqual(['image.png']);
  expect(att.chat).toEqual([]);
  await expect(attChips(page, 'chat')).toHaveCount(0);
  // The discussion surface's own upload API — the one its attach button uses.
  expect(uploadUrls.length).toBe(1);
  await assertNoConsoleErrors(errors);
});

test('plain-text paste and typing are untouched', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);
  await page.evaluate(() => (window as any).setMode('chat'));
  await expect(page.locator('#chat-input')).toBeVisible();

  // A REAL paste (trusted event → the browser performs the native insertion):
  // with no image in the clipboard the handler must not intercept it, so the
  // text lands in the input exactly as it did before paste support existed.
  await page.context().grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.evaluate(() => navigator.clipboard.writeText('standup notes'));
  await page.locator('#chat-input').click();
  await page.keyboard.press('ControlOrMeta+V');
  await expect(page.locator('#chat-input')).toHaveValue('standup notes');
  await expect(attChips(page, 'chat')).toHaveCount(0);
  expect(await page.evaluate(() => ATT.chat)).toEqual([]);
  // Silently ignored: no error toast for a text-only clipboard.
  await expect(page.locator('#toasts .toast.err')).toHaveCount(0);
  // And typing still works normally afterwards.
  await page.keyboard.type(' hello');
  await expect(page.locator('#chat-input')).toHaveValue('standup notes hello');
  await assertNoConsoleErrors(errors);
});

test('a mixed paste (text + image) attaches the image and leaves text entry working', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);
  await page.evaluate(() => (window as any).setMode('chat'));
  await expect(page.locator('#chat-input')).toBeVisible();

  const res = await pasteInto(
    page,
    'chat-input',
    [{ name: 'image.png', mime: 'image/png', bytes: PNG_BYTES }],
    'look at this',
  );

  // At least one image item → the handler intercepts the paste.
  expect(res.defaultPrevented).toBe(true);
  await expect(attChips(page, 'chat').locator('.att-nm')).toHaveText(['image.png']);
  expect(await page.evaluate(() => ATT.chat.map((a) => a.name))).toEqual(['image.png']);
  // The non-image item is never turned into an attachment — and text entry
  // itself keeps working after the intercepted paste.
  await page.keyboard.type('ok then');
  await expect(page.locator('#chat-input')).toHaveValue('ok then');
  await assertNoConsoleErrors(errors);
});

test('an oversized pasted image surfaces the existing size toast, not a silent drop', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);
  await page.evaluate(() => (window as any).setMode('chat'));
  await expect(page.locator('#chat-input')).toBeVisible();

  await pasteInto(page, 'chat-input', [{ name: 'image.png', mime: 'image/png', size: 26 * 1024 * 1024 }]);

  await expect(page.locator('#toasts .toast.err')).toContainText('image.png too large (max 25MB)');
  await expect(attChips(page, 'chat')).toHaveCount(0);
  expect(await page.evaluate(() => ATT.chat)).toEqual([]);
  await assertNoConsoleErrors(errors);
});

test('a failed pasted-image upload reuses the upload-failed toast, chip never dangles', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await page.route('**/api/chat/upload', (route) => route.fulfill({ status: 500, body: 'boom' }));
  await openApp(page);
  await page.evaluate(() => (window as any).setMode('chat'));
  await expect(page.locator('#chat-input')).toBeVisible();

  await pasteInto(page, 'chat-input', [{ name: 'image.png', mime: 'image/png', bytes: PNG_BYTES }]);

  await expect(page.locator('#toasts .toast.err')).toContainText('Upload failed: image.png');
  await expect(attChips(page, 'chat')).toHaveCount(0);
  expect(await page.evaluate(() => ATT.chat)).toEqual([]);
  // Chromium logs the injected 500 itself ("Failed to load resource…") — that
  // is the route we planted, not an app error; every other console error and
  // all page errors still fail the gate.
  await assertNoConsoleErrors(errors.filter((e) => !e.includes('Failed to load resource')));
});
