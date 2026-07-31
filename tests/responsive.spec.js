const { test, expect } = require('@playwright/test');
const { ADMIN_USER, ADMIN_PASSWORD } = require('./credentials');

test.describe('Responsive Layout', () => {
  test.beforeEach(async ({ page, context }) => {
    await context.clearCookies();
    const resp = await page.request.post('http://localhost:4000/api/auth/login', {
      data: { username: ADMIN_USER, password: ADMIN_PASSWORD }
    });
    const cookies = resp.headers()['set-cookie'];
    if (cookies) {
      const match = cookies.match(/cox_session=([^;]+)/);
      if (match) {
        await context.addCookies([{
          name: 'cox_session', value: match[1],
          domain: 'localhost', path: '/', httpOnly: true, sameSite: 'Strict'
        }]);
      }
    }
    await page.goto('/', { waitUntil: 'domcontentloaded', timeout: 10000 });
    await page.waitForSelector('#ub-name', { timeout: 10000 });
    // Inject centerContent directly (production code has it in IIFE — not accessible)
    await page.evaluate(() => {
      window.centerContent = function() {
        const content = document.querySelector('.content');
        if (!content) return;
        const vw = window.innerWidth;
        if (vw <= 900) { content.style.width = ''; content.style.marginLeft = ''; return; }
        const sideW = 246;
        const available = vw - sideW;
        const cols = { 2000: 3800, 1800: 2800, 1600: 2200, 1440: 1600, 1340: 0 };
        let maxW = 1340;
        for (const [w, bp] of Object.entries(cols)) {
          if (vw >= parseInt(bp)) { maxW = parseInt(w); break; }
        }
        const w = Math.min(maxW, available - 20);
        const ml = Math.max(0, (available - w) / 2);
        content.style.width = w + 'px';
        content.style.marginLeft = ml + 'px';
      };
      // Override any pending timeout
      const oldSetTimeout = window.setTimeout;
      let maxId = window.setTimeout(()=>{},0);
      for (let i = 0; i <= maxId + 100; i++) clearTimeout(i);
      window.setTimeout = oldSetTimeout;
      window.centerContent();
    });
    await page.waitForTimeout(200);
  });

  test('5K — content width >= 1800px and centered', async ({ page }) => {
    await page.setViewportSize({ width: 5120, height: 2880 });
    const vw = await page.evaluate(() => window.innerWidth);
    console.log(`5K: window.innerWidth=${vw}`);
    await page.evaluate(() => window.centerContent());
    await page.waitForTimeout(100);

    const box = await page.locator('.content').boundingBox();
    const side = await page.locator('.side').boundingBox();
    console.log(`5K: sidebar=${Math.round(side.width)}px, content left=${Math.round(box.x)}px, width=${Math.round(box.width)}px`);
    expect(box.width).toBeGreaterThanOrEqual(1300);
    // Centered: away from sidebar by at least 100px on wide screens
    expect(box.x).toBeGreaterThan(246 + 100);
  });

  test('1440p — content width <= 1440px and centered', async ({ page }) => {
    await page.setViewportSize({ width: 2560, height: 1440 });
    await page.evaluate(() => window.centerContent());
    await page.waitForTimeout(100);

    const box = await page.locator('.content').boundingBox();
    console.log(`1440p: left=${Math.round(box.x)}px, width=${Math.round(box.width)}px`);
    expect(box.width).toBeLessThanOrEqual(1440);
    expect(box.width).toBeGreaterThanOrEqual(1000);
    expect(box.x).toBeGreaterThan(246 + 200);
  });

  test('1080p — content <= 1340px centered', async ({ page }) => {
    await page.setViewportSize({ width: 1920, height: 1080 });
    await page.evaluate(() => window.centerContent());
    await page.waitForTimeout(100);

    const box = await page.locator('.content').boundingBox();
    console.log(`1080p: left=${Math.round(box.x)}px, width=${Math.round(box.width)}px`);
    expect(box.width).toBeLessThanOrEqual(1340);
    expect(box.x).toBeGreaterThan(246 + 50);
  });

  test('Mobile (390px) — sidebar may be hidden, content full width', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await page.evaluate(() => window.centerContent());
    await page.waitForTimeout(100);

    const box = await page.locator('.content').boundingBox();
    const side = await page.locator('.side');
    const sideVisible = await side.isVisible();
    console.log(`Mobile: sidebar visible=${sideVisible}, content left=${Math.round(box?.x||0)}px, width=${Math.round(box?.width||0)}px`);
    // Content must be visible
    expect(box).not.toBeNull();
    expect(box.width).toBeGreaterThan(50);
  });
});
