// End-to-end coverage for the Answer lightbox requirement
// (iter/reqs/answer-lightbox-requirement.md, filed 2026-08-28).
//
// The problem in one sentence: a work item's question can run to thousands of
// characters, and every write the engine makes to its database used to repaint
// the whole work-item list — which throws away the scroll position of whatever
// the reader was halfway through. These tests drive the REAL `iter` server
// against the .fixture project, using the two seeded question items from
// global-setup.js ("E2E long question" and "E2E answer and queue").
//
// A delta event is delivered here by calling the page's own applyDelta() — the
// exact function the EventSource handler calls when the server speaks. That
// makes "ten events arrived" deterministic instead of a race with the engine.
const { test, expect } = require('@playwright/test');
const fs = require('fs');
const path = require('path');

// The fixture the webServer serves (see playwright.config.js / global-setup.js).
// R3's test moves these two files' timestamps by hand — they are exactly the
// pair the server's change feed watches. The write-ahead log is checkpointed
// away whenever the last connection closes, so only the database itself is
// guaranteed to be there; both get touched when both exist.
const ENGINE = path.join(__dirname, '..', '.fixture', '.iter', '.engine');
const WATCHED = ['iter.db', 'iter.db-wal'].map((f) => path.join(ENGINE, f));

const LONG = 'E2E long question';
const SUBMIT = 'E2E answer and queue';

const row = (page, title) => page.locator('.item', { hasText: title }).first();

// This fixture's engine really runs: it picks queued items up, fails them
// against /usr/bin/false and retries, so genuine row deltas keep arriving and
// keep repainting the list. Both helpers below are written against that.

// The Actions menu is always in the DOM (the `open` class only reveals it), so
// its verbs are read in ONE synchronous pass inside the page — a repaint cannot
// land halfway through.
const menuVerbs = (page, title) => page.evaluate((t) => {
  const item = [...document.querySelectorAll('.item')].find((el) => el.textContent.includes(t));
  return item ? [...item.querySelectorAll('.actmenu button')].map((b) => b.textContent.trim()) : [];
}, title);

// Opening the menu and clicking an entry is one retried unit: a repaint between
// the two clicks takes the menu away, and the answer to that is to open it
// again, not to fail. The entry click is the last step, so a retry after it
// succeeded never happens.
async function menuClick(page, title, act) {
  const item = row(page, title);
  await expect(async () => {
    await item.locator('[data-actions]').click({ timeout: 2000 });
    await item.locator(`.actmenu [data-act="${act}"]`).click({ timeout: 1000 });
  }).toPass({ timeout: 20000 });
}

// Fire N delta events at the client exactly as the server's change feed does.
// An EMPTY delta is what the old server sent on every write-ahead-log commit —
// the churn this whole requirement is about.
async function fireDeltas(page, n = 10, changed = []) {
  for (let i = 0; i < n; i++) {
    await page.evaluate((c) => applyDelta({ type: 'delta', changed: c, removed: [], closed: [] }), changed);
    await page.waitForTimeout(30);
  }
}

// The page is ready when the seeded rows are on screen and their bodies can be
// fetched — every assertion below reads real question text.
async function ready(page) {
  await page.goto('/#/workitems');
  await expect(row(page, LONG)).toBeVisible();
}

