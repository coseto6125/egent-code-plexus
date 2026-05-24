/**
 * Global application keys for i18n localization.
 */
const K = Object.freeze({
    META_TITLE: 'meta.title',
    HERO_TAGLINE: 'hero.tagline',
    HERO_SUBTITLE: 'hero.subtitle',
    
    INSTALL_MAC_LINUX: 'install.mac_linux',
    INSTALL_WINDOWS: 'install.windows',
    INSTALL_CARGO: 'install.cargo',
    INSTALL_COPIED: 'install.copied',

    NAV_HIGHLIGHTS: 'nav.highlights',
    NAV_INTERVIEW: 'nav.interview',
    NAV_BENCHMARKS: 'nav.benchmarks',
    NAV_VISION: 'nav.vision',
    NAV_MATRIX: 'nav.matrix',
    NAV_SETUP: 'nav.setup',
    MAT_LEGEND: 'matrix.legend',
    MAT_TH_LANG: 'matrix.th.lang',
    MAT_RATIONALE: 'matrix.rationale.title',
    SETUP_STEP1_TITLE: 'setup.step1.title',
    SETUP_STEP1_DESC: 'setup.step1.desc',
    SETUP_STEP2_TITLE: 'setup.step2.title',
    SETUP_STEP2_DESC: 'setup.step2.desc',
    SETUP_STEP3_TITLE: 'setup.step3.title',
    SETUP_STEP3_DESC: 'setup.step3.desc',
    SETUP_STEP4_TITLE: 'setup.step4.title',
    SETUP_STEP4_DESC: 'setup.step4.desc',

    H_BLINDSPOT_TITLE: 'highlights.blindspot.title',
    H_BLINDSPOT_DESC: 'highlights.blindspot.desc',
    H_STATELESS_TITLE: 'highlights.stateless.title',
    H_STATELESS_DESC: 'highlights.stateless.desc',
    H_RADAR_TITLE: 'highlights.radar.title',
    H_RADAR_DESC: 'highlights.radar.desc',

    Q1_Q: 'interview.q1.q', Q1_A: 'interview.q1.a',
    Q2_Q: 'interview.q2.q', Q2_A: 'interview.q2.a',
    Q3_Q: 'interview.q3.q', Q3_A: 'interview.q3.a',
    Q4_Q: 'interview.q4.q', Q4_A: 'interview.q4.a',
    Q5_Q: 'interview.q5.q', Q5_A: 'interview.q5.a',
    Q6_Q: 'interview.q6.q', Q6_A: 'interview.q6.a',
    Q7_Q: 'interview.q7.q', Q7_A: 'interview.q7.a',

    TBL_ITEM: 'table.header.item',
    TBL_SAMPLE: 'table.header.sample',
    TBL_VSCODE: 'table.header.vscode',
    
    TBL_R1: 'table.row.r1', // 實體檔案
    TBL_R2: 'table.row.r2', // File 節點
    TBL_R3: 'table.row.r3', // graph 大小
    TBL_R4: 'table.row.r4', // force index 峰值 RSS
    TBL_R5: 'table.row.r5', // cold analyze
    TBL_R6: 'table.row.r6', // incremental analyze
    TBL_R7: 'table.row.r7', // cypher Class->Method
    TBL_R8: 'table.row.r8', // routes
    TBL_R9: 'table.row.r9', // inspect Class
    TBL_R10: 'table.row.r10',// find bm25
    TBL_R11: 'table.row.r11',// impact downstream
    TBL_R12: 'table.row.r12',// impact baseline HEAD~1

    TBL_NOTE: 'table.note',

    VISION_QUOTE: 'vision.quote',
    VISION_P1: 'vision.p1',
    VISION_P2: 'vision.p2',

    FOOTER_TEXT: 'footer.text'
});

const LOCALES = [
    { code: 'zh-TW', label: '繁體中文' },
    { code: 'zh-CN', label: '简体中文' },
    { code: 'en', label: 'English' },
    { code: 'ja', label: '日本語' },
    { code: 'ko', label: '한국어' },
    { code: 'es', label: 'Español' }
];

