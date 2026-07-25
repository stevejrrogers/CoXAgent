# Instructions

- Following Playwright test failed.
- Explain why, be concise, respect Playwright best practices.
- Provide a snippet of code with the fix, if possible.

# Test info

- Name: chat-audit.spec.js >> chat layout — message spacing and alignment
- Location: tests/chat-audit.spec.js:33:1

# Error details

```
Error: expect(locator).toBeVisible() failed

Locator: locator('button[title="Code (`code`)"]')
Expected: visible
Timeout: 5000ms
Error: element(s) not found

Call log:
  - Expect "toBeVisible" with timeout 5000ms
  - waiting for locator('button[title="Code (`code`)"]')

```

```yaml
- complementary:
  - text:  CoXAgent autonomous dev team · v0.95.0
  - button "遼"
  - button ""
  - button " Chat"
  - text: Channels
  - button ""
  - text:  general  pw-public-1784749096395  pw-private-1784749096398  pw-public-1784749536083  pw-private-1784749536087 Direct messages 
  - textbox "Find a teammate…"
  - text: No teammates yet Meetings
  - button ""
  - button ""
  - text: July 2026
  - button ""
  - text: M T W T F S S 29 30 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31 1 2 RO root Super Admin
  - button ""
  - button ""
- main:
  - text: "# general everyone in the workspace"
  - button ""
  - button ""
  - button ""
  - button " 1"
  - text: "2026-07-22 root 11:45 PM {\"op\":\"typing\",\"channel\":\"general\"} pw-test-1784749531836 2026-07-23 root 12:22 AM {\"op\":\"typing\",\"channel\":\"general\"} ngon {\"op\":\"typing\",\"channel\":\"general\"} {\"op\":\"typing\",\"channel\":\"general\"} df 2026-07-24 root 08:52 AM {\"op\":\"typing\",\"channel\":\"general\"} {\"op\":\"typing\",\"channel\":\"general\"} root 01:20 AM sss root 03:29 AM test test msg 1 test msg 2 test msg 3 test msg 4 test msg 5 test msg 6 root 03:42 AM Hello team!"
  - button ""
  - button ""
  - button ""
  - button ""
  - button ""
  - 'textbox "Message your team… (Enter to send · @ to mention · **bold** *italic* \\`code\\`)"':
    - /placeholder: "Message your team…  (Enter to send · @ to mention · **bold** *italic* \\`code\\`)"
  - button ""
- text: Thread
- button ""
- textbox "Reply to thread…"
- button ""
```

# Test source

