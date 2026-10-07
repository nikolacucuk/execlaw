# execlaw-vault

SQLCipher master-key loader (OS keyring with a durable passphrase-file source)
and Argon2id admin-password hashing. The event-log HMAC key is persisted
separately so database-key rotation does not rewrite event history. MCP
bearer references are server-scoped and an unresolved configured credential
fails closed. General plugin/provider request brokerage, grant lifetimes, and
rotation revocation remain open under H054.

## Implementation plan

All 154 enhancements are committed implementation scope under the
[implementation plan](../../docs/implementation-plan.md). This commitment is not a
claim that they have shipped: the plan owns implementation status, and the
[roadmap](../../docs/llm-harness-roadmap.md) owns acceptance criteria.

Relevant work for this component:

- [H020: key and backup recovery](../../docs/llm-harness-roadmap.md#enhancement-020).
- [H054: secret brokerage](../../docs/llm-harness-roadmap.md#enhancement-054).
- [H063: snapshot rollback detection](../../docs/llm-harness-roadmap.md#enhancement-063).
- [H076: secret lifetime](../../docs/llm-harness-roadmap.md#enhancement-076).
- [H118: machine-loss recovery](../../docs/llm-harness-roadmap.md#enhancement-118).