const TRANSLATIONS = {
    'zh-TW': {
        [K.NAV_MATRIX]: '支援語系',
        [K.NAV_SETUP]: '快速開始',
        [K.MAT_LEGEND]: '✓ 支援 | — 規劃中 | n/a 語言無此特性',
        [K.MAT_TH_LANG]: '語言',
        [K.MAT_RATIONALE]: 'Per-cell rationale (詳細解析邏輯)',
        [K.SETUP_STEP1_TITLE]: '1. 啟動 AI Onboarding Wizard',
        [K.SETUP_STEP1_DESC]: '在您的 AI Agent (如 Claude Code) 中貼上專屬指令，喚醒互動式精靈，為您完成環境檢測與自動安裝。',
        [K.SETUP_STEP2_TITLE]: '2. 建立索引 (非必要 / Auto-Index)',
        [K.SETUP_STEP2_DESC]: 'ECP 內建 auto-ensure 機制，在您第一次查詢時會自動建立圖譜，因此手動執行索引通常是非必要的。',
        [K.SETUP_STEP3_TITLE]: '3. 多專案群組 (Group)',
        [K.SETUP_STEP3_DESC]: '如果是微服務或前後端分離架構，可建立群組以實現跨 Repo 查詢。',
        [K.SETUP_STEP4_TITLE]: '4. 確認 MCP 整合 (Verify)',
        [K.SETUP_STEP4_DESC]: 'Onboarding 精靈會自動為 IDE 寫入 MCP 設定。完成後，您可透過 CLI 確認已暴露給 Agent 的工具清單。',
        [K.META_TITLE]: 'Egent Code Plexus',
        [K.HERO_TAGLINE]: 'Built for agents, not IDEs.',
        [K.HERO_SUBTITLE]: '專為 AI Agent 設計的程式碼結構感知與架構雷達',
        [K.INSTALL_MAC_LINUX]: 'macOS / Linux',
        [K.INSTALL_WINDOWS]: 'Windows (PowerShell)',
        [K.INSTALL_CARGO]: 'Cargo (從原始碼編譯)',
        [K.INSTALL_COPIED]: '已複製！',
        [K.NAV_HIGHLIGHTS]: '技術亮點',
        [K.NAV_INTERVIEW]: '開發問答',
        [K.NAV_BENCHMARKS]: '效能實測',
        [K.NAV_VISION]: '未來願景',
        [K.H_BLINDSPOT_TITLE]: 'BlindSpot Awareness',
        [K.H_BLINDSPOT_DESC]: '「誠實的不知道」比「模糊的猜測」重要。ECP 明確標記圖譜邊界，防止 Agent 把「沒有邊」誤認為「沒有依賴」，從根本上解決 LLM 的閉世界幻覺。',
        [K.H_STATELESS_TITLE]: 'Stateless & mmap',
        [K.H_STATELESS_DESC]: '拋棄傳統 Server Daemon 負擔。基於 Rust + rkyv 的無狀態架構，每次查詢直接 mmap 圖譜檔案，150ms 內完成分析，完美契合 Agent 頻繁且高併發的呼叫需求。',
        [K.H_RADAR_TITLE]: 'Architecture Radar',
        [K.H_RADAR_DESC]: '從簡單的 AST 關聯，升級到高階架構約束感知。內建 Saga 補償交易、EventTopic 發布訂閱、以及跨服務 API 契約的模式偵測，提前揭露隱性風險。',
        [K.Q1_Q]: '為什麼「誠實的不知道」重要？',
        [K.Q1_A]: '在知道「不知道」的情況下，LLM 才能深挖更深層的問題。人類工程師可能還會懷疑這裡有暗坑；但 Agent 可能會直接把「沒有邊」理解成「沒有依賴」。BlindSpot 的價值是誠實揭露圖譜的邊界。',
        [K.Q2_Q]: '為什麼選擇 mmap + rkyv 的無狀態架構？',
        [K.Q2_A]: 'Stateless 不是只為了快，而是為了讓失敗模式變少。當啟動 server 時需要維護複雜狀態與快取記憶體，反而會造成效能低落。透過 mmap，所有 agent 共享近乎即時的唯讀靜態資源，無需相信單一守護行程。',
        [K.Q3_Q]: '關於 PR 並行合併的治理 (Merge Governance)？',
        [K.Q3_A]: '把 merge queue 從「按時間排隊」提升成「按結構風險調度」。ECP 會計算 PR 修改了哪些 symbol 及其 blast radius (impact set)，藉此判斷並行 PR 間是否有語意層面的重疊與衝突，而不僅僅是比較檔案路徑。',
        [K.Q4_Q]: '從 Node.js (GitNexus) 轉向 Rust 的臨界點是什麼？',
        [K.Q4_A]: '常駐狀態反過來限制了工作流。當多 Agent 同時查詢時，daemon 模型變成隱性協調問題。且當查詢頻率提高到每次改檔、rename、review 前都要查一次時，Node 的 GC 與 IPC 成本變得刺眼。Rust 能做到真正的 Stateless 與毫秒級圖譜查詢。',
        [K.Q5_Q]: 'ECP 是否從解析程式碼走向理解架構？',
        [K.Q5_A]: '是。AI Agent 開發的最大挑戰，正從「寫對 Function」轉向「不破壞大型系統既有架構約束」。ECP 將 Saga、EventTopic 等高風險架構知識升級成一等訊號，Agent 不用慢慢猜，直接將其作為架構雷達。',
        [K.Q6_Q]: 'AI Agent 時代的文件 (Documentation) 會變成什麼？',
        [K.Q6_A]: '文件會變成「流程圖」最重要，因為人類需要專注在管理流程與架構設計。底層的細節與運作邏輯應該由 Agent 直接從 Source Code 中取得，避免紀錄與程式碼不同步的問題。原始碼才是 the only truth。',
        [K.Q7_Q]: 'Skill 應該是約束還是引導？',
        [K.Q7_A]: 'Skill 應該是一種引導，就如同教導人類如何使用火，而非硬性限制其只能用來取暖或燒烤。但其核心的第一原則應該是：「所有行動都應該從可驗證的結構事實出發，而不是從上下文幻覺出發。」',
        [K.TBL_ITEM]: '項目',
        [K.TBL_SAMPLE]: '.sample_repo (22k 檔案)',
        [K.TBL_VSCODE]: 'VS Code (14k 檔案)',
        [K.TBL_R1]: 'repo 實體檔案',
        [K.TBL_R2]: 'graph File 節點',
        [K.TBL_R3]: 'graph 大小',
        [K.TBL_R4]: 'force index 峰值 RSS',
        [K.TBL_R5]: 'cold index',
        [K.TBL_R6]: 'incremental analyze',
        [K.TBL_R7]: 'cypher Class->Method',
        [K.TBL_R8]: 'routes',
        [K.TBL_R9]: 'inspect Class',
        [K.TBL_R10]: 'find bm25',
        [K.TBL_R11]: 'impact downstream',
        [K.TBL_R12]: 'impact baseline HEAD~1',
        [K.TBL_NOTE]: '※ 基準硬體配置: AMD Ryzen 9 9950X, 39.2 GiB RAM, Linux 6.6.87',
        [K.VISION_QUOTE]: '「所有行動都應該從可驗證的結構事實出發，而不是從上下文幻覺出發。」',
        [K.VISION_P1]: '在 AI Agent 時代，開發速度不再是唯一瓶頸；真正的瓶頸是<strong>信任</strong>。人類害怕 Agent 改壞系統，Agent 也容易被自己的上下文誤導。Egent Code Plexus 試圖把信任建立在可驗證的結構事實上，讓 Agent 在每一次行動前，都能快速回到唯一真相：<strong>Source Code</strong>。',
        [K.VISION_P2]: '如果未來每個人、每個團隊、每間公司都會同時驅動更多 Agent、更多 Repo、更多變更，那真正重要的不是把更多內容塞進 context，而是擁有一個極快、可信、以結構感知為基礎的底層工具。',
        [K.FOOTER_TEXT]: '&copy; 2026 Egent Code Plexus Project. 所有片段節錄自 Native Design Deep Dive 訪談。'
    },
    'zh-CN': {
        [K.META_TITLE]: 'Egent Code Plexus',
        [K.HERO_TAGLINE]: 'Built for agents, not IDEs.',
        [K.HERO_SUBTITLE]: '专为 AI Agent 设计的代码结构感知与架构雷达',
        [K.INSTALL_MAC_LINUX]: 'macOS / Linux',
        [K.INSTALL_WINDOWS]: 'Windows (PowerShell)',
        [K.INSTALL_CARGO]: 'Cargo (从源码编译)',
        [K.INSTALL_COPIED]: '已复制！',
        [K.NAV_HIGHLIGHTS]: '技术亮点',
        [K.NAV_INTERVIEW]: '开发问答',
        [K.NAV_BENCHMARKS]: '性能实测',
        [K.NAV_VISION]: '未来愿景',
        [K.H_BLINDSPOT_TITLE]: 'BlindSpot Awareness',
        [K.H_BLINDSPOT_DESC]: '“诚实的不知道”比“模糊的猜测”重要。明确标记图谱边界，防止 Agent 把“没有边”误认为“没有依赖”，从根本上解决 LLM 的闭世界幻觉。',
        [K.H_STATELESS_TITLE]: 'Stateless & mmap',
        [K.H_STATELESS_DESC]: '抛弃传统 Server Daemon 负担。基于 Rust + rkyv 的无状态架构，每次查询直接 mmap 图谱文件，150ms 内完成分析，完美契合 Agent 频繁且高并发的需求。',
        [K.H_RADAR_TITLE]: 'Architecture Radar',
        [K.H_RADAR_DESC]: '从简单的 AST 关联，升级到高级架构约束感知。内置 Saga 补偿事务、EventTopic 发布订阅、以及跨服务 API 契约的模式检测，提前揭露隐性风险。',
        [K.Q1_Q]: '为什么“诚实的不知道”重要？',
        [K.Q1_A]: '在知道“不知道”的情况下，LLM 才能深挖更深层的问题。人类工程师可能还会怀疑这里有暗坑；但 Agent 可能会直接把“没有边”理解成“没有依赖”。BlindSpot 的价值是诚实揭露图谱的边界。',
        [K.Q2_Q]: '为什么选择 mmap + rkyv 的无状态架构？',
        [K.Q2_A]: 'Stateless 不是只为了快，而是为了让失败模式变少。当启动 server 时需要维护复杂状态与缓存内存，反而会造成性能低落。通过 mmap，所有 agent 共享近乎实时的只读静态资源，无需相信单一守护进程。',
        [K.Q3_Q]: '关于 PR 并行合并的治理 (Merge Governance)？',
        [K.Q3_A]: '把 merge queue 从“按时间排队”提升成“按结构风险调度”。ECP 会计算 PR 修改了哪些 symbol 及其 blast radius (impact set)，借此判断并行 PR 间是否有语义层面的重叠与冲突，而不仅仅是比较文件路径。',
        [K.Q4_Q]: '从 Node.js (GitNexus) 转向 Rust 的临界点是什么？',
        [K.Q4_A]: '常驻状态反过来限制了工作流。当多 Agent 同时查询时，daemon 模型变成隐性协调问题。且当查询频率提高到每次改档、rename、review 前都要查一次时，Node 的 GC 与 IPC 成本变得刺眼。Rust 能做到真正的 Stateless 与毫秒级查询。',
        [K.Q5_Q]: 'ECP 是否从解析代码走向理解架构？',
        [K.Q5_A]: '是。AI Agent 开发的最大挑战，正从“写对 Function”转向“不破坏大型系统既有架构约束”。ECP 将 Saga、EventTopic 等高风险架构知识升级成一等信号，Agent 不用慢慢猜，直接将其作为架构雷达。',
        [K.Q6_Q]: 'AI Agent 时代的文件 (Documentation) 会变成什么？',
        [K.Q6_A]: '文件会变成“流程图”最重要，因为人类需要专注在管理流程与架构设计。底层的细节与运作逻辑应该由 Agent 直接从 Source Code 中取得，避免记录与代码不同步的问题。源码才是 the only truth。',
        [K.Q7_Q]: 'Skill 应该是约束还是引导？',
        [K.Q7_A]: 'Skill 应该是一种引导，就如同教导人类如何使用火，而非硬性限制其只能用来取暖或烧烤。但其核心的第一原则应该是：“所有行动都应该从可验证的结构事实出发，而不是从上下文幻觉出发。”',
        [K.TBL_ITEM]: '项目',
        [K.TBL_SAMPLE]: '.sample_repo (22k 文件)',
        [K.TBL_VSCODE]: 'VS Code (14k 文件)',
        [K.TBL_R1]: 'repo 实体文件',
        [K.TBL_R2]: 'graph File 节点',
        [K.TBL_R3]: 'graph 大小',
        [K.TBL_R4]: 'force index 峰值 RSS',
        [K.TBL_R5]: 'cold index',
        [K.TBL_R6]: 'incremental analyze',
        [K.TBL_R7]: 'cypher Class->Method',
        [K.TBL_R8]: 'routes',
        [K.TBL_R9]: 'inspect Class',
        [K.TBL_R10]: 'find bm25',
        [K.TBL_R11]: 'impact downstream',
        [K.TBL_R12]: 'impact baseline HEAD~1',
        [K.TBL_NOTE]: '※ 基准硬件配置: AMD Ryzen 9 9950X, 39.2 GiB RAM, Linux 6.6.87',
        [K.VISION_QUOTE]: '“所有行动都应该从可验证的结构事实出发，而不是从上下文幻觉出发。”',
        [K.VISION_P1]: '在 AI Agent 时代，开发速度不再是唯一瓶颈；真正的瓶颈是<strong>信任</strong>。人类害怕 Agent 改坏系统，Agent 也容易被自己的上下文误导。Egent Code Plexus 试图把信任建立在可验证的结构事实上，让 Agent 在每一次行动前，都能快速回到唯一真相：<strong>Source Code</strong>。',
        [K.VISION_P2]: '如果未来每个人、每个团队、每间公司都会同时驱动更多 Agent、更多 Repo、更多变更，那真正重要的不是把更多内容塞进 context，而是拥有一个极快、可信、以结构感知为基础的底层工具。',
        [K.FOOTER_TEXT]: '&copy; 2026 Egent Code Plexus Project. 所有片段节录自 Native Design Deep Dive 访谈。'
    },
    'en': {
        [K.META_TITLE]: 'Egent Code Plexus',
        [K.HERO_TAGLINE]: 'Built for agents, not IDEs.',
        [K.HERO_SUBTITLE]: 'Code structure awareness and architecture radar designed for AI Agents.',
        [K.INSTALL_MAC_LINUX]: 'macOS / Linux',
        [K.INSTALL_WINDOWS]: 'Windows (PowerShell)',
        [K.INSTALL_CARGO]: 'Cargo (Build from source)',
        [K.INSTALL_COPIED]: 'Copied!',
        [K.NAV_HIGHLIGHTS]: 'Highlights',
        [K.NAV_INTERVIEW]: 'Dev Q&A',
        [K.NAV_BENCHMARKS]: 'Benchmarks',
        [K.NAV_VISION]: 'Vision',
        [K.H_BLINDSPOT_TITLE]: 'BlindSpot Awareness',
        [K.H_BLINDSPOT_DESC]: '"Honest ignorance" is better than "vague guessing." Explicitly marks graph boundaries to prevent Agents from mistaking "no edge" for "no dependency", solving closed-world hallucinations.',
        [K.H_STATELESS_TITLE]: 'Stateless & mmap',
        [K.H_STATELESS_DESC]: 'Discarding traditional daemon burdens. Built on Rust + rkyv, queries directly mmap the graph file completing in <150ms—perfect for high-frequency, concurrent Agent queries.',
        [K.H_RADAR_TITLE]: 'Architecture Radar',
        [K.H_RADAR_DESC]: 'Upgrading from simple AST linking to high-level architecture constraints. Built-in detection for Saga patterns, EventTopics, and cross-service API contracts to expose hidden risks.',
        [K.Q1_Q]: 'Why is "Honest Ignorance" crucial?',
        [K.Q1_A]: 'Knowing what is "unknown" allows LLMs to dig deeper. A human might suspect a hidden trap, but an Agent assumes "no edge" means "no dependency." BlindSpots honestly reveal the graph\'s boundaries.',
        [K.Q2_Q]: 'Why a stateless mmap + rkyv architecture?',
        [K.Q2_A]: 'Statelessness reduces failure modes. A long-running server requires complex state management and caching, which hurts performance. With mmap, all agents share near-instant read-only resources without relying on a daemon.',
        [K.Q3_Q]: 'What about PR Merge Governance?',
        [K.Q3_A]: 'Shifting merge queues from "time-based" to "risk-based." ECP calculates modified symbols and their blast radius (impact set) to detect semantic overlaps between concurrent PRs, rather than just checking file paths.',
        [K.Q4_Q]: 'What was the breaking point to switch from Node.js (GitNexus) to Rust?',
        [K.Q4_A]: 'Resident state became a bottleneck. When multiple Agents query simultaneously, daemons create coordination issues. At high query frequencies (every edit/rename), Node\'s GC/IPC costs became glaring. Rust enables true statelessness and millisecond queries.',
        [K.Q5_Q]: 'Is ECP moving from code parsing to architecture understanding?',
        [K.Q5_A]: 'Yes. The biggest challenge for AI Agents is shifting from "writing a function correctly" to "not breaking existing constraints." ECP elevates high-risk patterns (like Saga/EventTopics) to first-class signals, acting as an architecture radar.',
        [K.Q6_Q]: 'What happens to Documentation in the AI Agent era?',
        [K.Q6_A]: 'Documentation will pivot to "flowcharts" as humans focus on managing processes and design. Low-level details should be extracted directly from Source Code by Agents, avoiding out-of-sync docs. The source code is the only truth.',
        [K.Q7_Q]: 'Should Skills constrain or guide?',
        [K.Q7_A]: 'Skills should guide—like teaching humanity how to use fire, not restricting it to just cooking. However, the core principle remains: "All actions should start from verifiable structural facts, not from context hallucinations."',
        [K.TBL_ITEM]: 'Item',
        [K.TBL_SAMPLE]: '.sample_repo (22k files)',
        [K.TBL_VSCODE]: 'VS Code (14k files)',
        [K.TBL_R1]: 'Repo files',
        [K.TBL_R2]: 'Graph File nodes',
        [K.TBL_R3]: 'Graph size',
        [K.TBL_R4]: 'Force index peak RSS',
        [K.TBL_R5]: 'Cold index',
        [K.TBL_R6]: 'Incremental analyze',
        [K.TBL_R7]: 'Cypher Class->Method',
        [K.TBL_R8]: 'Routes',
        [K.TBL_R9]: 'Inspect Class',
        [K.TBL_R10]: 'Find bm25',
        [K.TBL_R11]: 'Impact downstream',
        [K.TBL_R12]: 'Impact baseline HEAD~1',
        [K.TBL_NOTE]: '※ Hardware: AMD Ryzen 9 9950X, 39.2 GiB RAM, Linux 6.6.87',
        [K.VISION_QUOTE]: '"All actions should start from verifiable structural facts, not from context hallucinations."',
        [K.VISION_P1]: 'In the AI Agent era, development speed is no longer the bottleneck; the bottleneck is <strong>trust</strong>. Humans fear Agents breaking systems, and Agents are easily misled by their own context. Egent Code Plexus builds trust on structural facts, ensuring Agents always return to the only truth: <strong>Source Code</strong>.',
        [K.VISION_P2]: 'As every team and company drives more Agents and repos concurrently, what matters isn\'t stuffing more into the context window, but having a blazing fast, trustworthy, structure-aware foundational tool.',
        [K.FOOTER_TEXT]: '&copy; 2026 Egent Code Plexus Project. Segments extracted from the Native Design Deep Dive.'
    },
    'ja': {
        [K.META_TITLE]: 'Egent Code Plexus',
        [K.HERO_TAGLINE]: 'Built for agents, not IDEs.',
        [K.HERO_SUBTITLE]: 'AIエージェント専用に設計されたコード構造認識とアーキテクチャレーダー',
        [K.INSTALL_MAC_LINUX]: 'macOS / Linux',
        [K.INSTALL_WINDOWS]: 'Windows (PowerShell)',
        [K.INSTALL_CARGO]: 'Cargo (ソースからビルド)',
        [K.INSTALL_COPIED]: 'コピーしました！',
        [K.NAV_HIGHLIGHTS]: 'ハイライト',
        [K.NAV_INTERVIEW]: '開発 Q&A',
        [K.NAV_BENCHMARKS]: 'ベンチマーク',
        [K.NAV_VISION]: 'ビジョン',
        [K.H_BLINDSPOT_TITLE]: 'BlindSpot の認識',
        [K.H_BLINDSPOT_DESC]: '「正直な無知」は「曖昧な推測」よりも重要です。グラフの境界を明示し、エージェントが「エッジなし」を「依存なし」と誤認するのを防ぎます。',
        [K.H_STATELESS_TITLE]: 'ステートレス & mmap',
        [K.H_STATELESS_DESC]: '従来のデーモンの負担を排除。Rust + rkyv ベースで、クエリは直接mmapされ150ms未満で完了し、高頻度なクエリに最適です。',
        [K.H_RADAR_TITLE]: 'アーキテクチャ レーダー',
        [K.H_RADAR_DESC]: '単純なASTから高度な制約認識へ。Saga、EventTopic、API契約などのパターン検出を内蔵し、潜在的なリスクを可視化します。',
        [K.Q1_Q]: 'なぜ「正直な無知」が重要なのか？',
        [K.Q1_A]: '「未知」を知ることで、LLMは深く掘り下げられます。人間は疑うかもしれませんが、エージェントは「エッジなし」を「依存なし」と解釈します。BlindSpotはグラフの境界を正直に示します。',
        [K.Q2_Q]: 'なぜ mmap + rkyv のステートレスアーキテクチャなのか？',
        [K.Q2_A]: 'ステートレス性は障害を減らします。サーバーは複雑な状態管理が必要でパフォーマンスを低下させます。mmapにより、デーモンに依存せずリソースを瞬時に共有します。',
        [K.Q3_Q]: 'PRマージのガバナンスについて？',
        [K.Q3_A]: 'マージキューを「リスクベース」にシフトします。ファイルパスの比較だけでなく、変更されたシンボルと影響セットを計算し、PR間のセマンティックな衝突を検出します。',
        [K.Q4_Q]: 'Node.jsからRustへの移行の限界点は何でしたか？',
        [K.Q4_A]: '常駐状態がワークフローを制限しました。複数エージェントがクエリを実行するとデーモンが調整問題になります。Rustにより真のステートレスとミリ秒のクエリが可能になりました。',
        [K.Q5_Q]: 'ECPはコード解析からアーキテクチャ理解へ移行している？',
        [K.Q5_A]: 'はい。エージェントの最大の課題は「関数の記述」から「制約を壊さないこと」にシフトしています。高リスクのパターンを第一級の信号に引き上げます。',
        [K.Q6_Q]: 'AIエージェント時代のドキュメントはどうなる？',
        [K.Q6_A]: '人間がプロセス設計に集中するため、ドキュメントは「フローチャート」にシフトします。詳細なロジックは、エージェントが直接ソースコードから取得すべきです。',
        [K.Q7_Q]: 'Skillは制約か、ガイドか？',
        [K.Q7_A]: 'ガイドであるべきです。ただし核心となる第一原則は「すべてのアクションは、コンテキストの幻覚からではなく、検証可能な構造的事実から始まるべきである」ということです。',
        [K.TBL_ITEM]: '項目',
        [K.TBL_SAMPLE]: '.sample_repo (22k ファイル)',
        [K.TBL_VSCODE]: 'VS Code (14k ファイル)',
        [K.TBL_R1]: 'リポジトリ実ファイル',
        [K.TBL_R2]: 'Graph File ノード',
        [K.TBL_R3]: 'Graph サイズ',
        [K.TBL_R4]: 'Force index ピーク RSS',
        [K.TBL_R5]: 'Cold index',
        [K.TBL_R6]: 'Incremental analyze',
        [K.TBL_R7]: 'Cypher Class->Method',
        [K.TBL_R8]: 'Routes',
        [K.TBL_R9]: 'Inspect Class',
        [K.TBL_R10]: 'Find bm25',
        [K.TBL_R11]: 'Impact downstream',
        [K.TBL_R12]: 'Impact baseline HEAD~1',
        [K.TBL_NOTE]: '※ ハードウェア: AMD Ryzen 9 9950X, 39.2 GiB RAM, Linux 6.6.87',
        [K.VISION_QUOTE]: '「すべてのアクションは、コンテキストのハルシネーションからではなく、検証可能な構造的事実から始まるべきです。」',
        [K.VISION_P1]: 'AIの時代、開発スピードはもはやボトルネックではありません。ボトルネックは<strong>信頼</strong>です。人間はエージェントを恐れ、エージェントは文脈に誤導されます。Egent Code Plexus は唯一の真実である<strong>ソースコード</strong>に根ざした信頼を構築します。',
        [K.VISION_P2]: '誰もがより多くのエージェントとリポジトリを同時に駆動する未来では、コンテキストを詰め込むことではなく、超高速で信頼できる構造認識ツールを持つことが重要です。',
        [K.FOOTER_TEXT]: '&copy; 2026 Egent Code Plexus プロジェクト。Native Design Deep Dive インタビューより抜粋。'
    },
    'ko': {
        [K.META_TITLE]: 'Egent Code Plexus',
        [K.HERO_TAGLINE]: 'Built for agents, not IDEs.',
        [K.HERO_SUBTITLE]: 'AI 에이전트를 위해 특별히 설계된 코드 구조 인식 및 아키텍처 레이더',
        [K.INSTALL_MAC_LINUX]: 'macOS / Linux',
        [K.INSTALL_WINDOWS]: 'Windows (PowerShell)',
        [K.INSTALL_CARGO]: 'Cargo (소스에서 빌드)',
        [K.INSTALL_COPIED]: '복사됨!',
        [K.NAV_HIGHLIGHTS]: '하이라이트',
        [K.NAV_INTERVIEW]: '개발 Q&A',
        [K.NAV_BENCHMARKS]: '벤치마크',
        [K.NAV_VISION]: '비전',
        [K.H_BLINDSPOT_TITLE]: 'BlindSpot 인식',
        [K.H_BLINDSPOT_DESC]: '"솔직한 모름"이 "모호한 추측"보다 중요합니다. 그래프 경계를 명시하여 에이전트가 의존성이 없다고 착각하는 것을 방지합니다.',
        [K.H_STATELESS_TITLE]: '상태 비저장 & mmap',
        [K.H_STATELESS_DESC]: '서버 데몬의 부담을 제거. Rust + rkyv 기반으로 직접 mmap하여 150ms 이내에 분석을 완료, 에이전트 쿼리에 최적화되었습니다.',
        [K.H_RADAR_TITLE]: '아키텍처 레이더',
        [K.H_RADAR_DESC]: '단순한 AST를 넘어 고급 아키텍처 제약 조건 인식. Saga, EventTopic, API 계약 패턴을 감지하여 위험을 조기에 드러냅니다.',
        [K.Q1_Q]: '왜 "솔직한 모름"이 중요한가요?',
        [K.Q1_A]: '"알 수 없음"을 아는 것은 LLM이 깊이 파고들게 합니다. 인간은 의심할 수 있지만 에이전트는 "엣지 없음"을 "의존성 없음"으로 해석합니다.',
        [K.Q2_Q]: '왜 mmap + rkyv 기반의 무상태 구조인가요?',
        [K.Q2_A]: '무상태는 실패를 줄입니다. 서버는 복잡한 상태 관리가 필요해 성능이 저하됩니다. mmap을 통해 데몬 없이 자원을 즉시 공유합니다.',
        [K.Q3_Q]: 'PR 병합 거버넌스에 대해?',
        [K.Q3_A]: '병합 큐를 "위험 기반"으로 전환. 파일 경로 비교뿐만 아니라 수정된 심볼과 영향 범위를 계산하여 PR 간의 의미론적 충돌을 감지합니다.',
        [K.Q4_Q]: 'Node.js(GitNexus)에서 Rust로 전환한 계기는?',
        [K.Q4_A]: '상주 상태가 워크플로를 제한했습니다. 에이전트가 동시에 쿼리할 때 데몬은 병목이 됩니다. Rust는 진정한 무상태와 밀리초 쿼리를 가능하게 합니다.',
        [K.Q5_Q]: 'ECP는 코드 파싱에서 아키텍처 이해로 나아가고 있나요?',
        [K.Q5_A]: '네. AI 에이전트의 최대 과제는 "제약 조건을 깨지 않는 것"으로 이동하고 있습니다. 고위험 패턴을 1급 신호로 격상시켜 레이더 역할을 합니다.',
        [K.Q6_Q]: 'AI 에이전트 시대에 문서는 어떻게 변할까요?',
        [K.Q6_A]: '인간이 프로세스 설계에 집중함에 따라 문서는 "순서도"로 전환될 것입니다. 세부 로직은 에이전트가 직접 소스 코드에서 가져와야 합니다.',
        [K.Q7_Q]: '스킬은 제약인가요, 가이드인가요?',
        [K.Q7_A]: '가이드여야 합니다. 그러나 핵심 원칙은 "모든 행동은 컨텍스트의 환각이 아닌 검증 가능한 구조적 사실에서 출발해야 한다"는 것입니다.',
        [K.TBL_ITEM]: '항목',
        [K.TBL_SAMPLE]: '.sample_repo (22k 파일)',
        [K.TBL_VSCODE]: 'VS Code (14k 파일)',
        [K.TBL_R1]: '리포지토리 실제 파일',
        [K.TBL_R2]: 'Graph File 노드',
        [K.TBL_R3]: 'Graph 크기',
        [K.TBL_R4]: 'Force index 피크 RSS',
        [K.TBL_R5]: 'Cold index',
        [K.TBL_R6]: 'Incremental analyze',
        [K.TBL_R7]: 'Cypher Class->Method',
        [K.TBL_R8]: 'Routes',
        [K.TBL_R9]: 'Inspect Class',
        [K.TBL_R10]: 'Find bm25',
        [K.TBL_R11]: 'Impact downstream',
        [K.TBL_R12]: 'Impact baseline HEAD~1',
        [K.TBL_NOTE]: '※ 하드웨어: AMD Ryzen 9 9950X, 39.2 GiB RAM, Linux 6.6.87',
        [K.VISION_QUOTE]: '"모든 행동은 컨텍스트의 환각이 아닌 검증 가능한 구조적 사실에서 출발해야 합니다."',
        [K.VISION_P1]: 'AI 에이전트 시대에 진정한 병목 현상은 <strong>신뢰</strong>입니다. 인간은 에이전트를 두려워하고 에이전트는 문맥에 현혹됩니다. Egent Code Plexus는 항상 <strong>소스 코드</strong>로 돌아가도록 신뢰를 구축합니다.',
        [K.VISION_P2]: '모두가 더 많은 에이전트를 동시에 구동하는 미래에는 컨텍스트를 채우는 것보다 초고속의 신뢰할 수 있는 구조 인식 도구를 갖는 것이 중요합니다.',
        [K.FOOTER_TEXT]: '&copy; 2026 Egent Code Plexus 프로젝트. Native Design Deep Dive에서 발췌.'
    },
    'es': {
        [K.META_TITLE]: 'Egent Code Plexus',
        [K.HERO_TAGLINE]: 'Built for agents, not IDEs.',
        [K.HERO_SUBTITLE]: 'Conciencia de la estructura del código y radar de arquitectura diseñado para Agentes de IA.',
        [K.INSTALL_MAC_LINUX]: 'macOS / Linux',
        [K.INSTALL_WINDOWS]: 'Windows (PowerShell)',
        [K.INSTALL_CARGO]: 'Cargo (Construir desde fuente)',
        [K.INSTALL_COPIED]: '¡Copiado!',
        [K.NAV_HIGHLIGHTS]: 'Destacados',
        [K.NAV_INTERVIEW]: 'Q&A de Desarrollo',
        [K.NAV_BENCHMARKS]: 'Métricas',
        [K.NAV_VISION]: 'Visión',
        [K.H_BLINDSPOT_TITLE]: 'Conciencia de BlindSpot',
        [K.H_BLINDSPOT_DESC]: 'La "ignorancia honesta" es mejor que las "conjeturas vagas". Marca límites del grafo para evitar que los Agentes asuman falsas dependencias.',
        [K.H_STATELESS_TITLE]: 'Sin estado & mmap',
        [K.H_STATELESS_DESC]: 'Arquitectura sin estado Rust + rkyv. Las consultas se completan en <150ms directamente vía mmap, ideal para Agentes concurrentes.',
        [K.H_RADAR_TITLE]: 'Radar de Arquitectura',
        [K.H_RADAR_DESC]: 'Detección integrada de patrones como Saga, EventTopics y contratos de API. Elevando la conciencia a las restricciones arquitectónicas.',
        [K.Q1_Q]: '¿Por qué es crucial la "Ignorancia Honesta"?',
        [K.Q1_A]: 'Saber qué es "desconocido" permite a los LLMs profundizar. Un Agente asume que "sin arista" significa "sin dependencia". BlindSpot previene esto.',
        [K.Q2_Q]: '¿Por qué una arquitectura mmap + rkyv sin estado?',
        [K.Q2_A]: 'La falta de estado reduce fallos. Un servidor requiere manejo de estado complejo. mmap comparte recursos instantáneamente sin un demonio.',
        [K.Q3_Q]: '¿Qué hay de la Gobernanza de Fusión de PRs?',
        [K.Q3_A]: 'Cambiando colas a "basadas en riesgo". ECP calcula el radio de impacto para detectar superposiciones semánticas, no solo rutas de archivos.',
        [K.Q4_Q]: '¿Cuál fue el punto de quiebre para pasar de Node.js a Rust?',
        [K.Q4_A]: 'El estado residente limitaba el flujo de trabajo. A altas frecuencias de consulta, los costos de GC/IPC de Node eran evidentes. Rust permite consultas milisegundo.',
        [K.Q5_Q]: '¿ECP pasa del análisis de código a la comprensión de arquitectura?',
        [K.Q5_A]: 'Sí. El mayor reto de los Agentes es "no romper restricciones existentes". ECP eleva los patrones de alto riesgo a señales de primera clase.',
        [K.Q6_Q]: '¿Qué pasará con la Documentación en la era de la IA?',
        [K.Q6_A]: 'Pivotará a "diagramas de flujo". Los detalles de bajo nivel deben ser extraídos directamente del Código Fuente por los Agentes, evitando desincronización.',
        [K.Q7_Q]: '¿Deben las Habilidades (Skills) restringir o guiar?',
        [K.Q7_A]: 'Deben guiar. Pero el principio central permanece: "Todas las acciones deben partir de hechos estructurales verificables, no de alucinaciones de contexto."',
        [K.TBL_ITEM]: 'Ítem',
        [K.TBL_SAMPLE]: '.sample_repo (22k archivos)',
        [K.TBL_VSCODE]: 'VS Code (14k archivos)',
        [K.TBL_R1]: 'Archivos del repositorio',
        [K.TBL_R2]: 'Nodos Graph File',
        [K.TBL_R3]: 'Tamaño del Grafo',
        [K.TBL_R4]: 'Pico RSS (force index)',
        [K.TBL_R5]: 'Cold index',
        [K.TBL_R6]: 'Incremental analyze',
        [K.TBL_R7]: 'Cypher Class->Method',
        [K.TBL_R8]: 'Routes',
        [K.TBL_R9]: 'Inspect Class',
        [K.TBL_R10]: 'Find bm25',
        [K.TBL_R11]: 'Impact downstream',
        [K.TBL_R12]: 'Impact baseline HEAD~1',
        [K.TBL_NOTE]: '※ Hardware: AMD Ryzen 9 9950X, 39.2 GiB RAM, Linux 6.6.87',
        [K.VISION_QUOTE]: '"Todas las acciones deben partir de hechos estructurales verificables, no de alucinaciones de contexto."',
        [K.VISION_P1]: 'En la era de la IA, el cuello de botella es la <strong>confianza</strong>. Egent Code Plexus asegura que los Agentes siempre regresen a la única verdad: <strong>el Código Fuente</strong>.',
        [K.VISION_P2]: 'A medida que impulsamos más Agentes y repositorios, lo que importa no es llenar la ventana de contexto, sino tener una herramienta fundacional ultra rápida y consciente de la estructura.',
        [K.FOOTER_TEXT]: '&copy; 2026 Proyecto Egent Code Plexus.'
    }
};

