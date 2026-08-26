// End-to-end coverage for the 2026-08-26 fixes (issues 3, 7, 8, 10, 12, 13 of
// the pdy-dev field report), driven against the REAL `iter` binary serving the
// .fixture project — same server users get, no mocks.
const { test, expect } = require('@playwright/test');

const row = (page, title) => page.locator('.item', { hasText: title }).first();

// Open a row's Actions menu and return the verbs it offers.
async function menuVerbs(page, title) {
  const item = row(page, title);
  await item.locator('[data-actions]').click();
  await page.waitForTimeout(150);
  return item.locator('.actmenu button').allTextContents();
}

test.describe('issue 10 — the embedded demo project never paints in LIVE mode', () => {
  test('the first paint shows a skeleton, never the demo dataset numbers', async ({ page }) => {
    // Hold the queue response open so the pre-fetch paint is observable at all:
    // on a fast local server it otherwise resolves before the first assertion,
    // which would make this pass for the wrong reason.
    let release;
    const held = new Promise((r) => { release = r; });
    await page.route('**/api/workitems', async (route) => {
      await held;
      await route.continue();
    });

    await page.goto('/#/workitems');
    await page.waitForSelector('.sumbtn');
    const first = await page.locator('.sumbtn.total .n').textContent();
    expect(first).not.toBe('17');            // the demo dataset's Total
    expect(first.trim()).not.toMatch(/^\d+$/); // no number at all until real data lands
    await expect(page.locator('#rows')).toContainText(/loading/i);

    release();
    await expect.poll(async () => page.locator('.sumbtn.total .n').textContent()).toMatch(/^\d+$/);
    expect(Number(await page.locator('.sumbtn.total .n').textContent())).toBeGreaterThan(0);
  });

  test('the demo project name never appears in the live header', async ({ page }) => {
    await page.goto('/#/workitems');
    await page.waitForTimeout(1200);
    // "Deploy greet 1.1.0 to the demo host" is a mock item; the greet sample is
    // the embedded demo project.
    await expect(page.locator('body')).not.toContainText('Deploy greet 1.1.0');
  });
});

test.describe('issue 7b — the three kinds of ToDo are visually distinct', () => {
  test('approval gate, guard park and broken config each get their own chip', async ({ page }) => {
    await page.goto('/#/workitems');
    await page.waitForTimeout(1200);

    await expect(row(page, 'E2E approval gate')).toContainText('awaiting your approval');
    await expect(row(page, 'E2E guard park')).toContainText('guard parked this');
    await expect(row(page, 'E2E broken config')).toContainText('broken configuration');

    // The three must not be interchangeable — a guard park is a judgment call,
    // an approval gate is a click.
    await expect(row(page, 'E2E guard park')).not.toContainText('awaiting your approval');
    await expect(row(page, 'E2E broken config')).not.toContainText('awaiting your approval');
  });

  test('a broken-config park shows the reason in the row, not only a tooltip', async ({ page }) => {
    await page.goto('/#/workitems');
    await page.waitForTimeout(1200);
    await row(page, 'E2E broken config').locator('[data-rowclick]').click();
    await page.waitForTimeout(300);
    // The path IS the fix — it has to be readable without hovering.
    await expect(row(page, 'E2E broken config')).toContainText('codepath does not exist');
  });
});

test.describe('issues 3a + 8 — closed items offer Clone and Delete, never a retry', () => {
  test('an archived row offers no verb that would 409', async ({ page }) => {
    await page.goto('/#/workitems');
    await page.waitForTimeout(1200);
    const verbs = (await menuVerbs(page, 'E2E closed failed')).join(' | ').toLowerCase();

    expect(verbs).toContain('clone');
    expect(verbs).toContain('delete');
    // The bug: these were offered on archived items and every click failed.
    expect(verbs).not.toContain('queue');
    expect(verbs).not.toContain('requeue');
    expect(verbs).not.toContain('retry');
    expect(verbs).not.toContain('move to todo');
    expect(verbs).not.toContain('pause');
  });

  test('deleting an archived item removes it from the archive for good', async ({ page }) => {
    await page.goto('/#/workitems');
    await page.waitForTimeout(1200);
    await expect(row(page, 'E2E closed failed')).toBeVisible();

    const item = row(page, 'E2E closed failed');
    await item.locator('[data-actions]').click();
    await page.waitForTimeout(150);
    await item.locator('.actmenu button', { hasText: /delete/i }).click();
    await page.waitForTimeout(300);

    // The confirm must say the removal is permanent — this is real history.
    await expect(page.locator('#lbBody')).toContainText(/permanent|no undo|history/i);
    await page.locator('#lbFoot button.danger').click();
    await page.waitForTimeout(1200);

    await expect(page.locator('.item', { hasText: 'E2E closed failed' })).toHaveCount(0);
    // And it is gone from the server, not just the DOM.
    const stillThere = await page.evaluate(async () => {
      const r = await fetch('/api/workitems').then((x) => x.json());
      return r.closed.some((i) => i.workid === 'e2e-closed-failed');
    });
    expect(stillThere).toBe(false);
  });
});

test.describe('issue 12 — a work item may name its own model', () => {
  test('an overriding item shows the model, and the form offers the choice', async ({ page }) => {
    await page.goto('/#/workitems');
    await page.waitForTimeout(1200);
    await expect(row(page, 'E2E cheap model')).toContainText('sonnet');

    await page.locator('#newBtn, [data-new], button', { hasText: /new workitem/i }).first().click();
    await page.waitForTimeout(400);
    const opts = await page.locator('#f_model option').allTextContents();
    expect(opts.join(' ')).toMatch(/sonnet/);
    expect(opts.join(' ')).toMatch(/default/i); // empty = the agent's own model
  });
});

test.describe('issue 13 — engine events patch the row in place', () => {
  test('a state change made outside the page arrives without a full refetch', async ({ page }) => {
    await page.goto('/#/workitems');
    await page.waitForTimeout(1200);
    await expect(row(page, 'E2E approval gate')).toContainText('ToDo');

    // Count whole-queue refetches from here on: the point of the delta is that
    // a one-field change costs zero of them.
    await page.evaluate(() => {
      window.__listFetches = 0;
      const real = window.fetch;
      window.fetch = (u, o) => {
        if (String(u).endsWith('/api/workitems')) window.__listFetches += 1;
        return real(u, o);
      };
    });

    // Change it server-side, the way the engine would.
    await page.evaluate(() =>
      fetch('/api/workitems/e2e-todo-approval/action', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ action: 'pause' }),
      })
    );

    await expect
      .poll(async () => row(page, 'E2E approval gate').textContent(), { timeout: 8000 })
      .toContain('Paused');
    expect(await page.evaluate(() => window.__listFetches)).toBe(0);
  });
});
