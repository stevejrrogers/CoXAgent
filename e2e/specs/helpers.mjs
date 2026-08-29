// Shared helpers: console-error gate + navigation that waits for the app.
import { expect } from '@playwright/test';

/// Fail the test on any console error or page crash — a refactor that keeps
/// pixels but breaks a module load shows up here, not in the screenshot.
export function armConsoleGate(page, errors) {
  page.on('console', (msg) => {
    if (msg.type() === 'error') errors.push(msg.text());
  });
  page.on('pageerror', (err) => errors.push(String(err)));
}

export async function assertNoConsoleErrors(errors) {
  expect(errors, `console errors:\n${errors.join('\n')}`).toEqual([]);
}

/// Open the app and wait until it has painted real content.
export async function openApp(page) {
  // Pin a runnable engine before first paint. On load, CoXAgent asks
  // /api/engines what agent CLIs this machine has; a bare runner (Linux CI,
  // this repo's docker images) has none, which pops the "Connect a coding
  // agent" modal over every click and blocks the whole suite. Screenshots must
  // not depend on which host renders them, so report one engine: wizard
  // suppressed, pixels identical on darwin and linux alike.
  await page.route('**/api/engines', (route) =>
    route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify([
        { name: 'opencode', path: '/usr/local/bin/opencode', where: 'hub' },
      ]),
    }),
  );
  // Same determinism rule for the go-live preflight (CXA-F239): its line
  // items probe THIS host (docker, free ports, accounts), so the Overview
  // panel would otherwise render differently per machine. Freeze it to a
  // healthy report; the real endpoint is covered by the Rust contract tests.
  await page.route('**/api/projects/*/preflight', (route) =>
    route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        ready: true,
        blocking: [],
        items: [
          { id: 'config', label: 'coxagent.json', status: 'ok', detail: 'parses; policy gates intact' },
          { id: 'engine.default', label: 'Engine · default', status: 'ok', detail: 'opencode · mock/model ready' },
          { id: 'host_port', label: 'Deploy host_port', status: 'ok', detail: 'assigned to 8101' },
          { id: 'publish', label: 'Publish port', status: 'ok', detail: '8101 is free to publish on this host' },
          { id: 'auth', label: 'Auth & accounts', status: 'ok', detail: 'RBAC enabled — 1 account(s) provisioned' },
          { id: 'docker', label: 'Docker', status: 'ok', detail: 'docker CLI present, daemon up' },
          { id: 'compose', label: 'Docker compose', status: 'ok', detail: 'docker compose answers (CLI + plugin present)' },
        ],
      }),
    }),
  );
  await page.goto('/');
  await page.waitForLoadState('networkidle');
}
