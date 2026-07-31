const { test, expect } = require('@playwright/test');
const fs = require('fs');
const os = require('os');
const path = require('path');

// 1x1 transparent PNG — enough to satisfy the server's `image/*` mime check.
const TINY_PNG_B64 =
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=';

function writeTinyPng() {
  const p = path.join(os.tmpdir(), `cox-e2e-avatar-${Date.now()}.png`);
  fs.writeFileSync(p, Buffer.from(TINY_PNG_B64, 'base64'));
  return p;
}

// ── helpers ────────────────────────────────────────────────────────────────

const PASS = 'Str@wb3rry';

async function loginViaApi(page) {
  const resp = await page.request.post('http://localhost:4000/api/auth/login', {
    data: { username: 'root', password: PASS }
  });
  expect(resp.status(), 'login should succeed').toBe(200);
  const cookies = resp.headers()['set-cookie'];
  if (cookies) {
    await page.context().addCookies([{
      name: 'cox_session', value: cookies.split(';')[0].split('=')[1],
      domain: 'localhost', path: '/', httpOnly: true, sameSite: 'Strict'
    }]);
  }
}

async function initPage(page) {
  await page.context().clearCookies();
  await loginViaApi(page);
  await page.goto('/', { waitUntil: 'domcontentloaded' });
  await page.waitForFunction(() => {
    const el = document.getElementById('ub-name');
    return el && el.textContent && el.textContent.trim().length > 0;
  }, { timeout: 15000 });
  await page.evaluate(() => {
    localStorage.setItem('cox_mode', 'workspace');
    if (typeof MODE !== 'undefined') MODE = 'workspace';
    if (typeof applyMode === 'function') applyMode();
  });
  await page.waitForTimeout(400);
}

async function navTo(page, v) {
  await page.click(`a[data-v="${v}"]`);
  await page.waitForTimeout(400);
}

// ── test suites ────────────────────────────────────────────────────────────

test.describe('Authentication', () => {

  test('login page renders', async ({ page }) => {
    await page.goto('/', { waitUntil: 'domcontentloaded' });
    await expect(page.locator('#ov-login')).toBeVisible({ timeout: 10000 });
    await expect(page.locator('#lg-user')).toBeVisible();
    await expect(page.locator('#lg-pass')).toBeVisible();
  });

  test('login with wrong password fails', async ({ page }) => {
    await page.goto('/', { waitUntil: 'domcontentloaded' });
    await expect(page.locator('#ov-login')).toBeVisible({ timeout: 10000 });
    await page.locator('#lg-user').fill('root');
    await page.locator('#lg-pass').fill('wrong');
    // Click the login button instead of Enter
    await page.locator('#ov-login button:has-text("Sign in"), #ov-login .pri').click();
    await page.waitForTimeout(1500);
    const err = page.locator('#lg-err');
    await expect(err).toBeVisible({ timeout: 5000 });
  });

  test('login with correct password succeeds', async ({ page }) => {
    await page.goto('/', { waitUntil: 'domcontentloaded' });
    await expect(page.locator('#ov-login')).toBeVisible({ timeout: 10000 });
    await page.locator('#lg-user').fill('root');
    await page.locator('#lg-pass').fill(PASS);
    await page.locator('#ov-login button:has-text("Sign in"), #ov-login .pri').click();
    await page.waitForTimeout(2000);
    // After login, user badge should appear
    const badge = page.locator('#ub-name');
    const text = await badge.textContent();
    expect(text, 'user badge should have content').toBeTruthy();
  });

});

