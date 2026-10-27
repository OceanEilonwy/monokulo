# The push audit's decisions

Made without the owner, who asked (2026-10-27) for a module-by-module audit of the Bell and relay build, coordinated through a SQLite database as in earlier reviews.

1. **Scope is the stack's diff.** Audited: every Rust file added or changed by monokulo 8c7d902e609b358caaff2e30916a170f1c647c3d..3304e560b (the head of PR #248, which stacks #245-#247) and monokular ca5e54018702ed8debb87112a63d3979f5d6e048..99a0c3b (PR #14). Unchanged code is read only as context.
2. **Changed files are reviewed by their diff.** A file the stack only touched is sized by its changed lines (added + removed), so a 1,000-line settings file with a 40-line change doesn't take a whole review.
3. **Smaller slices.** 1,500 lines per review job instead of 4,000: about 15,700 lines of non-test code, much of it cryptography and delivery, gets a deeper read per job.
4. **Its own database and tooling copy.** target/code-audit-push/audit.db in this worktree, on branch audit-push, apart from the workspace audit's database in the main checkout. The tooling is that audit's (branch worktree-rust-audit) with two-repository inventory, push-specific themes and this scope.
5. **Audit only.** Findings are recorded and each one is verified by a different agent. Nothing is fixed: the PRs stay drafts for the owner, and fixes follow as their own step once the owner has read the report, as with the workspace audit.
6. **Themes.** Web Push crypto, privacy, authz and abuse, delivery semantics, concurrency, type safety, dead code and wiring, error handling, house rules, test quality and design conformance. They start once every module review is done.