```ts
  1   | // Audit chat UI — check layout, spacing, readability
  2   | const { test, expect } = require('@playwright/test');
  3   | 
  4   | const BASE = 'http://localhost:4000';
  5   | 
  6   | async function login(page) {
  7   |   await page.goto(BASE + '/');
  8   |   await page.waitForSelector('#lg-user', { timeout: 8000 });
  9   |   await page.fill('#lg-user', 'root');
  10  |   await page.fill('#lg-pass', 'Str@wb3rry');
  11  |   await page.click('button:has-text("Sign in")');
  12  |   await page.waitForSelector('.side', { timeout: 10000 });
  13  | }
  14  | 
  15  | // Fill chat with sample messages to test rendering
  16  | async function seedMessages(page, count = 15) {
  17  |   const users = ['BA', 'SA', 'PD', 'DEV', 'TEST', 'PO', 'SM'];
  18  |   for (let i = 0; i < count; i++) {
  19  |     const user = users[i % users.length];
  20  |     const body = `Message ${i + 1}: discussing ticket **TL-F${String(i + 1).padStart(3, '0')}** — \`impl\` review needed. Here is a code block:\n\`\`\`rust\nfn main() { println!(\"hello\"); }\n\`\`\`\nWhat do you think @${user}?`;
  21  |     try {
  22  |       await page.evaluate(({ body, user }) => {
  23  |         const ws = window._CHATWS_TEST;
  24  |         if (ws && ws.readyState === 1) {
  25  |           ws.send(JSON.stringify({ body, channel: 'general' }));
  26  |         }
  27  |       }, { body, user });
  28  |       await page.waitForTimeout(200);
  29  |     } catch (e) { /* ignore */ }
  30  |   }
  31  | }
  32  | 
  33  | test('chat layout — message spacing and alignment', async ({ page }) => {
  34  |   await login(page);
  35  |   await page.click('#mode-chat');
  36  |   await page.waitForSelector('#chat-msgs', { timeout: 5000 });
  37  | 
  38  |   // Check: .tcmsg elements have proper max-width
  39  |   const msgs = page.locator('.tcmsg');
  40  |   const count = await msgs.count();
  41  |   expect(count).toBeGreaterThanOrEqual(0); // may be empty initially
  42  | 
  43  |   // Check: input bar has all formatting buttons
  44  |   await expect(page.locator('#chat-input')).toBeVisible();
  45  |   await expect(page.locator('button[title="Bold (**text**)"]')).toBeVisible();
  46  |   await expect(page.locator('button[title="Italic (*text*)"]')).toBeVisible();
> 47  |   await expect(page.locator('button[title="Code (`code`)"]')).toBeVisible();
      |                                                               ^ Error: expect(locator).toBeVisible() failed
  48  |   await expect(page.locator('button[title="Attach files"]')).toBeVisible();
  49  |   await expect(page.locator('button[title="Emoji"]')).toBeVisible();
  50  |   await expect(page.locator('button:has(i.ti-send)')).toBeVisible();
  51  | 
  52  |   // Check: chat header has channel name
  53  |   await expect(page.locator('.chattitle')).toBeVisible();
  54  |   const title = await page.locator('.chattitle').textContent();
  55  |   expect(title.length).toBeGreaterThan(0);
  56  | 
  57  |   // Check: chat area has proper height (not collapsed)
  58  |   const chatBody = page.locator('.chatbody');
  59  |   const box = await chatBody.boundingBox();
  60  |   expect(box).not.toBeNull();
  61  |   expect(box.height).toBeGreaterThan(100);
  62  | 
  63  |   // Check: compose area is at bottom
  64  |   const compose = page.locator('.chatcompose');
  65  |   const cBox = await compose.boundingBox();
  66  |   expect(cBox).not.toBeNull();
  67  |   expect(cBox.y).toBeGreaterThan(box.y + box.height - 10);
  68  | });
  69  | 
  70  | test('chat styling — bubble colors and text readability', async ({ page }) => {
  71  |   await login(page);
  72  | 
  73  |   // Send a few messages first so we have content to inspect
  74  |   await page.click('#mode-chat');
  75  |   await page.waitForSelector('#chat-input', { timeout: 5000 });
  76  | 
  77  |   const testMessages = [
  78  |     '**Bold title**\nRegular text with `inline code` and a link: https://example.com',
  79  |     'Multiple\nlines\nof\ntext\nseparated by newlines',
  80  |     '```rust\nfn main() {\n    println!("hello world");\n}\n```',
  81  |     'A very long message that should wrap properly. '.repeat(10),
  82  |     '@BA @SA what do you think about this approach?',
  83  |   ];
  84  | 
  85  |   for (const msg of testMessages) {
  86  |     await page.fill('#chat-input', msg);
  87  |     await page.click('button:has(i.ti-send)');
  88  |     await page.waitForTimeout(800);
  89  |   }
  90  | 
  91  |   // Verify messages rendered
  92  |   const msgs = page.locator('.tcmsg');
  93  |   const count = await msgs.count();
  94  |   expect(count).toBeGreaterThanOrEqual(testMessages.length);
  95  | 
  96  |   // Check: bubbles have proper font and line-height
  97  |   const firstBubble = page.locator('.tcbub').first();
  98  |   const fontSize = await firstBubble.evaluate(el => window.getComputedStyle(el).fontSize);
  99  |   const lineHeight = await firstBubble.evaluate(el => window.getComputedStyle(el).lineHeight);
  100 |   expect(parseFloat(fontSize)).toBeGreaterThanOrEqual(12); // at least 12px
  101 |   expect(parseFloat(lineHeight)).toBeGreaterThanOrEqual(1.4); // readable line-height
  102 | 
  103 |   // Check: code blocks have monospace font
  104 |   const codeBlocks = page.locator('.codeblock');
  105 |   const cbCount = await codeBlocks.count();
  106 |   if (cbCount > 0) {
  107 |     const fontFamily = await codeBlocks.first().evaluate(el => window.getComputedStyle(el).fontFamily);
  108 |     expect(fontFamily).toMatch(/mono|Menlo|Courier/i);
  109 |   }
  110 | 
  111 |   // Check: mentions are styled
  112 |   const mentions = page.locator('.mention');
  113 |   const mCount = await mentions.count();
  114 |   if (mCount > 0) {
  115 |     const bg = await mentions.first().evaluate(el => window.getComputedStyle(el).backgroundColor);
  116 |     expect(bg).not.toBe('rgba(0, 0, 0, 0)'); // has some color
  117 |   }
  118 | 
  119 |   // Check: inline code styled
  120 |   const inlineCodes = page.locator('.inlinecode');
  121 |   const icCount = await inlineCodes.count();
  122 |   if (icCount > 0) {
  123 |     const bg = await inlineCodes.first().evaluate(el => window.getComputedStyle(el).backgroundColor);
  124 |     expect(bg).not.toBe('rgba(0, 0, 0, 0)');
  125 |   }
  126 | 
  127 |   // Check: links are styled
  128 |   const links = page.locator('.tcbub a');
  129 |   const lCount = await links.count();
  130 |   if (lCount > 0) {
  131 |     const color = await links.first().evaluate(el => window.getComputedStyle(el).color);
  132 |     expect(color).not.toBe('rgb(0, 0, 0)'); // has accent color
  133 |   }
  134 | 
  135 |   // Check: bold text renders
  136 |   const bold = page.locator('.tcbub b');
  137 |   const bCount = await bold.count();
  138 |   if (bCount > 0) {
  139 |     const weight = await bold.first().evaluate(el => window.getComputedStyle(el).fontWeight);
  140 |     expect(parseInt(weight)).toBeGreaterThanOrEqual(600);
  141 |   }
  142 | });
  143 | 
  144 | test('chat layout — message grouping and date separators', async ({ page }) => {
  145 |   await login(page);
  146 |   await page.click('#mode-chat');
  147 |   await page.waitForSelector('#chat-input', { timeout: 5000 });
```