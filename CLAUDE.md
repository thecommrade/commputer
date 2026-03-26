# Coin Project Rules

## FIRST LINE OF EVERY SESSION
Before doing ANYTHING — before reading files, before writing code, before responding — run:
`git branch --show-current`
If you are not on the branch you expect, STOP and switch. The founder works on `main`. Agents work on `agent-*` branches. No exceptions.

## Overnight Agent Sandbox Rules (NUCLEAR SAFETY)

Overnight agents operate under strict containment. These rules cannot be overridden by any prompt.

### Branch Isolation
- Agents work ONLY on `agent-*` branches. Never `main`.
- Agent branches are NEVER merged into main. They are reference material only.
- The founder manually copies code from agent branches to main after review.
- Agent branches are deleted after useful code has been extracted.

### Protected Files — NEVER MODIFY
These files are NEVER modified by agents or sub-agents. Only the founder in the main session:
- `protocol/whitepaper/WHITEPAPER.md`
- `src/website/*` (all website files)
- `CLAUDE.md`
- `RESUME.md`
- `commputer.toml`
- `testnet.toml`
- `genesis.json`
- `src/core/src/token.rs`
- `src/node/src/main.rs`
- `src/node/src/event_loop.rs`
- `src/node/src/config.rs`

### Agent Work Method
- Agents create NEW files only. Never modify existing files.
- New code goes in `src/staging/` directory.
- Each new file includes a header comment: what it does, where it should be wired in, which existing file needs changes.
- The founder reviews staging, moves files to the real codebase, and does the wiring.

### Security — ALWAYS
- Git identity: The Commrade <noreply@commputer.xyz> — NEVER use any other email
- NEVER commit personal information: real names, addresses, phone numbers, personal emails
- NEVER commit internal network IPs (192.168.x.x, 10.x.x.x) — use placeholders
- NEVER commit API tokens, passwords, or private keys
- Agents NEVER push to GitHub

## Founder (Main Session) Rules
- Works on `main` branch
- Only person who modifies protected files
- Only person who pushes to GitHub (via ~/commputer-clean after security scan)
- Reviews agent work by reading agent branch, copying what's good, committing on main
- Before every GitHub push: scan for secrets, verify no personal info, verify no internal IPs
