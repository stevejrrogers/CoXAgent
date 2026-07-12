# CoXAgent — Kế hoạch xây dựng

> Nguồn tham khảo: `ai_workflow_guide_en (2).jsx` — hệ thống AI Multi-Agent Workflow
> (Python orchestrator + opencode CLI engine + state JSON + Docker deploy).
> CoXAgent = bản Rust, sửa các điểm yếu của thiết kế gốc, thêm dashboard UI.

## 1. Mục tiêu

Một daemon chạy 24/7, tự động phát triển một dự án phần mềm theo vòng lặp:

```
mode=kanban (mặc định):
  BA (mỗi N cycle) → SA (design gate) → DEV-BUG → DEV-FEATURE → TEST → DOCS → sleep → lặp lại

mode=scrum (vòng lặp 2 tầng — xem 3d):
  SPRINT #n = [ PO planning → SM mở sprint → cycle × K → PO review → SM retro ]
```

- Mỗi "agent" là một lần gọi CLI agent (opencode / claude) với system prompt + task prompt.
- Giao tiếp giữa các agent qua state có schema chặt (backlog, bugs, in_progress, completed, deploy_history).
- Deploy qua `docker compose up -d --build`, semver bump mỗi lần deploy.
- TEST: smoke (docker ps) + acceptance (curl) + regression + UI smoke (headless Chrome).
- Dashboard web theo dõi & điều khiển realtime.

## 2. Bài học từ thiết kế gốc — giữ gì, sửa gì

### Giữ
- **Vòng lặp tuần tự đơn luồng** — không race condition trên state, đơn giản, đủ nhanh (bottleneck là LLM, không phải orchestrator).
- **State dạng file JSON human-readable** trong `state/` — dễ inspect, dễ sửa tay, tương thích prompt gốc.
- **`project_context.md`** do người dùng điền 1 lần — nguồn sự thật về sản phẩm.
- **UI smoke test bằng headless Chrome** (`smoke.js` puppeteer) — giữ nguyên script Node vì đã battle-tested; port sang chromiumoxide (Rust CDP) để sau.
- **Semver + deploy_history + ghi APP_VERSION vào .env TRƯỚC khi build.**

### Sửa (điểm yếu gốc → giải pháp CoXAgent)
| Điểm yếu gốc | Giải pháp Rust |
|---|---|
| LLM tự sửa JSON state → dễ hỏng, prompt đầy luật phòng thủ | Orchestrator **own state**: struct serde chặt, validate sau mỗi agent run, atomic write (tmp + rename), backup & auto-repair. Agent mutate state qua subcommand `coxagent state ...` (hoặc MCP server) thay vì sửa file thô |
| Claim/release trong `in_progress.json` do prompt enforce | Orchestrator tự claim trước khi gọi agent, tự release sau — agent chỉ làm việc chính |
| Bump version, archive completed.json >40 records — luật trong prompt | Logic trong orchestrator (code, có test) |
| Watchdog = python script + pgrep, dễ double-start | Single binary, PID/lock file, `coxagent daemon` + launchd plist sinh tự động |
| Không quan sát được gì ngoài log | Dashboard web + SSE live log + transcript từng agent run |

## 3. Kiến trúc Rust

Clean Architecture / Hexagonal — cùng convention TonyDB. Dependency rule enforce
bằng CARGO: domain không phụ thuộc gì → application chỉ phụ thuộc domain →
infra/presentation phụ thuộc application (ports). Vi phạm = không compile.

```
coxagent/
├── Cargo.toml                 # workspace
├── crates/
│   ├── domain/                # DDD core — KHÔNG IO, không phụ thuộc crate nào
│   │   ├── ticket.rs          #   aggregate root Ticket (field private, parse-don't-validate)
│   │   ├── sprint.rs  claim.rs  discussion.rs  version.rs   # value objects = newtype
│   │   ├── transitions.rs     #   bảng transition + field-permission = pure functions
│   │   └── events.rs          #   domain events: TicketTransitioned, SprintClosed...
│   ├── application/           # use cases + ports (trait)
│   │   ├── ports/inbound/     #   RunCycle, ClaimTicket, TriggerAgent, ChatWithSm...
│   │   ├── ports/outbound/    #   StateStorePort, AgentEnginePort, DeployPort(docker),
│   │   │                      #   GitPort, ClockPort, EventBusPort
│   │   └── use_cases/         #   orchestrator = application services (cycle, sprint,
│   │                          #   agents/{ba,sa,pd,po,sm,dev_bug,dev_feature,test,docs})
│   ├── infrastructure/        # adapters outbound
│   │   ├── state/{json_store,remote_store}.rs
│   │   ├── engine/{opencode,claude,mock}.rs
│   │   └── deploy/docker_compose.rs  git/cli.rs  bus/sse.rs
│   ├── presentation/          # adapters inbound: axum REST/SSE/WS + CLI (clap)
│   └── app/                   # composition root DUY NHẤT — wire DI, main
│       # coxagent onboard | run | daemon | report | state <...> | ui
├── prompts/                   # xem 3j
├── tools/uitest/smoke.js      # giữ nguyên từ guide (puppeteer-core)
├── state/  codebase/          # runtime của dự án được quản lý
└── ui/                        # React + Vite + Tailwind → nhúng rust-embed
```

Domain events → EventBus → SSE: team board realtime là HỆ QUẢ kiến trúc,
không phải tính năng gắn thêm.

### Dependencies chính
- `tokio` (runtime + process), `serde`/`serde_json`, `minijinja` (prompt template)
- `axum` + SSE, `rust-embed` (nhúng UI), `tracing` + `tracing-appender`
- `fd-lock` (khóa state), `clap` (CLI), `semver`
- Sau này: `chromiumoxide` nếu muốn bỏ Node cho UI smoke test

### Luồng 1 cycle (pseudo)
```rust
loop {
    if cycle % cfg.ba_every == 1 { run_agent(BA).await; }
    if pd_has_work(&state) { run_agent(PD).await; }         // feature has_ui thiếu design.ux — PD trước SA
    if sa_has_work(&state, cycle) { run_agent(SA).await; }  // check bằng code, không có việc → không spawn LLM
    run_agent(DevBug).await;      // orchestrator claim → engine.run(prompt) → validate state → release
    if cfg.feature_dev { run_agent(DevFeature).await; }
    run_agent(Test).await;        // gồm cả `node tools/uitest/smoke.js`, parse JSON kết quả → bugs
    if docs_has_work(&state) { run_agent(Docs).await; }  // chỉ khi có feature verified chưa document
    render_changelog(&state)?;    // CHANGELOG.md sinh từ deploy_history — code, không LLM
    state.snapshot_and_validate()?;   // backup + kiểm schema, tự repair từ backup nếu hỏng
    sleep_interruptible(cfg.sleep_seconds).await;
}
```

