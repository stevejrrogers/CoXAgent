import { test, expect } from '@playwright/test';
import { ADMIN_USER } from './helpers-auth.mjs';

const WRONG_PASSWORD = 'WrongPass_000';

test('wrong credentials show error feedback without navigating', async ({ page }) => {
  await page.goto('/');
  await expect(page.locator('#ov-login')).toBeVisible();
  const urlBefore = new URL(page.url()).pathname;
  // Two consecutive bad attempts: both surface visible error feedback and the
  // route stays put — doLogin never clears fields on failure, so clicking again
  // re-submits the same bad pair (exactly "wrong password twice in a row").
  for (let i = 0; i < 2; i++) {
    if (i === 0) {
      await page.locator('#lg-user').fill(ADMIN_USER);
      await page.locator('#lg-pass').fill(WRONG_PASSWORD);
    }
    await page.locator('#ov-login button.pri').click();
    await expect(page.locator('#lg-err')).toContainText('Invalid credentials.');
    expect(new URL(page.url()).pathname).toBe(urlBefore);
    const stillThere = String(await page.inputValue('#lg-pass'));
    expect(stillThere).toBe(WRONG_PASSWORD); // never cleared by a failed submit
  }
});

test('empty credentials trigger inline validation without navigating', async ({ page }) => {
  await page.goto('/');
  await expect(page.locator('#ov-login')).toBeVisible();
  const urlBefore = new URL(page.url()).pathname;
  // Clear any prefilled values so both fields are empty.
  await page.locator('#lg-user').fill('');
  await page.locator('#lg-pass').fill('');
  await page.locator('#ov-login button.pri').click();
  await expect(page.locator('#lg-err')).toContainText('Enter username and password.');
  // The modal stays up and the route does not change.
  expect(new URL(page.url()).pathname).toBe(urlBefore);
});