class I18nManager {
    constructor(defaultFallback = 'en') {
        this.translations = TRANSLATIONS;
        this.locales = LOCALES;
        this.currentLang = this.detectBrowserLanguage(defaultFallback);
        this.init();
    }

    detectBrowserLanguage(fallback) {
        const browserLang = navigator.language || navigator.userLanguage;
        if (!browserLang) return fallback;

        if (this.translations[browserLang]) {
            return browserLang;
        }

        const baseLang = browserLang.split('-')[0];
        if (this.translations[baseLang]) {
            return baseLang;
        }

        if (baseLang === 'zh') {
            return 'zh-TW';
        }

        return fallback;
    }

    init() {
        this.renderDropdown();
        this.bindEvents();
        this.updateDOM();
    }

    setLanguage(lang) {
        if (!this.translations[lang]) return;
        this.currentLang = lang;
        document.documentElement.lang = lang;
        this.updateDOM();
        this.updateDropdownUI();
    }

    updateDOM() {
        const t = this.translations[this.currentLang];
        document.querySelectorAll('[data-i18n]').forEach(el => {
            const key = el.getAttribute('data-i18n');
            if (t[key]) {
                if (el.tagName === 'TITLE') {
                    document.title = t[key];
                } else {
                    el.innerHTML = t[key];
                }
            }
        });

        // Dynamic QA Accordion Rendering
        if (window.INTERVIEW_QAS && window.INTERVIEW_QAS[this.currentLang]) {
            const qaContainer = document.getElementById('qa-container');
            if (qaContainer) {
                const qaData = window.INTERVIEW_QAS[this.currentLang];
                let html = '';
                qaData.forEach((qa, index) => {
                    // Pre-open the first item or restore previous state if needed, keeping it simple here
                    html += `
                        <div class="acc-item">
                            <button class="acc-trigger">
                                <span class="acc-q">${qa.q}</span>
                                <svg class="acc-icon" width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><line x1="12" y1="5" x2="12" y2="19"></line><line x1="5" y1="12" x2="19" y2="12"></line></svg>
                            </button>
                            <div class="acc-content">
                                <div class="acc-inner">${qa.a}</div>
                            </div>
                        </div>
                    `;
                });
                qaContainer.innerHTML = html;
                this.bindAccordionEvents();
            }
        }
    }