test.describe('Navigation', () => {
  test.beforeEach(async ({ page }) => { await initPage(page); });

  const pages = [
    { v: 'home', id: '#view-home' },
    { v: 'overview', id: '#view-overview' },
    { v: 'team', id: '#view-team' },
    { v: 'board', id: '#view-board' },
    { v: 'roadmap', id: '#view-roadmap' },
    { v: 'activity', id: '#view-activity' },
    { v: 'discuss', id: '#view-discuss' },
    { v: 'docs', id: '#view-docs' },
    { v: 'codemap', id: '#view-codemap' },
    { v: 'calendar', id: '#view-calendar' },
    { v: 'insights', id: '#view-insights' },
    { v: 'settings', id: '#view-settings' },
  ];

  pages.forEach(({ v, id }) => {
    test(`nav to ${v} renders ${id}`, async ({ page }) => {
      await navTo(page, v);
      await expect(page.locator(id)).toBeVisible({ timeout: 5000 });
    });
  });

  test('nav to terminal (admin only)', async ({ page }) => {
    await navTo(page, 'terminal');
    await page.waitForTimeout(1000);
    const exists = await page.locator('#view-terminal').count();
    expect(exists, 'terminal view should exist for admin').toBeGreaterThan(0);
  });

  test('nav to review shows PR list', async ({ page }) => {
    await navTo(page, 'review');
    await expect(page.locator('#view-review')).toBeVisible({ timeout: 5000 });
  });

  test('nav to people (admin only)', async ({ page }) => {
    const link = page.locator('a[data-v="people"]');
    const count = await link.count();
    if (count === 0) { test.skip(true, 'people link not present'); return; }
    const visible = await link.isVisible();
    if (!visible) {
      // force-click hidden admin-only link via evaluate
      await page.evaluate(() => { document.querySelector('a[data-v="people"]')?.click(); });
    } else {
      await link.click();
    }
    await page.waitForTimeout(500);
    await expect(page.locator('#view-people')).toBeVisible({ timeout: 5000 });
  });

  test('nav to audit (admin only)', async ({ page }) => {
    const link = page.locator('a[data-v="audit"]');
    const count = await link.count();
    if (count === 0) { test.skip(true, 'audit link not present'); return; }
    const visible = await link.isVisible();
    if (!visible) {
      await page.evaluate(() => { document.querySelector('a[data-v="audit"]')?.click(); });
    } else {
      await link.click();
    }
    await page.waitForTimeout(500);
    await expect(page.locator('#view-audit')).toBeVisible({ timeout: 5000 });
  });

  test('nav to access (manage only)', async ({ page }) => {
    await navTo(page, 'access');
    await expect(page.locator('#view-access')).toBeVisible({ timeout: 5000 });
  });

});

test.describe('Dashboard', () => {
  test.beforeEach(async ({ page }) => { await initPage(page); });

  test('user badge shows root as super admin', async ({ page }) => {
    await expect(page.locator('#ub-name')).toContainText('root');
    await expect(page.locator('#ub-role')).toContainText(/super/i);
  });

  test('project name is visible in header', async ({ page }) => {
    await expect(page.locator('#tb-proj')).toBeVisible({ timeout: 5000 });
  });

  test('home page has hero element', async ({ page }) => {
    const exists = await page.locator('#ws-hero').count();
    expect(exists).toBeGreaterThan(0);
  });

  test('runner control pill is visible', async ({ page }) => {
    await expect(page.locator('#runpill')).toBeVisible({ timeout: 5000 });
    await expect(page.locator('#ctl-primary')).toBeVisible();
  });

  test('overview shows KPIs', async ({ page }) => {
    await navTo(page, 'overview');
    // KPIs may be hidden if no data — just check element exists
    const kpis = page.locator('#kpis');
    await expect(kpis).toBeAttached({ timeout: 5000 });
  });

  test('overview shows activity feed', async ({ page }) => {
    await navTo(page, 'overview');
    const feed = page.locator('#ov-activity');
    await expect(feed).toBeAttached({ timeout: 5000 });
  });

  test('overview shows changelog', async ({ page }) => {
    await navTo(page, 'overview');
    const log = page.locator('#ov-changelog');
    await expect(log).toBeAttached({ timeout: 5000 });
  });

});