test.describe('R1 — repaints hold while a lightbox is open', () => {
  // THE RED, recorded: this is the behavior the requirement was filed about,
  // and it is still exactly what an in-row question box does. If this test ever
  // starts failing, someone did R4 (in-row scroll restoration) and this control
  // should be rewritten rather than deleted.
  test('control: the IN-ROW question box loses its scroll on every delta', async ({ page }) => {
    await ready(page);
    await row(page, LONG).locator('[data-rowclick]').first().click();
    const qbox = row(page, LONG).locator('.textblock.question');
    await expect(qbox).toContainText('END OF QUESTION');

    await qbox.evaluate((el) => { el.scrollTop = el.scrollHeight; });
    expect(await qbox.evaluate((el) => el.scrollTop)).toBeGreaterThan(50);

    await fireDeltas(page, 3);
    // The rebuild replaced the element; the replacement starts at the top.
    expect(await qbox.evaluate((el) => el.scrollTop)).toBe(0);
  });

  test('ten deltas arrive and the list underneath is not rebuilt once', async ({ page }) => {
    await ready(page);
    // Expand the row first: an open row is the expensive case — a delta drops
    // its cached body, so the rebuild would repaint it as a one-line "Loading…"
    // placeholder, shorten the document, and drag the page scroll with it.
    await row(page, LONG).locator('[data-rowclick]').first().click();
    await expect(row(page, LONG).locator('.textblock.question')).toContainText('END OF QUESTION');
    await menuClick(page, LONG, 'answerlb');

    const qbox = page.locator('#lbBody [data-answerlbq]');
    await expect(qbox).toContainText('END OF QUESTION');
    await qbox.evaluate((el) => { el.scrollTop = el.scrollHeight; });
    const at = await qbox.evaluate((el) => el.scrollTop);
    expect(at).toBeGreaterThan(50);

    // Mark the live row nodes. renderRows() assigns #rows.innerHTML wholesale,
    // so a single repaint throws every marked node away — this is the direct
    // measure of "the paint was held", not a proxy for it.
    const marked = await page.evaluate(() => {
      const kids = [...document.getElementById('rows').children];
      kids.forEach((k, i) => { k.dataset.probe = String(i); });
      return kids.length;
    });
    expect(marked).toBeGreaterThan(0);
    const scrollBefore = await page.evaluate(() => window.scrollY);

    await fireDeltas(page, 10);

    expect(await page.locator('#rows [data-probe]').count(),
      'ten deltas, zero rebuilds of the list behind the modal').toBe(marked);
    expect(await page.evaluate(() => window.scrollY)).toBe(scrollBefore);
    await expect(page.locator('#overlay')).toHaveClass(/open/);
    await expect(qbox).toContainText('END OF QUESTION');
    expect(await qbox.evaluate((el) => el.scrollTop)).toBe(at);

    // …and closing lets the held repaint through, so the marks are gone.
    await page.locator('#lbFoot [data-lbclose]').click();
    await expect(page.locator('#rows [data-probe]')).toHaveCount(0);
  });

  test('closing replays the repaint the modal held', async ({ page }) => {
    await ready(page);
    await menuClick(page, LONG, 'answerlb');
    await expect(page.locator('#overlay')).toHaveClass(/open/);

    // A real row change lands while the modal is up: the DATA is patched, the
    // PAINT is not.
    await page.evaluate((title) => {
      const it = items.find((i) => i.title === title);
      applyDelta({ type: 'delta', changed: [{ ...it, title: 'E2E renamed behind the modal' }] });
    }, LONG);
    await expect(page.locator('#rows')).not.toContainText('E2E renamed behind the modal');

    await page.locator('#lbFoot [data-lbclose]').click();
    await expect(page.locator('#overlay')).not.toHaveClass(/open/);
    // Immediately — not on the next event.
    await expect(page.locator('#rows')).toContainText('E2E renamed behind the modal');
  });

  test('Pause & Edit opens under delta load instead of bailing with "Still loading"',
    async ({ page }) => {
      await ready(page);
      // Pause & Edit reloads the whole queue, which turns every row back into a
      // summary; concurrent refreshes used to make openForm's summary guard win
      // the race and the edit form never appeared.
      await menuClick(page, 'E2E cheap model', 'pauseedit');
      await expect(page.locator('#lbTitle')).toContainText('Edit WorkItem');
      await fireDeltas(page, 5);
      await expect(page.locator('#lbTitle')).toContainText('Edit WorkItem');
      await expect(page.locator('#f_title')).toHaveValue('E2E cheap model');
      await expect(page.locator('#toast')).not.toContainText('Still loading');
      await page.keyboard.press('Escape');
    });
});