    bindAccordionEvents() {
        const accItems = document.querySelectorAll('.acc-item');
        accItems.forEach(item => {
            const trigger = item.querySelector('.acc-trigger');
            // Remove old listeners to prevent duplicates if re-rendered
            const newTrigger = trigger.cloneNode(true);
            trigger.parentNode.replaceChild(newTrigger, trigger);
            
            newTrigger.addEventListener('click', () => {
                const isActive = item.classList.contains('active');
                accItems.forEach(i => i.classList.remove('active'));
                if (!isActive) {
                    item.classList.add('active');
                }
            });
        });
    }

    renderDropdown() {
        const container = document.getElementById('lang-selector-container');
        if (!container) return;

        const currentLocale = this.locales.find(l => l.code === this.currentLang);

        let html = `
            <div class="custom-select" id="lang-selector">
                <button class="select-trigger mono" aria-haspopup="listbox" aria-expanded="false">
                    <span class="selected-lang">${currentLocale ? currentLocale.label : 'Language'}</span>
                    <svg class="chevron" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="6 9 12 15 18 9"></polyline></svg>
                </button>
                <ul class="options-list mono" role="listbox">
                    ${this.locales.map(l => `
                        <li role="option" data-value="${l.code}" class="option ${l.code === this.currentLang ? 'selected' : ''}">
                            ${l.label}
                        </li>
                    `).join('')}
                </ul>
            </div>
        `;
        container.innerHTML = html;
    }