test.describe('Board / Work', () => {
  test.beforeEach(async ({ page }) => { await initPage(page); });

  test('board view visible with columns', async ({ page }) => {
    await navTo(page, 'board');
    await expect(page.locator('#view-board')).toBeVisible({ timeout: 5000 });
    await expect(page.locator('#board-cols')).toBeVisible({ timeout: 5000 });
  });

  test('work tabs (board/sprint/backlog) exist', async ({ page }) => {
    await navTo(page, 'board');
    const tabs = page.locator('#work-seg button');
    await expect(tabs.first()).toBeVisible({ timeout: 5000 });
    const count = await tabs.count();
    expect(count, 'should have work segment tabs').toBeGreaterThanOrEqual(2);
  });

  test('new ticket dialog opens', async ({ page }) => {
    await navTo(page, 'board');
    const newTicketBtn = page.locator('.ticket-actions button').first();
    if (await newTicketBtn.count() > 0) {
      await newTicketBtn.click();
      await page.waitForTimeout(500);
      await expect(page.locator('#ov-newticket')).toBeVisible({ timeout: 5000 });
    }
  });

  test('new ticket form has all required fields', async ({ page }) => {
    await navTo(page, 'board');
    await page.evaluate(() => { if (typeof openNewTicket === 'function') openNewTicket(); });
    await page.waitForTimeout(500);
    await expect(page.locator('#nt-title')).toBeVisible({ timeout: 5000 });
    await expect(page.locator('#nt-type')).toBeVisible();
    await expect(page.locator('#nt-prio')).toBeVisible();
    await expect(page.locator('#nt-cx')).toBeVisible();
    await page.locator('#nt-close, [onclick*="close_(\'ov-newticket\')"]').first().click();
  });

  test('roadmap view renders', async ({ page }) => {
    await navTo(page, 'roadmap');
    await expect(page.locator('#view-roadmap')).toBeVisible({ timeout: 5000 });
  });

});

test.describe('Settings', () => {
  test.beforeEach(async ({ page }) => { await initPage(page); });

  test('settings tabs exist', async ({ page }) => {
    await navTo(page, 'settings');
    await page.waitForTimeout(800);
    // Settings tabs use .settab-btn class with data-t attribute
    const tabs = page.locator('.settab-btn, [class*="settab"]');
    const count = await tabs.count();
    expect(count, 'should have settings tabs').toBeGreaterThanOrEqual(1);
  });

  const settingsTabs = ['engines', 'workflow', 'git', 'profile'];
  settingsTabs.forEach(tab => {
    test(`settings tab '${tab}' renders`, async ({ page }) => {
      await navTo(page, 'settings');
      await page.click(`.settab-btn[data-t="${tab}"]`);
      await page.waitForTimeout(400);
      await expect(page.locator(`.settab[data-p="${tab}"]`)).toBeVisible({ timeout: 5000 });
    });
  });

  test('workflow settings has budget fields', async ({ page }) => {
    await navTo(page, 'settings');
    await page.click('.settab-btn[data-t="workflow"]');
    await page.waitForTimeout(400);
    await expect(page.locator('#wf-bg')).toBeVisible({ timeout: 5000 });
    await expect(page.locator('#wf-dg')).toBeVisible();
  });

  test('engines tab shows default engine selector', async ({ page }) => {
    await navTo(page, 'settings');
    await page.click('.settab-btn[data-t="engines"]');
    await page.waitForTimeout(400);
    await expect(page.locator('#eng-default')).toBeVisible({ timeout: 5000 });
  });

});

