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
  // Keep the queue deterministic: pre-flag the Orphanage-review schedule.
  fs.writeFileSync(path.join(dst, '.iter', '.engine', 'orphan_schedule_seeded'), 'e2e');
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