    updateDropdownUI() {
        const currentLocale = this.locales.find(l => l.code === this.currentLang);
        const triggerLabel = document.querySelector('#lang-selector .selected-lang');
        if (triggerLabel && currentLocale) {
            triggerLabel.textContent = currentLocale.label;
        }

        document.querySelectorAll('#lang-selector .option').forEach(opt => {
            if (opt.getAttribute('data-value') === this.currentLang) {
                opt.classList.add('selected');
            } else {
                opt.classList.remove('selected');
            }
        });
    }

    bindEvents() {
        const container = document.getElementById('lang-selector-container');
        if (!container) return;

        container.addEventListener('click', (e) => {
            const select = document.getElementById('lang-selector');
            const trigger = select.querySelector('.select-trigger');
            
            if (e.target.closest('.select-trigger')) {
                const isExpanded = trigger.getAttribute('aria-expanded') === 'true';
                trigger.setAttribute('aria-expanded', !isExpanded);
                select.classList.toggle('open');
            }

            const option = e.target.closest('.option');
            if (option) {
                const value = option.getAttribute('data-value');
                this.setLanguage(value);
                trigger.setAttribute('aria-expanded', 'false');
                select.classList.remove('open');
            }
        });

        document.addEventListener('click', (e) => {
            const select = document.getElementById('lang-selector');
            if (select && !select.contains(e.target)) {
                select.classList.remove('open');
                const trigger = select.querySelector('.select-trigger');
                if (trigger) trigger.setAttribute('aria-expanded', 'false');
            }
        });
    }
}

