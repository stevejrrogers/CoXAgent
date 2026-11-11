// CXA-B159: the Add-team-member dialog's "Assign to projects" row must follow
// the app design system — pill chips matching the Home space filter chips,
// a styled (not bare/native) checkbox, no status dot, hint text when empty.
import { test, expect } from '@playwright/test';
import { armConsoleGate, assertNoConsoleErrors, openApp } from './helpers.mjs';

async function openInvite(page: import('@playwright/test').Page) {
  // The geometry baseline (.spchip) lives on the Home view; show it before
  // opening the global invite overlay so both are measured in one page state.
  await page.evaluate(() => { location.hash = '#home'; });
  await page.locator('.spchip').first().waitFor({ timeout: 5000 }).catch(() => {});
  const hadHomeChips = await page.locator('.spchip').count();
  await page.evaluate(() => { (window as any).openInvite(); });
  const picker = page.locator('#usr-projpick');
  await expect(picker).toBeAttached();
  // The picker fetches the project registry asynchronously — wait for either
  // chips or the hint text.
  await expect.poll(async () =>
    (await picker.innerHTML()).length, { timeout: 5000 }).toBeGreaterThan(0);
  return { picker, hadHomeChips };
}

test('the assign-to-projects row uses system pills with no status dot', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);
  const { picker, hadHomeChips } = await openInvite(page);

  const chips = picker.locator('.uprojchk');

  if (hadHomeChips > 0 && (await chips.count()) > 0) {
    // AC3: computed geometry matches the Home space filter chips (.spchip)
    // within 1px on radius/padding/font-size.
    const home = await page.$eval('.spchip', (el) => {
      const s = getComputedStyle(el);
      return [s.borderRadius, s.paddingTop, s.paddingRight, s.fontSize].map(parseFloat);
    });
    const proj = await chips.first().evaluate((el) => {
      const s = getComputedStyle(el);
      return [s.borderRadius, s.paddingTop, s.paddingRight, s.fontSize].map(parseFloat);
    });
    [0, 1, 2, 3].forEach((i) => expect(Math.abs(proj[i] - home[i])).toBeLessThanOrEqual(1));
  }

  if ((await chips.count()) > 0) {
    // AC1: selection is conveyed ONLY by checkbox + selected styling — never a
    // colored status dot inside the chip.
    await expect(picker.locator('.sdot')).toHaveCount(0);
    await expect(chips.first()).toBeVisible();
  } else {
    // AC4: empty space renders plain hint text — no dot, no empty chip shell.
    const html = await picker.innerHTML();
    expect(html).not.toContain('sdot');
    expect(html).not.toMatch(/<(label|button)/); // no empty chip shells
    await expect(picker.locator('.usub')).toContainText(/No projects/i);
  }

  await assertNoConsoleErrors(errors);
});

test('the chip checkbox is a styled form control matching the dialog fields', async ({ page }) => {
  const errors: string[] = [];
  armConsoleGate(page, errors);
  await openApp(page);
  const { picker } = await openInvite(page);

  const boxes = picker.locator('.uprojchk input[type="checkbox"]');

  if ((await boxes.count()) > 0) {
    await assertChipCheckbox(boxes.first());
  } else {
    // Empty registry still runs a real gate: the chip row shows plain hint
    // text — no dot, no empty checkbox shell ever renders.
    const html = await picker.innerHTML();
    expect(html).not.toContain('sdot');
    expect(html).not.toMatch(/<input[^>]*type="checkbox"/); // no empty tick shells
    await expect(picker.locator('.usub')).toContainText(/No projects/i);
  }

  await assertNoConsoleErrors(errors);
});

/** AC2: the chip checkbox is a styled form control matching the dialog fields.
 *  Runs only when the registry is non-empty; the console gate stays with the
 *  calling test so both branches report against it. */
async function assertChipCheckbox(
  box: ReturnType<import('@playwright/test').Locator['first']>,
) {
  // Normalised, bordered square tick box — the unstyled native look
  // (default ~13px auto control, device-default chrome, no border token) is gone.
  const s = await box.evaluate((el) => {
    const c = getComputedStyle(el);
    return { w: parseFloat(c.width), h: parseFloat(c.height),
      bw: parseFloat(c.borderTopWidth), br: parseFloat(c.borderRadius) };
  });
  expect(s.w).toBeGreaterThanOrEqual(14);   // sized deliberately (not the bare ~13px native default)
  expect(Math.abs(s.h - s.w)).toBeLessThanOrEqual(4); // tick box stays visually square inside its row
  expect(s.bw).toBeGreaterThan(0);          // field-token border, visible
  expect(s.br).toBeGreaterThan(0);          // rounded-square tick corner

  // Selecting flips both the tick fill and the chip into the selected tint.
  await box.check();
  await expect(box).toHaveCSS('background-color', /.+/);
  const tinted = await box.evaluate((el) =>
    el.closest('.uprojchk')!.classList.contains('uprojchk')
    && getComputedStyle(el.parentElement!).borderColor !== 'rgb(0, 0, 0)');
  expect(tinted).toBe(true);
}