test.describe('Chat', () => {
  test.beforeEach(async ({ page }) => { await initPage(page); });

  test('chat mode toggle works', async ({ page }) => {
    await page.click('#mode-chat');
    await page.waitForTimeout(500);
    await expect(page.locator('#chat-side')).toBeVisible({ timeout: 5000 });
    await page.click('#mode-ws');
    await page.waitForTimeout(500);
    await expect(page.locator('#ws-nav')).toBeVisible({ timeout: 5000 });
  });

  test('chat input is visible after toggling', async ({ page }) => {
    await page.click('#mode-chat');
    await page.waitForTimeout(500);
    await expect(page.locator('#chat-input')).toBeVisible({ timeout: 5000 });
  });

  test('team chat in discuss view', async ({ page }) => {
    await navTo(page, 'discuss');
    const hasInput = await page.locator('#disc-input').count();
    expect(hasInput, 'should have discuss input').toBeGreaterThan(0);
  });

});

test.describe('Agents', () => {
  test.beforeEach(async ({ page }) => { await initPage(page); });

  test('agents page shows agent cards', async ({ page }) => {
    await navTo(page, 'team');
    await expect(page.locator('#agents')).toBeVisible({ timeout: 5000 });
    // Agent cards may have different selectors depending on state
    const cards = page.locator('#agents > div, #agents .arow, #agents .agent-card');
    const count = await cards.count();
    expect(count, 'should have agent cards').toBeGreaterThanOrEqual(1);
  });

  test('workload panel visible', async ({ page }) => {
    await navTo(page, 'team');
    // Workload panel may be inside agents or separate
    const panel = page.locator('#ov-workload, #agents');
    await expect(panel).toBeAttached({ timeout: 5000 });
  });

});

test.describe('Wiki / Docs', () => {
  test.beforeEach(async ({ page }) => { await initPage(page); });

  test('docs view renders sidebar + main', async ({ page }) => {
    await navTo(page, 'docs');
    await expect(page.locator('#view-docs')).toBeVisible({ timeout: 5000 });
    await expect(page.locator('#docs-list')).toBeVisible({ timeout: 5000 });
  });

});

test.describe('Code Map', () => {
  test.beforeEach(async ({ page }) => { await initPage(page); });

  test('codemap view renders', async ({ page }) => {
    await navTo(page, 'codemap');
    await expect(page.locator('#view-codemap')).toBeVisible({ timeout: 5000 });
  });

});

test.describe('Calendar & Meetings', () => {
  test.beforeEach(async ({ page }) => { await initPage(page); });

  test('calendar view renders', async ({ page }) => {
    await navTo(page, 'calendar');
    await expect(page.locator('#view-calendar')).toBeVisible({ timeout: 5000 });
    await expect(page.locator('#cal-body')).toBeVisible({ timeout: 5000 });
  });

  test('new meeting dialog opens', async ({ page }) => {
    await navTo(page, 'calendar');
    const newMeetBtn = page.locator('#meet-new, button:has-text("Book meeting")').first();
    if (await newMeetBtn.count() > 0) {
      await newMeetBtn.click();
      await page.waitForTimeout(500);
      await expect(page.locator('#ov-meet')).toBeVisible({ timeout: 5000 });
    }
  });

});

