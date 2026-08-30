import { test, expect } from '@playwright/test';

// CXA-B112 regression: the primary "Sign in" button rendered text-only in
// deployed containers. Root cause: the Tabler webfont stylesheet came from
// cdn.jsdelivr.net, and deployed containers have no CDN egress — without the
// stylesheet no `.ti-*:before{content:…}` rule exists, so every Tabler glyph
// (icon characters exist only as CSS-generated content) paints zero ink. The
// fix vendors the webfont like Mermaid/xterm; this spec pins the guarantee
// under the deploy condition itself: ALL external requests aborted.
test('sign-in icon renders with every external CDN unreachable', async ({ page }) => {
  // Simulate the air-gapped deploy: anything not served by the app itself fails.
  const external: string[] = [];
  await page.route(/^https?:\/\/(?!127\.0\.0\.1)/, async (route) => {
    external.push(route.request().url());
    await route.abort();
  });

  // No session cookie → the login overlay is the first thing a guest sees,
  // which is exactly the surface PD visual QA screenshotted.
  await page.goto('/');
  await expect(page.locator('#ov-login')).toBeVisible();
  await page.evaluate(() => document.fonts.ready);

  // 1. No stylesheet may be requested from a third-party CDN any more.
  expect(
    external.filter((url) => url.includes('cdn.jsdelivr.net')),
    'Tabler webfont must be served same-origin, never from jsDelivr',
  ).toEqual([]);

  // 2. The glyph content rule must exist (pre-fix it was `none`: no icon at all).
  const icon = page.locator('#ov-login button.pri i.ti-login');
  await expect(icon).toHaveCount(1);
  const content = await icon.evaluate((el) => getComputedStyle(el, '::before').content);
  expect(content, 'ti-login :before must carry the glyph character').not.toBe('none');

  // 3. The vendored face must actually be loaded and cover the glyph, i.e. the
  //    browser can rasterize the icon with zero network dependency. (Read
  //    FontFace fields inside the page — they do not serialize to Node.)
  const faceStatus = await page.evaluate(
    () =>
      [...document.fonts].find(
        (f) => f.family.replace(/"/g, '') === 'tabler-icons',
      )?.status,
  );
  expect(faceStatus, 'vendored tabler-icons @font-face must load').toBe('loaded');
  expect(
    await page.evaluate(() =>
      document.fonts.check('18px "tabler-icons"', '\uEBA7'),
    ),
  ).toBe(true);
});
