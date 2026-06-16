# rein: LLM Task Journal 계획

## 배경

기존 방식은 별도 Markdown 파일에 작업 목록을 쓰고, LLM에게 작업이 끝나면 체크해달라고 지시하는 형태였다. 이 방식은 간단하지만 다음 문제가 있다.

- 작업 전 초안을 다른 사람에게 공유하기 어렵다.
- GitHub Issue UI는 긴 task list를 빠르게 편집하기 불편하다.
- 진행 중인 작업, 앞으로 할 작업, 완료한 작업의 히스토리가 한 곳에 정리되지 않는다.
- Issue body와 PR body가 중복될 수 있다.
- 모든 issue나 PR을 로컬에 캐싱하는 것은 과하다.

따라서 새 도구의 목적은 GitHub Issues/PRs 전체를 파일시스템처럼 복제하는 것이 아니라, LLM에게 맡길 작업 문서를 관리하고 필요한 경우 GitHub와 연결하는 것이다.

## 제품 정의

도구 이름은 **rein**(고삐)다. harness를 채운 말에게 명령을 전달하는 인터페이스라는 뜻으로, LLM에게 작업을 지시하는 이 도구의 성격을 담는다. 영어에서 고삐는 좌우 한 쌍으로 쓰여 명사로는 복수이지만 우리는 cli의 편의를 위해 단수 취급한다.

이 도구는 **LLM task journal + shared inbox manager**다.

역할 분리는 다음과 같다.

- GitHub Issues: 작업 전 공유 가능한 inbox와 협업 표면
- rein store (repo별 로컬 store, 기본 홈 경로): 개인 작업 문서, 실행 상태, 히스토리
- PR body: 현재 코드 변경의 리뷰 가능한 공개 요약
- Claude Skill 또는 MCP: LLM이 작업 문서를 읽고 실행하는 규칙과 안전한 상태 변경 API

### 각 플랫폼 별 명칭

- Product/brand: Rein
- GitHub repo: rein
- Rust crate: reins (rein 이미 존재 — `-rs` 접미사 대신 복수 명사로. 설치는 `cargo install reins`, 명령은 `rein`)
- CLI binary: rein

## 핵심 원칙

- 모든 GitHub issue/PR을 동기화하지 않는다.
- 도구가 소유한 task issue만 동기화한다.
- 로컬 파일은 실제 Markdown 파일이어야 한다.
- FUSE는 MVP 범위에서 제외한다.
- GitHub는 source of truth가 아니라 공유/출판 대상이다.
- 현재 실행 중인 작업은 로컬 task 문서가 source of truth다.
- PR이 생성된 이후에는 PR body를 리뷰용 뷰로 갱신한다.
- task 항목에는 안정적인 ID를 둔다.
- 원격 issue/PR body는 통째로 소유하지 않는다. 마커로 구획된 managed section만 도구가 갱신한다.
- LLM은 상태 변경을 Markdown 직접 편집이 아니라 CLI mutation 명령으로 한다.

### Single source of truth

truth는 한 곳에만 두고, 나머지는 파생되거나 재생성 가능해야 한다.

| 정보 | canonical | 파생/캐시 |
| --- | --- | --- |
| task 상태 | 디렉토리 위치 (`inbox/`, `active/`, ...) | frontmatter `status` (도구가 이동 시 자동 갱신) |
| task identity, GitHub 링크 | frontmatter (`id`, `github_issue`, `github_pr`) | `state/<id>.json`의 task 항목 |
| repo ↔ store 매핑 | `git config rein.store` (UUID) | `meta.json`의 repo 힌트 (common-dir 경로, remote URL) |
| 실행 중 task↔worktree 바인딩 | worktree git-dir의 `rein-task` 포인터 | `state/<id>.json`의 `branch`/`worktree` (표시용) |
| current task (단일 모드) | store의 `current` 파일 포인터 | 없음 (별도 `current.md`를 두지 않는다) |
| 동기화 base | `state/<id>.json`의 synced hash | 없음 |
| 원격 issue/PR body의 managed section | 로컬 task 문서 | 원격 body는 출판 결과물 |

store의 `state/`가 깨지거나 사라져도 `rein doctor`가 task 파일 스캔으로 재생성할 수 있어야 한다.

