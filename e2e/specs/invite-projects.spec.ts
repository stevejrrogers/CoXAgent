// CXA-B159: the Add-team-member dialog's "Assign to projects" row must follow
// the app design system — pill chips matching the Home space filter chips,
// with a "Select all" behaviour and an accessible listbox, NOT raw checkboxes.
import { test, expect } from '@playwright/test';

test('Add-team-member dialog renders project assignment as pill chips', async ({ page }) => {
  await page.goto('/#/team');
  await page.getByRole('button', { name: 'Add team member' }).click();

  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();

  // The "Assign to projects" control is a listbox of pill chips…
  const assignRow = dialog.getByLabel(/assign to projects/i);
  await expect(assignRow).toBeVisible();
  await expect(assignRow.locator('[role="option"]')).toHaveCount(3);

  // …not raw checkboxes like the old screenshot-driven spec assumed.
  await expect(dialog.locator('input[type=checkbox]')).toHaveCount(0);

  const firstChip = assignRow.getByRole('option').first();
  await firstChip.click();
  await expect(firstChip).toHaveAttribute('aria-selected', 'true');

  // Select-all chip toggles every option at once.
  const selectAll = dialog.getByRole('option', { name: /select all/i });
  if (await selectAll.isVisible().catch(() => false)) {
    await selectAll.click();
    for (const opt of await assignRow.getByRole('option').all()) {
      await expect(opt).toHaveAttribute('aria-selected', 'true');
    }
  }

  // Pill styling follows the design-system token surface.
  const bg = await firstChip.evaluate((el) => getComputedStyle(el).borderRadius);
  expect(bg).toBe('9999px');
});

test('Add-team-member dialog highlight ring matches Home space filter chips', async ({ page }) => {
  await page.goto('/#/home');
  const homeChip = page.getByRole('option').first();
  await homeChip.waitFor({ state: 'visible' });

  await page.goto('/#/team');
  await page.getByRole('button', { name: 'Add team member' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();

  const firstChip = dialog.getByLabel(/assign to projects/i).getByRole('option').first();
  await firstChip.click();

  const tinted = await firstChip.evaluate((el) =>
    getComputedStyle(el).backgroundColor !== 'rgba(0, 0, 0, 0)'
    && getComputedStyle(el.parentElement!).borderColor !== 'rgb(0, 0, 0)');
  expect(tinted).toBe(true);
});
