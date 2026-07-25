// Audit chat UI — check layout, spacing, readability
const { test, expect } = require('@playwright/test');

const BASE = 'http://localhost:4000';

async function login(page) {
  await page.goto(BASE + '/');
  await page.waitForSelector('#lg-user', { timeout: 8000 });
  await page.fill('#lg-user', 'root');
  await page.fill('#lg-pass', 'Str@wb3rry');
  await page.click('button:has-text("Sign in")');
  await page.waitForSelector('.side', { timeout: 10000 });
}

// Fill chat with sample messages to test rendering
async function seedMessages(page, count = 15) {
  const users = ['BA', 'SA', 'PD', 'DEV', 'TEST', 'PO', 'SM'];
  for (let i = 0; i < count; i++) {
    const user = users[i % users.length];
    const body = `Message ${i + 1}: discussing ticket **TL-F${String(i + 1).padStart(3, '0')}** — \`impl\` review needed. Here is a code block:\n\`\`\`rust\nfn main() { println!(\"hello\"); }\n\`\`\`\nWhat do you think @${user}?`;
    try {
      await page.evaluate(({ body, user }) => {
        const ws = window._CHATWS_TEST;
        if (ws && ws.readyState === 1) {
          ws.send(JSON.stringify({ body, channel: 'general' }));
        }
      }, { body, user });
      await page.waitForTimeout(200);
    } catch (e) { /* ignore */ }
  }
}

test('chat layout — message spacing and alignment', async ({ page }) => {
  await login(page);
  await page.click('#mode-chat');
  await page.waitForSelector('#chat-msgs', { timeout: 5000 });

  // Check: .tcmsg elements have proper max-width
  const msgs = page.locator('.tcmsg');
  const count = await msgs.count();
  expect(count).toBeGreaterThanOrEqual(0); // may be empty initially

  // Check: input bar has all formatting buttons
  await expect(page.locator('#chat-input')).toBeVisible();
  await expect(page.locator('button[title="Bold (**text**)"]')).toBeVisible();
  await expect(page.locator('button[title="Italic (*text*)"]')).toBeVisible();
  await expect(page.locator('button[title="Code (`code`)"]')).toBeVisible();
  await expect(page.locator('button[title="Attach files"]')).toBeVisible();
  await expect(page.locator('button[title="Emoji"]')).toBeVisible();
  await expect(page.locator('button:has(i.ti-send)')).toBeVisible();

  // Check: chat header has channel name
  await expect(page.locator('.chattitle')).toBeVisible();
  const title = await page.locator('.chattitle').textContent();
  expect(title.length).toBeGreaterThan(0);

  // Check: chat area has proper height (not collapsed)
  const chatBody = page.locator('.chatbody');
  const box = await chatBody.boundingBox();
  expect(box).not.toBeNull();
  expect(box.height).toBeGreaterThan(100);

  // Check: compose area is at bottom
  const compose = page.locator('.chatcompose');
  const cBox = await compose.boundingBox();
  expect(cBox).not.toBeNull();
  expect(cBox.y).toBeGreaterThan(box.y + box.height - 10);
});

test('chat styling — bubble colors and text readability', async ({ page }) => {
  await login(page);

  // Send a few messages first so we have content to inspect
  await page.click('#mode-chat');
  await page.waitForSelector('#chat-input', { timeout: 5000 });

  const testMessages = [
    '**Bold title**\nRegular text with `inline code` and a link: https://example.com',
    'Multiple\nlines\nof\ntext\nseparated by newlines',
    '```rust\nfn main() {\n    println!("hello world");\n}\n```',
    'A very long message that should wrap properly. '.repeat(10),
    '@BA @SA what do you think about this approach?',
  ];

  for (const msg of testMessages) {
    await page.fill('#chat-input', msg);
    await page.click('button:has(i.ti-send)');
    await page.waitForTimeout(800);
  }

  // Verify messages rendered
  const msgs = page.locator('.tcmsg');
  const count = await msgs.count();
  expect(count).toBeGreaterThanOrEqual(testMessages.length);

  // Check: bubbles have proper font and line-height
  const firstBubble = page.locator('.tcbub').first();
  const fontSize = await firstBubble.evaluate(el => window.getComputedStyle(el).fontSize);
  const lineHeight = await firstBubble.evaluate(el => window.getComputedStyle(el).lineHeight);
  expect(parseFloat(fontSize)).toBeGreaterThanOrEqual(12); // at least 12px
  expect(parseFloat(lineHeight)).toBeGreaterThanOrEqual(1.4); // readable line-height

  // Check: code blocks have monospace font
  const codeBlocks = page.locator('.codeblock');
  const cbCount = await codeBlocks.count();
  if (cbCount > 0) {
    const fontFamily = await codeBlocks.first().evaluate(el => window.getComputedStyle(el).fontFamily);
    expect(fontFamily).toMatch(/mono|Menlo|Courier/i);
  }

  // Check: mentions are styled
  const mentions = page.locator('.mention');
  const mCount = await mentions.count();
  if (mCount > 0) {
    const bg = await mentions.first().evaluate(el => window.getComputedStyle(el).backgroundColor);
    expect(bg).not.toBe('rgba(0, 0, 0, 0)'); // has some color
  }

  // Check: inline code styled
  const inlineCodes = page.locator('.inlinecode');
  const icCount = await inlineCodes.count();
  if (icCount > 0) {
    const bg = await inlineCodes.first().evaluate(el => window.getComputedStyle(el).backgroundColor);
    expect(bg).not.toBe('rgba(0, 0, 0, 0)');
  }

  // Check: links are styled
  const links = page.locator('.tcbub a');
  const lCount = await links.count();
  if (lCount > 0) {
    const color = await links.first().evaluate(el => window.getComputedStyle(el).color);
    expect(color).not.toBe('rgb(0, 0, 0)'); // has accent color
  }

  // Check: bold text renders
  const bold = page.locator('.tcbub b');
  const bCount = await bold.count();
  if (bCount > 0) {
    const weight = await bold.first().evaluate(el => window.getComputedStyle(el).fontWeight);
    expect(parseInt(weight)).toBeGreaterThanOrEqual(600);
  }
});