## 디렉토리 구조

rein store는 프로젝트 안이 아니라 repo별 홈 경로에 둔다. worktree 면역만이 이유는 아니다 — 그것만이라면 `$(git rev-parse --git-common-dir)/rein/`(`.git` 안)으로도 충분하다. 외부에 두는 이유는:

- task 문서는 사람이 `$EDITOR`나 Obsidian으로 편집하는 1급 데이터인데, `.git` 내부는 에디터·백업 도구·인덱서가 무시하거나 위험하게 취급하는 영역이다.
- `done/` 히스토리는 repo를 지워도 남길 가치가 있다. store 수명을 repo 수명과 분리한다.
- `~/.local/share/rein/*` 스캔으로 전 프로젝트 뷰(future `rein ui --all`)가 공짜로 나온다.
- task 문서가 실수로 커밋되거나 gitignore 관리가 새는 사고가 구조적으로 사라진다.

이하 예시의 `<store>`는 이 경로를 가리킨다.

```text
${XDG_DATA_HOME:-~/.local/share}/rein/<repo-key>/   # = <store>
  inbox/
    settings-cleanup.md
    auth-refactor.md
  active/
    settings-cleanup.md
  done/
    2026-06/
      settings-cleanup.md
  canceled/
  conflicts/
    settings-cleanup.remote.md
    settings-cleanup.local.md
  state/
    task-20260612-settings-cleanup.json   # per-task: synced hash, branch, worktree
    task-20260612-auth-refactor.json
  current        # 단일 모드 task 포인터 (작은 파일)
  sync.lock      # sync 명령(issue/pull/pull-inbox/push) 직렬화용 flock
  meta.json      # store 버전 + repo 힌트(common-dir 경로, remote URL)

project/                              # git repo (worktree 1..N)
  .claude/skills/run-rein-task/SKILL.md     # repo에 커밋되는 실행 recipe
  .git/worktrees/<name>/rein-task           # worktree → task 바인딩 (per-worktree)
```

store 위치 해석:

- `<repo-key>`는 `rein init`이 발급해 `git config rein.store`에 기록하는 UUID다. `.git/config`는 common dir에 있으므로 모든 worktree가 같은 키를 공유하고, 프로젝트 디렉토리를 옮기거나 이름을 바꿔도 store가 따라간다. clone 시에는 복사되지 않으므로 클론별 분리도 유지된다.
- 경로 기반 키(common-dir 경로 해시)는 쓰지 않는다. 디렉토리 이동/rename 시 store가 사라진 것처럼 보이기 때문이다 (VS Code workspaceStorage가 이 방식의 악명 높은 선례).
- 어느 worktree에서 `rein`을 실행하든 cwd에서 repo를 찾아 `rein.store` config → store를 해석한다. config가 없으면 "run `rein init`" 에러를 낸다.
- `REIN_ROOT`로 store 위치를 override할 수 있다.
- store가 프로젝트 밖에 있으므로 git에 섞일 일이 없다(`.git/info/exclude` 불필요). 사람의 가시성은 파일 트리가 아니라 `rein open`(특정 task를 `$EDITOR`로)/`rein ui`(목록+preview)/`rein root`(store 경로 출력)가 담당한다.

파일명 규칙:

- 파일명은 slug만 쓴다. 날짜는 frontmatter와 task ID에 이미 있다.
- 상태 전환(inbox → active → done) 시 파일명을 바꾸지 않는다. 디렉토리만 이동한다.
- slug가 충돌하면 `-2`, `-3` suffix를 붙인다.

## Task 문서 포맷

```markdown
---
id: task-20260612-settings-cleanup
title: Settings cleanup
status: inbox
created_at: 2026-06-12T18:30:00+09:00
updated_at: 2026-06-12T18:30:00+09:00
github_issue: 456
github_pr:
branch:
tags: [ui, cleanup]
shared: true
---

## Goal

Settings page cleanup and error handling.

## Tasks

- [ ] <!-- task:1 --> Settings page responsive layout
- [ ] <!-- task:2 --> Show toast on save failure
- [ ] <!-- task:3 --> Add failure-path tests

## Validation

- [ ] <!-- task:4 --> Tests pass
- [ ] <!-- task:5 --> Manual desktop check
- [ ] <!-- task:6 --> Manual mobile check

## Notes

Constraints, discussion summary, or context for the agent.

## Agent Log

<!-- append-only -->
```

