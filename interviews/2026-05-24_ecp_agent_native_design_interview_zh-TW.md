# 訪談紀錄：ECP Agent-Native Design Deep Dive

**時間：** 2026-05-24  
**專案：** EgentCodePlexus (`ecp`)  
**主題：** Agent-native 程式碼理解、架構感知、無狀態圖譜、協作治理  
**形式：** 一問一答整理稿

---

## 訪談背景

EgentCodePlexus，簡稱 ECP，是一個為 AI code agent workflow 設計的程式碼結構感知工具。它不是另一個 Codex、Claude Code 或 OpenClaw，也不試圖成為完整的 agent framework。它的定位更接近 LLM 專用的 graph-aware `grep` / `read` tool：當 Agent 需要理解程式碼時，ECP 提供快速、可信、直接來自 source code 的結構化事實。

本次訪談聚焦於 ECP 背後的設計理念：為什麼要強調 BlindSpot、為什麼採用 Rust + `rkyv` + `mmap` 的 stateless 架構、為什麼需要 peers 與 `ecp dev pr-analyze`，以及在 AI Agent 時代，文件、技能、工作流與架構治理會如何重新定位。

---

## Q1：為什麼「誠實的不知道」比「模糊的猜測」重要？

**問：**  
ECP 的核心使命中提到「Built for agents, not IDEs」。在傳統程式碼分析工具裡，開發者通常追求完整圖譜與漂亮 UI，但 ECP 選擇明確標記 BlindSpot。為什麼你認為對 AI Agent 來說，告知它「我不知道」比提供一個可能的猜測更重要？

**答：**  
在 agent 溝通的過程中，LLM 很容易呈現閉世界假設。對 LLM 來說，任何未知都可能被當成不存在，所以必須讓 LLM 認知到「不知道」是非常重要的，這能避免 LLM 產生幻覺。

在知道「不知道」的情況下，LLM 才能深挖更深層的問題，找到實際答案，而不是因為工具掩蓋了不知道，讓 LLM 以為不知道的事情不存在。

所以 BlindSpot 的價值不是補完整圖譜，而是誠實揭露圖譜的邊界。沒有標出 BlindSpot 時，人類工程師可能還會懷疑這裡有暗坑；但 Agent 可能會直接把「沒有邊」理解成「沒有依賴」。這對大型系統非常危險。

---

## Q2：`ecp peers` 是否代表未來會是多 Agent 並行開發？

**問：**  
`ecp peers` 讓不同 LLM session 能交換訊息，看到彼此 dirty surface，甚至協調誰正在修改什麼。這是否代表你認為未來開發不再是「一個人類帶一個 Copilot」，而是一群專責 Agent 在同一個 repo 並行工作？

**答：**  
實際上所有先進的 Agentic Developer 或 Department 都已經開始逐步採用這個機制，但更多是在工作流上實現。工作流的建立往往複雜且龐大，但透過簡單的 peer 設計，就能讓 code agent 在開發中彼此即時互通當前開發資訊，避免重工或引入雙方開發間的衝突問題。

現今我們有 sub-agent 的開發模式，但這種 sub-agent 機制只是對單一 session 的拓展。Sub-agent 只是完成 main agent 交付的任務。

透過 `ecp peers`，我希望達成的是一群龐大的 main agent 在互通資訊的情況下，完成更龐大的計畫，而不只是透過文件紀錄來達到溝通。這樣的設計更能即時推進彼此進度，也能完成更大規模的設計規劃。

---

## Q3：為什麼是 `mmap` + `rkyv` 的 stateless 架構？

**問：**  
許多程式碼圖譜工具會選資料庫或長駐 server。ECP 選擇每次查詢都直接 mmap `graph.bin`，不維護 daemon 狀態。這是否是為了規避高併發查詢下的 lock contention、server cache 與連線瓶頸？

**答：**  
`mmap` 與 `rkyv` 的 stateless 概念，取決於效能和穩定性。

