# Record a movement — end-to-end tests

Journey tests, not component tests: everything here goes through the CLI the way
a person does, and nothing reaches into a part directly. When one of these turns
red it means the person's experience broke, whichever component actually caused
it — which is why fixing them is scoped wide and reviewed rather than automated.

<!-- iterapp:testgroups
{"label":"record a movement E2E","desc":"The whole journey through the CLI: a movement is written down, the confirmation names the new total, the report reads back in order, and a refused request leaves nothing behind.","auto_fix":false,"lastrun":"2026-08-21T22:13:52Z","result":"passed","counts":"6/6","testlist":[{"id":"t1","name":"journey","desc":"walks the happy path and the refusal fork end to end, touching only the CLI","shell":"t1-journey.sh"}]}
-->