item ID 규칙:

- item ID는 `<!-- task:1 -->`처럼 **task별로 안정적인 정수**를 HTML comment로 둔다. 줄 번호가 아니라 한 번 부여되면 고정되는 일련번호다: 항목을 재정렬하거나 문장을 바꿔도, 위에 줄을 추가해도 번호는 그 항목에 그대로 남는다. 새 항목은 그 task의 `max(기존 정수 ID) + 1`을 받는다. 이래야 사람 편집·도구 편집·GitHub projection·LLM의 비동기 호출을 가로질러 같은 항목을 가리킬 수 있다. (줄 번호는 이 네 가지에서 모두 어긋난다.)
- HTML comment라 Obsidian/GitHub 렌더에는 보이지 않는다. 사용자는 `rein status`가 보여주는 번호로 `rein check <n>`을 친다.
- ID 부여는 도구가 task 문서를 만지는 모든 시점에 일어난다: `open`(에디터 종료 후), mutation(`check`/`uncheck`/`log`/`fail`), `doctor`, 그리고 GitHub 동기화(`issue`/`push`/`pull`). 로컬 전용 워크플로우에서도 GitHub 없이 ID가 부여된다.
- Tasks와 Validation은 하나의 정수 시퀀스를 공유한다. `check`와 MCP tool이 둘을 구분 없이 다룬다.

## 기본 워크플로우

### 1. 로컬 작업 초안 생성

```sh
rein new "settings cleanup"
rein open settings-cleanup
```

결과:

- `<store>/inbox/settings-cleanup.md` 생성
- `$EDITOR`로 파일 열기
- 아직 GitHub에는 공개하지 않음

### 2. 공유 inbox로 발행 (`issue`)

```sh
rein issue settings-cleanup
```

결과:

- GitHub issue 생성
- issue body는 로컬 문서의 projection이다: frontmatter와 `Agent Log`를 제외하고, 전체를 ownership 마커로 감싼다.
- label은 `rein` 하나만 쓴다. 상태(inbox/active)는 로컬에서만 추적한다.
- local frontmatter에 `github_issue` 기록

issue body 형태:

```markdown
<!-- rein:begin task-20260612-settings-cleanup -->

## Goal

...

## Tasks

...

## Validation

...

<!-- rein:end -->
```

label이 실수로 제거되어도 body의 마커로 도구 소유 issue를 식별할 수 있다.

### 3. 공유 inbox 동기화

```sh
rein pull-inbox
```

결과:

- `rein` label이 붙은 issue만 가져옴
- 전체 issue 목록은 캐싱하지 않음
- identity는 body 마커의 task ID다: 마커 ID가 로컬에 있으면 그 문서를 갱신하고, 없으면 새 ID 발급 없이 마커의 ID로 생성한다. 마커가 없는 issue(사람이 GitHub에서 직접 만든 것)만 새 ID를 발급하고 다음 push 때 마커를 기록한다.
- 같은 issue는 누가 몇 번을 pull해도 같은 문서로 수렴한다 (idempotent)

### 4. 작업 시작

```sh
rein start settings-cleanup
```

결과:

- 문서를 `<store>/active/`로 이동 (inbox→active는 원자적 rename = claim)
- `--worktree`면 git worktree + 브랜치를 만들고, 그 worktree의 `.git/worktrees/<n>/rein-task`에 task ID를 기록한다 (이후 그 worktree의 `rein` 명령은 cwd로 task를 안다)
- `--worktree`가 없으면 store의 `current`를 이 task ID로 갱신 (단일 모드)
- 선택적으로 draft PR 생성
- GitHub issue에 `Started in PR #123` 코멘트 추가

### 5. LLM 실행

```sh
claude
/run-rein-task
```

skill은 `rein current --path`로 현재 task 문서를 찾는다.

LLM은 다음 규칙을 따른다.