## 3b. SA (Solution Architect) — role mới, bản gốc không có

Bản gốc để DEV vừa nghĩ kiến trúc vừa code mỗi cycle; vì LLM không có trí nhớ giữa
các run nên kết quả là drift kiến trúc, mỗi feature một kiểu. SA vá đúng chỗ này.

### 3 nhiệm vụ
1. **Design gate (chính — chạy ngay sau BA).** Đọc feature `pending` chưa có design,
   ghi `design` vào feature: modules/files đụng tới, API contract, data model, thư viện
   (phải khớp stack hiện có), test plan → chuyển `pending → ready`.
   Feature `complexity=large` → **split** thành feature nhỏ có `parent_id`.
   **Orchestrator enforce bằng code**: DEV-FEATURE chỉ được cấp feature `ready`
   (orchestrator lọc và nhét đúng 1 feature vào prompt, agent không tự chọn).
2. **Own bộ nhớ kiến trúc**: duy trì `state/architecture.md` (tech stack thực tế,
   module map, conventions — tài liệu SỐNG, khác `project_context.md` tĩnh của người)
   + `state/adr/ADR-xxx.md` cho quyết định lớn. Mọi agent đọc architecture.md mỗi run
   → các run rời rạc mới nhất quán.
3. **Architecture review định kỳ** (mỗi `sa_review_every_n_cycles` ~10 cycle): quét
   codebase tìm drift/duplication/tech debt/security → ghi chore/refactor vào backlog
   hoặc bug. Kèm: bug bị reopen ≥2 lần → SA phân tích root cause, viết fix design cho DEV-BUG.

### Chống lãng phí & chống quan liêu
- `sa_has_work()` check state bằng code trước: không có feature thiếu design và chưa tới
  hạn review → **skip, không tốn token**.
- Feature `complexity=small` được auto-pass gate (configurable `sa_gate_min_complexity`).

### Definition of Ready — `ready` nghĩa là gì
`pending` = mới đề xuất, CHƯA qua cổng nào (không phải "chờ SA" — nó chờ lần lượt
2 cổng: PO gate "đáng làm không" (scrum) → SA gate "làm thế nào"). Không đẻ status
trung gian: orchestrator chọn item bằng code, tính eligibility từ field.

Feature chỉ chuyển `ready` khi đủ checklist — **code validate sự tồn tại field,
LLM chịu trách nhiệm nội dung**:
- BA: description + acceptance_criteria (kết quả nhìn từ phía user) + `has_ui` + complexity đề xuất
- PO (scrum): priority đã chốt, không bị reject
- SA: `design.technical` = { approach, files, api_contract, data_changes, test_plan }
- PD: `design.ux` — **BẮT BUỘC khi has_ui=true** = { user_flow, screens/routes,
  component_states (loading/empty/error), responsive_notes } — nhất quán với design_system.md
  → nguồn sự thật cho DEV dựng UI và cho TEST `visual_sanity` đối chiếu
  (PD tắt qua config → SA kiêm design.ux)
- SA: complexity xác nhận lại; `large` → đã split (parent_id)

### Schema thay đổi
- `Feature.status`: thêm `ready` (pending → ready → in_progress → done)
- `Feature.design`: { technical: {...}, ux?: {...} } theo DoR trên
- `Feature.has_ui: bool`; `Feature.parent_id`: cho feature bị split
- Thêm `state/architecture.md`, `state/adr/`

## 3c. Chiến lược tài liệu — chia theo CÁCH SINH RA, không theo tên gọi

Nguyên tắc: trong loop tự động, tài liệu không có chủ sở hữu + không được enforce
= mục nát ngay. Ba loại:

### (1) Sinh được từ state → CODE render, không dùng LLM
- `codebase/CHANGELOG.md`: orchestrator render từ `deploy_history.json` + `completed.json`
  (version, ngày, features_added, bugs_fixed). Deterministic, 0 token, không bao giờ lệch.
- Quy tắc chung: cái gì đã có trong state thì không bắt LLM viết lại.

### (2) Tài liệu kỹ thuật → gắn vào DoD của agent có sẵn
- `state/architecture.md` + `state/adr/` — SA own (mục 3b).
- `codebase/README.md` (setup/run) + `codebase/docs/api.md` — DEV own, là một bước
  trong definition-of-done: "thêm/đổi endpoint → cập nhật api.md trước khi deploy".
- **Chống mục nát**: TEST verify docs khớp thực tế mỗi cycle — curl từng endpoint
  trong api.md, endpoint 404 → bug `severity=medium "docs drift"`. Docs được kiểm như code.

### (3) Tài liệu sản phẩm (user guide) → agent DOCS (Tech Writer), chạy SAU TEST
- Vị trí sau TEST là cố ý: **chỉ document feature đã verified** — không bao giờ
  viết hướng dẫn cho tính năng đang hỏng.
- Input: record `completed.json` có `documented: false` và không còn bug open liên quan.
- Output: `codebase/docs/user-guide/<feature>.md` (viết cho end-user: làm gì, ở đâu,
  ví dụ sử dụng) → đánh dấu `documented: true`.
- `docs_has_work()` check bằng code — không có gì mới verified → skip, không spawn LLM
  (cùng pattern với SA).
- Docs nằm trong `codebase/` để đi cùng git + deploy của sản phẩm; `state/` chỉ chứa
  tài liệu nội bộ workflow.

### Schema/config thay đổi
- `Completed.documented: bool` (mặc định false)
- config: `docs_agent_enabled` (tắt được cho dự án không cần user guide)

## 3d. PO & SM — tầng Sprint (mode=scrum, optional)

PO/SM KHÔNG đứng trong chuỗi per-cycle — họ chạy ở **biên sprint** (sprint = K cycles).
Chạy mỗi cycle thì vừa đốt token vừa quan liêu. Standup bỏ (state files là standup),
refinement bỏ (đã có SA). Chỉ giữ ceremony nào tạo ra artifact bền: planning / review / retro.