當啟動 server 後，server 可以擁有狀態，但為了維護狀態，我們需要更多複雜流程與程式碼去確保所有狀態的穩定性。每一次與 server 的交互都是成本；必要 cache 也是記憶體成本。諸多限制會造成效能低落。

透過 `mmap`，所有 agent 共享底層共通資源，彼此不需要擁有狀態，而是透過近乎即時查詢的靜態資料工作。以實測來看，1.6 萬檔案、30 萬級節點的 graph 建立只需要 2 到 3 秒，產生的 `graph.bin` 實際不到 70 MB。因為是共享的 `mmap` 設計，各項 CLI 查詢基本落在 100 ms 以內。

實測資料如下：

| 項目 | `.sample_repo` | VS Code |
|---|---:|---:|
| repo 實體檔案 | 22,859 | 14,874 |
| graph `File` 節點 | 15,722 | 12,185 |
| graph 大小 | 58,006,852 bytes，約 55.3 MiB | 68,224,508 bytes，約 65.1 MiB |
| force index 峰值 RSS | 約 1.13 GiB | 約 1.23 GiB |
| cold index | 2.49s | 3.29s |
| incremental analyze | 4.4ms | 4.4ms |
| cypher Class->Method | 29.5ms | 30.3ms |
| routes | 10.7ms | 14.3ms |
| inspect Class | 18.5ms | 26.5ms |
| find bm25 | 16.9ms | 22.5ms |
| impact downstream | 15.3ms | 22.2ms |
| impact baseline HEAD~1 | 670.5ms | 702.8ms |

這個模型對 Agent 很重要，因為 Agent 系統本來就高併發、短生命、容易重啟、容易跨 session。Stateless 不是只為了快，而是為了讓失敗模式變少。

---

## Q4：ECP 是否從解析程式碼走向理解架構？

**問：**  
ECP 加入 `find-transaction-patterns`、`find-event-mirrors`、`contracts` 等功能，已經超越「誰呼叫誰」的 AST 層級。你是否認為 AI Agent 最大挑戰會從寫對 function 轉向理解並遵守架構約束？

**答：**  
是。AI Agent 在軟體開發裡更大的挑戰，正在從「能不能寫對一個 Function」轉向「能不能在大型系統裡不破壞既有架構約束」。

單一 function 的正確性可以靠型別、測試、lint、局部上下文解決；但真實系統的風險常常藏在跨檔案、跨服務、跨語言、跨時間的約束裡。例如：

- 這個 handler 不是普通函式，而是某個 Saga step。
- 這個 publish topic 會被另一個 service 消費。
- 這個欄位不是孤立 schema，而是 response shape 與 frontend consumer 的契約。
- 這個 rename 看似局部，實際會破壞 resolver binding 或 route contract。

LLM 可以透過基礎圖譜查詢自己推導，但那不是穩定方案。問題不在於 LLM 不會推理，而是每次都讓它從低階節點和邊重新發現架構模式，成本高、結果不一致，也容易漏掉「負空間」：那些沒有顯式 call edge，但靠命名、topic、decorator、framework convention、consumer access pattern 維持的關係。

所以 ECP 內建 Saga、EventTopic 這類模式偵測，是把高風險、常見、可結構化的架構知識提前升級成一等訊號。LLM 不需要每次從 `(Function)-[:Calls]->(Function)` 慢慢猜，而是直接看到：

- 這裡可能是補償交易模式。
- 這個 publisher / subscriber 經由 topic 隱含耦合。
- 這個欄位被外部 consumer 依賴。
- 這段改動可能造成 binding tier degradation。

我不會把 ECP 定位成絕對的 Architecture Oracle，因為架構判斷不該偽裝成百分之百真理。更準確地說，它應該是 Agent 的架構雷達：在動手改程式碼前，把高信心約束、可能的隱性耦合、需要人工驗證的 heuristic 明確攤開。

---