- unchecked task만 수행한다.
- 상태 변경은 Markdown 직접 편집이 아니라 CLI mutation 명령으로 한다.
  - `rein check <item-id>`: 구현과 검증이 끝난 항목 체크
  - `rein log "<text>"`: Agent Log에 append
  - `rein fail <item-id> --reason "<text>"`: blocker 기록
- 완료하지 않은 항목은 체크하지 않는다.
- 각 task 완료 후 관련 검증을 수행한다.
- 필요하면 `rein push`로 PR body나 issue body를 갱신한다.

### 6. PR body 갱신

```sh
rein attach-pr 123
rein push
```

결과:

- PR body 전체를 덮어쓰지 않는다. `rein:begin`/`rein:end` 마커 사이의 managed section만 갱신하고, 마커 바깥에 사람이 쓴 내용은 보존한다.
- managed section에 task list, validation 상태, 요약을 넣는다.
- `Agent Log`는 접거나 요약해서 리뷰어가 보기 좋게 변환한다.

### 7. 완료 처리

```sh
rein done settings-cleanup
```

결과:

- pre-flight: worktree가 있고 dirty(미커밋 변경)면 아무것도 하지 않고 에러를 낸다. 커밋/스태시 후 재시도하거나 `--keep-worktree`로 worktree를 남기고 done 처리만 한다. 부작용을 시작한 뒤 중간에 실패해 어중간한 상태가 남는 것을 막는다.
- 문서를 `<store>/done/YYYY-MM/`로 이동
- 관련 issue를 닫거나 완료 코멘트를 남김
- PR body의 managed section에 최종 summary와 validation 갱신
- worktree가 있으면 `git worktree remove` (포인터도 함께 사라짐). 브랜치는 지우지 않는다 — 머지된 브랜치 정리는 GitHub/`gh`의 몫이고, rein이 지우면 미머지 작업을 날릴 위험만 진다.

cancel의 경우:

- 같은 pre-flight를 거친다. 작업을 버리려는 의도일 수 있으므로 `--force`(dirty여도 worktree 제거)를 둔다.
- 문서를 `<store>/canceled/`로 이동
- 발행된 issue가 있으면 "not planned"로 닫고 코멘트를 남김

## CLI 명령

```text
rein init [--skill]           # store 생성 + git config rein.store 발급
rein new <title> [--shared]
rein list [--status inbox|active|done|canceled]
rein open [task]              # 인자 없으면 fuzzy picker
rein current [--path]         # 조회 전용. resolution 순서(#1→#4) 적용
rein use <task>               # 전환. 바인딩된 worktree 안이면 포인터, 밖이면 current 파일
rein start <task> [--worktree] [--branch] [--draft-pr]

# LLM-safe mutation (skill이 사용. --task 생략 시 resolution 순서를 따른다)
rein check <item-id> [--task <id>]
rein uncheck <item-id> [--task <id>]
rein log <text> [--task <id>]
rein fail <item-id> --reason <text> [--task <id>]

rein issue <task>
rein pull-inbox
rein pull
rein push [--resolved]
rein attach-issue <number>
rein attach-pr <number>
rein done [task] [--keep-worktree]
rein cancel [task] [--keep-worktree] [--force]
rein doctor                   # state/ 재생성, 무결성 검사
rein status
rein root                     # store 경로 출력
rein ui                       # TUI dashboard (Phase 5)
```

`search`는 두지 않는다. store가 평범한 Markdown 디렉토리라 `rg $(rein root)`로 충분하고, 필요해지면 추가한다.

`init`의 역할: git repo 확인 → `git config rein.store <uuid>` 기록(없을 때만) → store 스켈레톤 생성 → `meta.json`에 store 버전과 repo 힌트 기록. `--skill`은 repo에 커밋되는 `.claude/skills/run-rein-task/SKILL.md`를 스캐폴드한다(opt-in). init 안 된 repo에서 다른 명령을 실행하면 "run `rein init`" 에러를 낸다. auto-init은 git config와 홈 디렉토리에 암묵적으로 쓰는 부작용이 있어 MVP에선 두지 않는다.

## GitHub 동기화 범위

동기화 대상은 다음으로 제한한다.

