// Build the fixture project the webServer serves: a fresh copy of sampleV1
// (already structureV2 format) plus one deliberately-orphaned code node file,
// so the Orphanage UI has something real to link.
const fs = require('fs');
const path = require('path');

const prepare = async () => {
  const src = path.join(__dirname, '..', 'sampleV1');
  const dst = path.join(__dirname, '.fixture');
  fs.rmSync(dst, { recursive: true, force: true });
  fs.cpSync(src, dst, { recursive: true });
  // Storage is SQLite now, and the engine imports the jsonl seed only when the
  // database is EMPTY. A database copied in from sampleV1 (left by any earlier
  // run) would make the seeded rows below invisible, so the fixture always
  // starts without one — the jsonl files are this fixture's source of truth.
  for (const f of ['iter.db', 'iter.db-wal', 'iter.db-shm', 'queue.meta.json']) {
    fs.rmSync(path.join(dst, '.iter', '.engine', f), { force: true });
  }
  for (const f of ['workitems.jsonl', 'workitems_closed.jsonl']) {
    const imported = path.join(dst, '.iter', '.engine', f + '.imported');
    if (fs.existsSync(imported)) fs.renameSync(imported, path.join(dst, '.iter', '.engine', f));
  }
  // Keep the queue deterministic: pre-flag the Orphanage-review schedule.
  fs.writeFileSync(path.join(dst, '.iter', '.engine', 'orphan_schedule_seeded'), 'e2e');
  fs.writeFileSync(path.join(dst, '.iter', '.engine', 'tempsweep_schedule_seeded'), 'e2e');
  // Work items covering the states the 2026-08-26 fixes introduced: the three
  // kinds of `todo` (issue 7b), a per-item model override, and a CLOSED failed
  // item whose row must offer Clone/Delete and never a retry (issues 3a + 8).
  const queue = path.join(dst, '.iter', '.engine', 'workitems.jsonl');
  const mk = (o) => JSON.stringify(Object.assign({
    type: 'code', source: 'user', priority: 5, risk: 0, codepath: '.', mainwork: 'm',
    times: { added: '2026-08-26T09:00:00Z' },
  }, o));
  fs.appendFileSync(queue, [
    mk({ workid: 'e2e-todo-approval', title: 'E2E approval gate', state: 'todo' }),
    mk({ workid: 'e2e-todo-guard', title: 'E2E guard park', state: 'todo',
         todo_reason: 'guard', lasterror: 'DEPENDENCY FAILED: upstream closed failed' }),
    mk({ workid: 'e2e-todo-config', title: 'E2E broken config', state: 'todo',
         todo_reason: 'config', lasterror: 'CONFIGURATION ERROR — codepath does not exist: /gone' }),
    mk({ workid: 'e2e-model-override', title: 'E2E cheap model', state: 'paused', model: 'sonnet' }),
  ].join('\n') + '\n');
  fs.appendFileSync(path.join(dst, '.iter', '.engine', 'workitems_closed.jsonl'),
    mk({ workid: 'e2e-closed-failed', title: 'E2E closed failed', state: 'failed', attempts: 50,
         lasterror: 'attempts exhausted',
         times: { added: '2026-08-20T09:00:00Z', closed: '2026-08-25T13:16:04Z' } }) + '\n');

  // The orphan: a valid V2 code node no children link claims.
  const orphanDir = path.join(dst, 'stray');
  fs.mkdirSync(orphanDir, { recursive: true });
  fs.writeFileSync(
    path.join(orphanDir, 'stray.code.iter.md'),
    '---\nname: "Stray Component"\nlevel: component\ndescription: "an unlinked node for the Orphanage e2e"\nowner: bespoke\nchildren:\n  bizreqs: []\n---\n\n# Long Description\nA component nothing links yet.\n'
  );
};

module.exports = prepare;
if (require.main === module) prepare();