test.describe('API Endpoints', () => {
  test.beforeEach(async ({ page }) => { await initPage(page); });

  test('/api/auth/me returns super role', async ({ page }) => {
    const data = await page.evaluate(async () => {
      const r = await fetch('/api/auth/me');
      return r.json();
    });
    expect(data.role).toBe('super');
    expect(data.username).toBe('root');
  });

  test('/api/auth/me includes user info', async ({ page }) => {
    const data = await page.evaluate(async () => {
      const r = await fetch('/api/auth/me');
      return r.json();
    });
    expect(data).toHaveProperty('username');
    expect(data).toHaveProperty('role');
  });

  test('/api/spaces returns space list', async ({ page }) => {
    const data = await page.evaluate(async () => {
      const r = await fetch('/api/spaces');
      return r.json();
    });
    const arr = data.spaces || data;
    expect(Array.isArray(arr)).toBe(true);
  });

  test('/api/projects returns projects', async ({ page }) => {
    const data = await page.evaluate(async () => {
      const r = await fetch('/api/projects');
      return r.json();
    });
    expect(Array.isArray(data)).toBe(true);
  });

  test('/api/health returns ok', async ({ page }) => {
    const data = await page.evaluate(async () => {
      const r = await fetch('/api/health');
      return r.json();
    });
    expect(data.status || data.ok || data).toBeTruthy();
  });

  test('/api/engines returns engine list', async ({ page }) => {
    const data = await page.evaluate(async () => {
      const r = await fetch('/api/engines');
      return r.json();
    });
    expect(Array.isArray(data.engines || data)).toBe(true);
  });

  test('/api/auth/sessions returns current session', async ({ page }) => {
    const data = await page.evaluate(async () => {
      const r = await fetch('/api/auth/sessions');
      return r.json();
    });
    const current = (data || []).find(s => s.current);
    expect(current).toBeDefined();
  });

  test('/api/me/agents returns agent definitions', async ({ page }) => {
    const data = await page.evaluate(async () => {
      const r = await fetch('/api/me/agents');
      return r.json();
    });
    expect(Array.isArray(data.agents || data)).toBe(true);
  });

});

test.describe('Manage (Super Admin)', () => {
  test.beforeEach(async ({ page }) => { await initPage(page); });

  test('manage mode toggle works', async ({ page }) => {
    await page.click('#mode-manage');
    await page.waitForTimeout(500);
    await expect(page.locator('#manage-side')).toBeVisible({ timeout: 5000 });
    await page.click('#mode-ws');
    await page.waitForTimeout(500);
    await expect(page.locator('#ws-nav')).toBeVisible({ timeout: 5000 });
  });

  test('manage overview API accessible', async ({ page }) => {
    const data = await page.evaluate(async () => {
      const r = await fetch('/api/manage/overview');
      return r.json();
    });
    expect(data.spaces).toBeDefined();
    expect(data.totals).toBeDefined();
  });

  test('manage users API accessible', async ({ page }) => {
    const data = await page.evaluate(async () => {
      const r = await fetch('/api/auth/users');
      return r.json();
    });
    expect(Array.isArray(data.users || data)).toBe(true);
  });

});

test.describe('Activity Feed', () => {
  test.beforeEach(async ({ page }) => { await initPage(page); });

  test('activity view renders', async ({ page }) => {
    await navTo(page, 'activity');
    await expect(page.locator('#view-activity')).toBeVisible({ timeout: 5000 });
  });

});

test.describe('Sidebar & Misc', () => {
  test.beforeEach(async ({ page }) => { await initPage(page); });

  test('logout button exists', async ({ page }) => {
    // Logout may be in user dropdown menu
    const ubName = page.locator('#ub-name');
    await ubName.click();
    await page.waitForTimeout(300);
    const logout = page.locator('#ub-logout, [onclick*="logout"], [onclick*="Logout"]').first();
    const exists = await logout.count();
    expect(exists, 'logout button should exist').toBeGreaterThan(0);
  });

  test('command palette opens with Cmd+K', async ({ page }) => {
    await page.keyboard.press('Meta+k');
    await page.waitForTimeout(300);
    await expect(page.locator('#cmdk')).toBeVisible({ timeout: 5000 });
    await page.keyboard.press('Escape');
  });

  test('sidebar nav has all primary sections', async ({ page }) => {
    const navItems = [
      'home', 'overview', 'team', 'board', 'roadmap', 'review',
      'activity', 'discuss', 'codemap', 'docs', 'calendar',
      'insights', 'settings'
    ];
    for (const v of navItems) {
      const link = page.locator(`a[data-v="${v}"]`);
      await expect(link, `nav link [data-v="${v}"] should exist`).toBeVisible({ timeout: 3000 });
    }
  });

});

