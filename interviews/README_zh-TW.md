# AI Agent 訪談紀錄與案例研究

為了瞭解 `cgn` 在真實自主工作流中的表現，我們會定期對使用它的 AI Agent（如 Gemini CLI、Codex 等）進行「訪談」。這些對話紀錄從主要受眾（Agent）的角度，深入探討了效能、可靠性與架構選擇。

> **名稱註記：** 較早期的訪談紀錄可能使用 `gnx` 或 `graph-nexus`。這些是舊名稱；目前 CLI 與專案名稱為 `cgn` 與 Code Graph Nexus。

## 📁 訪談分類

### ⚡ 效能與可擴展性
深入探討索引引擎、零拷貝 mmap 以及亞秒級查詢延遲。
- [索引與查詢效能深度解析](./zh-TW/performance/0002_rust_0.1.5_563add9_gemini-cli_20260519_021636.md)
- [基準效能稽核](./zh-TW/performance/0001_rust_0.1.5_83c1ae1_gemini-cli_20260519_000000.md)

### 🔍 代碼審查與可靠性
Agent 如何利用結構化圖譜進行更精確、更快速的代碼審查（Code Review）。
- [代碼審查中的應用分析](./zh-TW/code_review/0001_rust_0.1.5_83c1ae1_gemini-cli_20260518_211749.md)
- [Codex 協助 PR #154 審查案例](./zh-TW/code_review/0002_rust_0.1.5_83c1ae1_codex_20260518_214111.md)

---
*註：所有訪談均在任務完成後，透過 shell 與 Agent 進行問答獲取。*