## Q5：ECP 與 GitNexus 的關係是什麼？

**問：**  
ECP 常被拿來和 GitNexus 比較。從 Node.js 的 GitNexus 轉向 Rust 的 ECP，那個讓你決定徹底重做的臨界點是什麼？

**答：**  
這個問題需要重新梳理。GitNexus 不是我的項目，而是我的項目起源。我不是 GitNexus 的作者，而是透過使用 GitNexus 遇到一些問題：效能、多個 daemon server、以及多個架構瓶頸，於是自主開發了 Rust 架構。

Breaking point 不是單一事件，而是兩件事疊在一起。

第一個臨界點是常駐狀態開始反過來限制 Agent 工作流。多個 Agent、不同 worktree、不同 repo、不同分支同時查詢時，daemon 模型很容易變成隱性協調問題：誰持有最新 graph、誰負責 invalidation、哪個 session 的 cache 是可信的、daemon 掛掉或卡住時誰恢復。這些不是核心產品價值，卻會消耗大量工程注意力。

第二個臨界點是查詢頻率的想像變了。早期 code graph 像工具，需要時問一次。後來我想要的是更接近 Agent 的 L1 / L2 cache：每次改檔前、rename 前、review 前、impact 分析前，都可以毫秒級查一次。到這個頻率後，Node.js daemon、GC、IPC、async runtime、物件圖 deserialization 的成本都會變得刺眼。不是 Node.js 做不到，而是它不再是最簡單的正確形狀。

所以 Rust stateless 架構的核心判斷是：把 graph 做成固定格式的 `graph.bin`，讓每次 CLI / MCP 查詢都直接 `mmap`，不用暖機、不靠 daemon 狀態、不需要相信某個長生命 process。

```text
source repo -> index -> immutable graph.bin
query process -> mmap graph.bin -> answer -> exit
```

至於 ECP 在生態系裡的角色，我不希望它只是另一個更聰明的 CLI。更準確的定位是 Agent 的結構化程式碼感知層。

---

## Q6：`ecp dev pr-analyze` 與 Merge Queue 的故事是什麼？

**問：**  
在 0.3.0 到 0.4.0 的演進中，因為 PR 不斷被 merge 進 main，其他開發中的 Agent 必須不斷 rebase、重新測試，導致開發週期延緩。`ecp dev pr-analyze` 這項功能是如何誕生的？

**答：**  
最早我沒有想重造 merge queue。我只是想把 `ecp impact` 的訊號餵給現有隊列，所以做了 `ecp dev pr-analyze`，讓 workflow 自動標上 `area`、`risk`、`cross-pr-conflict`。Mergify 是第一個載體。

但 dogfooding 後發現，真正有價值的不是 queue 本身，而是「用 graph 判斷 PR 是否能安全並行」。如果這個判斷最後被壓成幾個 label，再交給外部 queue engine，那 ECP 反而被外部工具的模型限制住了。因此後來把 Mergify 移除，保留 ECP 自己的分析層與 GitHub workflow，讓未來可以往更 agent-native 的 merge governance 前進。

傳統 queue 的判斷大多是：

- PR 是否 up-to-date。
- required checks 是否綠。
- 是否有 merge conflict。
- 排隊順序 / batch trial 是否通過。

ECP 加進去的是另一層：

- 這個 PR 改了哪些 symbol。
- 這些 symbol 的 impact set 有多大。
- 它屬於 parser / cli / tests / docs 哪個 area。
- 它和其他 queued PR 的 changed symbols / impact set 是否重疊。

關於 interference detection，設計上有，而且這是核心差異。

```text
self.changed_symbols = 這個 PR 直接改到的 symbols
self.impact_set = ecp impact 算出的 blast radius

for other queued PR:
    other.impact_set = 從該 PR 的 hidden cache comment 讀出
    overlap = self.changed_symbols ∩ other.impact_set
    if overlap 非空:
        ecp/cross-pr-conflict = pending
    else:
        ecp/cross-pr-conflict = success
```

