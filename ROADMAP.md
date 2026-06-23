# rein 로드맵 — 보류된 아이디어

지금 만들기엔 위험하거나 시기상조라 **의도적으로 미룬** 기능을 보관한다. 각 항목은 "왜 미뤘는지"와 "다시 꺼낼 때 이미 내린 설계 결론"을 함께 적어, 미래에 처음부터 다시 고민하지 않도록 한다. 현재 동작하는 설계는 [PLAN.md](PLAN.md) 참고.

---

## LLM 보조 계획 저술 (enrich skill)

**상태: 보류 (too risky for now)**

LLM이 task 문서의 `## Goal` / `## Tasks` / `## Validation`에 초안을 채워 넣어 문서를 "풍부하게" 만드는 skill(가칭 `enrich`)과, 그것이 호출할 저술용 CLI.

### 출발점이 된 문제

issue/PR 생성 시 title을 어떻게 정할 것인가:

1. 생성 시 사람이 입력
2. task 문서를 활용
3. LLM이 title+body 생성

핵심 관찰: `rein issue`는 이미 **frontmatter `title` + `issue_projection`(Goal/Tasks/Validation/Notes)** 으로 발행한다. 즉 title의 source of truth는 이미 문서 안에 있다. PR 시점은 "이 작업이 뭐였나"를 *diff에서 재구성*하는 자리라 title 결정에 부적합하다 — 그 사고는 *계획 시점*에 속하고, rein엔 그 칸(문서)이 이미 있다.

따라서 **문서를 앞단에서 풍부하게 만들면 title 문제는 상류에서 녹는다.** PR 시점 LLM 생성 없이 `rein issue`/PR projection이 그대로 좋은 title+body를 뽑는다. 파이프라인:

`rein new`(title) → enrich(Goal/Tasks/Validation) → `rein run`(실행: check/log/fail) → `rein issue`/PR(풍부해진 문서에서 투영)

### 왜 미뤘나

rein의 핵심 불변식과 정면으로 맞닿는다 — 현재 모델은 **"사람이 계획하고, LLM은 실행만 한다."** LLM-safe mutation(`check`/`uncheck`/`log`/`fail`/`retry`)은 *이미 존재하는* 항목을 토글/주석할 뿐이고, 계획 내용(Goal 텍스트, Tasks/Validation 항목)은 `rein open`으로 **사람만** $EDITOR에서 작성한다. enrich는 rein이 의도적으로 사람에게 남겨둔 바로 그 칸을 LLM에게 여는 것이라, "사람 계획 / LLM 실행"을 "LLM도 (리뷰하에) 계획"으로 바꾸는 product 차원의 전환이다. 지금 서두를 이유가 없어 보관한다.

### 다시 꺼낼 때 — 이미 내린 설계 결론

처음부터 다시 고민하지 말 것. 아래는 확정된 방향이다.

**1) 진짜 축은 "사람 vs LLM"이 아니라 "raw-text 편집 vs rein-매개 mutation"이다.**
불변식의 실제 내용은 "LLM이 파일을 못 바꾼다"가 아니다 — `check`/`log`도 파일을 바꾼다. 정확히는 **"LLM이 rein이 통제하지 못하는 방식으로 파일을 못 바꾼다"** 이고, 안전성은 rein이 직렬화(바이트)를 소유하고 LLM은 텍스트만 넘기는 데서 온다.

**2) freeform 파일 편집 틈은 절대 열지 않는다 — append-only CLI로만 매개한다.**
raw 파일에 "Goal 섹션만 고쳐줘"는 prompt(부탁)이지 enforcement(강제)가 아니다. "일부만" 편집 틈은 최악의 절충 — 기계적 보장은 잃고(prompt는 깨짐) 편의는 절반만 얻는다. 질문은 "틈이 얼마나 큰가"가 아니라 **"틈이 기계적으로 갇혔나 prompt로만 갇혔나"** 이다. freeform 틈이 다시 여는 실패 모드: marker(`<!-- task:id -->`, managed-section) 손상으로 sync/check 붕괴, append-only Agent Log 위조, 병렬 worktree 동시 편집 race, 그리고 자율 background 실행의 신뢰 상실.

**3) 섹션별 의미론 — "additive only, never destructive" 단일 규칙으로 통일한다.**

- **Goal**: `set-if-empty`. 빈 문서엔 초안을 쓰되 이미 사람이 쓴 Goal은 못 덮는다. (자유 rewrite는 사람 intent를 조용히 덮어쓰고, 실행 agent가 Goal을 편집해 골대를 옮겨 완료를 조작할 수 있어 금지.) 기존 Goal 다듬기는 `rein open`, 또는 *대화형 사람*에게만 여는 `--force` escape hatch.
- **Tasks**: 새 항목 **append만**. 기존 항목 텍스트는 불변(상태는 `check`/`fail`로만). 이유는 스타일이 아니라 **ID↔check-state 바인딩 보호** — 항목을 rewrite하면 `<!-- task:id -->`와 상태가 날아간다.
- **Validation**: Tasks와 동일.

세 섹션 모두 *기존 내용을 건드리지 않는다*. Goal=빈칸채우기는 단일 값에 대한 'add'일 뿐. 이 한 줄로 추론이 단순해지고 반자율 실행도 안전해진다.

**4) 우아한 보상 — additive 의미론이 role gating 없이 per-role 안전을 준다.**
실행 중 Goal은 non-empty → `goal set`은 **no-op** → 실행 agent가 골대를 못 옮긴다. 실행 agent가 실수로 `task add`를 불러도 최악이 "잉여 항목 하나"지 corruption이 아니다. 즉 "어느 agent가 이 command를 쓸 수 있나"를 기계적으로 막지 않아도 command 자체가 비파괴적이라 안전하다. prompt 수준 role 분리(enrich엔 add/goal, run-rein-task엔 check/log/fail)만으로 충분.

**5) 재실행 중복.**
enrich를 두 번 돌리면 append-only Tasks에 중복 항목이 쌓인다. → skill이 `rein todo`로 기존 항목을 먼저 읽고 *진짜 새 것만* 넘긴다(dedup은 skill 책임, 바이트는 여전히 rein 소유). Goal은 set-if-empty라 2회차엔 자동 no-op.

### 짓기 전 후회 없는 첫 걸음

저술 CLI(`rein task add`, `rein goal`)부터 만들지 말 것. 먼저 **enrich skill이 초안을 stdout으로만 뽑고 사람이 `rein open`으로 붙여넣는 형태(새 command 0개, 새 불변식 표면 0개)** 로, "LLM 초안이 commit할 가치가 있을 만큼 좋은가"를 검증한다. 좋다고 확인된 뒤에야 위 3)의 의미론으로 CLI를 추가한다.
