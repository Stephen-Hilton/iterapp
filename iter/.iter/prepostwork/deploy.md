# Postwork step: deploy

Placeholder deploy step.

Real deployments will use the paired `.md` + `.sh` pattern once `.sh` prepostwork steps
land: this file will hold the nondeterministic judgment (verify the build is green,
choose the target, sanity-check the version), and its output will feed a `deploy.sh`
that executes deterministically. Until then:

1. Verify the relevant test groups pass (read the `iterapp:testgroups` blocks — do not
   re-run tests here).
2. Report `DEPLOY-SKIPPED: no deploy target configured` along with the version/commit
   that WOULD have been deployed.
3. Never attempt an actual deployment from this placeholder.