document.addEventListener('DOMContentLoaded', () => {
    // Initialize i18n
    const i18n = new I18nManager();

    // Content Section Navigation
    const sectionBtns = document.querySelectorAll('.section-btn');
    const sections = document.querySelectorAll('.content-section');

    sectionBtns.forEach(btn => {
        btn.addEventListener('click', () => {
            const targetId = btn.getAttribute('data-target');
            
            sectionBtns.forEach(b => b.classList.remove('active'));
            btn.classList.add('active');
            
            sections.forEach(sec => {
                sec.classList.remove('active');
                void sec.offsetWidth; // Force reflow
            });
            document.getElementById(targetId).classList.add('active');
        });
    });

    // Install Tabs Navigation
    const installTabs = document.querySelectorAll('.install-tab');
    const installPanes = document.querySelectorAll('.install-pane');

    installTabs.forEach(tab => {
        tab.addEventListener('click', () => {
            const os = tab.getAttribute('data-os');
            
            installTabs.forEach(t => t.classList.remove('active'));
            tab.classList.add('active');
            
            installPanes.forEach(pane => pane.classList.remove('active'));
            document.getElementById(`pane-${os}`).classList.add('active');
        });
    });

    // Copy to Clipboard
    const copyBtns = document.querySelectorAll('.copy-btn');
    const toast = document.getElementById('toast');
    let toastTimeout;

    copyBtns.forEach(btn => {
        btn.addEventListener('click', () => {
            const textToCopy = btn.getAttribute('data-clipboard');
            navigator.clipboard.writeText(textToCopy).then(() => {
                toast.classList.add('show');
                clearTimeout(toastTimeout);
                toastTimeout = setTimeout(() => {
                    toast.classList.remove('show');
                }, 2000);
            }).catch(err => {
                console.error('Failed to copy text: ', err);
            });
        });
    });
});