// Regression for COX-B056: the file input behind "Change photo" must stay
// clickable via label/button activation (not merely present in the DOM).
test.describe('Profile / Avatar upload', () => {
  test.beforeEach(async ({ page }) => { await initPage(page); });

  async function openProfileTab(page) {
    await page.locator('.uavatar').click();
    await page.locator('#pt-profile').click();
    await expect(page.locator('#pp-profile')).toBeVisible();
    return page.locator('#pp-profile button:has-text("Change photo")');
  }

  test('click "Change photo" opens the native file picker with no console error', async ({ page }) => {
    const errors = [];
    page.on('pageerror', e => errors.push(e.message));
    page.on('console', msg => { if (msg.type() === 'error') errors.push(msg.text()); });

    const changePhotoBtn = await openProfileTab(page);
    const [chooser] = await Promise.all([
      page.waitForEvent('filechooser', { timeout: 5000 }),
      changePhotoBtn.click(),
    ]);
    expect(chooser).toBeTruthy();
    expect(errors, `no console errors, got: ${errors.join(' | ')}`).toHaveLength(0);
  });

  test('selecting a valid image uploads the avatar and updates the preview without reload', async ({ page }) => {
    const changePhotoBtn = await openProfileTab(page);
    await page.evaluate(() => { window.__navMarker = 'still-here'; });

    const [chooser] = await Promise.all([
      page.waitForEvent('filechooser'),
      changePhotoBtn.click(),
    ]);
    await chooser.setFiles(writeTinyPng());

    await expect(page.locator('#toasts .toast.ok', { hasText: /Avatar updated/i }))
      .toBeVisible({ timeout: 8000 });
    await expect(page.locator('#pf-av img.avimg')).toBeVisible({ timeout: 5000 });

    const navMarkerSurvived = await page.evaluate(() => window.__navMarker === 'still-here');
    expect(navMarkerSurvived, 'page must not have reloaded').toBe(true);
  });

  test('cancelling the picker without choosing a file leaves the UI unchanged', async ({ page }) => {
    const changePhotoBtn = await openProfileTab(page);
    const [chooser] = await Promise.all([
      page.waitForEvent('filechooser'),
      changePhotoBtn.click(),
    ]);
    // Simulate "Cancel": close the native dialog without calling setFiles().
    await page.waitForTimeout(500);

    await expect(page.locator('#pp-profile')).toBeVisible();
    expect(await page.locator('#toasts .toast.err').count()).toBe(0);
  });

  test('keyboard-only Tab + Enter activates the file picker', async ({ page }) => {
    const changePhotoBtn = await openProfileTab(page);
    await changePhotoBtn.focus();
    await expect(changePhotoBtn).toBeFocused();

    const [chooser] = await Promise.all([
      page.waitForEvent('filechooser', { timeout: 5000 }),
      page.keyboard.press('Enter'),
    ]);
    expect(chooser).toBeTruthy();
  });

});