```
SPRINT #n (K cycles):
  ┌ PO: planning — re-rank priority, reject item xấu, chọn sprint goal,
  │     commit feature `ready` vào sprint backlog   → SM: mở sprint
  │  cycle × K:  BA → SA → DEV-BUG → DEV-FEATURE → TEST → DOCS
  │     · DEV-FEATURE chỉ lấy từ sprint backlog (orchestrator enforce bằng code)
  │     · BA vẫn đề xuất nhưng chỉ vào product backlog, không vào sprint
  │     · SM impediment-watch: `sm_has_impediment()` check bằng code
  │       (claim kẹt lâu, bug reopen ≥2, deploy fail liên tiếp) → có chuyện mới spawn
  └ PO: review completed vs sprint goal (mức sản phẩm, khác TEST verify kỹ thuật)
    → SM: retro + metrics + cập nhật working_agreements.md
```

### PO (Product Owner) — own "cái gì đáng làm"
1. **Own `priority` + quyền reject**: BA chỉ đề xuất; PO re-rank theo product goal
   trong `project_context.md`, reject item trùng/ngoài scope (status `rejected`).
2. **Sprint planning**: sprint goal + commit sprint backlog.
3. **Sprint review**: feature chạy đúng kỹ thuật nhưng lệch ý đồ sản phẩm
   → follow-up/bug "not as intended".

### SM (Scrum Master) — own "quy trình có khỏe không"
1. **Mở/đóng sprint + diễn giải metrics**: velocity, reopen rate, deploy fail rate,
   carryover — **số liệu do code tính từ state, SM chỉ diễn giải**.
2. **Impediment watch** giữa sprint (conditional-skip): escalate, ví dụ đề nghị
   tạm tắt feature-dev khi bug dồn ứ.
3. **Retro** — giá trị nhất với LLM loop: ghi bài học vào `state/working_agreements.md`
   (≤ ~30 dòng), file này append vào prompt MỌI agent → loop tự cải thiện qua từng
   sprint, mà không cho agent sửa trực tiếp prompt của nhau (nguy hiểm).

### Enforce bằng code (làm được vì orchestrator own state)
- **Quyền theo role trên từng field**: chỉ PO sửa `priority`/`sprint`; DEV không đụng
  priority; BA không tự approve. Bản gốc không thể làm vì agent sửa file thô.
- Sprint backlog filter: orchestrator chỉ đưa feature thuộc sprint hiện tại vào prompt DEV.

### Schema/config thay đổi
- `state/sprint.json`: { number, goal, started_at, length_cycles, status,
  committed: [FEAT-ids], done: [], carryover: [] }
- `Feature.status` thêm `rejected`; `Feature.sprint: Option<u32>`
- `state/working_agreements.md` (SM own), metrics tính từ state (code)
- config: `mode: kanban|scrum` (mặc định kanban), `sprint_length_cycles` (vd 10)

## 3e. PD (Product Designer) — role UI/UX, optional

Cùng logic SA-own-architecture.md: giá trị chính của PD là **own bộ nhớ thiết kế**,
không chỉ viết spec từng feature.

1. **Own `state/design_system.md`** — design tokens (màu/spacing/typography), inventory
   component, pattern chuẩn (empty state, error, navigation, form). Mảnh shared-memory
   thứ ba của hệ: architecture.md (kỹ thuật) · working_agreements.md (quy trình) ·
   design_system.md (thiết kế). DEV/SA/TEST đều đọc.
2. **Viết `design.ux` trong cổng DoR** cho feature `has_ui` (xem 3b). Chạy **TRƯỚC SA**
   vì UX định hình API (frontend cần gì → backend phục vụ nấy), không phải ngược lại.
   Chuỗi gate: PO → PD (has_ui) → SA → ready.
3. **Design QA (M6)**: smoke.js chụp screenshot trang mới/đổi → PD (engine multimodal)
   soi so với design_system.md → bug "design inconsistency".

`pd_has_work()`: có feature has_ui pending (đã qua PO) thiếu design.ux → mới spawn.
Config `pd_agent_enabled` (mặc định false → SA kiêm). Chạy cả kanban lẫn scrum.

## 3f. Lifecycle — 4 tầng lồng nhau: Daemon ⊃ Sprint ⊃ Cycle ⊃ Item

### (1) Daemon — vòng đời tiến trình
`coxagent onboard` (cycle 0 — xem 3h; hoặc `init` rồi tự điền project_context.md)
→ `coxagent run|daemon`:
- preflight: check engine CLI + docker, lock file chống double-start
- **recovery**: claim mồ côi trong `in_progress.json` (chết giữa chừng ở lần chạy trước)
  → resume hoặc release — nhờ đặt ở tầng daemon, crash lúc nào cũng không mất việc
- chạy loop → pause/resume qua API → SIGINT/SIGTERM: chạy nốt agent hiện tại rồi thoát sạch
- launchd restart → quay lại bước recovery

### (2) Sprint — vòng đời lô việc (chỉ mode=scrum)
planning (PO: rank/reject/goal/commit) → SM mở sprint → **cycle × K** → review (PO)
→ retro (SM: metrics + working_agreements.md) → carryover đổ vào planning sprint kế.
mode=kanban: tầng này biến mất, cycle nối cycle liên tục.

### (3) Cycle — nhịp tim (~30s+ mỗi vòng)
1. BA (khi `cycle % N == 1`) — đề xuất vào product backlog
2. SA (khi `sa_has_work()`) — design gate + arch review định kỳ
3. DEV-BUG → 4. DEV-FEATURE (mỗi agent: orchestrator claim → engine.run → validate → release)
5. TEST — smoke + acceptance + regression + UI smoke + docs-drift
6. DOCS (khi `docs_has_work()`)
7. render CHANGELOG (code) → 8. snapshot + validate state (code) → 9. sleep interruptible

### (4) Item — state machine, transition enforce bằng code

```
Feature: pending(BA) ──PO loại──▶ rejected ✕
            │ cổng PO: chốt priority (scrum) → cổng SA: design đủ DoR
            ▼
          ready ──[scrum: PO commit vào sprint]──▶ in_progress(claim tự động)
            ▼ DEV deploy + bump MINOR
          done(completed.json, documented:false) ──TEST fail──▶ sinh bug (feature_id)
            ▼ DOCS
          documented ──▶ archived (completed.json > 40 record)

Bug:     open(TEST phát hiện: curl/UI smoke/docs-drift)
            ▼ claim tự động
         in_progress ──DEV-BUG fix + bump PATCH + deploy──▶ fixed
            ▼ TEST regression
         pass → verified ✓   |   fail → open (reopen++; reopen ≥ 2 → SA root-cause design)
```

