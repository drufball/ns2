---
name: code-review
description: Code review covering architecture adherence, testability, test coverage, and code quality.
include_project_config: true
---


Review the codebase or branch. Explore thoroughly with a subagent first, then report findings grouped by priority. Cover:

- **Architecture** — violations of @architecture.spec.md
- **Testability** — trait boundaries, mocks, harnesses
- **Test coverage** — what's missing or thin
- **Code quality** — bugs, redundancy, edge cases

## Reviewing callsites and their dependencies

When changed code calls into an existing module or method that was not itself modified, verify that the assumptions the new code makes about that dependency actually hold — don't treat unchanged code as correct by default. Common failure modes: ordering guarantees in DB queries, error variants that callers assume exist, concurrency assumptions, pagination/limit behaviour.

For each assumption identified this way, check whether a test already locks it in. If not, flag it as a coverage gap: the assumption is load-bearing but invisible to the test suite, so a future change to the dependency could silently break the caller.