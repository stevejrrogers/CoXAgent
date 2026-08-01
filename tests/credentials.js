// The one place the e2e suite learns who to sign in as.
//
// COX-B030: every spec used to carry the literal admin password of a real,
// running hub. That made the test files a second copy of the secret — as
// readable to anyone with the repo as the credential in deploy/.env was, and
// just as easy to forget when rotating. The credential now comes from the
// environment, so rotating the hub means exporting a new value, not editing
// six files, and a clone of this repo grants nobody a login.
//
// Run the suite against a hub with:
//   COXAGENT_ADMIN_PASSWORD='…' npx playwright test
// using the same value that hub was started with (docker compose reads it from
// deploy/.env; the macOS app keeps it in ~/CoXAgent/admin-password).

/// Fail at collection with an actionable message rather than at the first
/// login with a bare 401 — an empty password is a setup mistake, not a bug in
/// the code under test, and the two should not look alike.
function required(name) {
  const value = process.env[name];
  if (!value) {
    throw new Error(
      `${name} is not set. The e2e suite signs in to a real hub and no ` +
        `credential is committed to this repo (COX-B030). Export ${name} ` +
        `with the password that hub was started with, then re-run:\n\n` +
        `  ${name}='…' npx playwright test\n`
    );
  }
  return value;
}

// The username is configuration, not a secret — the hub's default admin is
// `root` and several specs assert the badge shows it.
const ADMIN_USER = process.env.COXAGENT_ADMIN_USER || 'root';
const ADMIN_PASSWORD = required('COXAGENT_ADMIN_PASSWORD');

module.exports = { ADMIN_USER, ADMIN_PASSWORD };
