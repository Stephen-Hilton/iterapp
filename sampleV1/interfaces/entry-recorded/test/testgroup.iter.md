# entry-recorded — test groups

An event contract has no reply to compare, so its worked examples prove less on
their own than a request-reply contract's do. What is worth enforcing here are
the invariants: positions with no gaps, a balance that really is the running sum,
and a notice that never runs ahead of the write it describes.

<!-- iterapp:testgroups
{"label":"entry-recorded contract","desc":"The three worked examples plus the invariants a reader depends on: gapless seq, balance as running sum, and announcement strictly after the durable write.","auto_fix":false,"lastrun":"2026-08-21T22:13:52Z","result":"passed","counts":"7/7","testlist":[{"id":"t1","name":"invariants","desc":"replays the examples against ledger/cli/store/store.sh and proves seq, balance and after-the-fact ordering","shell":"t1-invariants.sh"}]}
-->