- `rein` label이 붙은 issue
- 로컬 task 문서에 `github_issue`가 기록된 issue
- 로컬 task 문서에 `github_pr`이 기록된 PR

동기화 단위와 소유권:

- 동기화 단위는 body 전체가 아니라 마커로 구획된 managed section이다.
- 소유권 판정은 label이 아니라 body의 `rein:begin` 마커로 한다. label은 검색 필터일 뿐이다.

동기화하지 않는 것:

- 전체 issue 목록
- 전체 PR 목록
- 댓글 전체 히스토리의 완전한 로컬 복제
- GitHub Projects 전체 상태

transport는 MVP에서 `gh` CLI subprocess를 쓴다. 인증을 `gh`에 위임할 수 있어 토큰 관리가 필요 없다. API 클라이언트(octocrab 등) 도입은 필요해질 때 한다.

sync 명령(`issue`/`pull`/`pull-inbox`/`push`)은 store 단위 `sync.lock`(flock)으로 직렬화한다. 드물고 네트워크에 묶인 명령이라 직렬화 비용은 체감되지 않고, 동시 import로 같은 issue에서 문서가 두 개 생기는 race를 차단한다. mutation 명령(`check`/`log`/`fail`)은 이 lock과 무관하게 lock-free다.

## 상태 파일

store의 `state/`는 재생성 가능한 캐시 + 동기화 메타데이터만 가진다. identity와 GitHub 링크의 truth는 각 task 파일의 frontmatter다.

task마다 파일 하나로 분리한다. 이유는 병렬 실행이다(아래 "병렬 worktree 실행" 참고): "task 하나 = 소유 워커 하나 = writer 하나" 불변식이 성립하므로 워커끼리 같은 파일을 동시에 쓰지 않는다.

```json
// <store>/state/task-20260612-settings-cleanup.json
{
  "version": 1,
  "path": "active/settings-cleanup.md",
  "branch": "rein/settings-cleanup",
  "worktree": "<store>/worktrees/settings-cleanup",
  "issue_synced_hash": "sha256:...",
  "pr_synced_hash": "sha256:..."
}
```

```text
// <store>/current   (단일 모드 포인터, 작은 파일)
task-20260612-settings-cleanup
```

- `path`는 빠른 lookup용 캐시다. truth는 파일 자체의 위치다.
- `branch`/`worktree`는 표시·정리용 캐시다. 실행 중 바인딩의 truth는 worktree git-dir의 `rein-task` 포인터다.
- `*_synced_hash`는 마지막으로 성공한 push/pull 시점의 projection(managed section 내용) 해시다. 충돌 판정의 base가 된다.
- 쓰기는 temp 파일 + `rename()`으로 원자적으로 한다. writer가 하나이므로 글로벌 lock은 불필요하고, 부모/워커가 같은 task를 동시에 건드리는 희귀 케이스만 그 파일 하나에 per-file flock으로 막는다.
- `state/`가 깨지면 `rein doctor`가 task 파일 스캔으로 재생성한다. synced hash는 유실 시 다음 sync에서 conflict로 안전하게 fallback한다.

## 충돌 처리

로컬 문서와 GitHub issue/PR body가 동시에 바뀐 경우 자동 병합을 무리하게 시도하지 않는다.

판정은 3-way hash 비교로 한다.

- base: `state/<id>.json`의 synced hash (마지막 동기화 시점의 managed section 해시)
- local: 현재 로컬 문서의 projection 해시
- remote: 현재 원격 managed section 해시

| local  | remote | 처리      |
| ------ | ------ | --------- |
| = base | = base | 변경 없음 |
| ≠ base | = base | push      |
| = base | ≠ base | pull      |
| ≠ base | ≠ base | conflict  |

- issue의 `updated_at`은 댓글, label, assignee 변경에도 갱신되므로 충돌 판정에 쓰지 않는다.
- conflict 시 `<store>/conflicts/`에 local/remote 백업을 저장한다.
- 사용자가 해결한 뒤 `rein push --resolved`를 실행한다.

## 병렬 worktree 실행

rein은 스케줄러나 오케스트레이터가 아니다. 병렬 실행은 Claude Code(agent view) 같은 외부가 주도하고, rein은 그 아래에서 **동시 접근에 안전한 store**를 제공하는 역할만 한다.