所以它不是只看檔案路徑。兩個 PR 即使改不同檔案，只要一個改到另一個 blast radius 內的 symbol，就會被視為語意衝突。反過來，如果兩個 PR 位於不相關 area，且 impact set 沒交集，就可以被標成低風險、無 cross-PR conflict，交給 workflow 做更積極的合併。

這項功能真正節省的不是單次 CI 時間，而是減少這種循環：

```text
PR A merge
PR B main drift
B rebase
B rerun CI
PR C 又 drift
C rebase
C rerun CI
```

ECP 在這裡的願景是：把 merge queue 從「按時間排隊」提升成「按結構風險調度」。這是它從靜態分析工具跨到開發工作流治理的地方。

---

## Q7：Agent 開發的真正瓶頸是什麼？

**問：**  
ECP 0.4.0 之後，你開始用 ECP 反過來管理 ECP 自己的開發節奏。你覺得這次 dogfooding 最大收穫是什麼？

**答：**  
傳統人類團隊用來管理變更的節奏並不能說是錯誤，而是在整個流程制度上應該更自動化，以免阻礙開發。

現在 Agent coding 速度非常快，但阻礙的反倒是人們的擔心害怕。擔心程式碼相互衝突、擔心 LLM 改 A 壞 B、擔心各種程式碼出錯，這些恐懼反倒成為阻礙開發的最大問題。

小項目不會有這種感覺，因為 context 足夠塞下，或是程式碼不那麼複雜。但隨著未來每個人、每個團隊、每間公司開發的項目都指數型成長，類似工具會更重要，而效能會成為致命關鍵。

如何讓 LLM 更快理解當前架構，而不是靠長期記憶的 context 處理，變得相對重要。又或者說，我們以文字方式記憶架構與設計是不合理的，因為只有原始程式碼才是 the only truth。

我們更應該透過原始程式碼讓 LLM 了解架構，而不是透過文字紀錄當前程式架構。過去文字紀錄是給人查看，LLM 應該直接從根源理解，這樣才不會有紀錄與程式碼不同步的問題。

---

## Q8：AI Agent 時代的 documentation 會變成什麼？

**問：**  
如果 source code 是唯一真相，那未來的技術文件、架構文件、ADR、README 還應該扮演什麼角色？

**答：**  
AI Agent 時代的 documentation 反倒會變成「流程圖」最重要，因為人需要管理的是流程與架構設計。透過圖表，我們能快速看到每個設計，但程式碼如何生成、工具如何驗證、Agent 如何消費，將會逐漸變得不那麼重要。

但底層核心設計仍應該記錄，例如如何利用 cache、是否該有 warm-up、client timeout 該設多少、工具調用應該併發還是串行、挑選哪個依賴套件等。這些設計文件仍然重要。

只是這些紀錄會抽象到更大的範圍，而不是每份文件都把所有內容鉅細靡遺地記錄下來。正確來說，所有文件都會再往更高維度的設計移動。

就像我們現在寫程式不會再去深究機器碼如何編譯、如何運行。但這不代表那些工作不重要，仍會需要有人深究新的程式語言、更高效的設計，來取代現在所有熱門語言。我相信有天，目前當紅的 Rust、Python、C、C++、JavaScript、TypeScript 等語言，都會隨著時代發展退居二線或被洪流沖走。

包含目前所設計的 EgentCodePlexus，也只是當前時代下因應趨勢的過渡產物。

---

## Q9：ECP 的產品邊界是什麼？

**問：**  
既然 ECP 也是過渡產物，它現在最應該解決的問題邊界是什麼？它應該往自動化架構治理平台發展，還是專注在極快、可信、source-code-grounded 的結構感知層？

**答：**  
ECP 應該專注在成為一個極快、可信、source-code-grounded 的結構感知層。