Điểm mấu chốt: orchestrator giữ **bảng transition hợp lệ** — agent yêu cầu chuyển trạng
thái sai (vd DEV đòi `done` một feature chưa `ready`, hay sửa bug thành `verified` mà
không qua TEST) bị từ chối ngay ở tầng state store, không cần prompt phòng thủ.

## 3g. Onboarding — "cycle 0", chỗ DUY NHẤT bắt buộc human-in-the-loop

Không cần role mới: onboarding = pipeline role có sẵn chạy MỘT LẦN ở chế độ tương tác.
Phân công: PO trả lời "làm gì, cho ai, thành công là gì"; BA trả lời "gồm feature nào".
Goal sai → loop tự động sai hướng mãi, nên kết thúc onboard bắt buộc có người duyệt.

### Flow A — project mới: `coxagent onboard`
1. User nhập đề bài thô
2. PO dẫn interview: người dùng đích, must-have vs nice-to-have, ràng buộc
   (stack, auth, deploy), tiêu chí thành công → draft project_context.md
3. SA: tech stack + architecture.md v0 + docker-compose skeleton chạy được
4. BA: epics → seed backlog.json (vẫn `pending` — đi qua gate PO/PD/SA như thường)
5. PD (nếu có UI): design_system.md v0
6. **Human gate**: duyệt/sửa toàn bộ draft → ghi file → loop mới được chạy
7. Seed FEAT-000 "walking skeleton" (hello-world + /health + compose up được)
   → cycle 1 có ngay thứ để deploy & TEST, chứng minh đường ống trước khi làm feature thật

### Flow B — project có sẵn: `coxagent onboard --existing <path>`
Học hiện trạng trước khi mơ tương lai:
1. SA code archaeology: quét codebase → sinh architecture.md v0 mô tả AS-IS
2. PO + user: goal giai đoạn tới → project_context.md + mục "Existing features" user xác nhận
3. PD: trích design_system.md từ UI code có sẵn (as-is)
4. BA: seed backlog cho hướng phát triển tiếp
5. **Baseline TEST ngay** (trước cycle 1): build + smoke → bug hiện trạng vào bugs.json
   — khởi động bằng việc biết mình đang đứng đâu
6. `docker compose up` không chạy (project chưa dockerize) → BUG-000 critical
   "not deployable" → DEV-BUG xử lý ngay cycle 1 (cả loop deploy/test dựa trên compose)
7. Seed version: git tag mới nhất, không có thì 0.1.0

### Chung
- `onboard --refresh`: chạy lại khi pivot goal (update context, không đụng state đang chạy)
- Dashboard (M5): wizard onboarding trên UI thay CLI

## 3h. Chiến lược test — AI và UI

Triết lý chung: **không test trực tiếp thứ bất định (LLM output, pixel) — kẹp nó
giữa các oracle tất định** (state machine cho AI, DOM/screenshot cho UI).

### Test về mặt AI — 4 lớp
1. **Core tất định = unit test thường** (`cargo test`, CI): state store, bảng
   transition, claim/release, semver, DoR validation, các hàm `*_has_work()`.
2. **MockEngine — test orchestrator không cần LLM**: engine là trait → inject mock
   trả kịch bản định sẵn (cả kịch bản xấu: JSON hỏng, transition cấm, timeout)
   → full loop end-to-end trong CI. Bản Python gốc không làm được điều này.
3. **Eval prompt — assert trên STATE, không phải văn bản**: fixture state
   (vd 3 bug open + 2 feature pending) → chạy agent thật → assert hệ quả:
   chỉ transition hợp lệ, không đụng field cấm, không xóa record, ID unique,
   tham chiếu feature_id tồn tại. Check đếm chính xác (absolute invariant),
   không check "có vẻ ổn". Chạy nightly với engine thật để bắt drift model/prompt.
4. **Production = eval liên tục**: mọi run đều bị validate → metrics SM
   (tỷ lệ reject transition, reopen rate, pass-TEST-lần-đầu) chính là thước đo
   chất lượng prompt. Prompt/model tệ đi → metric trồi → SM retro bắt được.
   Không cần hạ tầng eval riêng ở v1.

### Test về mặt UI — 3 tầng (cho sản phẩm agents xây)
1. **Functional smoke** (`smoke.js`, giữ từ guide): headless Chrome mỗi cycle —
   mount/login, thăm mọi route, click mọi button (dismiss dialog, skip nút nguy hiểm),
   JS error/CSP/request fail/broken image, visual_sanity (oversized/clipped/collapsed).
   Guard chống flaky: retry nav 1 lần; >50% trang fail đồng loạt = lỗi MÔI TRƯỜNG.
2. **Đúng spec — nhờ design.ux**: screens/routes trong ux spec → checklist trang
   phải tồn tại; component_states → kiểm empty/error/loading có render;
   acceptance criteria → mỗi criterion map 1 check. Fail = bug gắn feature_id.
3. **Visual regression + design QA (M6)**: screenshot từng route mỗi version,
   pixel-diff giữa version; PD (multimodal) soi screenshot vs design_system.md.

Dashboard của chính CoXAgent (M5): Playwright e2e trên mock API + dogfood smoke.js.

### Sản phẩm xây ra CÓ tính năng AI thì sao
Acceptance criteria phải viết dạng **bất biến kiểm được** (schema output hợp lệ,
latency, guardrail từ chối đúng), không phải so khớp chuỗi chính xác;
cần chấm chất lượng → LLM-as-judge với rubric trong criteria (M6).

## 3i. Teamwork layer — team board, chat với SM, thảo luận giữa agents

### Team board (dashboard)
- Mỗi agent 1 card: `running` (task đang claim + transcript STREAMING qua SSE —
  engine bắt stdout theo dòng), `standby` (kèm lý do từ `*_has_work()`), `next up`.
- Nhịp tim: cycle #, uptime, tiến trình cycle, đèn đỏ khi stall.
  "Cycle luôn chạy" là việc của daemon+launchd — UI chứng minh điều đó.
- Agent điều kiện không chạy mỗi cycle là BY DESIGN (chi phí) — hiển thị
  "standby + lý do" vẫn đọc như team thật.

### Chat với SM — cửa giao tiếp duy nhất user ↔ team
- Panel chat nối vào phiên SM PERSISTENT (có nhớ, không phải one-shot).
- SM tools: đọc state/metrics/transcripts, mở thread, kích scrum event.
- Trả lời từ DỮ LIỆU THẬT (transcript, metrics), không bịa.
- User đòi đổi priority → SM KHÔNG tự sửa (field của PO) → mở escalation cho PO.
  Field-permission thiêng liêng kể cả khi lệnh đến từ chat.