상정하는 시나리오: 하나의 Claude Code 세션이 여러 task를 각각 다른 git worktree에서 백그라운드 에이전트로 동시에 돌린다.

### task ↔ worktree 바인딩 (env가 아니라 cwd)

세션이 하나이고 Bash 호출 간 shell 상태가 유지되지 않으므로, "내 task가 무엇인지"를 env로 들고 있을 수 없다. 대신 각 백그라운드 에이전트가 서로 다른 worktree에 고정(pinned)된다는 점을 이용해, task를 cwd에서 resolve한다.

- `rein start <task> --worktree`가 worktree + 브랜치를 만들고, 그 worktree의 `$(git rev-parse --git-dir)/rein-task`(= `.git/worktrees/<n>/rein-task`)에 task ID를 기록한다.
- 모든 `rein` 명령은 매 호출마다 이 포인터를 fresh read해서 task를 정한다. shell 상태 비유지·단일 세션과 무관하게 정확하다.
- 포인터는 워킹트리를 더럽히지 않고, `git worktree remove` 시 함께 사라진다.
- main checkout에서는 `git rev-parse --git-dir`가 `.git`이므로 포인터가 `.git/rein-task`에 놓인다. 메커니즘은 동일하고, 포인터가 없으면 #3/#4로 떨어진다.

task resolution 순서:

```text
1. --task <id>            (명시 플래그)
2. cwd worktree 포인터      (.git/worktrees/<n>/rein-task)  ← 병렬 모델의 주 경로
3. REIN_TASK env          (worktree 없이 세션을 task별로 쓰는 fallback)
4. store의 current 파일     (단일 대화형 모드 fallback)
```

`rein current`는 조회 전용 명령으로 이 순서를 그대로 적용한다 — worktree 안에서는 #2가 반환되므로 skill은 병렬 모드에서 수정 없이 동작한다. 전환은 별도 명령 `rein use <task>`가 맡는다: 바인딩된 worktree 안이면 그 포인터를 다시 쓰고, 밖이면 store의 `current` 파일을 쓴다. 전환 대상이 resolution에서 읽히는 위치와 대칭이다.

mutation footgun 방지: mutation 명령(`check`/`uncheck`/`log`/`fail`)이 #4(current 파일)로 resolve됐고 active task가 2개 이상이면 에러로 거부한다. 에러 메시지에 active task 목록과 "worktree에서 실행하거나 `--task <id>`를 쓰라"는 안내를 담는다. 워커가 잘못된 cwd에서 실행해 조용히 엉뚱한 task를 변경하는 것을 막기 위함이다. active가 1개면 모호하지 않으므로 허용하고, #3(env)은 의도적 설정이므로 게이트하지 않으며, 조회 명령도 게이트하지 않는다.

### claim: 원자적 rename

inbox/active 상태가 디렉토리 위치이므로, `rein start`의 inbox→active 이동은 파일 `rename()`이다. 같은 파일시스템에서 rename은 원자적이라, 두 워커가 같은 task를 동시에 start하면 먼저 rename한 쪽이 이기고 나머지는 실패한다. 별도 lock 없이 이것이 곧 claim이다. 결과적으로 한 task는 동시에 한 워커만 소유한다.

### 상태 동시 쓰기

claim 덕분에 "task 하나 = 소유 워커 하나 = `state/<id>.json` writer 하나" 불변식이 성립한다. 그래서:

- 워커끼리는 서로 다른 task 파일만 쓴다 → 교차 task 경합 0, hot path에 글로벌 lock 불필요.
- 쓰기는 temp + `rename()`으로 원자적. 동시 RMW가 없으므로 이것만으로 충분하다.
- 부모 세션이 워커가 도는 task를 동시에 건드리는 희귀 케이스만 그 파일 하나에 per-file flock으로 막는다(글로벌 lock 아님).

task 문서(.md)도 워커마다 다른 파일이라 편집 충돌이 없다. 충돌 감지(3-way hash)는 local↔remote용이지 local↔local용이 아니다.

### store 공유

