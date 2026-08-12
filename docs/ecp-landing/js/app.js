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

    NAV_INTEGRATIONS: 'nav.integrations',
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
    S_TOKENS_TITLE: 'strengths.tokens.title',
    S_TOKENS_DESC: 'strengths.tokens.desc',
    S_REFACTOR_TITLE: 'strengths.refactor.title',
    S_REFACTOR_DESC: 'strengths.refactor.desc',
    S_DISPATCH_TITLE: 'strengths.dispatch.title',
    S_DISPATCH_DESC: 'strengths.dispatch.desc',
    S_POLYGLOT_TITLE: 'strengths.polyglot.title',
    S_POLYGLOT_DESC: 'strengths.polyglot.desc',
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
    { code: 'es', label: 'Español' },
    { code: 'pt-BR', label: 'Português' },
    { code: 'ru', label: 'Русский' },
    { code: 'hi', label: 'हिन्दी' },
    { code: 'fr', label: 'Français' },
    { code: 'de', label: 'Deutsch' }
];

const TRANSLATIONS = {
    'zh-TW': {
        [K.S_TOKENS_TITLE]: '比 grep 省 7.5× tokens',
        [K.S_TOKENS_DESC]: '<code>ecp impact</code> 回傳的 3-hop 呼叫鏈約 111 tokens；等效 grep 輸出約 830 tokens——這還沒算 grep 之後必須補讀的檔案。實測於本專案。',
        [K.S_REFACTOR_TITLE]: '重構安全的邊語意',
        [K.S_REFACTOR_DESC]: '<code>is_test</code> / <code>is_direct</code> 旗標可排除測試呼叫者、辨識動態派發；<code>impact --literal</code> 區分檔案讀與寫——grep 在結構上做不到的區別。',
        [K.S_DISPATCH_TITLE]: '在 dispatch 當下攔截',
        [K.S_DISPATCH_DESC]: 'PreToolUse tripwire 把「探索整個 codebase」的 agent 派發改導向一次圖查詢——作用在靜態指引早已從模型注意力中流失的那個時刻。',
        [K.S_POLYGLOT_TITLE]: '31 種語言，一次遍歷',
        [K.S_POLYGLOT_DESC]: '服務程式碼、IaC、字串內 SQL 全部解析進同一張圖（<code>QueriesTable</code> 邊）——混合技術棧 repo 在單語言工具失明之處，圖譜仍然亮著。',
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
        [K.NAV_INTEGRATIONS]: '整合方式',
        [K.NAV_HIGHLIGHTS]: '技術亮點',
        [K.NAV_INTERVIEW]: '開發問答',
        [K.NAV_BENCHMARKS]: '效能實測',
        [K.NAV_VISION]: '未來願景',
        [K.H_BLINDSPOT_TITLE]: 'BlindSpot Awareness',
        [K.H_BLINDSPOT_DESC]: '「誠實的不知道」比「模糊的猜測」重要。ECP 明確標記圖譜邊界，防止 Agent 把「沒有邊」誤認為「沒有依賴」，從根本上解決 LLM 的閉世界幻覺。',
        [K.H_STATELESS_TITLE]: 'Stateless & mmap',
        [K.H_STATELESS_DESC]: '拋棄常駐 daemon 的負擔。基於 Rust + rkyv，每次查詢直接 mmap 圖檔後退出——實測 65–76 ms（含進程啟動成本）。為高頻並發的 Agent 查詢而生。',
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
        [K.NAV_SETUP]: '快速开始',
        [K.NAV_MATRIX]: '支持语言',
        [K.SETUP_STEP1_TITLE]: '1. 启动 AI Onboarding Wizard',
        [K.SETUP_STEP1_DESC]: '在您的 AI Agent（如 Claude Code）中贴上专属指令，唤醒交互式向导，为您完成环境检测与自动安装。',
        [K.SETUP_STEP2_TITLE]: '2. 建立索引（非必要 / Auto-Index）',
        [K.SETUP_STEP2_DESC]: 'ECP 内置 auto-ensure 机制，在您第一次查询时会自动建立图谱，因此手动执行索引通常是非必要的。',
        [K.SETUP_STEP3_TITLE]: '3. 多项目群组（Group）',
        [K.SETUP_STEP3_DESC]: '如果是微服务或前后端分离架构，可建立群组以实现跨 Repo 查询。',
        [K.SETUP_STEP4_TITLE]: '4. 确认 MCP 集成（Verify）',
        [K.SETUP_STEP4_DESC]: 'Onboarding 向导会自动为 IDE 写入 MCP 设置。完成后，您可通过 CLI 确认已暴露给 Agent 的工具清单。',
        [K.MAT_LEGEND]: '✓ 支持 | — 规划中 | n/a 语言无此特性',
        [K.MAT_TH_LANG]: '语言',
        [K.MAT_RATIONALE]: 'Per-cell rationale（详细解析逻辑）',
        [K.S_TOKENS_TITLE]: '比 grep 省 7.5× tokens',
        [K.S_TOKENS_DESC]: '<code>ecp impact</code> 返回的 3-hop 调用链约 111 tokens；等效 grep 输出约 830 tokens——这还没算 grep 之后必须补读的文件。实测于本项目。',
        [K.S_REFACTOR_TITLE]: '重构安全的边语义',
        [K.S_REFACTOR_DESC]: '<code>is_test</code> / <code>is_direct</code> 标志可排除测试调用者、识别动态派发；<code>impact --literal</code> 区分文件读与写——grep 在结构上做不到的区别。',
        [K.S_DISPATCH_TITLE]: '在 dispatch 当下拦截',
        [K.S_DISPATCH_DESC]: 'PreToolUse tripwire 把「探索整个 codebase」的 agent 派发改导向一次图查询——作用在静态指引早已从模型注意力中流失的那个时刻。',
        [K.S_POLYGLOT_TITLE]: '31 种语言，一次遍历',
        [K.S_POLYGLOT_DESC]: '服务代码、IaC、字符串内 SQL 全部解析进同一张图（<code>QueriesTable</code> 边）——混合技术栈 repo 在单语言工具失明之处，图谱仍然亮着。',
        [K.META_TITLE]: 'Egent Code Plexus',
        [K.HERO_TAGLINE]: 'Built for agents, not IDEs.',
        [K.HERO_SUBTITLE]: '专为 AI Agent 设计的代码结构感知与架构雷达',
        [K.INSTALL_MAC_LINUX]: 'macOS / Linux',
        [K.INSTALL_WINDOWS]: 'Windows (PowerShell)',
        [K.INSTALL_CARGO]: 'Cargo (从源码编译)',
        [K.INSTALL_COPIED]: '已复制！',
        [K.NAV_INTEGRATIONS]: '集成方式',
        [K.NAV_HIGHLIGHTS]: '技术亮点',
        [K.NAV_INTERVIEW]: '开发问答',
        [K.NAV_BENCHMARKS]: '性能实测',
        [K.NAV_VISION]: '未来愿景',
        [K.H_BLINDSPOT_TITLE]: 'BlindSpot Awareness',
        [K.H_BLINDSPOT_DESC]: '“诚实的不知道”比“模糊的猜测”重要。明确标记图谱边界，防止 Agent 把“没有边”误认为“没有依赖”，从根本上解决 LLM 的闭世界幻觉。',
        [K.H_STATELESS_TITLE]: 'Stateless & mmap',
        [K.H_STATELESS_DESC]: '抛弃常驻 daemon 的负担。基于 Rust + rkyv，每次查询直接 mmap 图文件后退出——实测 65–76 ms（含进程启动成本）。为高频并发的 Agent 查询而生。',
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
        [K.NAV_SETUP]: 'Quick Start',
        [K.NAV_MATRIX]: 'Languages',
        [K.SETUP_STEP1_TITLE]: '1. Launch the AI Onboarding Wizard',
        [K.SETUP_STEP1_DESC]: 'Paste the command into your AI agent (e.g. Claude Code) to wake an interactive wizard that checks your environment and installs everything for you.',
        [K.SETUP_STEP2_TITLE]: '2. Build the index (optional / auto-index)',
        [K.SETUP_STEP2_DESC]: 'ECP ships an auto-ensure mechanism: the graph is built automatically on your first query, so running the indexer manually is usually unnecessary.',
        [K.SETUP_STEP3_TITLE]: '3. Multi-repo groups',
        [K.SETUP_STEP3_DESC]: 'For microservices or split frontend/backend architectures, create a group to enable cross-repo queries.',
        [K.SETUP_STEP4_TITLE]: '4. Verify the MCP integration',
        [K.SETUP_STEP4_DESC]: 'The onboarding wizard writes the MCP config for your IDE automatically. Afterwards, list the tools exposed to your agent from the CLI.',
        [K.MAT_LEGEND]: '✓ supported | — planned | n/a not applicable to the language',
        [K.MAT_TH_LANG]: 'Language',
        [K.MAT_RATIONALE]: 'Per-cell rationale',
        [K.S_TOKENS_TITLE]: '7.5× fewer tokens than grep',
        [K.S_TOKENS_DESC]: 'A 3-hop caller chain from <code>ecp impact</code> costs ~111 tokens; the equivalent grep dump is ~830 — before the follow-up file reads grep still needs. Measured live on this repository.',
        [K.S_REFACTOR_TITLE]: 'Refactor-safe edge semantics',
        [K.S_REFACTOR_DESC]: '<code>is_test</code> / <code>is_direct</code> flags exclude test callers and expose dynamic dispatch; <code>impact --literal</code> tells file reads from writes — a distinction grep structurally cannot make.',
        [K.S_DISPATCH_TITLE]: 'Intercepts at the dispatch moment',
        [K.S_DISPATCH_DESC]: 'A PreToolUse tripwire redirects "explore the codebase" agent dispatches into one graph query — acting at the exact moment static guidance has already leaked from the model\'s attention.',
        [K.S_POLYGLOT_TITLE]: '31 languages, one traversal',
        [K.S_POLYGLOT_DESC]: 'Service code, IaC, and SQL inside string literals resolve into one graph (<code>QueriesTable</code> edges) — the graph stays lit exactly where mixed-stack repos go dark for single-language tools.',
        [K.META_TITLE]: 'Egent Code Plexus',
        [K.HERO_TAGLINE]: 'Built for agents, not IDEs.',
        [K.HERO_SUBTITLE]: 'Code structure awareness and architecture radar designed for AI Agents.',
        [K.INSTALL_MAC_LINUX]: 'macOS / Linux',
        [K.INSTALL_WINDOWS]: 'Windows (PowerShell)',
        [K.INSTALL_CARGO]: 'Cargo (Build from source)',
        [K.INSTALL_COPIED]: 'Copied!',
        [K.NAV_INTEGRATIONS]: 'Integrations',
        [K.NAV_HIGHLIGHTS]: 'Highlights',
        [K.NAV_INTERVIEW]: 'Dev Q&A',
        [K.NAV_BENCHMARKS]: 'Benchmarks',
        [K.NAV_VISION]: 'Vision',
        [K.H_BLINDSPOT_TITLE]: 'BlindSpot Awareness',
        [K.H_BLINDSPOT_DESC]: '"Honest ignorance" is better than "vague guessing." Explicitly marks graph boundaries to prevent Agents from mistaking "no edge" for "no dependency", solving closed-world hallucinations.',
        [K.H_STATELESS_TITLE]: 'Stateless & mmap',
        [K.H_STATELESS_DESC]: 'No daemon to babysit. Built on Rust + rkyv, each query mmaps the graph and exits — 65–76 ms wall-clock measured, spawn cost included. Built for high-frequency, concurrent agent queries.',
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
        [K.NAV_SETUP]: 'クイックスタート',
        [K.NAV_MATRIX]: '対応言語',
        [K.SETUP_STEP1_TITLE]: '1. AIオンボーディングウィザードを起動',
        [K.SETUP_STEP1_DESC]: 'AIエージェント（Claude Code など）にコマンドを貼り付けると、対話型ウィザードが環境チェックと自動インストールを行います。',
        [K.SETUP_STEP2_TITLE]: '2. インデックス作成（任意 / 自動）',
        [K.SETUP_STEP2_DESC]: 'ECP には auto-ensure 機構があり、最初のクエリ時にグラフを自動構築します。手動インデックスは通常不要です。',
        [K.SETUP_STEP3_TITLE]: '3. マルチリポジトリのグループ',
        [K.SETUP_STEP3_DESC]: 'マイクロサービスやフロント／バックエンド分離構成では、グループを作成してリポジトリ横断クエリを有効化できます。',
        [K.SETUP_STEP4_TITLE]: '4. MCP連携の確認',
        [K.SETUP_STEP4_DESC]: 'オンボーディングウィザードが IDE 用の MCP 設定を自動で書き込みます。完了後、CLI からエージェントに公開されたツール一覧を確認できます。',
        [K.MAT_LEGEND]: '✓ 対応 | — 計画中 | n/a 言語に該当機能なし',
        [K.MAT_TH_LANG]: '言語',
        [K.MAT_RATIONALE]: 'セル別の判定根拠',
        [K.S_TOKENS_TITLE]: 'grep より 7.5× 少ないトークン',
        [K.S_TOKENS_DESC]: '<code>ecp impact</code> の 3 ホップ呼び出しチェーンは約 111 トークン。同等の grep 出力は約 830 トークンで、その後のファイル読み込みは別途必要。本リポジトリで実測。',
        [K.S_REFACTOR_TITLE]: 'リファクタ安全なエッジ意味論',
        [K.S_REFACTOR_DESC]: '<code>is_test</code> / <code>is_direct</code> フラグでテスト呼び出し元を除外し動的ディスパッチを可視化。<code>impact --literal</code> はファイルの読み書きを区別 — grep には構造的に不可能。',
        [K.S_DISPATCH_TITLE]: 'ディスパッチの瞬間に介入',
        [K.S_DISPATCH_DESC]: 'PreToolUse トリップワイヤが「コード探索」エージェント派遣を 1 回のグラフクエリへ転送 — 静的ガイダンスがモデルの注意から漏れた瞬間に作用。',
        [K.S_POLYGLOT_TITLE]: '31言語をひとつの走査で',
        [K.S_POLYGLOT_DESC]: 'サービスコード、IaC、文字列内の SQL までひとつのグラフに解決（<code>QueriesTable</code> エッジ）。単一言語ツールが盲目になる混合スタックでもグラフは見えている。',
        [K.META_TITLE]: 'Egent Code Plexus',
        [K.HERO_TAGLINE]: 'Built for agents, not IDEs.',
        [K.HERO_SUBTITLE]: 'AIエージェント専用に設計されたコード構造認識とアーキテクチャレーダー',
        [K.INSTALL_MAC_LINUX]: 'macOS / Linux',
        [K.INSTALL_WINDOWS]: 'Windows (PowerShell)',
        [K.INSTALL_CARGO]: 'Cargo (ソースからビルド)',
        [K.INSTALL_COPIED]: 'コピーしました！',
        [K.NAV_INTEGRATIONS]: '連携方法',
        [K.NAV_HIGHLIGHTS]: 'ハイライト',
        [K.NAV_INTERVIEW]: '開発 Q&A',
        [K.NAV_BENCHMARKS]: 'ベンチマーク',
        [K.NAV_VISION]: 'ビジョン',
        [K.H_BLINDSPOT_TITLE]: 'BlindSpot の認識',
        [K.H_BLINDSPOT_DESC]: '「正直な無知」は「曖昧な推測」よりも重要です。グラフの境界を明示し、エージェントが「エッジなし」を「依存なし」と誤認するのを防ぎます。',
        [K.H_STATELESS_TITLE]: 'ステートレス & mmap',
        [K.H_STATELESS_DESC]: 'デーモン常駐の負担を排除。Rust + rkyv 上で各クエリはグラフを mmap して終了 — 実測 65–76 ms（プロセス起動コスト込み）。高頻度・並行のエージェントクエリ向け。',
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
        [K.NAV_SETUP]: '빠른 시작',
        [K.NAV_MATRIX]: '지원 언어',
        [K.SETUP_STEP1_TITLE]: '1. AI 온보딩 마법사 실행',
        [K.SETUP_STEP1_DESC]: 'AI 에이전트(예: Claude Code)에 명령을 붙여넣으면 대화형 마법사가 환경 점검과 자동 설치를 수행합니다.',
        [K.SETUP_STEP2_TITLE]: '2. 인덱스 생성(선택 / 자동)',
        [K.SETUP_STEP2_DESC]: 'ECP는 auto-ensure 메커니즘을 내장해 첫 쿼리 시 그래프를 자동 생성하므로 수동 인덱싱은 대개 불필요합니다.',
        [K.SETUP_STEP3_TITLE]: '3. 멀티 레포 그룹',
        [K.SETUP_STEP3_DESC]: '마이크로서비스나 프런트/백엔드 분리 구조라면 그룹을 만들어 레포 간 쿼리를 활성화하세요.',
        [K.SETUP_STEP4_TITLE]: '4. MCP 통합 확인',
        [K.SETUP_STEP4_DESC]: '온보딩 마법사가 IDE의 MCP 설정을 자동으로 작성합니다. 완료 후 CLI에서 에이전트에 노출된 도구 목록을 확인하세요.',
        [K.MAT_LEGEND]: '✓ 지원 | — 계획됨 | n/a 해당 언어에 없음',
        [K.MAT_TH_LANG]: '언어',
        [K.MAT_RATIONALE]: '셀별 판정 근거',
        [K.S_TOKENS_TITLE]: 'grep보다 7.5× 적은 토큰',
        [K.S_TOKENS_DESC]: '<code>ecp impact</code>의 3홉 호출 체인은 약 111 토큰. 동등한 grep 출력은 약 830 토큰이며 이후 파일 읽기는 별도입니다. 본 저장소에서 실측.',
        [K.S_REFACTOR_TITLE]: '리팩터링 안전한 엣지 시맨틱',
        [K.S_REFACTOR_DESC]: '<code>is_test</code> / <code>is_direct</code> 플래그로 테스트 호출자를 제외하고 동적 디스패치를 드러냅니다. <code>impact --literal</code>은 파일 읽기와 쓰기를 구분 — grep은 구조적으로 불가능.',
        [K.S_DISPATCH_TITLE]: '디스패치 순간에 개입',
        [K.S_DISPATCH_DESC]: 'PreToolUse 트립와이어가 \'코드 탐색\' 에이전트 디스패치를 그래프 쿼리 한 번으로 전환 — 정적 가이드가 모델의 주의에서 사라진 바로 그 순간에 작동.',
        [K.S_POLYGLOT_TITLE]: '31개 언어, 단일 순회',
        [K.S_POLYGLOT_DESC]: '서비스 코드, IaC, 문자열 안의 SQL까지 하나의 그래프로 해석(<code>QueriesTable</code> 엣지). 단일 언어 도구가 어두워지는 혼합 스택에서도 그래프는 밝게 유지됩니다.',
        [K.META_TITLE]: 'Egent Code Plexus',
        [K.HERO_TAGLINE]: 'Built for agents, not IDEs.',
        [K.HERO_SUBTITLE]: 'AI 에이전트를 위해 특별히 설계된 코드 구조 인식 및 아키텍처 레이더',
        [K.INSTALL_MAC_LINUX]: 'macOS / Linux',
        [K.INSTALL_WINDOWS]: 'Windows (PowerShell)',
        [K.INSTALL_CARGO]: 'Cargo (소스에서 빌드)',
        [K.INSTALL_COPIED]: '복사됨!',
        [K.NAV_INTEGRATIONS]: '연동 방법',
        [K.NAV_HIGHLIGHTS]: '하이라이트',
        [K.NAV_INTERVIEW]: '개발 Q&A',
        [K.NAV_BENCHMARKS]: '벤치마크',
        [K.NAV_VISION]: '비전',
        [K.H_BLINDSPOT_TITLE]: 'BlindSpot 인식',
        [K.H_BLINDSPOT_DESC]: '"솔직한 모름"이 "모호한 추측"보다 중요합니다. 그래프 경계를 명시하여 에이전트가 의존성이 없다고 착각하는 것을 방지합니다.',
        [K.H_STATELESS_TITLE]: '상태 비저장 & mmap',
        [K.H_STATELESS_DESC]: '상주 데몬의 부담 제거. Rust + rkyv 기반으로 각 쿼리는 그래프를 mmap 후 종료 — 실측 65–76 ms(프로세스 기동 비용 포함). 고빈도 병렬 에이전트 쿼리를 위한 설계.',
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
        [K.NAV_SETUP]: 'Inicio rápido',
        [K.NAV_MATRIX]: 'Lenguajes',
        [K.SETUP_STEP1_TITLE]: '1. Inicia el asistente de incorporación',
        [K.SETUP_STEP1_DESC]: 'Pega el comando en tu agente de IA (p. ej. Claude Code) para activar un asistente interactivo que verifica el entorno e instala todo por ti.',
        [K.SETUP_STEP2_TITLE]: '2. Construye el índice (opcional / automático)',
        [K.SETUP_STEP2_DESC]: 'ECP incluye un mecanismo auto-ensure: el grafo se construye automáticamente en tu primera consulta, por lo que indexar manualmente suele ser innecesario.',
        [K.SETUP_STEP3_TITLE]: '3. Grupos multi-repo',
        [K.SETUP_STEP3_DESC]: 'Para microservicios o arquitecturas con frontend/backend separados, crea un grupo para habilitar consultas entre repos.',
        [K.SETUP_STEP4_TITLE]: '4. Verifica la integración MCP',
        [K.SETUP_STEP4_DESC]: 'El asistente escribe la configuración MCP de tu IDE automáticamente. Después, lista las herramientas expuestas a tu agente desde la CLI.',
        [K.MAT_LEGEND]: '✓ soportado | — planificado | n/a no aplica al lenguaje',
        [K.MAT_TH_LANG]: 'Lenguaje',
        [K.MAT_RATIONALE]: 'Justificación por celda',
        [K.S_TOKENS_TITLE]: '7,5× menos tokens que grep',
        [K.S_TOKENS_DESC]: 'Una cadena de llamadas de 3 saltos de <code>ecp impact</code> cuesta ~111 tokens; el volcado equivalente de grep ~830 — antes de las lecturas de archivos que grep aún necesita. Medido en vivo en este repositorio.',
        [K.S_REFACTOR_TITLE]: 'Semántica de aristas segura para refactors',
        [K.S_REFACTOR_DESC]: 'Los flags <code>is_test</code> / <code>is_direct</code> excluyen llamadores de test y exponen el dispatch dinámico; <code>impact --literal</code> distingue lecturas de escrituras — algo que grep estructuralmente no puede.',
        [K.S_DISPATCH_TITLE]: 'Intercepta en el momento del despacho',
        [K.S_DISPATCH_DESC]: 'Un tripwire PreToolUse redirige los despachos de agentes "explora el código" a una sola consulta del grafo — actuando justo cuando la guía estática ya se ha perdido de la atención del modelo.',
        [K.S_POLYGLOT_TITLE]: '31 lenguajes, un solo recorrido',
        [K.S_POLYGLOT_DESC]: 'Código de servicios, IaC y SQL dentro de literales se resuelven en un solo grafo (aristas <code>QueriesTable</code>) — el grafo sigue iluminado justo donde los repos políglotas dejan a oscuras a las herramientas monolenguaje.',
        [K.META_TITLE]: 'Egent Code Plexus',
        [K.HERO_TAGLINE]: 'Built for agents, not IDEs.',
        [K.HERO_SUBTITLE]: 'Conciencia de la estructura del código y radar de arquitectura diseñado para Agentes de IA.',
        [K.INSTALL_MAC_LINUX]: 'macOS / Linux',
        [K.INSTALL_WINDOWS]: 'Windows (PowerShell)',
        [K.INSTALL_CARGO]: 'Cargo (Construir desde fuente)',
        [K.INSTALL_COPIED]: '¡Copiado!',
        [K.NAV_INTEGRATIONS]: 'Integraciones',
        [K.NAV_HIGHLIGHTS]: 'Destacados',
        [K.NAV_INTERVIEW]: 'Q&A de Desarrollo',
        [K.NAV_BENCHMARKS]: 'Métricas',
        [K.NAV_VISION]: 'Visión',
        [K.H_BLINDSPOT_TITLE]: 'Conciencia de BlindSpot',
        [K.H_BLINDSPOT_DESC]: 'La "ignorancia honesta" es mejor que las "conjeturas vagas". Marca límites del grafo para evitar que los Agentes asuman falsas dependencias.',
        [K.H_STATELESS_TITLE]: 'Sin estado & mmap',
        [K.H_STATELESS_DESC]: 'Sin demonios que cuidar. Sobre Rust + rkyv, cada consulta hace mmap del grafo y termina — 65–76 ms medidos, coste de arranque incluido. Hecho para consultas de agentes concurrentes y de alta frecuencia.',
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
    },
    'pt-BR': {
        [K.NAV_SETUP]: 'Início rápido',
        [K.NAV_MATRIX]: 'Linguagens',
        [K.SETUP_STEP1_TITLE]: '1. Inicie o assistente de onboarding de IA',
        [K.SETUP_STEP1_DESC]: 'Cole o comando no seu agente de IA (ex.: Claude Code) para acionar um assistente interativo que verifica seu ambiente e instala tudo para você.',
        [K.SETUP_STEP2_TITLE]: '2. Construa o índice (opcional / auto-indexação)',
        [K.SETUP_STEP2_DESC]: 'O ECP traz um mecanismo de auto-ensure: o grafo é construído automaticamente na primeira consulta, então rodar o indexador manualmente costuma ser desnecessário.',
        [K.SETUP_STEP3_TITLE]: '3. Grupos multi-repo',
        [K.SETUP_STEP3_DESC]: 'Para microsserviços ou arquiteturas com frontend e backend separados, crie um grupo para habilitar consultas entre repositórios.',
        [K.SETUP_STEP4_TITLE]: '4. Verifique a integração MCP',
        [K.SETUP_STEP4_DESC]: 'O assistente de onboarding grava a configuração MCP do seu IDE automaticamente. Depois, liste pela CLI as ferramentas expostas ao seu agente.',
        [K.MAT_LEGEND]: '✓ suportado | — planejado | n/a não se aplica à linguagem',
        [K.MAT_TH_LANG]: 'Linguagem',
        [K.MAT_RATIONALE]: 'Justificativa por célula',
        [K.S_TOKENS_TITLE]: '7.5× menos tokens que o grep',
        [K.S_TOKENS_DESC]: 'Uma cadeia de chamadores de 3 saltos via <code>ecp impact</code> custa ~111 tokens; o dump equivalente do grep fica em ~830 — antes das leituras de arquivo que o grep ainda exige em seguida. Medido ao vivo neste repositório.',
        [K.S_REFACTOR_TITLE]: 'Semântica de arestas segura para refatoração',
        [K.S_REFACTOR_DESC]: 'As flags <code>is_test</code> / <code>is_direct</code> excluem chamadores de teste e expõem despacho dinâmico; <code>impact --literal</code> distingue leituras de escritas de arquivo — uma distinção que o grep, por estrutura, não consegue fazer.',
        [K.S_DISPATCH_TITLE]: 'Intercepta no momento do despacho',
        [K.S_DISPATCH_DESC]: 'Um tripwire de PreToolUse redireciona despachos de agente do tipo "explorar o código" para uma única consulta ao grafo — agindo no momento exato em que a orientação estática já escapou da atenção do modelo.',
        [K.S_POLYGLOT_TITLE]: '31 linguagens, uma travessia',
        [K.S_POLYGLOT_DESC]: 'Código de serviço, IaC e SQL dentro de literais de string resolvem em um único grafo (arestas <code>QueriesTable</code>) — o grafo continua aceso exatamente onde repositórios de stack mista ficam apagados para ferramentas de linguagem única.',
        [K.META_TITLE]: 'Egent Code Plexus',
        [K.HERO_TAGLINE]: 'Criado para agentes, não para IDEs.',
        [K.HERO_SUBTITLE]: 'Percepção da estrutura do código e radar de arquitetura, projetados para agentes de IA.',
        [K.INSTALL_MAC_LINUX]: 'macOS / Linux',
        [K.INSTALL_WINDOWS]: 'Windows (PowerShell)',
        [K.INSTALL_CARGO]: 'Cargo (build do código-fonte)',
        [K.INSTALL_COPIED]: 'Copiado!',
        [K.NAV_INTEGRATIONS]: 'Integrações',
        [K.NAV_HIGHLIGHTS]: 'Destaques',
        [K.NAV_INTERVIEW]: 'Q&A do Dev',
        [K.NAV_BENCHMARKS]: 'Desempenho',
        [K.NAV_VISION]: 'Visão',
        [K.H_BLINDSPOT_TITLE]: 'Consciência de BlindSpots',
        [K.H_BLINDSPOT_DESC]: '"Ignorância honesta" é melhor que "palpite vago". Marca explicitamente as fronteiras do grafo para impedir que agentes confundam "sem aresta" com "sem dependência", resolvendo alucinações de mundo fechado.',
        [K.H_STATELESS_TITLE]: 'Sem estado & mmap',
        [K.H_STATELESS_DESC]: 'Nenhum daemon para manter ativo. Construído em Rust + rkyv, cada consulta faz mmap do grafo e encerra — 65–76 ms de wall-clock medidos, custo de spawn incluído. Feito para consultas de agentes concorrentes e de alta frequência.',
        [K.H_RADAR_TITLE]: 'Radar de Arquitetura',
        [K.H_RADAR_DESC]: 'Do simples encadeamento de AST para restrições de arquitetura de alto nível. Detecção embutida de padrões Saga, EventTopics e contratos de API entre serviços para expor riscos ocultos.',
        [K.Q1_Q]: 'Por que a "ignorância honesta" é crucial?',
        [K.Q1_A]: 'Saber o que é "desconhecido" permite que LLMs investiguem mais fundo. Um humano pode desconfiar de uma armadilha escondida, mas um agente assume que "sem aresta" significa "sem dependência". Os BlindSpots revelam honestamente as fronteiras do grafo.',
        [K.Q2_Q]: 'Por que uma arquitetura sem estado com mmap + rkyv?',
        [K.Q2_A]: 'A ausência de estado reduz os modos de falha. Um servidor de longa duração exige gerenciamento de estado e cache complexos, o que prejudica o desempenho. Com mmap, todos os agentes compartilham recursos somente leitura quase instantâneos sem depender de um daemon.',
        [K.Q3_Q]: 'E a governança de merge de PRs?',
        [K.Q3_A]: 'Mudar as filas de merge de "por tempo" para "por risco". O ECP calcula os símbolos modificados e seu raio de impacto (impact set) para detectar sobreposições semânticas entre PRs concorrentes, em vez de só checar caminhos de arquivo.',
        [K.Q4_Q]: 'Qual foi o ponto de ruptura para migrar de Node.js (GitNexus) para Rust?',
        [K.Q4_A]: 'O estado residente virou gargalo. Quando vários agentes consultam ao mesmo tempo, daemons criam problemas de coordenação. Em alta frequência de consultas (a cada edição/renomeação), os custos de GC/IPC do Node ficaram gritantes. Rust permite ausência de estado de verdade e consultas em milissegundos.',
        [K.Q5_Q]: 'O ECP está indo do parsing de código para o entendimento de arquitetura?',
        [K.Q5_A]: 'Sim. O maior desafio dos agentes de IA é passar de "escrever uma função corretamente" para "não quebrar restrições existentes". O ECP eleva padrões de alto risco (como Saga/EventTopics) a sinais de primeira classe, atuando como um radar de arquitetura.',
        [K.Q6_Q]: 'O que acontece com a documentação na era dos agentes de IA?',
        [K.Q6_A]: 'A documentação vai migrar para "fluxogramas", com humanos focados em gerenciar processos e design. Detalhes de baixo nível devem ser extraídos direto do código-fonte pelos agentes, evitando docs dessincronizadas. O código-fonte é a única verdade.',
        [K.Q7_Q]: 'As skills devem restringir ou orientar?',
        [K.Q7_A]: 'Skills devem orientar — como ensinar a humanidade a usar o fogo, não restringi-lo só a cozinhar. Mas o princípio central permanece: "Toda ação deve partir de fatos estruturais verificáveis, não de alucinações de contexto."',
        [K.TBL_ITEM]: 'Item',
        [K.TBL_SAMPLE]: '.sample_repo (22 mil arquivos)',
        [K.TBL_VSCODE]: 'VS Code (14 mil arquivos)',
        [K.TBL_R1]: 'Arquivos do repo',
        [K.TBL_R2]: 'Nós File do grafo',
        [K.TBL_R3]: 'Tamanho do grafo',
        [K.TBL_R4]: 'RSS de pico (force index)',
        [K.TBL_R5]: 'Índice a frio',
        [K.TBL_R6]: 'Análise incremental',
        [K.TBL_R7]: 'Cypher Class->Method',
        [K.TBL_R8]: 'Routes',
        [K.TBL_R9]: 'Inspect Class',
        [K.TBL_R10]: 'Find bm25',
        [K.TBL_R11]: 'Impact downstream',
        [K.TBL_R12]: 'Impact baseline HEAD~1',
        [K.TBL_NOTE]: '※ Hardware: AMD Ryzen 9 9950X, 39.2 GiB de RAM, Linux 6.6.87',
        [K.VISION_QUOTE]: '"Toda ação deve partir de fatos estruturais verificáveis, não de alucinações de contexto."',
        [K.VISION_P1]: 'Na era dos agentes de IA, a velocidade de desenvolvimento não é mais o gargalo; o gargalo é a <strong>confiança</strong>. Humanos temem que agentes quebrem sistemas, e agentes se deixam enganar facilmente pelo próprio contexto. O Egent Code Plexus constrói confiança sobre fatos estruturais, garantindo que os agentes sempre voltem à única verdade: o <strong>código-fonte</strong>.',
        [K.VISION_P2]: 'À medida que cada equipe e empresa opera mais agentes e repos em paralelo, o que importa não é enfiar mais coisa na janela de contexto, e sim ter uma ferramenta de base extremamente rápida, confiável e consciente da estrutura.',
        [K.FOOTER_TEXT]: '&copy; 2026 Projeto Egent Code Plexus. Trechos extraídos do Native Design Deep Dive.'
    },
    'ru': {
        [K.NAV_SETUP]: 'Быстрый старт',
        [K.NAV_MATRIX]: 'Языки',
        [K.SETUP_STEP1_TITLE]: '1. Запустите ИИ-мастер онбординга',
        [K.SETUP_STEP1_DESC]: 'Вставьте команду в свой ИИ-агент (например, Claude Code) — проснётся интерактивный мастер, который проверит окружение и установит всё за вас.',
        [K.SETUP_STEP2_TITLE]: '2. Постройте индекс (опционально / авто-индексация)',
        [K.SETUP_STEP2_DESC]: 'В ECP встроен механизм auto-ensure: граф строится автоматически при первом запросе, так что запускать индексатор вручную обычно не нужно.',
        [K.SETUP_STEP3_TITLE]: '3. Группы репозиториев',
        [K.SETUP_STEP3_DESC]: 'Для микросервисов или раздельных frontend/backend-архитектур создайте группу, чтобы выполнять запросы между репозиториями.',
        [K.SETUP_STEP4_TITLE]: '4. Проверьте интеграцию MCP',
        [K.SETUP_STEP4_DESC]: 'Мастер онбординга сам записывает конфигурацию MCP для вашей IDE. После этого выведите из CLI список инструментов, открытых вашему агенту.',
        [K.MAT_LEGEND]: '✓ поддерживается | — запланировано | n/a неприменимо к языку',
        [K.MAT_TH_LANG]: 'Язык',
        [K.MAT_RATIONALE]: 'Обоснование по ячейкам',
        [K.S_TOKENS_TITLE]: 'В 7.5× меньше токенов, чем grep',
        [K.S_TOKENS_DESC]: 'Цепочка вызывающих в 3 хопа из <code>ecp impact</code> стоит ~111 токенов; эквивалентный дамп grep — ~830, и это ещё без последующих чтений файлов, которые grep всё равно потребует. Измерено вживую на этом репозитории.',
        [K.S_REFACTOR_TITLE]: 'Семантика рёбер для безопасного рефакторинга',
        [K.S_REFACTOR_DESC]: 'Флаги <code>is_test</code> / <code>is_direct</code> исключают тестовых вызывающих и выявляют динамическую диспетчеризацию; <code>impact --literal</code> отличает чтение файла от записи — различие, которое grep структурно провести не может.',
        [K.S_DISPATCH_TITLE]: 'Перехват в момент диспетчеризации',
        [K.S_DISPATCH_DESC]: 'PreToolUse-триггер перенаправляет диспетчеризацию агентов «исследовать кодовую базу» в один запрос к графу — срабатывая ровно в тот момент, когда статические инструкции уже выпали из внимания модели.',
        [K.S_POLYGLOT_TITLE]: '31 язык, один обход',
        [K.S_POLYGLOT_DESC]: 'Сервисный код, IaC и SQL внутри строковых литералов разрешаются в один граф (рёбра <code>QueriesTable</code>) — граф остаётся освещённым ровно там, где репозитории со смешанным стеком темнеют для одноязычных инструментов.',
        [K.META_TITLE]: 'Egent Code Plexus',
        [K.HERO_TAGLINE]: 'Создан для агентов, а не для IDE.',
        [K.HERO_SUBTITLE]: 'Понимание структуры кода и архитектурный радар, созданные для ИИ-агентов.',
        [K.INSTALL_MAC_LINUX]: 'macOS / Linux',
        [K.INSTALL_WINDOWS]: 'Windows (PowerShell)',
        [K.INSTALL_CARGO]: 'Cargo (сборка из исходников)',
        [K.INSTALL_COPIED]: 'Скопировано!',
        [K.NAV_INTEGRATIONS]: 'Интеграции',
        [K.NAV_HIGHLIGHTS]: 'Особенности',
        [K.NAV_INTERVIEW]: 'Интервью',
        [K.NAV_BENCHMARKS]: 'Бенчмарки',
        [K.NAV_VISION]: 'Видение',
        [K.H_BLINDSPOT_TITLE]: 'Осведомлённость о BlindSpot',
        [K.H_BLINDSPOT_DESC]: '«Честная неизвестность» лучше «расплывчатых догадок». Границы графа помечаются явно, чтобы агент не принимал «нет ребра» за «нет зависимости», — это решает проблему галлюцинаций замкнутого мира.',
        [K.H_STATELESS_TITLE]: 'Без состояния и mmap',
        [K.H_STATELESS_DESC]: 'Никакого демона, за которым нужно присматривать. На базе Rust + rkyv каждый запрос делает mmap графа и завершается — измерено 65–76 ms реального времени, включая стоимость запуска процесса. Создан для частых конкурентных запросов агентов.',
        [K.H_RADAR_TITLE]: 'Архитектурный радар',
        [K.H_RADAR_DESC]: 'Шаг от простого связывания AST к высокоуровневым архитектурным ограничениям. Встроенное обнаружение паттернов Saga, EventTopics и межсервисных API-контрактов вскрывает скрытые риски.',
        [K.Q1_Q]: 'Почему «честная неизвестность» так важна?',
        [K.Q1_A]: 'Знание того, что именно «неизвестно», позволяет LLM копать глубже. Человек может заподозрить скрытую ловушку, а агент считает, что «нет ребра» значит «нет зависимости». Записи BlindSpot честно показывают границы графа.',
        [K.Q2_Q]: 'Почему архитектура без состояния на mmap + rkyv?',
        [K.Q2_A]: 'Отсутствие состояния сокращает число режимов сбоя. Долгоживущему серверу нужны сложное управление состоянием и кэширование, а это бьёт по производительности. С mmap все агенты почти мгновенно разделяют ресурсы только для чтения, не завися от демона.',
        [K.Q3_Q]: 'Как насчёт управления слиянием PR?',
        [K.Q3_A]: 'Перевод merge-очередей с принципа «по времени» на «по риску». ECP вычисляет изменённые символы и их радиус взрыва (impact set), чтобы находить семантические пересечения между параллельными PR, а не просто сравнивать пути файлов.',
        [K.Q4_Q]: 'Что стало переломным моментом для перехода с Node.js (GitNexus) на Rust?',
        [K.Q4_A]: 'Резидентное состояние стало узким местом. Когда несколько агентов делают запросы одновременно, демоны создают проблемы координации. При высокой частоте запросов (каждое редактирование или переименование) издержки GC/IPC в Node стали бросаться в глаза. Rust даёт настоящее отсутствие состояния и миллисекундные запросы.',
        [K.Q5_Q]: 'ECP движется от разбора кода к пониманию архитектуры?',
        [K.Q5_A]: 'Да. Главный вызов для ИИ-агентов — переход от «правильно написать функцию» к «не сломать существующие ограничения». ECP поднимает рискованные паттерны (вроде Saga/EventTopics) до сигналов первого класса и работает как архитектурный радар.',
        [K.Q6_Q]: 'Что будет с документацией в эпоху ИИ-агентов?',
        [K.Q6_A]: 'Документация сместится к «блок-схемам»: люди сосредоточатся на управлении процессами и дизайне. Низкоуровневые детали агенты должны извлекать напрямую из исходного кода — так документация не расходится с ним. Исходный код — единственная истина.',
        [K.Q7_Q]: 'Навыки должны ограничивать или направлять?',
        [K.Q7_A]: 'Навыки должны направлять — как научить человечество пользоваться огнём, а не разрешать ему только готовить. Но базовый принцип неизменен: «Все действия должны начинаться с проверяемых структурных фактов, а не с галлюцинаций контекста».',
        [K.TBL_ITEM]: 'Метрика',
        [K.TBL_SAMPLE]: '.sample_repo (22k файлов)',
        [K.TBL_VSCODE]: 'VS Code (14k файлов)',
        [K.TBL_R1]: 'Файлы репозитория',
        [K.TBL_R2]: 'Узлы File в графе',
        [K.TBL_R3]: 'Размер графа',
        [K.TBL_R4]: 'Пиковый RSS при force-индексации',
        [K.TBL_R5]: 'Холодная индексация',
        [K.TBL_R6]: 'Инкрементальный анализ',
        [K.TBL_R7]: 'Cypher Class->Method',
        [K.TBL_R8]: 'Маршруты',
        [K.TBL_R9]: 'Инспекция класса',
        [K.TBL_R10]: 'Поиск bm25',
        [K.TBL_R11]: 'Нисходящее влияние',
        [K.TBL_R12]: 'Базовое влияние HEAD~1',
        [K.TBL_NOTE]: '※ Оборудование: AMD Ryzen 9 9950X, 39.2 ГиБ ОЗУ, Linux 6.6.87',
        [K.VISION_QUOTE]: '«Все действия должны начинаться с проверяемых структурных фактов, а не с галлюцинаций контекста».',
        [K.VISION_P1]: 'В эпоху ИИ-агентов узкое место — уже не скорость разработки, а <strong>доверие</strong>. Люди боятся, что агенты сломают систему, а агентов легко сбивает с толку их собственный контекст. Egent Code Plexus строит доверие на структурных фактах, чтобы агенты всегда возвращались к единственной истине: <strong>исходному коду</strong>.',
        [K.VISION_P2]: 'По мере того как каждая команда и компания параллельно ведёт всё больше агентов и репозиториев, важно не запихнуть больше в контекстное окно, а иметь молниеносный, надёжный, понимающий структуру базовый инструмент.',
        [K.FOOTER_TEXT]: '&copy; 2026 Egent Code Plexus Project. Фрагменты взяты из Native Design Deep Dive.'
    },
    'hi': {
        [K.NAV_SETUP]: 'शुरुआत',
        [K.NAV_MATRIX]: 'भाषाएं',
        [K.SETUP_STEP1_TITLE]: '1. AI Onboarding Wizard शुरू करें',
        [K.SETUP_STEP1_DESC]: 'Command को अपने AI agent (जैसे Claude Code) में paste करें — एक interactive wizard जागेगा जो आपका environment जाँचता है और सब कुछ आपके लिए install कर देता है।',
        [K.SETUP_STEP2_TITLE]: '2. Index बनाएं (optional / auto-index)',
        [K.SETUP_STEP2_DESC]: 'ECP में auto-ensure mechanism built-in है: पहली query पर graph अपने आप बन जाता है, इसलिए indexer को manually चलाना आमतौर पर ज़रूरी नहीं।',
        [K.SETUP_STEP3_TITLE]: '3. Multi-repo group बनाएं',
        [K.SETUP_STEP3_DESC]: 'Microservices या अलग frontend/backend architecture के लिए एक group बनाएं ताकि cross-repo queries चल सकें।',
        [K.SETUP_STEP4_TITLE]: '4. MCP integration verify करें',
        [K.SETUP_STEP4_DESC]: 'Onboarding wizard आपके IDE के लिए MCP config अपने आप लिख देता है। इसके बाद CLI से देखें कि आपके agent को कौन से tools exposed हैं।',
        [K.MAT_LEGEND]: '✓ समर्थित | — योजना में | n/a language पर लागू नहीं',
        [K.MAT_TH_LANG]: 'भाषा',
        [K.MAT_RATIONALE]: 'हर cell का rationale',
        [K.S_TOKENS_TITLE]: 'grep से 7.5× कम tokens',
        [K.S_TOKENS_DESC]: '<code>ecp impact</code> से एक 3-hop caller chain ~111 tokens लेती है; बराबर का grep dump ~830 — और उसमें grep के बाद ज़रूरी follow-up file reads गिनी भी नहीं गईं। इसी repository पर live मापा गया।',
        [K.S_REFACTOR_TITLE]: 'Refactor के लिए सुरक्षित edge semantics',
        [K.S_REFACTOR_DESC]: '<code>is_test</code> / <code>is_direct</code> flags test callers को बाहर रखते हैं और dynamic dispatch उजागर करते हैं; <code>impact --literal</code> बताता है कि file read हो रही है या write — यह फ़र्क़ grep structurally कर ही नहीं सकता।',
        [K.S_DISPATCH_TITLE]: 'Dispatch के क्षण पर intercept',
        [K.S_DISPATCH_DESC]: 'एक PreToolUse tripwire "explore the codebase" जैसे agent dispatches को एक graph query में मोड़ देता है — ठीक उस क्षण काम करते हुए जब static guidance model के attention से पहले ही छूट चुकी होती है।',
        [K.S_POLYGLOT_TITLE]: '31 languages, एक traversal',
        [K.S_POLYGLOT_DESC]: 'Service code, IaC, और string literals के अंदर की SQL एक ही graph में resolve होते हैं (<code>QueriesTable</code> edges) — graph ठीक वहाँ रोशन रहता है जहाँ mixed-stack repos single-language tools के लिए अंधेरे में चले जाते हैं।',
        [K.META_TITLE]: 'Egent Code Plexus',
        [K.HERO_TAGLINE]: 'Agents के लिए बनाया गया, IDEs के लिए नहीं।',
        [K.HERO_SUBTITLE]: 'AI Agents के लिए design किया गया code structure awareness और architecture radar।',
        [K.INSTALL_MAC_LINUX]: 'macOS / Linux',
        [K.INSTALL_WINDOWS]: 'Windows (PowerShell)',
        [K.INSTALL_CARGO]: 'Cargo (source से build)',
        [K.INSTALL_COPIED]: 'Copy हो गया!',
        [K.NAV_INTEGRATIONS]: 'इंटीग्रेशन',
        [K.NAV_HIGHLIGHTS]: 'ख़ासियतें',
        [K.NAV_INTERVIEW]: 'Dev सवाल-जवाब',
        [K.NAV_BENCHMARKS]: 'बेंचमार्क',
        [K.NAV_VISION]: 'विज़न',
        [K.H_BLINDSPOT_TITLE]: 'BlindSpot जागरूकता',
        [K.H_BLINDSPOT_DESC]: '"धुंधले अनुमान" से बेहतर हैं "ईमानदार unknowns"। graph की boundaries को explicitly mark करता है ताकि Agents "no edge" को "no dependency" न समझ लें — closed-world hallucinations का हल।',
        [K.H_STATELESS_TITLE]: 'स्टेटलेस & mmap',
        [K.H_STATELESS_DESC]: 'कोई daemon जिंदा रखने की ज़रूरत नहीं। Rust + rkyv पर बना, हर query graph को mmap करती है और बाहर निकल जाती है — measured 65–76 ms wall-clock, spawn cost सहित। High-frequency, concurrent agent queries के लिए बनाया गया।',
        [K.H_RADAR_TITLE]: 'आर्किटेक्चर रडार',
        [K.H_RADAR_DESC]: 'साधारण AST linking से high-level architecture constraints तक की छलांग। Saga patterns, EventTopics, और cross-service API contracts की built-in detection — छुपे risks उजागर करने के लिए।',
        [K.Q1_Q]: '"ईमानदार unknowns" इतने अहम क्यों हैं?',
        [K.Q1_A]: 'क्या "unknown" है यह पता होना LLMs को और गहरे खोदने देता है। किसी मनुष्य को छुपे जाल का शक हो सकता है, पर Agent मान लेता है कि "no edge" मतलब "no dependency"। BlindSpots graph की boundaries ईमानदारी से दिखा देते हैं।',
        [K.Q2_Q]: 'Stateless mmap + rkyv architecture ही क्यों?',
        [K.Q2_A]: 'Statelessness failure modes घटाती है। Long-running server को जटिल state management और caching चाहिए, जो performance को नुकसान पहुँचाती है। mmap के साथ सभी agents बिना किसी daemon पर निर्भर हुए near-instant read-only resources share करते हैं।',
        [K.Q3_Q]: 'PR Merge Governance का क्या?',
        [K.Q3_A]: 'Merge queues को "time-based" से "risk-based" पर ले जाना। ECP modified symbols और उनका blast radius (impact set) निकालता है ताकि concurrent PRs के बीच semantic overlaps पकड़े जा सकें — सिर्फ़ file paths जाँचने के बजाय।',
        [K.Q4_Q]: 'Node.js (GitNexus) से Rust पर जाने का breaking point क्या था?',
        [K.Q4_A]: 'Resident state bottleneck बन गया। कई Agents एक साथ query करें तो daemons coordination की समस्याएँ खड़ी करते हैं। ऊँची query frequency (हर edit/rename) पर Node की GC/IPC लागत चुभने लगी। Rust सच्ची statelessness और millisecond queries संभव करता है।',
        [K.Q5_Q]: 'क्या ECP code parsing से architecture understanding की ओर बढ़ रहा है?',
        [K.Q5_A]: 'हाँ। AI Agents की सबसे बड़ी चुनौती "function सही लिखने" से "मौजूदा constraints न तोड़ने" पर shift होना है। ECP high-risk patterns (जैसे Saga/EventTopics) को first-class signals बना देता है — एक architecture radar की तरह।',
        [K.Q6_Q]: 'AI Agent युग में Documentation का क्या होगा?',
        [K.Q6_A]: 'Documentation "flowcharts" की ओर मुड़ेगी, क्योंकि मनुष्य processes और design के प्रबंधन पर ध्यान देंगे। Low-level details Agents को सीधे Source Code से निकालनी चाहिए — out-of-sync docs से बचने का यही रास्ता है। Source code ही एकमात्र सच है।',
        [K.Q7_Q]: 'Skills बाँधें या राह दिखाएं?',
        [K.Q7_A]: 'Skills को राह दिखानी चाहिए — जैसे मानवता को आग का उपयोग सिखाना, न कि उसे सिर्फ़ खाना पकाने तक सीमित करना। फिर भी core सिद्धांत वही रहता है: "हर action verifiable structural facts से शुरू हो, context hallucinations से नहीं।"',
        [K.TBL_ITEM]: 'आइटम',
        [K.TBL_SAMPLE]: '.sample_repo (22k फ़ाइलें)',
        [K.TBL_VSCODE]: 'VS Code (14k फ़ाइलें)',
        [K.TBL_R1]: 'Repo फ़ाइलें',
        [K.TBL_R2]: 'Graph में File nodes',
        [K.TBL_R3]: 'Graph का आकार',
        [K.TBL_R4]: 'Force index का peak RSS',
        [K.TBL_R5]: 'Cold index समय',
        [K.TBL_R6]: 'Incremental analyze समय',
        [K.TBL_R7]: 'Cypher Class->Method समय',
        [K.TBL_R8]: 'Routes समय',
        [K.TBL_R9]: 'Inspect Class समय',
        [K.TBL_R10]: 'Find bm25 समय',
        [K.TBL_R11]: 'Impact downstream समय',
        [K.TBL_R12]: 'Impact baseline HEAD~1 समय',
        [K.TBL_NOTE]: '※ Hardware: AMD Ryzen 9 9950X, 39.2 GiB RAM, Linux 6.6.87',
        [K.VISION_QUOTE]: '"हर action verifiable structural facts से शुरू हो, context hallucinations से नहीं।"',
        [K.VISION_P1]: 'AI Agent युग में development की रफ़्तार अब bottleneck नहीं रही; bottleneck है <strong>भरोसा</strong>। मनुष्यों को डर है कि Agents systems तोड़ देंगे, और Agents अपने ही context से आसानी से भटक जाते हैं। Egent Code Plexus भरोसे को structural facts पर खड़ा करता है, ताकि Agents हमेशा एकमात्र सच पर लौटें: <strong>Source Code</strong>।',
        [K.VISION_P2]: 'जैसे-जैसे हर team और company ज़्यादा Agents और repos एक साथ चलाती है, असली सवाल context window में और ठूँसने का नहीं, बल्कि एक बेहद तेज़, भरोसेमंद, structure-aware बुनियादी tool रखने का है।',
        [K.FOOTER_TEXT]: '&copy; 2026 Egent Code Plexus Project. अंश Native Design Deep Dive से लिए गए हैं।'
    },
    'fr': {
        [K.NAV_SETUP]: 'Démarrage rapide',
        [K.NAV_MATRIX]: 'Langages',
        [K.SETUP_STEP1_TITLE]: "1. Lancez l'assistant d'onboarding IA",
        [K.SETUP_STEP1_DESC]: 'Collez la commande dans votre agent IA (par ex. Claude Code) pour réveiller un assistant interactif qui vérifie votre environnement et installe tout pour vous.',
        [K.SETUP_STEP2_TITLE]: "2. Construisez l'index (optionnel / auto-index)",
        [K.SETUP_STEP2_DESC]: "ECP embarque un mécanisme d'auto-ensure : le graphe se construit automatiquement à la première requête, lancer l'indexation à la main est donc rarement nécessaire.",
        [K.SETUP_STEP3_TITLE]: '3. Groupes multi-dépôts',
        [K.SETUP_STEP3_DESC]: 'Pour les microservices ou les architectures frontend/backend séparées, créez un groupe pour activer les requêtes inter-dépôts.',
        [K.SETUP_STEP4_TITLE]: "4. Vérifiez l'intégration MCP",
        [K.SETUP_STEP4_DESC]: "L'assistant d'onboarding écrit automatiquement la config MCP de votre IDE. Ensuite, listez depuis la CLI les outils exposés à votre agent.",
        [K.MAT_LEGEND]: '✓ pris en charge | — prévu | n/a sans objet pour ce langage',
        [K.MAT_TH_LANG]: 'Langage',
        [K.MAT_RATIONALE]: 'Justification par cellule',
        [K.S_TOKENS_TITLE]: '7.5× moins de tokens que grep',
        [K.S_TOKENS_DESC]: "Une chaîne d'appelants sur 3 sauts via <code>ecp impact</code> coûte ~111 tokens ; le dump grep équivalent en fait ~830, avant même les lectures de fichiers que grep exige ensuite. Mesuré en direct sur ce dépôt.",
        [K.S_REFACTOR_TITLE]: 'Des arêtes sûres pour le refactoring',
        [K.S_REFACTOR_DESC]: 'Les flags <code>is_test</code> / <code>is_direct</code> excluent les appelants de test et révèlent le dispatch dynamique ; <code>impact --literal</code> distingue les lectures de fichiers des écritures, une distinction que grep est structurellement incapable de faire.',
        [K.S_DISPATCH_TITLE]: 'Intercepte au moment du dispatch',
        [K.S_DISPATCH_DESC]: "Un déclencheur PreToolUse redirige les dispatchs d'agents « explore la base de code » vers une seule requête de graphe, au moment précis où les consignes statiques ont déjà quitté l'attention du modèle.",
        [K.S_POLYGLOT_TITLE]: '31 langages, une seule traversée',
        [K.S_POLYGLOT_DESC]: 'Code des services, IaC et SQL dans les chaînes de caractères se résolvent en un seul graphe (arêtes <code>QueriesTable</code>) : le graphe reste éclairé exactement là où les dépôts multi-stack deviennent opaques pour les outils mono-langage.',
        [K.META_TITLE]: 'Egent Code Plexus',
        [K.HERO_TAGLINE]: 'Conçu pour les agents, pas pour les IDE.',
        [K.HERO_SUBTITLE]: "Conscience de la structure du code et radar d'architecture, conçus pour les agents IA.",
        [K.INSTALL_MAC_LINUX]: 'macOS / Linux',
        [K.INSTALL_WINDOWS]: 'Windows (PowerShell)',
        [K.INSTALL_CARGO]: 'Cargo (compiler depuis les sources)',
        [K.INSTALL_COPIED]: 'Copié !',
        [K.NAV_INTEGRATIONS]: 'Intégrations',
        [K.NAV_HIGHLIGHTS]: 'Points forts',
        [K.NAV_INTERVIEW]: 'Q&R dev',
        [K.NAV_BENCHMARKS]: 'Benchmarks',
        [K.NAV_VISION]: 'Vision',
        [K.H_BLINDSPOT_TITLE]: 'Conscience des BlindSpots',
        [K.H_BLINDSPOT_DESC]: "« L'ignorance honnête » vaut mieux que « la supposition vague ». Les frontières du graphe sont marquées explicitement, pour éviter qu'un agent confonde « pas d'arête » avec « pas de dépendance » et régler les hallucinations en monde clos.",
        [K.H_STATELESS_TITLE]: 'Sans état & mmap',
        [K.H_STATELESS_DESC]: "Aucun démon à surveiller. Construit en Rust + rkyv : chaque requête mmap le graphe puis se termine, 65–76 ms mesurés au chrono, lancement du processus compris. Conçu pour des requêtes d'agents fréquentes et concurrentes.",
        [K.H_RADAR_TITLE]: "Radar d'architecture",
        [K.H_RADAR_DESC]: "Au-delà du simple lien AST : des contraintes d'architecture de haut niveau. Détection intégrée des patterns Saga, des EventTopics et des contrats d'API inter-services pour révéler les risques cachés.",
        [K.Q1_Q]: "Pourquoi « l'ignorance honnête » est-elle cruciale ?",
        [K.Q1_A]: "Savoir ce qui est « inconnu » permet aux LLM de creuser. Un humain flairerait un piège caché ; un agent, lui, suppose que « pas d'arête » veut dire « pas de dépendance ». Les BlindSpots exposent honnêtement les frontières du graphe.",
        [K.Q2_Q]: 'Pourquoi une architecture sans état, mmap + rkyv ?',
        [K.Q2_A]: "L'absence d'état réduit les modes de défaillance. Un serveur résident impose une gestion d'état et un cache complexes, au détriment des performances. Avec mmap, tous les agents partagent des ressources en lecture seule quasi instantanées, sans dépendre d'un démon.",
        [K.Q3_Q]: 'Et la gouvernance du merge des PR ?',
        [K.Q3_A]: "Faire passer les files de merge d'une logique « temporelle » à une logique « par risque ». ECP calcule les symboles modifiés et leur rayon d'impact (impact set) pour détecter les chevauchements sémantiques entre PR concurrentes, au lieu de comparer de simples chemins de fichiers.",
        [K.Q4_Q]: "Qu'est-ce qui a provoqué le passage de Node.js (GitNexus) à Rust ?",
        [K.Q4_A]: "L'état résident est devenu le goulot d'étranglement. Quand plusieurs agents interrogent en même temps, les démons posent des problèmes de coordination. À haute fréquence de requêtes (chaque édition, chaque renommage), les coûts GC/IPC de Node sont devenus criants. Rust permet un vrai sans-état et des requêtes en millisecondes.",
        [K.Q5_Q]: "ECP passe-t-il du parsing de code à la compréhension d'architecture ?",
        [K.Q5_A]: "Oui. Le vrai défi pour les agents IA n'est plus « écrire une fonction correcte » mais « ne pas casser les contraintes existantes ». ECP élève les patterns à risque (Saga, EventTopics) au rang de signaux de premier ordre et joue le rôle de radar d'architecture.",
        [K.Q6_Q]: "Que devient la documentation à l'ère des agents IA ?",
        [K.Q6_A]: 'La documentation basculera vers les « diagrammes de flux », les humains se concentrant sur les processus et la conception. Les détails de bas niveau doivent être extraits du code source par les agents, pour éviter les docs désynchronisées. Le code source est la seule vérité.',
        [K.Q7_Q]: 'Les skills doivent-ils contraindre ou guider ?',
        [K.Q7_A]: "Guider. Comme apprendre à l'humanité à se servir du feu, plutôt que de le cantonner à la cuisine. Le principe de fond reste : « toute action doit partir de faits structurels vérifiables, pas d'hallucinations de contexte ».",
        [K.TBL_ITEM]: 'Élément',
        [K.TBL_SAMPLE]: '.sample_repo (22k fichiers)',
        [K.TBL_VSCODE]: 'VS Code (14k fichiers)',
        [K.TBL_R1]: 'Fichiers du dépôt',
        [K.TBL_R2]: 'Nœuds File du graphe',
        [K.TBL_R3]: 'Taille du graphe',
        [K.TBL_R4]: 'Pic RSS, indexation forcée',
        [K.TBL_R5]: 'Indexation à froid',
        [K.TBL_R6]: 'Analyze incrémental',
        [K.TBL_R7]: 'Cypher Class->Method',
        [K.TBL_R8]: 'Routes',
        [K.TBL_R9]: 'Inspect Class',
        [K.TBL_R10]: 'Find bm25',
        [K.TBL_R11]: 'Impact downstream',
        [K.TBL_R12]: 'Impact baseline HEAD~1',
        [K.TBL_NOTE]: '※ Matériel : AMD Ryzen 9 9950X, 39.2 GiB de RAM, Linux 6.6.87',
        [K.VISION_QUOTE]: "« Toute action doit partir de faits structurels vérifiables, pas d'hallucinations de contexte. »",
        [K.VISION_P1]: "À l'ère des agents IA, la vitesse de développement n'est plus le goulot d'étranglement ; le goulot, c'est la <strong>confiance</strong>. Les humains craignent qu'un agent casse le système, et les agents se laissent égarer par leur propre contexte. Egent Code Plexus fonde la confiance sur des faits structurels, pour que les agents reviennent toujours à la seule vérité : le <strong>code source</strong>.",
        [K.VISION_P2]: "À mesure que chaque équipe et chaque entreprise fait tourner plus d'agents et de dépôts en parallèle, l'enjeu n'est pas d'entasser davantage dans la fenêtre de contexte, mais de disposer d'un outil de fond ultra-rapide, fiable et conscient de la structure.",
        [K.FOOTER_TEXT]: '&copy; 2026 Projet Egent Code Plexus. Extraits du Native Design Deep Dive.'
    },
    'de': {
        [K.NAV_SETUP]: 'Schnellstart',
        [K.NAV_MATRIX]: 'Sprachen',
        [K.SETUP_STEP1_TITLE]: '1. KI-Onboarding-Assistent starten',
        [K.SETUP_STEP1_DESC]: 'Füge den Befehl in deinen KI-Agenten ein (z. B. Claude Code). Ein interaktiver Assistent prüft deine Umgebung und installiert alles Nötige.',
        [K.SETUP_STEP2_TITLE]: '2. Index bauen (optional / Auto-Index)',
        [K.SETUP_STEP2_DESC]: 'ECP bringt einen Auto-Ensure-Mechanismus mit: Der Graph wird bei der ersten Abfrage automatisch gebaut, den Indexer manuell zu starten ist meist unnötig.',
        [K.SETUP_STEP3_TITLE]: '3. Multi-Repo-Gruppen',
        [K.SETUP_STEP3_DESC]: 'Bei Microservices oder getrenntem Frontend/Backend legst du eine Gruppe an und schaltest damit repoübergreifende Abfragen frei.',
        [K.SETUP_STEP4_TITLE]: '4. MCP-Integration prüfen',
        [K.SETUP_STEP4_DESC]: 'Der Onboarding-Assistent schreibt die MCP-Konfiguration für deine IDE automatisch. Danach listest du über die CLI auf, welche Tools dein Agent sieht.',
        [K.MAT_LEGEND]: '✓ unterstützt | — geplant | n/a für die Sprache nicht anwendbar',
        [K.MAT_TH_LANG]: 'Sprache',
        [K.MAT_RATIONALE]: 'Begründung pro Zelle',
        [K.S_TOKENS_TITLE]: '7.5× weniger Tokens als grep',
        [K.S_TOKENS_DESC]: 'Eine Aufruferkette über 3 Hops aus <code>ecp impact</code> kostet ~111 Tokens; der äquivalente grep-Dump ~830 – noch ohne die Datei-Reads, die grep danach ohnehin braucht. Live an diesem Repository gemessen.',
        [K.S_REFACTOR_TITLE]: 'Refactoring-sichere Kantensemantik',
        [K.S_REFACTOR_DESC]: '<code>is_test</code>- / <code>is_direct</code>-Flags filtern Test-Aufrufer heraus und machen dynamischen Dispatch sichtbar; <code>impact --literal</code> unterscheidet Datei-Lesezugriffe von Schreibzugriffen – eine Unterscheidung, die grep strukturell nicht treffen kann.',
        [K.S_DISPATCH_TITLE]: 'Greift im Dispatch-Moment ein',
        [K.S_DISPATCH_DESC]: 'Ein PreToolUse-Tripwire leitet Agent-Dispatches wie „erkunde die Codebasis“ in eine einzige Graph-Abfrage um – und greift genau dann, wenn statische Anweisungen der Aufmerksamkeit des Modells längst entglitten sind.',
        [K.S_POLYGLOT_TITLE]: '31 Sprachen, eine Traversierung',
        [K.S_POLYGLOT_DESC]: 'Service-Code, IaC und SQL in String-Literalen landen in einem gemeinsamen Graphen (<code>QueriesTable</code>-Kanten) – der Graph leuchtet genau dort, wo Mixed-Stack-Repos für einsprachige Tools im Dunkeln liegen.',
        [K.META_TITLE]: 'Egent Code Plexus',
        [K.HERO_TAGLINE]: 'Für Agenten gebaut, nicht für IDEs.',
        [K.HERO_SUBTITLE]: 'Codestruktur-Verständnis und Architektur-Radar, entwickelt für KI-Agenten.',
        [K.INSTALL_MAC_LINUX]: 'macOS / Linux',
        [K.INSTALL_WINDOWS]: 'Windows (PowerShell)',
        [K.INSTALL_CARGO]: 'Cargo (aus dem Quellcode bauen)',
        [K.INSTALL_COPIED]: 'Kopiert!',
        [K.NAV_INTEGRATIONS]: 'Integrationen',
        [K.NAV_HIGHLIGHTS]: 'Highlights',
        [K.NAV_INTERVIEW]: 'Dev-Q&A',
        [K.NAV_BENCHMARKS]: 'Benchmarks',
        [K.NAV_VISION]: 'Vision',
        [K.H_BLINDSPOT_TITLE]: 'BlindSpot-Awareness',
        [K.H_BLINDSPOT_DESC]: '„Ehrliches Nichtwissen“ schlägt „vages Raten“. Graphgrenzen werden explizit markiert, damit Agenten „keine Kante“ nicht mit „keine Abhängigkeit“ verwechseln – das löst Closed-World-Halluzinationen.',
        [K.H_STATELESS_TITLE]: 'Zustandslos & mmap',
        [K.H_STATELESS_DESC]: 'Kein Daemon, der gepflegt werden will. Auf Rust + rkyv gebaut: Jede Abfrage mappt den Graphen per mmap und beendet sich – gemessene 65–76 ms Wall-Clock, Spawn-Kosten inklusive. Ausgelegt auf hochfrequente, parallele Agent-Abfragen.',
        [K.H_RADAR_TITLE]: 'Architektur-Radar',
        [K.H_RADAR_DESC]: 'Vom simplen AST-Linking hinauf zu Architektur-Constraints auf höherer Ebene. Eingebaute Erkennung für Saga-Muster, EventTopics und serviceübergreifende API-Contracts deckt versteckte Risiken auf.',
        [K.Q1_Q]: 'Warum ist „ehrliches Nichtwissen“ entscheidend?',
        [K.Q1_A]: 'Zu wissen, was „unbekannt“ ist, lässt LLMs gezielt nachgraben. Ein Mensch ahnt vielleicht eine versteckte Falle, ein Agent nimmt an, „keine Kante“ heiße „keine Abhängigkeit“. BlindSpots legen die Grenzen des Graphen ehrlich offen.',
        [K.Q2_Q]: 'Warum eine zustandslose Architektur mit mmap + rkyv?',
        [K.Q2_A]: 'Zustandslosigkeit reduziert Fehlermodi. Ein dauerhaft laufender Server braucht komplexes State-Management und Caching, was auf die Performance geht. Mit mmap teilen sich alle Agenten nahezu verzögerungsfreie Read-only-Ressourcen, ganz ohne Daemon.',
        [K.Q3_Q]: 'Wie steht es um PR-Merge-Governance?',
        [K.Q3_A]: 'Merge-Queues wandern von „zeitbasiert“ zu „risikobasiert“. ECP berechnet geänderte Symbole und ihren Blast Radius (Impact-Set) und erkennt so semantische Überschneidungen zwischen parallelen PRs, statt nur Dateipfade zu vergleichen.',
        [K.Q4_Q]: 'Was war der Auslöser für den Wechsel von Node.js (GitNexus) zu Rust?',
        [K.Q4_A]: 'Residenter State wurde zum Engpass. Wenn mehrere Agenten gleichzeitig abfragen, erzeugen Daemons Koordinationsprobleme. Bei hoher Abfragefrequenz (jedes Edit/Rename) fielen Nodes GC-/IPC-Kosten deutlich ins Gewicht. Rust ermöglicht echte Zustandslosigkeit und Abfragen im Millisekundenbereich.',
        [K.Q5_Q]: 'Bewegt sich ECP vom Code-Parsing zum Architekturverständnis?',
        [K.Q5_A]: 'Ja. Die größte Herausforderung für KI-Agenten verschiebt sich von „eine Funktion korrekt schreiben“ zu „bestehende Constraints nicht brechen“. ECP hebt Hochrisiko-Muster (etwa Saga/EventTopics) zu First-Class-Signalen und wirkt so als Architektur-Radar.',
        [K.Q6_Q]: 'Was passiert mit Dokumentation im Zeitalter der KI-Agenten?',
        [K.Q6_A]: 'Dokumentation verlagert sich zu „Flussdiagrammen“, während Menschen sich auf Prozesse und Design konzentrieren. Low-Level-Details sollten Agenten direkt aus dem Quellcode ziehen – das vermeidet veraltete Doku. Der Quellcode ist die einzige Wahrheit.',
        [K.Q7_Q]: 'Sollen Skills einschränken oder anleiten?',
        [K.Q7_A]: 'Skills sollen anleiten – so wie man der Menschheit den Umgang mit Feuer beibringt, statt es aufs Kochen zu beschränken. Das Kernprinzip bleibt trotzdem: „Jede Aktion beginnt bei verifizierbaren strukturellen Fakten, nicht bei Kontext-Halluzinationen.“',
        [K.TBL_ITEM]: 'Metrik',
        [K.TBL_SAMPLE]: '.sample_repo (22k Dateien)',
        [K.TBL_VSCODE]: 'VS Code (14k Dateien)',
        [K.TBL_R1]: 'Repo-Dateien',
        [K.TBL_R2]: 'File-Knoten im Graph',
        [K.TBL_R3]: 'Graphgröße',
        [K.TBL_R4]: 'Force-Index Peak-RSS',
        [K.TBL_R5]: 'Cold Index',
        [K.TBL_R6]: 'Inkrementelles analyze',
        [K.TBL_R7]: 'Cypher Class->Method',
        [K.TBL_R8]: 'Routes',
        [K.TBL_R9]: 'Inspect Class',
        [K.TBL_R10]: 'Find bm25',
        [K.TBL_R11]: 'Impact downstream',
        [K.TBL_R12]: 'Impact baseline HEAD~1',
        [K.TBL_NOTE]: '※ Hardware: AMD Ryzen 9 9950X, 39.2 GiB RAM, Linux 6.6.87',
        [K.VISION_QUOTE]: '„Jede Aktion beginnt bei verifizierbaren strukturellen Fakten, nicht bei Kontext-Halluzinationen.“',
        [K.VISION_P1]: 'Im Zeitalter der KI-Agenten ist Entwicklungsgeschwindigkeit nicht mehr der Engpass; der Engpass ist <strong>Vertrauen</strong>. Menschen fürchten, dass Agenten Systeme kaputt machen, und Agenten lassen sich von ihrem eigenen Kontext leicht in die Irre führen. Egent Code Plexus baut Vertrauen auf strukturellen Fakten auf und stellt sicher, dass Agenten immer zur einzigen Wahrheit zurückkehren: dem <strong>Quellcode</strong>.',
        [K.VISION_P2]: 'Wenn jedes Team und jede Firma mehr Agenten und Repos parallel betreibt, zählt nicht, noch mehr ins Kontextfenster zu stopfen, sondern ein blitzschnelles, vertrauenswürdiges, strukturbewusstes Basiswerkzeug zu haben.',
        [K.FOOTER_TEXT]: '&copy; 2026 Egent Code Plexus Project. Auszüge aus dem Native Design Deep Dive.'
    }
};

