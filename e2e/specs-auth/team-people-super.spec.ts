import { test, expect } from '@playwright/test';
import { ADMIN_USER, ADMIN_PASSWORD, signInAs } from './helpers-auth.mjs';

// CXA-B132: the hub owner's role is "super" (bootstrap_admin provisions it
// Super), but the UI gated every admin surface on role==="admin" exactly:
// applyRole left .admin-only display:none and the Team view's "People on this
// project" panel never had its renderer called — stuck on bare "loading…"
// forever for the most privileged account. Both halves must hold for super:
// the panel is visible AND reaches a terminal state.
//
// State independence: specs in this suite share one auth server, and earlier
// files create/assign accounts (brakes, rbac-viewer) — so the panel may hold
// member rows OR the "no humans assigned yet" empty state. The terminal-state
// contract is what must hold either way: renderTeamPeople is the only writer
// of the "Manage who works…" footer, and a refused/failed fetch renders
// "admin only" instead — neither may be the stuck "loading…" placeholder.
test('the team people panel renders for the super (hub owner) role', async ({ page }) => {
  await signInAs(page, ADMIN_USER, ADMIN_PASSWORD);
  await page.goto('/');
  // Signed-in shell: the badge only shows once ME is loaded and applyRole ran.
  await expect(page.locator('#user-badge')).toBeVisible();
  await page.evaluate(() => { (window as any).nav('team'); });

  const panel = page.locator('#team-people');
  // applyRole half: .admin-only must not be display:none for the super role.
  await expect(panel).toBeVisible();
  // Renderer half: the fetch succeeded (array → no "admin only" refusal) and
  // a terminal list rendered (footer written by renderTeamPeople only).
  await expect(panel).toContainText('Manage who works on which project in');
  await expect(panel).not.toContainText('loading…');
  await expect(panel).not.toContainText('admin only');
});

// Third call site of the same predicate: the ⌘K palette filtered admin views
// on role==="admin" exactly, so the hub owner could see the sidebar links
// (after the applyRole fix) yet never find them via the palette. cmdkBuild
// runs off ME alone — no fetch — so the assertion is deterministic.
test('the command palette offers the admin views to the super role too', async ({ page }) => {
  await signInAs(page, ADMIN_USER, ADMIN_PASSWORD);
  await page.goto('/');
  await expect(page.locator('#user-badge')).toBeVisible();
  await page.evaluate(() => { (window as any).openCmdk(); });
  const list = page.locator('#cmdk-list');
  await expect(list).toContainText('People');
  await expect(list).toContainText('Audit');
});
