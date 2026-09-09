import { defineConfig, devices } from '@playwright/test';

export default defineConfig({
  testDir: '.',
  timeout: 30000,
  reporter: [['list']],
  use: {
    ...devices['Desktop Chrome'],
    headless: true,
  },
  // Start the mock koda server the specs talk to. Without this every test
  // fails with ECONNREFUSED on 8790, which reads like a broken UI rather than
  // a forgotten `python3 mock_webui_server.py 8790`.
  webServer: {
    command: 'python3 mock_webui_server.py 8790',
    url: 'http://127.0.0.1:8790/',
    cwd: import.meta.dirname,
    reuseExistingServer: true,
    timeout: 20000,
  },
});
