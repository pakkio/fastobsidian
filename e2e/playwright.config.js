// Browser tests: a copy of sample-vault served by `fastobsidian --serve`, driven in Chromium and WebKit
// (WebKit is the engine of the app window on macOS and Linux).
//
//   npx playwright test                         # both browsers (after `npx playwright install`)
//   PW_CHANNEL=msedge npx playwright test --project=chromium   # use the installed Edge, no download
//
// FO_BIN points at a built fastobsidian binary; by default the debug build is used.
const { defineConfig } = require('@playwright/test');
const fs = require('fs');
const os = require('os');
const path = require('path');

const root = path.resolve(__dirname, '..');
const exe = process.platform === 'win32' ? 'fastobsidian.exe' : 'fastobsidian';
const bin = process.env.FO_BIN || path.join(root, 'target', 'debug', exe);
const port = Number(process.env.FO_PORT || 8790);

// a fresh copy of the sample vault per run: the tests create notes and attachments in it
if (!process.env.FO_VAULT) {
  const vault = fs.mkdtempSync(path.join(os.tmpdir(), 'fo-e2e-'));
  fs.cpSync(path.join(root, 'sample-vault'), vault, { recursive: true });
  process.env.FO_VAULT = vault;
}

module.exports = defineConfig({
  testDir: './tests',
  workers: 1, // one server and one vault
  timeout: 30_000,
  expect: { timeout: 5_000 },
  reporter: process.env.CI ? [['list'], ['html', { open: 'never' }]] : 'list',
  use: { baseURL: `http://127.0.0.1:${port}`, trace: 'retain-on-failure' },
  projects: [
    { name: 'chromium', use: { browserName: 'chromium', channel: process.env.PW_CHANNEL || undefined } },
    { name: 'webkit', use: { browserName: 'webkit' } },
  ],
  webServer: {
    command: `"${bin}" "${process.env.FO_VAULT}" --serve --port ${port} --no-cache`,
    url: `http://127.0.0.1:${port}/`,
    reuseExistingServer: false,
    timeout: 60_000,
  },
});