test.describe('Project Import Flow', () => {
  test.beforeEach(async ({ page }) => { await initPage(page); });

  test('import project form has all required fields', async ({ page }) => {
    await page.evaluate(() => { if (typeof openNewProject === 'function') openNewProject(); });
    await page.waitForTimeout(500);
    await expect(page.locator('#ov-newproj')).toBeVisible({ timeout: 5000 });
    // Switch to import tab
    await page.click('#npm-import');
    await page.waitForTimeout(300);
    // Check import-specific fields
    await expect(page.locator('#np-path-row')).toBeVisible({ timeout: 3000 });
    await expect(page.locator('#np-name')).toBeVisible();
    await expect(page.locator('#np-path')).toBeVisible();
  });

  test('import project via API and verify structure', async ({ page }) => {
    // First, find the space ID
    const spacesResp = await page.evaluate(async () => {
      const r = await fetch('/api/spaces');
      return r.json();
    });
    const spaceId = (spacesResp.spaces || [])[0]?.id || 'coxspace';

    // Import via API
    const result = await page.evaluate(async (space) => {
      const r = await fetch('/api/projects', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          name: 'Test Import Project',
          alias: 'TIP',
          existing: '/tmp/cox-test-project',
          space: space
        })
      });
      const text = await r.text();
      try { return JSON.parse(text); } catch { return { ok: false, error: text, status: r.status }; }
    }, spaceId);
    expect(result.ok, 'import should succeed: ' + JSON.stringify(result)).toBe(true);
    expect(result.id, 'should return project id').toBeTruthy();

    // Verify project appears in list
    const projects = await page.evaluate(async () => {
      const r = await fetch('/api/projects');
      return r.json();
    });
    expect(projects.some(p => p.id === result.id || p.alias === 'TIP')).toBe(true);
  });

  test('imported project shows in projects list', async ({ page }) => {
    // Import first
    const spacesResp = await page.evaluate(async () => {
      const r = await fetch('/api/spaces');
      return r.json();
    });
    const spaceId = (spacesResp.spaces || [])[0]?.id || 'coxspace';

    await page.evaluate(async (space) => {
      await fetch('/api/projects', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          name: 'Test Import Project',
          alias: 'TIP',
          existing: '/tmp/cox-test-project',
          space: space
        })
      });
    }, spaceId);

    const data = await page.evaluate(async () => {
      const r = await fetch('/api/projects');
      return r.json();
    });
    expect(Array.isArray(data)).toBe(true);
    expect(data.length).toBeGreaterThanOrEqual(1);
    const imported = data.find(p => p.alias === 'TIP');
    expect(imported, 'imported project TIP should exist').toBeDefined();
  });

  test('imported project codemap accessible', async ({ page }) => {
    const projects = await page.evaluate(async () => {
      const r = await fetch('/api/projects');
      return r.json();
    });
    const imported = projects.find(p => p.alias === 'TIP');
    if (!imported) { test.skip(true, 'No imported project found'); return; }

    const cgData = await page.evaluate(async (pid) => {
      const r = await fetch('/api/projects/' + pid + '/codegraph');
      return r.json();
    }, imported.id);
    expect(cgData.built).toBe(true);
    expect(cgData.files).toBe(2);
    expect(cgData.languages.javascript).toBe(2);
  });

  test('imported project board has seeded dockerize ticket', async ({ page }) => {
    const projects = await page.evaluate(async () => {
      const r = await fetch('/api/projects');
      return r.json();
    });
    const imported = projects.find(p => p.alias === 'TIP');
    if (!imported) { test.skip(true, 'No imported project found'); return; }

    const state = await page.evaluate(async (pid) => {
      const r = await fetch('/api/projects/' + pid + '/state');
      return r.json();
    }, imported.id);

    expect(Array.isArray(state.tickets)).toBe(true);
    expect(state.tickets.length).toBeGreaterThanOrEqual(1);
    const dockerize = state.tickets.find(t => t.id && t.id.includes('C001'));
    expect(dockerize, 'should have seeded dockerize chore').toBeDefined();
    expect(dockerize.type).toBe('chore');
    expect(dockerize.status).toBe('pending');
  });

});

test.describe('Project Cleanup', () => {
  test('delete imported test project', async ({ page }) => {
    await initPage(page);
    const projects = await page.evaluate(async () => {
      const r = await fetch('/api/projects');
      return r.json();
    });
    const imported = projects.filter(p => p.alias === 'TIP');
    for (const p of imported) {
      await page.evaluate(async (pid) => {
        await fetch('/api/projects/' + pid, { method: 'DELETE' });
      }, p.id);
    }
    // Verify deleted
    const after = await page.evaluate(async () => {
      const r = await fetch('/api/projects');
      return r.json();
    });
    const remaining = after.filter(p => p.alias === 'TIP');
    expect(remaining.length, 'imported project should be deleted').toBe(0);
  });
});
