# AI Agent Interviews & Case Studies

To understand how `cgn` performs in real-world autonomous workflows, we conduct regular "interviews" with the AI agents (Gemini CLI, Codex, etc.) that use it. These transcripts provide deep dives into performance, reliability, and architectural choices from the perspective of the primary consumer: the agent.

> **Naming note:** Older interview transcripts may use `gnx` or `graph-nexus`. Those are historical names; the current CLI and project name are `cgn` and Code Graph Nexus.

## 📁 Interview Categories

### ⚡ Performance & Scalability
Deep dives into the indexing engine, zero-copy mmap, and sub-second query latencies.
- [Indexing & Query Performance Deep Dive](./en/performance/0002_rust_0.1.5_563add9_gemini-cli_20260519_021636.md)
- [Baseline Performance Audit](./en/performance/0001_rust_0.1.5_83c1ae1_gemini-cli_20260519_000000.md)

### 🔍 Code Review & Reliability
How agents use the structural graph to perform more accurate and faster code reviews.
- [Application Analysis in Code Review](./en/code_review/0001_rust_0.1.5_83c1ae1_gemini-cli_20260518_211749.md)
- [Codex Assisted PR #154 Review](./en/code_review/0002_rust_0.1.5_83c1ae1_codex_20260518_214111.md)

---
*Note: All interviews are conducted via shell-mediated Q&A with the agent after task completion.*