- Scrum events (planning/review/retro) hiện thực thành thread nhìn thấy được
  trong team room — user dự thính hoặc tham gia.

### Discussion threads — họp có agenda, biên bản, time-box
- **Mở theo trigger** (triết lý has_work): DEV bí design → thread với SA;
  TEST thấy bug mâu thuẫn acceptance criteria → thread BA/PO; reopen ≥2 →
  thread root-cause SA+DEV+TEST; user nhắn SM → SM mở thread role liên quan.
  Không trigger = không họp.
- **Moderated, turn-based, budget ~6 lượt**, chỉ role liên quan.
  Format phát biểu bắt buộc: quan điểm → căn cứ (evidence từ state/code/metrics)
  → đề xuất. Không phán suông, được phản bác có lý lẽ → "nói chuyện có suy nghĩ".
- **Kết thúc bắt buộc = decision block**: { decision, rationale, actions[] } —
  actions đi qua state store đúng role-permission (sửa design, ADR, backlog...).
- **Không chốt được trong budget → escalate**: thread đỏ trên dashboard,
  SM ping user "team bế tắc, cần anh quyết" — human gate thứ 2 (sau onboarding).
- Chi phí: trigger-based + turn budget + ≤2 thread/cycle + lưu summary
  (không re-send cả lịch sử thread mỗi lượt).

### Schema/hạ tầng
- `state/discussions.json`: thread { id, topic, item_ref (FEAT/BUG/ADR),
  participants, status: open|decided|escalated, messages[], decision? }
- Engine thêm chế độ session (persistent cho SM chat) bên cạnh one-shot
- Server: WebSocket/SSE cho chat + stream transcript theo dòng

## 3j. System prompts — cấu trúc & lưu trữ

Chưa viết (M1-M2). Ngắn hơn guide gốc nhiều vì luật đã vào code.

```
prompts/
├── _base.md        # chung mọi role: nguyên tắc evidence, cấm sửa state thô, format output
├── ba.md  po.md  sm.md  sa.md  pd.md  dev.md  test.md  docs.md
├── onboard/        # po_interview.md, sa_archaeology.md
└── discussion.md   # luật phát biểu trong thread: quan điểm → căn cứ → đề xuất
```

- Runtime compose: `_base` + role + con trỏ bộ nhớ theo role (architecture.md /
  design_system.md / working_agreements.md) + task prompt sinh từ state + time header.
- Lưu trữ: default nhúng trong binary (`include_str!`); `coxagent init` chép ra
  `prompts/`; có file thì override — tinh chỉnh per-project không mất bản gốc.

## 3k. Dữ liệu — local layout, server hóa, ticket tối giản

### Local: workspace tự chứa, là git repo (audit trail miễn phí, portable)
```
<workspace>/            # git repo
├── coxagent.json       # config
├── prompts/            # override
├── state/              # tickets, sprint, discussions, *.md bộ nhớ
│   └── .backups/       # snapshot mỗi cycle (xoay vòng)
├── logs/transcripts/<cycle>/<agent>.md
└── codebase/           # sản phẩm (git repo riêng/submodule)
```
Config toàn cục của tool: `~/.config/coxagent/`.
**`StateStore` là TRAIT ngay từ M0**: local = JsonStateStore, server = SqlStateStore
— orchestrator không đổi một dòng.

### Server hóa (M8): control plane + workers
- Control plane (Postgres): users, projects, memberships (owner/manager/viewer),
  audit events. Auth: OIDC / magic link.
- Mỗi project = 1 orchestrator worker cách ly (process + docker riêng).
- "User nào quản agent nào làm ticket gì" = MIỄN PHÍ từ thiết kế claim
  (claim đã ghi agent + item + started_at; thêm chiều project → user là xong).

### Ticket tối giản — anti-Jira có chủ đích
State files CHÍNH LÀ ticket system. Nguyên tắc chống phình:
1. Một loại ticket, `type: feature|bug|chore`; fields: id, title, description,
   priority (3 mức), complexity (S/M/L), status (lifecycle), design, links,
   `depends_on` (xem 3l), actor+timestamp mỗi transition. Hết.
2. KHÔNG custom workflow — lifecycle cố định enforce bằng code.
3. KHÔNG hierarchy sâu — chỉ `parent_id` khi split.
4. Máy làm bookkeeping (tự tạo/chuyển/link ticket) — 90% nỗi khổ Jira biến mất.
5. Click ticket = trọn câu chuyện tự động: ai đề xuất, design, version, test result,
   thread thảo luận, docs — nhờ mọi thứ link qua feature_id/version sẵn trong state.
6. User là actor: tạo ticket → pending qua gate như thường; user = "super-PO"
   (ghi đè priority, ghi nhận như hành động PO).

### Website
= dashboard M5, MỘT codebase 2 chế độ: local (localhost, không auth) và
server (auth + project switcher + viewer read-only + share-link status page).

## 3l. Team mode — hub & workers (nhiều user, nhiều máy, chung sprint)

Máy mỗi user = **worker** chạy agents; **hub** (server) = source of truth cho
sprint/tickets/claims. Local đơn giản vẫn là MẶC ĐỊNH — hub là chế độ thêm.
`StateStore` trait (M0) trả công: local = JsonStateStore, team = RemoteStateStore.

### Cơ chế
1. **Claim nguyên tử trên hub + lease/heartbeat**: CAS `ready → in_progress + worker_id`;
   máy chết/rớt mạng → lease hết hạn → ticket tự nhả. Không bao giờ pick trùng.
2. **Hai chế độ nhận việc**: auto (hub phát theo priority + sprint + dependency)
   hoặc pinned (user chọn queue cho máy mình, agents chỉ pick từ đó).
3. **`depends_on: [ids]`** (SA điền khi design/split): hub chỉ phát ticket khi mọi
   depends_on đã done; UI highlight graph blocker/dependent khi user chọn ticket.
4. **Conflict code (vấn đề khó nhất)** — 3 lớp:
   a. branch-per-ticket: mỗi claim làm trên `agent/<ticket-id>`, push repo chung
   b. merge queue trên hub: merge tuần tự; conflict → job "rebase & fix"
   c. né trước bằng `design.files`: không phát 2 ticket chồng file cùng lúc
5. **Job queue thống nhất**: cycle = máy sinh job tự động; nút trigger trên web
   (pending → "PD/SA viết spec", ready → "dev làm") = user sinh job thủ công.
   CÙNG hàng đợi, cùng đường thực thi, cùng validate. Chat SM cũng sinh job.
