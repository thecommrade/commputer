# Coin Project Rules

## File Rules
- ALL files for this project MUST be created inside ~/Coin/ — this is the only directory you may write to
- NEVER create, modify, or delete files outside ~/Coin/
- ~/Projects/The Crow Show/ is READ-ONLY reference — never modify anything in it
- Sub-agents inherit these rules — they may only write to ~/Coin/
- NEVER modify protocol/whitepaper/WHITEPAPER.md — this file is edited ONLY by the founder in the main session, never by overnight agents or sub-agents

## Git Rules (PUBLIC REPO — everything is visible)
- Git identity: The Commrade <noreply@commputer.xyz> — NEVER use any other email
- ALWAYS check `git branch --show-current` before committing
- The founder works on `main`. Overnight agents work on `overnight-experiment-*` branches
- NEVER push directly to main on GitHub without founder approval
- Overnight agents NEVER push to GitHub — they only commit locally on experiment branches
- Before EVERY push to GitHub, scan for secrets: emails, passwords, IPs, tokens, API keys
- NEVER commit personal information: real names, addresses, phone numbers, personal emails
- NEVER commit internal network IPs (192.168.x.x, 10.x.x.x) — use placeholders like seed.commputer.xyz
- NEVER commit API tokens, passwords, or private keys
- The clean public repo is at ~/commputer-clean — push to GitHub from there, not from ~/Coin
