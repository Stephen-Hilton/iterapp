// Playwright config for the iterapp webapp.
//
// globalSetup builds a throwaway fixture project (a copy of ../sampleV1 plus a
// deliberately-orphaned code node file) and the webServer runs the real `iter`
// binary against it — the same server users get, no mocks.
const { defineConfig } = require('@playwright/test');
const path = require('path');

const PORT = 9979;
const FIXTURE = path.join(__dirname, '.fixture');

module.exports = defineConfig({
  testDir: './tests',
  fullyParallel: false, // one shared server; teststate tests mutate the fixture
  workers: 1,
  retries: 0,
  timeout: 30000,
  use: {
    baseURL: `http://localhost:${PORT}`,
    trace: 'retain-on-failure',
  },
  webServer: {
    // Fixture prep runs INSIDE the server command: Playwright may start the
    // webServer before/alongside globalSetup, and rebuilding the fixture
    // after the server registered would wipe its registry row.
    command: `node global-setup.js && ../target/debug/iter start --project ${FIXTURE} --port ${PORT}`,
    port: PORT,
    reuseExistingServer: false,
    env: {
      // Playwright REPLACES the env with this object — keep the parent env
      // (PATH included) and override the isolation knobs: ~ (server registry,
      // usage snapshot) points into the fixture, and any picked-up agent item
      // fails fast instead of burning tokens.
      ...process.env,
      HOME: FIXTURE,
      ITER_CLAUDE_BIN: '/usr/bin/false',
    },
  },
});
