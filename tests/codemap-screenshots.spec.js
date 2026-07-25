#!/usr/bin/env node
// Screenshot test — captures code map visual state for inspection
const { test } = require('@playwright/test');
const path = require('path');

const BASE = 'http://localhost:4000';

async function login(page) {
  await page.goto(BASE + '/');
  await page.waitForSelector('#lg-user', { timeout: 8000 });
  await page.fill('#lg-user', 'root');
  await page.fill('#lg-pass', 'Str@wb3rry');
  await page.click('button:has-text("Sign in")');
  await page.waitForSelector('.side', { timeout: 10000 });
}

test('screenshot code map', async ({ page }) => {
  await login(page);
  await page.click('a[data-v="codemap"]');
  await page.waitForSelector('.cg-svg', { timeout: 15000 });

  // Scroll to make graph visible
  await page.locator('.cg-graph').scrollIntoViewIfNeeded();

  // Give graph a moment to finish any animation
  await page.waitForTimeout(500);

  await page.screenshot({
    path: path.join(__dirname, '..', 'codemap-screenshot.png'),
    fullPage: true,
  });
});

test('screenshot files tab', async ({ page }) => {
  await login(page);
  await page.click('a[data-v="codemap"]');
  await page.waitForSelector('#codemap-body', { timeout: 15000 });

  await page.click('#cmt-files');
  await page.waitForSelector('#ws-panel:not([hidden])', { timeout: 8000 });
  await page.waitForTimeout(500);

  await page.screenshot({
    path: path.join(__dirname, '..', 'codemap-files-screenshot.png'),
    fullPage: true,
  });
});

test('screenshot graph zoomed in', async ({ page }) => {
  await login(page);
  await page.click('a[data-v="codemap"]');
  await page.waitForSelector('.cg-svg', { timeout: 15000 });

  const svg = page.locator('.cg-svg');
  const box = await svg.boundingBox();

  // Zoom in 3 times
  for (let i = 0; i < 3; i++) {
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.mouse.wheel(0, -200);
    await page.waitForTimeout(200);
  }

  await page.screenshot({
    path: path.join(__dirname, '..', 'codemap-zoomed-screenshot.png'),
    fullPage: true,
  });
});

test('screenshot graph with pinned node', async ({ page }) => {
  await login(page);
  await page.click('a[data-v="codemap"]');
  await page.waitForSelector('.cg-svg', { timeout: 15000 });

  // Click a hub node to pin it
  await page.locator('.cg-hub').first().click();

  // Now click the graph node
  await page.waitForTimeout(800);

  await page.screenshot({
    path: path.join(__dirname, '..', 'codemap-pinned-screenshot.png'),
    fullPage: true,
  });
});