- `git config rein.store` → 모든 worktree가 공유하는 키(`.git/config`는 common dir에 있다) → 같은 task 풀(inbox/active). 브랜치가 달라도 키는 같다.
- `git rev-parse --git-dir` → worktree마다 다름 → 어느 task인지.

config 키와 git-dir 포인터가 "공유 풀"과 "내 task"를 분리한다.

## Claude Skill

초기 skill은 얇게 유지한다. 상태 변경은 전부 CLI에 위임한다.

```markdown
---
description: Run the current LLM task document, implement unchecked tasks, update status via rein commands, and append execution notes.
disable-model-invocation: true
---

Run `rein current --path` to find the active task document, then read it.

Rules:

1. Execute only unchecked tasks.
2. Never edit checkboxes or Agent Log in the Markdown directly. Use:
   - `rein check <item-id>` after a task is implemented and verified
   - `rein log "<text>"` to append a concise entry after each completed task
   - `rein fail <item-id> --reason "<text>"` when blocked
3. Preserve `<!-- task:... -->` ID comments when editing other sections.
4. Run relevant tests before checking validation items.
5. If a PR is attached, run `rein push` when finished.
```

## MCP 확장

MVP에서는 CLI + skill로 시작한다. mutation을 처음부터 CLI 명령(`check`/`log`/`fail`)으로 강제하므로, LLM이 Markdown을 깨뜨리는 위험은 MCP 없이도 막는다.

MCP 서버는 CLI를 실행하기 어려운 환경(샌드박스, 비-CLI agent)을 위한 것이며, 같은 mutation 명령을 wrapping하는 thin layer로 만든다.

MCP tool 후보:

```text
task_list()
task_read(task_id)
task_current()
task_claim(task_id, item_id)
task_complete(task_id, item_id, summary)
task_fail(task_id, item_id, reason)
task_append_log(task_id, text)
task_sync_pull(task_id)
task_sync_push(task_id)
```

## UI

UI는 단계적으로 간다. 어느 단계에서든 편집은 `$EDITOR`에 위임한다. 파일이 진짜 Markdown이라는 원칙과 일치하고, UI를 view + dispatcher로 얇게 유지할 수 있다.

### Phase 1~4: CLI + $EDITOR + fuzzy picker

- `rein open`을 인자 없이 실행하면 내장 fuzzy picker로 task를 고른다. nucleo(helix의 fuzzy matcher) 기반이면 외부 의존성이 없다.
- store가 평범한 Markdown 디렉토리이므로 `$(rein root)`를 Obsidian vault나 VS Code workspace로 열어 보는 것도 그대로 동작한다.

### Phase 5: ratatui TUI dashboard

`rein ui`로 진입하는 ratatui 기반 dashboard.

- layout: 좌측 task 목록(status별 그룹), 우측 Markdown preview
- keybinding: `j/k` 이동, `Tab` status 전환, `Enter`로 `$EDITOR` 열기, `s` start, `d` done, `p` issue/push, `/` filter
- TUI 안에서 Markdown을 직접 편집하지 않는다. 편집은 `$EDITOR`, 상태 변경은 기존 CLI 동사와 같은 내부 함수를 호출한다.
- crates: ratatui + crossterm, nucleo(filter), preview는 tui-markdown 또는 자체 간단 렌더링

### 검토한 대안

- local web UI (axum + htmx 등): Markdown 렌더링과 GitHub 링크 연결은 좋지만, 서버를 띄우는 마찰이 있고 터미널 중심 워크플로우와 어긋난다.
- native GUI (Tauri, egui, iced): 배포와 창 관리 오버헤드 대비 이득이 없다. claude를 터미널에서 실행하는 워크플로우와 컨텍스트가 분리된다.
- editor-native (Obsidian, VS Code, Neovim): 코드가 필요 없다는 장점이 있고 Phase 5 전까지의 보완재로 유효하다. 다만 start/issue 같은 상태 전환 동작을 붙일 수 없어 대체재는 아니다.

TUI를 선택한 이유: 워크플로우 전체(claude, git, `$EDITOR`)가 터미널에 있고, SSH 환경에서도 동작하며, gitui/yazi/atuin 같은 검증된 선례가 있다.

## MVP 범위

### Phase 1: Local task journal