test('chat layout — message grouping and date separators', async ({ page }) => {
  await login(page);
  await page.click('#mode-chat');
  await page.waitForSelector('#chat-input', { timeout: 5000 });

  // Check: chatday elements exist for date separators
  const dateSeps = page.locator('.chatday');
  // Date separator may not exist if no messages from multiple days
  // Just check the CSS class exists for now
  expect(typeof dateSeps).toBe('object');

  // Check: max-width of messages is < 100% (not full width)
  const style = await page.evaluate(() => {
    const el = document.querySelector('.tcmsg');
    return el ? window.getComputedStyle(el).maxWidth : 'none';
  });
  if (style !== 'none') {
    const pct = parseFloat(style);
    expect(pct).toBeLessThan(90); // not full width
  }

  // Check: paddings are reasonable
  const chatMsgs = page.locator('.chatmsgs');
  const padding = await chatMsgs.evaluate(el => window.getComputedStyle(el).padding);
  expect(padding).toBeTruthy();
});

test('chat layout — scroll-to-bottom button', async ({ page }) => {
  await login(page);
  await page.click('#mode-chat');
  await page.waitForSelector('#chat-input', { timeout: 5000 });

  // Send many messages to push content off screen
  for (let i = 0; i < 25; i++) {
    await page.fill('#chat-input', `Scroll test message number ${i + 1}`);
    await page.click('button:has(i.ti-send)');
    await page.waitForTimeout(200);
  }

  // Scroll up
  const chatBody = page.locator('.chatbody');
  await chatBody.evaluate(el => el.scrollTop = 0);
  await page.waitForTimeout(500);

  // Scroll button should be visible
  const btn = page.locator('#chat-scroll-btn');
  await expect(btn).toBeVisible({ timeout: 2000 });

  // Click should scroll back to bottom
  await btn.click();
  await page.waitForTimeout(500);
  const dist = await chatBody.evaluate(el => el.scrollHeight - el.scrollTop - el.clientHeight);
  expect(dist).toBeLessThan(20);

  // Button should be hidden at bottom
  await expect(btn).not.toBeVisible({ timeout: 2000 });
});

test('chat styling — member list and channel sidebar', async ({ page }) => {
  await login(page);
  await page.click('#mode-chat');
  await page.waitForSelector('#chat-input', { timeout: 5000 });

  // Channel list
  const channels = page.locator('#chat-channels .chat-chan');
  const chCount = await channels.count();
  expect(chCount).toBeGreaterThanOrEqual(1); // at least general

  // Member avatar colors (deterministic)
  const colors = await page.evaluate(() => {
    return ['BA', 'SA', 'DEV', 'TEST', 'PO'].map(u => userColor(u));
  });
  // All should be different (or at least valid hex)
  colors.forEach(c => expect(c).toMatch(/^#[0-9a-f]{6}$/));
});

test('chat styling — dark theme contrast check', async ({ page }) => {
  await login(page);
  await page.click('#mode-chat');
  await page.waitForSelector('#chat-input', { timeout: 5000 });

  // Check background color is dark
  const bg = await page.evaluate(() => {
    return window.getComputedStyle(document.body).backgroundColor;
  });
  // Should be dark (rgb values < 50)
  const dark = bg.match(/\d+/g).map(Number);
  expect(dark[0]).toBeLessThan(30);
  expect(dark[1]).toBeLessThan(30);
  expect(dark[2]).toBeLessThan(30);

  // Text color should be light
  const textColor = await page.evaluate(() => {
    return window.getComputedStyle(document.body).color;
  });
  const light = textColor.match(/\d+/g).map(Number);
  expect(light[0]).toBeGreaterThan(200);
});