6. **Fleet view trên website**: máy nào online (heartbeat), agent nào chạy ticket nào,
   trigger/board/chat đầy đủ — "để máy ở nhà, ra ngoài lên web xem" đúng nghĩa đen.
   1 dev nhiều project / nhiều dev chung project = worker khai capacity, hub phân job.

### MỘT scrum team logic per project (không phải N team)
Máy các user KHÔNG phải các team riêng — là "đôi tay" thực thi của cùng một team:
SA máy A và SA máy B = cùng role, cùng đọc architecture.md trên hub, cùng queue.
- **Job thực thi (DEV/TEST): song song** giữa các máy — điểm ăn tiền của team mode.
- **Job ra quyết định: tuần tự per project** — scrum events, lượt thread, ghi memory
  files (architecture.md/design_system.md/working_agreements.md) hub phát 1 lượt 1 lúc.
- **SM & PO = một tiếng nói per project**: phiên chat/bộ nhớ hội thoại sống ở HUB,
  máy nào thực thi lượt trả lời cũng được. Không để mỗi máy một SM riêng.
- Cần "squad" theo epic sau này: pinned queue + filter milestone, không cần khái niệm mới.

### Chat 3 lớp (đều hub-resident)
1. **Team room per project**: tất cả users + SM; biên bản scrum events post vào đây;
   users chat với nhau; SM route yêu cầu thành job/escalation đúng role.
2. **Thread per ticket/discussion**: agents thảo luận (quan điểm→căn cứ→đề xuất),
   user tham gia được — comment của owner có thể là lượt quyết định.
3. **Chat với SM** (3i): hỏi status, ra yêu cầu.

### Schema thêm
- Ticket: `depends_on: [id]`, `pinned_to: Option<worker_id>`
- Claim: `worker_id`, `lease_expires_at` (+ heartbeat)
- Hub: bảng `workers` { id, user, capacity, last_heartbeat }, bảng `jobs`
  { id, role, ticket_ref, source: cycle|manual(user)|sm_chat, status,
    kind: exec(song song) | decision(tuần tự per project) }
- Team room: `messages` { room: project|ticket|sm, author: user|agent, body, refs }

## 3m. Technical spec — chuẩn code CoXAgent (SA áp cùng chuẩn khi thiết kế sản phẩm)

### Kiến trúc: Clean Architecture / Hexagonal + DDD
- Dependency rule enforce bằng Cargo (xem tree §3) — mạnh hơn mọi linter/ArchUnit.
- DDD thực chất: Ticket = aggregate root, field private, parse-don't-validate ngay
  constructor (state hỏng không tồn tại được); value objects = newtype; mọi thay đổi
  trạng thái phát domain event → EventBus → SSE.

### Design patterns có chủ đích (không trang trí)
| Pattern | Dùng ở đâu |
|---|---|
| Ports & Adapters | mọi ranh giới IO |
| Repository | StateStorePort |
| Strategy | AgentEnginePort (opencode/claude/mock) |
| State machine | domain/transitions.rs |
| Command | jobs trong queue (cycle/manual/sm_chat) |
| Observer | domain events → SSE |
| Builder | compose prompt theo lớp |
| Newtype | mọi value object |

### SOLID theo nghĩa Rust
- **S**: mỗi use case 1 struct, 1 lý do thay đổi
- **O**: thêm engine/store = thêm adapter, không sửa use case
- **L**: mọi adapter của 1 port phải pass CÙNG contract test suite (kiểm L thật,
  không nói miệng)
- **I**: port nhỏ theo nhu cầu use case — cấm God-trait
- **D**: use case nhận port qua generic/dyn; DI tại composition root duy nhất (crates/app)

### Clean & clear — enforce được, không khẩu hiệu
- CI: rustfmt + clippy pedantic + deny warnings; cấm unwrap/expect ngoài test;
  error theo layer bằng thiserror
- Mỗi use case: unit test với mock port; mỗi adapter: contract test
- **Guard ngược — không abstraction đón đầu**: trait chỉ mở khi có ≥2 adapter thật
  hoặc cần mock; không dyn-trait soup. DDD hình thức tệ hơn không DDD.

### SA thiết kế sản phẩm theo cùng chuẩn
- `sa.md` + template architecture.md ghi cứng: layers/boundaries bắt buộc,
  design.technical phải map component vào layer, checklist SOLID khi review design
- Onboarding: SA seed lint/boundary config theo stack sản phẩm (vd eslint-boundaries)
- TEST (M7): architecture conformance check nhẹ

### Presentation swappable — chuyện App macOS
Web dashboard trước (M5). Native macOS shell (menubar app điều khiển daemon —
Tauri/SwiftUI gọi cùng API) thêm sau KHÔNG đụng domain/application —
phần thưởng trực tiếp của kiến trúc này.

## 3n. Phân quyền, roadmap view, engine config

### RBAC — 2 tầng tách bạch, đều enforce ở hub
- **Tầng người**: `admin` (root: quản hệ thống, tạo project, invite) → `owner`
  (per project: settings, invite, điều khiển sprint) → `member` (pin/assign ticket,
  trigger job, chat SM, chạy worker) → `viewer` (read-only).
  User login chỉ thấy project được invite (memberships — xem 3l).
- **Tầng agent**: field-permission theo role (3d) — độc lập với tầng người;
  member không thể khiến hub sửa field ngoài quyền role agent.
- **Root bootstrap — KHÔNG hardcode mật khẩu** (không ghi vào code/git):
  lần đầu hub chạy tạo `root`, mật khẩu từ env `COXAGENT_ROOT_PASSWORD`
  (hoặc sinh ngẫu nhiên in console một lần), lưu argon2 hash,
  BẮT đổi mật khẩu ở lần login đầu.

### Scrum events trong team mode
Mỗi client chạy đủ 9 role, NHƯNG scrum event (planning/review/retro) = job cấp hub,
mỗi sprint chạy đúng MỘT lần — hub chỉ định 1 worker thực thi, mọi client
xem/tham gia qua team room. Không để nhiều máy tự chạy planning riêng.

### Product roadmap + project detail = VIEW tự sinh (anti-Jira)
- Roadmap render từ tickets + sprint + depends_on + priority → timeline theo phase;
  PO chỉ thêm field nhẹ `milestone/phase` trên ticket, mục tiêu phase ghi trong
  project_context.md. Không có "module roadmap" nhập liệu riêng.
- Project detail = render của thứ đang sống: project_context.md, architecture.md,
  design_system.md, metrics, deploy history, changelog.

