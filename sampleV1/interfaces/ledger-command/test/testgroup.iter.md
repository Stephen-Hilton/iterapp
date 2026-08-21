# ledger-command — test groups

The contract lists eight worked examples and calls them normative. This group
takes that literally: it replays each one against the real provider, the Command
Parser, and fails when the two disagree. That is what keeps the contract from
decaying into a description of what the code used to do.

<!-- iterapp:testgroups
{"label":"ledger-command contract","desc":"All eight worked examples replayed against the real provider, plus the closed-vocabulary invariant on refusal codes.","auto_fix":false,"lastrun":"2026-08-21T22:13:52Z","result":"passed","counts":"14/14","testlist":[{"id":"t1","name":"worked examples","desc":"every normative request/reply pair holds against ledger/cli/parse/parse.sh, and no refusal names a code outside the contract","shell":"t1-worked-examples.sh"}]}
-->