這個專案不會、也不需要成為另一個 Codex、Claude Code 或 OpenClaw，而是讓這些需要執行 code agent workflow 的框架，在需要了解程式碼時，可以選用的工具功能。它就是一個 LLM 專用的 `grep` / `read` tool。

傳統工具回答：

```text
grep: 這個字串在哪裡？
read: 這個檔案內容是什麼？
```

ECP 要回答的是：

```text
這個 symbol 是誰？
它被誰呼叫？
它會影響哪些 upstream / downstream？
這個 route / event / schema 被誰依賴？
這裡有沒有 BlindSpot？
這次 diff 的結構風險是什麼？
```

ECP 的核心不是幫 Agent 做決策，而是提供 Agent 做決策前必須知道的結構事實。

---

## Q10：CLI、MCP 與工具介面的取捨是什麼？

**問：**  
如果 ECP 是 LLM 專用的 graph-aware `grep` / `read` tool，那最重要的產品介面是 CLI、MCP，還是 library / client API？

**答：**  
是 CLI。

MCP 有較多維護上的麻煩，但因為不是每個 code agent framework 都可以 native 呼叫 CLI，所以採用了雙重架構，同時擁有 MCP 去引導使用 CLI 呼叫。

但更好的方式應該是讓工具能直接內嵌為 CLI tool，就如同 `read` 那樣。這對提高效能與減少 token 都是一大幫助。

CLI 的優勢是簡單、低維護、容易測試、容易放進 CI，也沒有 server lifecycle、tool registration、schema 相容與 session 狀態問題。MCP 的價值是相容層，不是核心抽象。

---

## Q11：Skill 應該是約束還是引導？

**問：**  
如果未來 code agent framework native 支援 ECP，Agent 何時該自動呼叫 ECP？這些行為應該靠硬性 workflow contract，還是靠 skill 引導？

**答：**  
這些設計在被 LLM 學習起來之前，都需要 skill 去規範與引導。就如同人類還沒學會用火之前，是不知道火的用途。但隨著這些知識被廣泛運用，多數的人應該沒有上過如何使用火的課程與引導。

目前還是需要透過 skill 去規範與引導，除非哪天 LLM 內建了此類相關知識。

但我希望 skill 不是用來約束 LLM 該如何使用工具，而是一種引導。就如同若把火當成一種工具，它可以燒烤、也可以取暖、還能用來照明。工具應該描述它的功能，而如何使用則是透過 skill 做基本引導，也能依照個人需求調整，例如 merge queue 的開發。

更希望的當然是 LLM 自主理解而做到更廣泛的運用。

---

## Q12：ECP Skill 最核心的第一原則是什麼？

**問：**  
如果 Skill 是基本引導而不是硬性約束，那它最應該教給 Agent 的第一原則是什麼？

**答：**  
「所有行動都應該從可驗證的結構事實出發，而不是從上下文幻覺出發。」這是比較期望的。

任何前文都可能成為幻覺，只有即時確認的情況才可驗證。這也就是為何會想設計出 peer 的功能：透過 hook / peer 達到半即時互動，以避免開發衝突問題產生。

---

## 結語：ECP 想解決的是信任問題

AI Agent 時代，開發速度不再是唯一瓶頸；真正的瓶頸是信任。

人類害怕 Agent 改壞系統，Agent 也容易被自己的上下文誤導。ECP 試圖解決的，是把信任建立在可驗證的結構事實上，而不是建立在文字記憶、過期文件或模型猜測上。

ECP 不想成為另一個 Agent，也不想取代開發框架。它更像是一個給 Agent 使用的結構化感知層：像 `grep` 和 `read` 一樣基礎，但理解的是 symbol、impact、contracts、BlindSpot 與 peer 狀態。

如果未來每個人、每個團隊、每間公司都會同時驅動更多 Agent、更多 repo、更多變更，那真正重要的不是把更多內容塞進 context，而是讓 Agent 在每一次行動前，都能快速回到唯一真相：source code。
