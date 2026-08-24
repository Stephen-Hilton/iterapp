// End-to-end tests for the iterapp webapp against the REAL `iter start`
// server, serving the .fixture project (sampleV1 in structureV2 form plus one
// deliberate orphan — see global-setup.js).
const { test, expect } = require('@playwright/test');

test.describe('workitems view', () => {
  test('loads the queue with state columns and seeded items', async ({ page }) => {
    await page.goto('/#/workitems');
    // Seeded queue: user items + sweep-born testwriter items + Test Loop template.
    await expect(page.locator('body')).toContainText('Test Loop');
    await expect(page.locator('body')).toContainText('queued');
  });
});

test.describe('projects view (the structureV2 DAG)', () => {
  test('renders the linked code node tree with levels', async ({ page }) => {
    await page.goto('/#/projects');
    await expect(page.locator('body')).toContainText('Ledger System');
    await expect(page.locator('body')).toContainText('Command Parser');
    // Level legend (V2: context | container | component — no "project" level).
    await expect(page.locator('.lvlchip').first()).toContainText('context');
    await expect(page.locator('body')).not.toContainText('level: project');
  });

  test('interfaces and use-cases render as global objects', async ({ page }) => {
    await page.goto('/#/projects');
    await expect(page.locator('body')).toContainText('ledger-command');
    await expect(page.locator('body')).toContainText('entry-recorded');
    await expect(page.locator('body')).toContainText('Record a movement');
    // Use-cases link code via children.codenodes now.
    await expect(page.locator('body')).toContainText('code node');
  });

  test('node detail shows V2 fields and teststate controls', async ({ page }) => {
    await page.goto('/#/projects');
    await page.locator('.prow', { hasText: 'Command Parser' }).first().click();
    await expect(page.locator('.pdetail').first()).toContainText('Node file');
    await expect(page.locator('.pdetail').first()).toContainText('CodeDirs');
    await expect(page.locator('.pdetail').first()).toContainText('Inputs / Outputs');
    await expect(page.locator('[data-tlsel]').first()).toBeVisible();
    // "View Node File" shows the REAL file — the children mapping included,
    // never a synthesized frontmatter fragment (the V1 lightbox regression).
    await page.locator('[data-viewmarker]').first().click();
    await expect(page.locator('#lbBody .textblock')).toContainText('children:');
    await expect(page.locator('#lbBody .textblock')).toContainText('testgroups:');
    await page.keyboard.press('Escape');
  });

  test('the Orphanage lists the stray node and links it into a parent', async ({ page }) => {
    await page.goto('/#/projects');
    await expect(page.locator('body')).toContainText('Orphanage');
    const orphanRow = page.locator('.prow', { hasText: 'stray.code.iter.md' });
    await expect(orphanRow).toBeVisible();
    // Pick the Ledger System context as the parent and link.
    await orphanRow.locator('select').selectOption({ label: 'context: Ledger System (ledger)' });
    await orphanRow.locator('[data-orphanlink]').click();
    // The row disappears on rescan; the node joins the tree under ledger.
    await expect(page.locator('.prow', { hasText: 'stray.code.iter.md' })).toHaveCount(0);
    await expect(page.locator('body')).toContainText('Stray Component');
  });

  test('the teststate selector round-trips every state, block lift included', async ({ page }) => {
    await page.goto('/#/projects');
    await page.locator('.prow', { hasText: 'Entry Store' }).first().click();
    const sel = () => page.locator('[data-tlsel]').first();
    await sel().selectOption('omit');
    await expect(page.locator('body')).toContainText('teststate → omit');
    await expect(sel()).toHaveValue('omit');
    await sel().selectOption('include');
    await expect(page.locator('body')).toContainText('teststate → include');
    // Back to the default — no separate "clear flag" button anymore.
    await sel().selectOption('inherit');
    await expect(page.locator('body')).toContainText('teststate → inherit');
    // Block is agent-proof, NOT human-proof: the selector lifts it.
    await sel().selectOption('block');
    await expect(page.locator('body')).toContainText('teststate → block');
    await expect(sel()).toBeEnabled();
    await sel().selectOption('inherit');
    await expect(page.locator('body')).toContainText('teststate → inherit');
  });
});

test.describe('settings view (the two head files)', () => {
  test('renders head-file settings with live values', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('[data-ps="projectname"]')).toHaveValue('Sample Ledger');
    await expect(page.locator('[data-ps="iterglob"]')).toHaveValue('**/*.iter.md');
    await expect(page.locator('[data-ps="topdir"]')).toBeVisible();
    // The retired V1 settings are gone.
    await expect(page.locator('[data-ps="marker_glob"]')).toHaveCount(0);
    await expect(page.locator('[data-cfg="globalsettings.global_bizreq_path"]')).toHaveCount(0);
  });

  test('saving the project half lands in main.iter.md frontmatter', async ({ page }) => {
    await page.goto('/#/settings');
    await page.locator('[data-ps="projectdescription"]').fill('edited by playwright');
    await page.locator('#cfgSave').click();
    await expect(page.locator('body')).toContainText('Saved');
    await page.reload();
    await page.goto('/#/settings');
    await expect(page.locator('[data-ps="projectdescription"]')).toHaveValue('edited by playwright');
  });
});

test.describe('servers list', () => {
  test('the running-servers nav lists this server', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('#serverSwitch .srv.here')).toBeVisible();
    await expect(page.locator('#serverSwitch')).toContainText('sample-ledger');
  });
});