### Engine discovery + gán model theo role
- Client khởi động/login: quét PATH theo registry engine đã biết (opencode, claude,
  hermes, gemini, codex...), lấy version + auth status → báo hub năng lực máy.
- Settings: mapping `role → { engine, model }`, ưu tiên 3 cấp:
  worker override > project default > global default.
- AgentEnginePort (Strategy) đã sẵn — chỉ thêm config + detection.
- Tối ưu chi phí thật: DEV/SA model đắt, BA/DOCS model rẻ — theo từng role.
- Schema: `Ticket.milestone: Option<String>`; worker report thêm `engines_detected[]`;
  config `engine_mapping{}` 3 cấp.

## 3o. Deployment & Git/Codebase

### Deploy sản phẩm agents xây — DeployPort + adapter theo target
- `docker-compose` (mặc định v1): compose up local như guide
- `test-only`: project library/CLI không có gì deploy — chạy test suite thay compose
- `ssh-compose` / `k8s` (M7+)
- Team mode: worker deploy compose LOCAL để TEST branch của mình; sau merge,
  1 worker được chỉ định chạy integration deploy môi trường chung.

### Deploy chính CoXAgent
- Local: single binary — GitHub Releases qua cargo-dist (+ brew tap),
  `coxagent self-update`; **state có `schema_version` + migration** khi nâng binary.
- Hub (M8): docker-compose { hub (website nhúng sẵn) + postgres } sau Caddy (TLS).
- CI repo CoXAgent: fmt + clippy pedantic + test + contract tests (chuẩn 3m) + release.

### Git — cấu hình trong coxagent.json (onboard điền)
```json
"git": { "codebase_repo": "git@... | null", "default_branch": "main",
         "branch_prefix": "agent/", "auto_push": false, "commit_style": "conventional" }
```
- **Credentials KHÔNG nằm trong config** — dùng cơ chế máy (ssh key, credential
  helper, gh auth); worker dùng danh tính máy nó; hub không giữ private key user.
- Agent commit với author riêng + trailer `Ticket: FEAT-xxx` → git log tự audit.
- Qua GitPort → adapter GitCli.

### Codebase — resolution 3 trường hợp (onboard), không bao giờ mơ hồ
| Khai gì | Làm gì |
|---|---|
| Không khai (greenfield) | Mặc định `<workspace>/codebase/`, git init, commit đầu = FEAT-000 |
| Path local (brownfield) | Dùng trực tiếp; chưa git → init + commit "baseline" (BẮT BUỘC — branch-per-ticket & audit cần git) |
| Remote URL | Clone về `<workspace>/codebase/` |

Preflight: không có codebase-là-git-repo → daemon từ chối chạy, báo rõ.

### Agent "vào" code thế nào
- Local: spawn engine CLI với cwd = codebase (kiểu `opencode --dir`), sandbox trong đó.
- Team mode: worker nhận job → fetch + branch `agent/<ticket-id>` từ main mới nhất
  → **git worktree riêng mỗi claim** (job song song cùng máy không giẫm nhau)
  → engine cwd = worktree → commit/push → hub merge queue.
- Agent chỉ đụng code trong codebase; state không sờ trực tiếp (chỉ qua coxagent state).

## 3p. Desktop app — đóng gói & thư mục mặc định

### Đóng gói
- Tauri shell bọc dashboard UI + bundle binary `coxagent` (presentation-swappable 3m
  trả công — không viết UI mới). Build: .dmg (ký + notarize), .msi (signing),
  .AppImage/.deb, auto-update Tauri. CLI song song qua cargo-dist/brew.

### Thư mục mặc định — dữ liệu app giấu theo chuẩn OS, code ở chỗ nhìn thấy
| Loại | macOS | Windows |
|---|---|---|
| App config + danh sách workspace | ~/Library/Application Support/CoXAgent/ | %APPDATA%\CoXAgent\ |
| **Workspaces (chứa code)** | ~/CoXAgent/<project>/ | %USERPROFILE%\CoXAgent\<project>\ |
| App logs | ~/Library/Logs/CoXAgent/ | %LOCALAPPDATA%\CoXAgent\logs\ |

Bên trong workspace: layout 3k y nguyên (state/, prompts/, logs/, codebase/).

### First-run flow — không bao giờ "mở lên không biết trỏ đâu"
1. Chưa có workspace → wizard (= onboarding 3g bản GUI): tạo mới (path điền sẵn
   ~/CoXAgent/<tên>, đổi được) / mở có sẵn / clone repo / kết nối hub.
2. **Prerequisite check trong wizard** (sống còn với máy user thường): Docker Desktop?
   Engine CLI nào đã cài (engine discovery 3n chạy tại đây)? Thiếu → hướng dẫn cài.
3. Onboard xong → daemon start → icon menubar/tray (nhịp tim: cycle #, agent đang chạy,
   click mở dashboard), toggle "Start on login" (launchd / Task Scheduler).
4. Lần sau: đọc danh sách workspace → vào project gần nhất (recent projects).

## 4. Rust có OK không? — CÓ

**Hợp lý vì:**
- Orchestrator là daemon chạy 24/7 → Rust cho single static binary, không venv/pip, không chết vì dependency drift; launchd chỉ cần trỏ 1 binary.
- Phần "thông minh" nằm ở CLI agent (opencode/claude) — Rust chỉ làm glue: spawn process, parse JSON, serve HTTP. Tất cả đều là thế mạnh của Rust (tokio, serde, axum).
- Schema state được type chặt → loại cả lớp bug "LLM ghi JSON sai" mà bản Python phải phòng thủ bằng prompt.
- Server + dashboard + CLI gói trong 1 binary.

**Trade-off chấp nhận được:**
- Viết glue code chậm hơn Python một chút — nhưng codebase chỉ ~2-3k LOC.
- Phần cần iterate nhiều nhất là **prompt** (markdown, không phải code) → ngôn ngữ orchestrator gần như không ảnh hưởng tốc độ tinh chỉnh hành vi agent.

## 5. UI — nên làm, nhưng theo phase

**v0 không cần UI** (CLI `coxagent report` thay `state_report.py`). **v1 làm dashboard web** — đây là chỗ ăn tiền vì bản gốc gần như mù:

- **Team board (war room)**: card từng agent (running/standby + lý do), transcript
  streaming, nhịp tim cycle, thread thảo luận + highlight escalation (xem 3i)