- `init`(`git config rein.store` UUID 발급 + store 스켈레톤), `new`, `list`, `open`(fuzzy picker 포함), `current`(조회), `use`(전환), `start`, `done`(pre-flight 포함), `cancel`
- mutation 명령: `check`, `uncheck`, `log`, `fail` (`--task` 플래그, #4 fallback 게이트)
- `doctor` (`state/` 재생성), `root`
- store 구조 생성 (repo별 홈 경로, `REIN_ROOT` override)
- per-task `state/<id>.json` + 원자적 temp+rename 쓰기
- task Markdown 템플릿 생성
- item ID(안정 정수) 자동 부여 — `open`/mutation/`doctor` 등 로컬 터치포인트 포함

### Phase 2: Shared inbox via GitHub Issues

- `new --shared`
- `issue`
- `pull-inbox`
- `attach-issue`
- issue body managed section push/pull
- 마커 task ID 기반 idempotent import
- sync 명령 `sync.lock` 직렬화
- `rein` label 관리 (label은 검색 필터, 소유권은 body 마커)
- 3-way hash conflict detection
- transport: `gh` CLI subprocess

### Phase 3: PR body integration

- `attach-pr`
- draft PR 생성 옵션
- active task 문서에서 PR body managed section 생성
- issue와 PR 상호 링크
- completion summary 생성

### Phase 4: Agent integration

- Claude skill 생성
- `/run-rein-task` workflow 문서화
- `rein start --worktree` + task↔worktree 바인딩 (`.git/worktrees/<n>/rein-task` 포인터, cwd 기반 resolution)
- 병렬 동시 접근 안전성 (per-task state, 원자적 rename claim, 필요시 per-file flock)
- optional hooks
- optional MCP server

### Phase 5: TUI dashboard

- `rein ui` (ratatui)
- task 목록 + Markdown preview
- 상태 전환을 기존 CLI 동사로 dispatch

## 비목표

- FUSE filesystem 구현
- Dropbox처럼 전체 GitHub issue/PR 백그라운드 동기화
- GitHub Projects 전체 복제
- 모든 댓글의 완전한 오프라인 편집
- 여러 agent의 동시 작업 스케줄러 (rein은 동시 접근에 안전할 뿐, 스케줄링은 외부가 한다)
- 자동 병렬 실행 오케스트레이션 (병렬은 Claude Code 등이 주도, rein은 store 안전성만 보장 — "병렬 worktree 실행" 참고)
- TUI 내장 Markdown 에디터

## 추후 고민

- shared inbox issue 하나에 여러 task를 둘지, task 하나당 issue 하나를 둘지
- `Agent Log`를 PR body에 얼마나 노출할지
- issue 댓글을 로컬 문서의 `Discussion`으로 가져올지 링크만 둘지
- task 문서를 git에 커밋하는 팀 모드를 지원할지 (외부 store 구조에선 증분이 아니라 구조 전환이라는 비용 인지)
- store 자체를 git repo로 만들어 mutation마다 auto-commit할지 (로컬 전용 task의 백업 — shared task는 GitHub issue가 부분 백업이지만 로컬 전용은 redundancy가 없다)
- Claude 외 Codex, Cursor, Gemini CLI용 skill/recipe 포맷을 같이 만들지
- 두 클론이 같은 inbox를 공유하길 원할 때의 처리 (`rein.store` 키는 클론별 분리가 기본)

## 현재 결론

가장 작은 유용한 도구는 다음이다.

```text
로컬 Markdown task journal
  + LLM-safe mutation CLI (check / log / fail)
  + 선택적 GitHub issue 발행/pull for shared inbox (managed section)
  + 선택적 PR body push for active work (managed section)
  + Claude skill로 실행 규칙 제공
  + ratatui TUI dashboard (Phase 5)
```

이 구조는 GitHub를 공유와 리뷰 표면으로 쓰면서도, 사람이 실제로 편집하고 LLM이 실행하는 중심 문서는 repo별 로컬 store의 Markdown으로 유지한다(worktree 영향 없음, `rein open`/`ui`로 접근). truth는 항상 한 곳에만 있고, `state/`와 원격 body는 파생물이므로 어긋나도 복구 가능하다.
