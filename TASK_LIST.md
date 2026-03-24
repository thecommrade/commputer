# Commputer — Autonomous Task List

Rules for autonomous execution:
- Work ONLY within ~/Coin/
- NEVER modify anything outside ~/Coin/
- Read the design spec at ~/Coin/docs/superpowers/specs/2026-03-22-commputer-design.md for context
- Mark each task [DONE] when complete
- If a task is unclear or requires a design decision, skip it and mark [SKIPPED - needs human input]

---

## Task 1: Design the project filing system
- [ ] Given the full scope of Commputer (L1 blockchain, multi-dimensional PoW, validator software, analytics platform, Humanities Archive, AI/LLM, L2 ecosystem, whitepaper, documentation, research, applications), design a comprehensive directory structure for ~/Coin/
- [ ] Cover: source code, documentation, research, specifications, legal/whitepaper, applications, tooling, tests, deployment, community
- [ ] Create all directories
- [ ] Write a README.md at ~/Coin/ explaining the structure
- [ ] Move existing files into their proper locations within the new structure (update any internal references)

## Task 2: Research consensus mechanisms for multi-dimensional PoW
- [ ] Research how existing L1s select block producers (Solana, Near, Polkadot, Sui)
- [ ] Document pros/cons of each approach for Commputer's 5-channel PoW
- [ ] Write findings to the research directory
- [ ] Include a "Recommended for Commputer" section with reasoning

## Task 3: Research Sybil resistance and identity primitives
- [ ] Research how existing projects define validator identity (hardware fingerprinting, stake-based, reputation)
- [ ] Document approaches to detecting multi-node operators (latency triangulation, behavioral analysis)
- [ ] Research Proof of Personhood projects (Worldcoin, BrightID, Gitcoin Passport) for lessons learned
- [ ] Write findings to the research directory

## Task 4: Research existing Rust blockchain frameworks and libraries
- [ ] Survey Substrate (Polkadot), Solana's codebase, Reth, and libp2p
- [ ] Identify reusable crates for: P2P networking, consensus, cryptographic proofs, serialization
- [ ] Document which libraries are production-proven and actively maintained
- [ ] Write findings to the research directory

## Task 5: Scaffold Rust workspace
- [ ] Initialize a Cargo workspace within the source directory
- [ ] Create crate structure:
  - commputer-core (block, transaction, proof types)
  - commputer-consensus (consensus logic)
  - commputer-network (P2P gossip + DHT)
  - commputer-proofs (5 proof channel implementations)
  - commputer-node (main binary, CLI)
  - commputer-validator (validator logic, resource monitoring)
- [ ] Each crate gets a lib.rs with module structure comments
- [ ] Root Cargo.toml with workspace members

## Task 6: Define core data types as Rust structs
- [ ] Block, BlockHeader
- [ ] Transaction, TransactionType (transfer, burn, proof_submission)
- [ ] ProofChallenge, ProofResult (per channel: CPU, GPU, Storage, RAM, Bandwidth)
- [ ] ValidatorIdentity, ResourceContribution, ResourceProfile
- [ ] ComplianceStatus, NerfState
- [ ] HolderTier, ResourceAllocation
- [ ] Write to commputer-core/src/types.rs

## Task 7: Write project glossary
- [ ] Define all Commputer-specific terms (The Commrade, nerf, grace balance, reference node, gold standard, etc.)
- [ ] Include tokenomics terms (milestone burn, usage burn, charitable burn, hybrid curve, floor rate, $RAD)
- [ ] Include architectural terms (proof channel, resource orchestration, buffer pool, will function)
- [ ] Write to the docs directory