- **Chat với SM**: panel chat persistent — hỏi status, ra yêu cầu, dự scrum events
- **Trang chính**: cycle hiện tại + timeline các cycle, agent đang chạy, claim đang mở
- **Backlog / Bugs / Completed**: bảng có filter theo status/priority/severity
- **Deploy history**: version, features/bugs mỗi bản, so version dashboard đang chạy (thay `dashboard_version()` scrape)
- **Live log** (SSE tail `logs/workflow.log`) + **transcript viewer** từng agent run
- **Điều khiển**: pause/resume loop, chạy ngay 1 agent, bật/tắt feature-dev, sửa `project_context.md`
- Tech: React + Vite + Tailwind, build tĩnh nhúng `rust-embed` → vẫn 1 binary, mở `http://localhost:4000`
- Không cần Tauri/desktop — web localhost là đủ và khớp mô hình `dashboard_url` của guide

## 6. Roadmap

| Milestone | Nội dung | Ước lượng |
|---|---|---|
| **M0** | Scaffold clean-architecture workspace (domain/application/infrastructure/presentation/app — xem 3m), domain model + transitions + **StateStorePort** + JsonStateStore (atomic, lock, validate) + unit/contract test + CI (fmt, clippy pedantic, deny warnings). Schema có sẵn `ready`/`design`/`parent_id`/`depends_on` | 1 ngày |
| **M1** | Engine trait + OpencodeEngine + engine registry/discovery (quét PATH) + engine_mapping theo role trong config, chạy end-to-end 1 agent (BA ghi backlog thật) | 1 ngày |
| **M2** | Full loop 4 agents, claim/release trong orchestrator, port 3 prompt, graceful shutdown, tracing (DEV tạm lấy feature `pending` — bypass gate) + `coxagent onboard` greenfield (draft + human gate) | 1-1.5 ngày |
| **M2.5** | Bật SA agent: prompt `sa_system.md`, design gate + architecture.md/ADR + review định kỳ, enforce DEV chỉ lấy `ready` | 0.5-1 ngày |
| **M3** | Robustness: validate/repair state, archive, `coxagent report`, daemon + launchd install, schema_version + migration, release pipeline cargo-dist. Kèm: render CHANGELOG.md từ state (code) | 1-1.5 ngày |
| **M3.5** | Tài liệu: agent DOCS (user guide sau TEST), DoD docs cho DEV (README/api.md), TEST check docs-drift | 0.5-1 ngày |
| **M4** | Mở rộng đội hình: scrum mode (sprint.rs, PO + SM, field-permission theo role, metrics) + agent PD (design.ux + design_system.md) + `onboard --existing` brownfield (archaeology + baseline TEST) | 2-2.5 ngày |
| **M5** | axum API + SSE + React dashboard: team board live (transcript streaming), sprint board, metrics, roadmap view + project detail, settings engine-mapping, nhúng rust-embed | 2.5-3.5 ngày |
| **M6** | Teamwork: discussion threads (trigger + turn budget + decision block), SM chat persistent, escalation/highlight, scrum events thành thread | 2-3 ngày |
| **M7** | Nâng cao: ClaudeEngine, chromiumoxide UI test, PD design-QA bằng screenshot (multimodal), multi-project, **desktop app Tauri (.dmg/.msi, tray, auto-update, first-run wizard — xem 3p)** | sau |
| **M8** | Team mode (hub & workers — xem 3l, 3n): hub Postgres (users/projects/tickets/claims/jobs), RBAC + root bootstrap + invites, RemoteStateStore, claim lease + heartbeat, job queue + trigger theo role, scrum event = job hub một-lần, depends_on + dependency graph, branch-per-ticket + merge queue, fleet view + engine capability report | sau |
| **M9-M11** | Enterprise track (xem 6b): SSO/SCIM/2FA, AI governance (policy engine, budget, BYO endpoint, air-gapped), SRE (HA, DR, SLO), tích hợp (Slack/Teams, Jira import, API) + compliance | sau |

## 6b. Enterprise track — nguyên tắc & 4 trụ

**Nguyên tắc: không xây enterprise trước khi M0–M6 chạy.** Chỉ gieo 5 hạt giống
rẻ-bây-giờ-đắt-về-sau vào core:
1. **Audit log bất biến**: domain events persist append-only từ đầu.
2. **Token/cost tracking per run** (M1): adapter ghi usage mỗi run → roll-up
   ticket/role/project → "feature X tốn $2.30".
3. **`org_id` trong schema hub** từ ngày đầu M8.
4. **OpenTelemetry hooks** trong tracing từ M5.
5. **Policy hook trong job queue** (v1 rỗng) — guardrails sau này chỉ là điền luật.

### M9 — Security & Identity
SSO SAML+OIDC (Okta/AD), SCIM, AD groups → role; 2FA; service accounts + API tokens
có scope; vault/KMS; encryption at rest; SBOM + signed releases + dep scanning.

### M9-M10 — AI Governance & Cost (trụ khác biệt nhất)
- Policy engine per project: duyệt-trước-deploy theo môi trường, path cấm,
  model allowlist, max spend/ngày — human gates thành CẤU HÌNH ĐƯỢC.
- Budget token theo project/user: chạm trần → pause loop + SM báo.
- Data privacy: BYO LLM endpoint / self-host (ollama, bedrock, vertex) qua
  AgentEnginePort — code không rời hạ tầng khách; air-gapped khả thi.
- Phòng thủ prompt injection ở orchestrator (nội dung từ ticket/web không trộn
  vào prompt hệ thống).

### M10 — Vận hành/SRE
Hub stateless nhiều replica (job queue FOR UPDATE SKIP LOCKED), Postgres replication,
backup có KIỂM RESTORE định kỳ, RPO/RTO tuyên bố, zero-downtime migration,
status page, SLO + alerting, retry/DLQ cho jobs.

### M10-M11 — Tích hợp & Compliance
Slack/Teams (SM ping channel), GitHub/GitLab Enterprise, webhook + REST API versioned,
Jira import/export (cần cho adoption/migration), license/seats, telemetry opt-out,
LTS; nền SOC 2/ISO 27001: audit bất biến, retention config, access review.

## 7. Quyết định mặc định (đổi được qua config)

- **Engine mặc định**: `opencode` (khớp guide, provider/model trong config.json); `claude` là engine thứ hai qua trait.
- **Mode mặc định**: `kanban` (flow liên tục như bản gốc); `scrum` bật tầng sprint + PO/SM khi cần kỷ luật ưu tiên.
- **State**: file JSON trong `state/` (không SQLite) — tương thích guide, human-readable, git-friendly.
- **UI**: sau M3, không chặn core loop.
- **UI smoke test**: giữ `smoke.js` (Node + puppeteer-core), orchestrator parse JSON output.