class I18nManager {
    constructor(defaultFallback = 'en') {
        this.translations = TRANSLATIONS;
        this.locales = LOCALES;
        // Every locale is its own URL, so the URL wins. Without this a reader
        // arriving on /ja/ from a search result would be switched to whatever
        // their browser prefers, leaving the address bar and the canonical tag
        // describing a page they are not reading.
        const pageLocale = window.__ECP_LOCALE__;
        this.currentLang = this.translations[pageLocale]
            ? pageLocale
            : this.detectBrowserLanguage(defaultFallback);
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

    /** Path of each locale's prerendered page, relative to the site root. */
    static LOCALE_PATHS = {
        'en': '', 'zh-TW': 'zh-TW/', 'zh-CN': 'zh-CN/',
        'ja': 'ja/', 'ko': 'ko/', 'es': 'es/'
    };

    /** Site root. The page states it; guessing from the locale breaks as soon
     *  as a locale has more than one page under it. */
    siteRoot() {
        // Empty string is a real answer here — it means "this page is at the
        // site root" — so test for the declaration, not for truthiness.
        if (typeof window.__ECP_ROOT__ === 'string') return window.__ECP_ROOT__ || './';
        const here = I18nManager.LOCALE_PATHS[this.currentLang] || '';
        const depth = here ? here.split('/').filter(Boolean).length : 0;
        return depth ? '../'.repeat(depth) : './';
    }

    setLanguage(lang) {
        if (!this.translations[lang]) return;
        // Navigate rather than swap in place: the reader ends up on the URL
        // that search engines and shared links point at for this language.
        const target = I18nManager.LOCALE_PATHS[lang];
        if (window.__ECP_LOCALE__ && lang !== this.currentLang && target !== undefined) {
            window.location.href = this.siteRoot() + target + (window.__ECP_PAGE__ || '');
            return;
        }
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