test.describe('R2 — the Answer lightbox', () => {
  test('"Answer…" is the first entry for an item with an unanswered question', async ({ page }) => {
    await ready(page);
    const verbs = await menuVerbs(page, LONG);
    expect(verbs[0]).toBe('Answer…');
    expect(verbs).toContain('Queue without answering');

    // An item with no question never offers it.
    const plain = await menuVerbs(page, 'E2E approval gate');
    expect(plain).not.toContain('Answer…');
  });

  test('it shows the whole question, the asked stamp and a live answer box', async ({ page }) => {
    await ready(page);
    await menuClick(page, LONG, 'answerlb');

    await expect(page.locator('#lbTitle')).toContainText(LONG);
    await expect(page.locator('#lbBody')).toContainText('LINE 001');
    await expect(page.locator('#lbBody')).toContainText('END OF QUESTION');
    await expect(page.locator('#lbBody')).toContainText('asked');
    await expect(page.locator('#lbBody .answerbox')).toBeVisible();
    await expect(page.locator('#lbFoot')).toContainText('Answer and Queue');

    // The question keeps its own scroll box, smaller than the row's 60vh, so
    // the answer area stays on screen with it.
    const cap = await page.locator('#lbBody [data-answerlbq]')
      .evaluate((el) => getComputedStyle(el).maxHeight);
    const vh = await page.evaluate(() => window.innerHeight);
    expect(parseFloat(cap)).toBeLessThan(vh * 0.5);
  });

  test('the draft autosaves, survives a close, and keeps both status lines in step',
    async ({ page }) => {
      await ready(page);
      // Expand the row too — now TWO boxes on the page carry the same
      // data-answer, and the row's comes first in document order.
      await row(page, LONG).locator('[data-rowclick]').first().click();
      await expect(row(page, LONG).locator('.answerbox')).toBeVisible();

      await menuClick(page, LONG, 'answerlb');
      await page.locator('#lbBody .answerbox').fill('half an answer');

      // Both status lines — the modal's and the row's underneath.
      const stats = page.locator('[data-astat]');
      await expect(stats).toHaveCount(2);
      for (let i = 0; i < 2; i++) await expect(stats.nth(i)).toContainText('unsaved');

      await page.keyboard.press('Escape');
      await expect(page.locator('#overlay')).not.toHaveClass(/open/);

      // Reopening finds the draft; so does the row's own box.
      await menuClick(page, LONG, 'answerlb');
      await expect(page.locator('#lbBody .answerbox')).toHaveValue('half an answer');
      await page.keyboard.press('Escape');
      await expect(row(page, LONG).locator('.answerbox')).toHaveValue('half an answer');
    });

  test('Answer and Queue submits the MODAL text and moves the item', async ({ page }) => {
    await ready(page);
    // The row's box is filled with something else first: whichever text lands
    // on the server proves which box was read.
    await row(page, SUBMIT).locator('[data-rowclick]').first().click();
    await row(page, SUBMIT).locator('.answerbox').fill('stale row copy');
    await row(page, SUBMIT).locator('.answerbox').blur();

    await menuClick(page, SUBMIT, 'answerlb');
    await page.locator('#lbBody .answerbox').fill('hourly, and log the skew');
    await page.locator('#lbAnswerQueue').click();

    await expect(page.locator('#overlay')).not.toHaveClass(/open/);
    await expect(page.locator('#toast')).toContainText('queued');
    await expect(row(page, SUBMIT)).toContainText('Queued');

    const stored = await page.evaluate(async () =>
      (await (await fetch('/api/workitems/e2e-answer-submit')).json()).item);
    expect(stored.answer).toBe('hourly, and log the skew');
    expect(stored.state).toBe('queued');

    // A queued item is no longer answerable, so the lightbox is the record now.
    const verbs = await menuVerbs(page, SUBMIT);
    expect(verbs).not.toContain('Answer…');
  });
});

test.describe('R3 — the server stops emitting empty deltas', () => {
  test('storage churn with no row behind it ships nothing; a real change still ships fast',
    async ({ page }) => {
      await ready(page);
      // Listen on the change feed the way the app does, recording every data:
      // frame. The keepalive is a bare comment line and never arrives as one.
      await page.evaluate(() => {
        window.__sse = [];
        window.__es = new EventSource('/api/events');
        window.__es.onmessage = (ev) => window.__sse.push(ev.data);
      });
      await page.waitForTimeout(1500);   // let the connection settle

      // Phase 1 — the churn this requirement is about. An agent heartbeat, a
      // log row or an attempt counter lands in iter.db-wal and moves that file
      // without moving any row a reader can see. Bumping its timestamp is that
      // event exactly, and the server must decide it is not worth telling
      // anyone about.
      expect(fs.existsSync(WATCHED[0]), `no database at ${WATCHED[0]}`).toBe(true);
      const quietStart = await page.evaluate(() => window.__sse.length);
      for (let i = 0; i < 6; i++) {
        const t = new Date();
        for (const f of WATCHED) if (fs.existsSync(f)) fs.utimesSync(f, t, t);
        await page.waitForTimeout(800);   // longer than the server's 700ms poll
      }
      // "Empty" is measured exactly as the server measures it: no row in any of
      // the three lists AND a header tally identical to the previous frame the
      // client received. A counts-only move is a real change and may ship.
      const all = await page.evaluate(() => window.__sse.map((x) => JSON.parse(x)));
      const hollow = all.filter((m, i) => i >= quietStart && m.type === 'delta'
        && !(m.changed || []).length && !(m.removed || []).length && !(m.closed || []).length
        && (i === 0 || JSON.stringify(m.counts) === JSON.stringify(all[i - 1].counts)));
      expect(hollow, `six storage touches produced ${hollow.length} empty deltas`)
        .toHaveLength(0);

      // Phase 2 — the half that must still work. A field the row actually
      // paints (the R: chip), on an item no other test reads by name.
      const before = await page.evaluate(() => window.__sse.length);
      const at = Date.now();
      await page.evaluate(() =>
        fetch('/api/workitems/e2e-long-question',
          { method: 'PATCH', body: JSON.stringify({ risk: 7 }) }));
      await expect.poll(
        async () => page.evaluate((n) => window.__sse.slice(n).some((x) => x.includes('e2e-long-question')), before),
        { timeout: 4000 }).toBe(true);
      expect(Date.now() - at, 'a visible change should not wait').toBeLessThan(3000);

      await page.evaluate(() => window.__es.close());
    });
});
