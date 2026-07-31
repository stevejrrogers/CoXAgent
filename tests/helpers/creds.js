// Admin login for e2e specs. Same env vars the server itself reads
// (COXAGENT_ADMIN_USER / COXAGENT_ADMIN_PASSWORD) — set them to whatever the
// test server was booted with before running these specs.
const ADMIN_USER = process.env.COXAGENT_ADMIN_USER || 'root';
const ADMIN_PASS = process.env.COXAGENT_ADMIN_PASSWORD;

if (!ADMIN_PASS) {
  throw new Error(
    'COXAGENT_ADMIN_PASSWORD is not set — export the admin password the ' +
    'target server was booted with before running these specs.'
  );
}

module.exports = { ADMIN_USER, ADMIN_PASS };
