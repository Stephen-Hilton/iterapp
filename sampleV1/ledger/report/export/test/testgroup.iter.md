# CSV Exporter — test groups

**This object is parked out of the test sweep** — its marker carries
`test_loop: omit`. The group below is written down anyway, because the gap is
worth being explicit about: the claim that matters here is "a spreadsheet opens
this file and shows the right numbers", and nothing on a bare machine (TR-1) can
check that claim honestly. A group of tests that only re-checked the string
formatting would look like coverage while proving nothing about the requirement.

The testlist is deliberately empty. Bringing the object back in is
`iter testloop --include export` once there is a real way to open the file.

<!-- iterapp:testgroups
{"label":"csv export","desc":"BR-5: the exported file opens in a spreadsheet and shows every movement plus the total. Needs a spreadsheet program to check honestly, which TR-1 rules out — the object is parked with test_loop: omit until that changes.","auto_fix":false,"lastrun":"","result":"","counts":"","testlist":[]}
-->
