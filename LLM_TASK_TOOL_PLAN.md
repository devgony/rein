# LLM Task Journal 계획

## 배경

기존 방식은 별도 Markdown 파일에 작업 목록을 쓰고, LLM에게 작업이 끝나면 체크해달라고 지시하는 형태였다. 이 방식은 간단하지만 다음 문제가 있다.

- 작업 전 초안을 다른 사람에게 공유하기 어렵다.
- GitHub Issue UI는 긴 task list를 빠르게 편집하기 불편하다.
- 진행 중인 작업, 앞으로 할 작업, 완료한 작업의 히스토리가 한 곳에 정리되지 않는다.
- Issue body와 PR body가 중복될 수 있다.
- 모든 issue나 PR을 로컬에 캐싱하는 것은 과하다.

따라서 새 도구의 목적은 GitHub Issues/PRs 전체를 파일시스템처럼 복제하는 것이 아니라, LLM에게 맡길 작업 문서를 관리하고 필요한 경우 GitHub와 연결하는 것이다.

## 제품 정의

이 도구는 **LLM task journal + shared inbox manager**다.

역할 분리는 다음과 같다.

- GitHub Issues: 작업 전 공유 가능한 inbox와 협업 표면
- 로컬 `.llm-task/`: 개인 작업 문서, 실행 상태, 히스토리
- PR body: 현재 코드 변경의 리뷰 가능한 공개 요약
- Claude Skill 또는 MCP: LLM이 작업 문서를 읽고 실행하는 규칙과 안전한 상태 변경 API

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
| task identity, GitHub 링크 | frontmatter (`id`, `github_issue`, `github_pr`) | `state.json`의 task 항목 |
| current task | `state.json`의 `current` 포인터 | 없음 (별도 `current.md` 파일을 두지 않는다) |
| 동기화 base | `state.json`의 synced hash | 없음 |
| 원격 issue/PR body의 managed section | 로컬 task 문서 | 원격 body는 출판 결과물 |

`state.json`이 깨지거나 사라져도 `llm-task doctor`가 task 파일 스캔으로 재생성할 수 있어야 한다.

## 디렉토리 구조

```text
project/
  .llm-task/
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

    state.json

  .claude/
    skills/
      run-llm-task/
        SKILL.md
```

파일명 규칙:

- 파일명은 slug만 쓴다. 날짜는 frontmatter와 task ID에 이미 있다.
- 상태 전환(inbox → active → done) 시 파일명을 바꾸지 않는다. 디렉토리만 이동한다.
- slug가 충돌하면 `-2`, `-3` suffix를 붙인다.

`.llm-task/`는 기본적으로 git에 포함하지 않는다. 도구가 `init` 시 `.git/info/exclude`에 추가한다. 팀 차원의 공유 템플릿이나 skill만 repo에 포함할지는 별도 옵션으로 둔다.

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

- [ ] <!-- task:layout --> Settings page responsive layout
- [ ] <!-- task:toast --> Show toast on save failure
- [ ] <!-- task:tests --> Add failure-path tests

## Validation

- [ ] <!-- task:v-tests --> Tests pass
- [ ] <!-- task:v-desktop --> Manual desktop check
- [ ] <!-- task:v-mobile --> Manual mobile check

## Notes

Constraints, discussion summary, or context for the agent.

## Agent Log

<!-- append-only -->
```

item ID 규칙:

- item ID는 `<!-- task:layout -->`처럼 HTML comment로 둔다. 사람이 문장을 바꾸거나 순서를 바꿔도 LLM과 도구가 동일한 항목을 추적할 수 있게 하기 위함이다.
- 사람이 에디터나 GitHub UI에서 ID 없이 추가한 항목은 도구가 `new`/`publish`/`push`/`pull` 시점에 자동으로 ID를 부여한다.
- Validation 항목도 같은 ID 체계를 쓴다. `check`와 MCP tool이 Tasks/Validation을 구분 없이 다룰 수 있다.

## 기본 워크플로우

### 1. 로컬 작업 초안 생성

```sh
llm-task new "settings cleanup"
llm-task open settings-cleanup
```

결과:

- `.llm-task/inbox/settings-cleanup.md` 생성
- `$EDITOR`로 파일 열기
- 아직 GitHub에는 공개하지 않음

### 2. 공유 inbox로 publish

```sh
llm-task publish settings-cleanup
```

결과:

- GitHub issue 생성
- issue body는 로컬 문서의 projection이다: frontmatter와 `Agent Log`를 제외하고, 전체를 ownership 마커로 감싼다.
- label은 `llm-task` 하나만 쓴다. 상태(inbox/active)는 로컬에서만 추적한다.
- local frontmatter에 `github_issue` 기록

issue body 형태:

```markdown
<!-- llm-task:begin task-20260612-settings-cleanup -->

## Goal
...

## Tasks
...

## Validation
...

<!-- llm-task:end -->
```

label이 실수로 제거되어도 body의 마커로 도구 소유 issue를 식별할 수 있다.

### 3. 공유 inbox 동기화

```sh
llm-task pull-inbox
```

결과:

- `llm-task` label이 붙은 issue만 가져옴
- 전체 issue 목록은 캐싱하지 않음
- 로컬 문서와 연결된 issue만 업데이트

### 4. 작업 시작

```sh
llm-task start settings-cleanup
```

결과:

- 문서를 `.llm-task/active/`로 이동
- `state.json`의 `current`를 이 task ID로 갱신
- 선택적으로 branch 생성
- 선택적으로 draft PR 생성
- GitHub issue에 `Started in PR #123` 코멘트 추가

### 5. LLM 실행

```sh
claude
/run-llm-task
```

skill은 `llm-task current --path`로 현재 task 문서를 찾는다.

LLM은 다음 규칙을 따른다.

- unchecked task만 수행한다.
- 상태 변경은 Markdown 직접 편집이 아니라 CLI mutation 명령으로 한다.
  - `llm-task check <item-id>`: 구현과 검증이 끝난 항목 체크
  - `llm-task log "<text>"`: Agent Log에 append
  - `llm-task fail <item-id> --reason "<text>"`: blocker 기록
- 완료하지 않은 항목은 체크하지 않는다.
- 각 task 완료 후 관련 검증을 수행한다.
- 필요하면 `llm-task push`로 PR body나 issue body를 갱신한다.

### 6. PR body 갱신

```sh
llm-task attach-pr 123
llm-task push
```

결과:

- PR body 전체를 덮어쓰지 않는다. `llm-task:begin`/`llm-task:end` 마커 사이의 managed section만 갱신하고, 마커 바깥에 사람이 쓴 내용은 보존한다.
- managed section에 task list, validation 상태, 요약을 넣는다.
- `Agent Log`는 접거나 요약해서 리뷰어가 보기 좋게 변환한다.

### 7. 완료 처리

```sh
llm-task done settings-cleanup
```

결과:

- 문서를 `.llm-task/done/YYYY-MM/`로 이동
- 관련 issue를 닫거나 완료 코멘트를 남김
- PR body의 managed section에 최종 summary와 validation 갱신

cancel의 경우:

- 문서를 `.llm-task/canceled/`로 이동
- published issue가 있으면 "not planned"로 닫고 코멘트를 남김

## CLI 명령

```text
llm-task init
llm-task new <title> [--shared]
llm-task list [--status inbox|active|done|canceled]
llm-task open [task]              # 인자 없으면 fuzzy picker
llm-task current [task] [--path]  # 조회 / 전환
llm-task start <task> [--branch] [--draft-pr]

# LLM-safe mutation (skill이 사용)
llm-task check <item-id>
llm-task uncheck <item-id>
llm-task log <text>
llm-task fail <item-id> --reason <text>

llm-task publish <task>
llm-task pull-inbox
llm-task pull
llm-task push [--resolved]
llm-task attach-issue <number>
llm-task attach-pr <number>
llm-task done [task]
llm-task cancel [task]
llm-task doctor                   # state.json 재생성, 무결성 검사
llm-task status
llm-task ui                       # TUI dashboard (Phase 5)
```

`search`는 두지 않는다. 로컬 Markdown 디렉토리는 grep/ripgrep으로 충분하고, 필요해지면 추가한다.

## GitHub 동기화 범위

동기화 대상은 다음으로 제한한다.

- `llm-task` label이 붙은 issue
- 로컬 task 문서에 `github_issue`가 기록된 issue
- 로컬 task 문서에 `github_pr`이 기록된 PR

동기화 단위와 소유권:

- 동기화 단위는 body 전체가 아니라 마커로 구획된 managed section이다.
- 소유권 판정은 label이 아니라 body의 `llm-task:begin` 마커로 한다. label은 검색 필터일 뿐이다.

동기화하지 않는 것:

- 전체 issue 목록
- 전체 PR 목록
- 댓글 전체 히스토리의 완전한 로컬 복제
- GitHub Projects 전체 상태

transport는 MVP에서 `gh` CLI subprocess를 쓴다. 인증을 `gh`에 위임할 수 있어 토큰 관리가 필요 없다. API 클라이언트(octocrab 등) 도입은 필요해질 때 한다.

## 상태 파일

`state.json`은 재생성 가능한 캐시 + 동기화 메타데이터만 가진다. identity와 GitHub 링크의 truth는 각 task 파일의 frontmatter다.

```json
{
  "version": 1,
  "current": "task-20260612-settings-cleanup",
  "tasks": {
    "task-20260612-settings-cleanup": {
      "path": ".llm-task/active/settings-cleanup.md",
      "issue_synced_hash": "sha256:...",
      "pr_synced_hash": "sha256:..."
    }
  }
}
```

- `current`는 task ID 포인터다. 별도 `current.md` 파일은 없다.
- `path`는 빠른 lookup용 캐시다. truth는 파일 자체의 위치다.
- `*_synced_hash`는 마지막으로 성공한 push/pull 시점의 projection(managed section 내용) 해시다. 충돌 판정의 base가 된다.
- `state.json`이 깨지면 `llm-task doctor`가 task 파일 스캔으로 재생성한다. synced hash는 유실 시 다음 sync에서 conflict로 안전하게 fallback한다.

## 충돌 처리

로컬 문서와 GitHub issue/PR body가 동시에 바뀐 경우 자동 병합을 무리하게 시도하지 않는다.

판정은 3-way hash 비교로 한다.

- base: `state.json`의 synced hash (마지막 동기화 시점의 managed section 해시)
- local: 현재 로컬 문서의 projection 해시
- remote: 현재 원격 managed section 해시

| local | remote | 처리 |
| --- | --- | --- |
| = base | = base | 변경 없음 |
| ≠ base | = base | push |
| = base | ≠ base | pull |
| ≠ base | ≠ base | conflict |

- issue의 `updated_at`은 댓글, label, assignee 변경에도 갱신되므로 충돌 판정에 쓰지 않는다.
- conflict 시 `.llm-task/conflicts/`에 local/remote 백업을 저장한다.
- 사용자가 해결한 뒤 `llm-task push --resolved`를 실행한다.

## Claude Skill

초기 skill은 얇게 유지한다. 상태 변경은 전부 CLI에 위임한다.

```markdown
---
description: Run the current LLM task document, implement unchecked tasks, update status via llm-task commands, and append execution notes.
disable-model-invocation: true
---

Run `llm-task current --path` to find the active task document, then read it.

Rules:

1. Execute only unchecked tasks.
2. Never edit checkboxes or Agent Log in the Markdown directly. Use:
   - `llm-task check <item-id>` after a task is implemented and verified
   - `llm-task log "<text>"` to append a concise entry after each completed task
   - `llm-task fail <item-id> --reason "<text>"` when blocked
3. Preserve `<!-- task:... -->` ID comments when editing other sections.
4. Run relevant tests before checking validation items.
5. If a PR is attached, run `llm-task push` when finished.
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

- `llm-task open`을 인자 없이 실행하면 내장 fuzzy picker로 task를 고른다. nucleo(helix의 fuzzy matcher) 기반이면 외부 의존성이 없다.
- 파일이 평범한 Markdown 디렉토리이므로 Obsidian vault나 VS Code workspace로 열어 보는 것도 그대로 동작한다.

### Phase 5: ratatui TUI dashboard

`llm-task ui`로 진입하는 ratatui 기반 dashboard.

- layout: 좌측 task 목록(status별 그룹), 우측 Markdown preview
- keybinding: `j/k` 이동, `Tab` status 전환, `Enter`로 `$EDITOR` 열기, `s` start, `d` done, `p` publish/push, `/` filter
- TUI 안에서 Markdown을 직접 편집하지 않는다. 편집은 `$EDITOR`, 상태 변경은 기존 CLI 동사와 같은 내부 함수를 호출한다.
- crates: ratatui + crossterm, nucleo(filter), preview는 tui-markdown 또는 자체 간단 렌더링

### 검토한 대안

- local web UI (axum + htmx 등): Markdown 렌더링과 GitHub 링크 연결은 좋지만, 서버를 띄우는 마찰이 있고 터미널 중심 워크플로우와 어긋난다.
- native GUI (Tauri, egui, iced): 배포와 창 관리 오버헤드 대비 이득이 없다. claude를 터미널에서 실행하는 워크플로우와 컨텍스트가 분리된다.
- editor-native (Obsidian, VS Code, Neovim): 코드가 필요 없다는 장점이 있고 Phase 5 전까지의 보완재로 유효하다. 다만 start/publish 같은 상태 전환 동작을 붙일 수 없어 대체재는 아니다.

TUI를 선택한 이유: 워크플로우 전체(claude, git, `$EDITOR`)가 터미널에 있고, SSH 환경에서도 동작하며, gitui/yazi/atuin 같은 검증된 선례가 있다.

## MVP 범위

### Phase 1: Local task journal

- `init`, `new`, `list`, `open`(fuzzy picker 포함), `current`, `start`, `done`, `cancel`
- mutation 명령: `check`, `uncheck`, `log`, `fail`
- `doctor` (state.json 재생성)
- `.llm-task/` 구조 생성
- task Markdown 템플릿 생성
- item ID 자동 부여
- `.git/info/exclude` 자동 등록

### Phase 2: Shared inbox via GitHub Issues

- `new --shared`
- `publish`
- `pull-inbox`
- `attach-issue`
- issue body managed section push/pull
- `llm-task` label 관리 (label은 검색 필터, 소유권은 body 마커)
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
- `/run-llm-task` workflow 문서화
- optional hooks
- optional MCP server

### Phase 5: TUI dashboard

- `llm-task ui` (ratatui)
- task 목록 + Markdown preview
- 상태 전환을 기존 CLI 동사로 dispatch

## 비목표

- FUSE filesystem 구현
- Dropbox처럼 전체 GitHub issue/PR 백그라운드 동기화
- GitHub Projects 전체 복제
- 모든 댓글의 완전한 오프라인 편집
- 여러 agent의 동시 작업 스케줄러
- 자동 병렬 실행 오케스트레이션
- TUI 내장 Markdown 에디터

## 추후 고민

- shared inbox issue 하나에 여러 task를 둘지, task 하나당 issue 하나를 둘지
- `Agent Log`를 PR body에 얼마나 노출할지
- issue 댓글을 로컬 문서의 `Discussion`으로 가져올지 링크만 둘지
- task 문서를 git에 커밋하는 팀 모드를 지원할지
- Claude 외 Codex, Cursor, Gemini CLI용 skill/recipe 포맷을 같이 만들지
- task item 단위 lock/claim이 필요한지
- branch/worktree 생성까지 도구가 책임질지

## 현재 결론

가장 작은 유용한 도구는 다음이다.

```text
로컬 Markdown task journal
  + LLM-safe mutation CLI (check / log / fail)
  + 선택적 GitHub issue publish/pull for shared inbox (managed section)
  + 선택적 PR body push for active work (managed section)
  + Claude skill로 실행 규칙 제공
  + ratatui TUI dashboard (Phase 5)
```

이 구조는 GitHub를 공유와 리뷰 표면으로 쓰면서도, 사람이 실제로 편집하고 LLM이 실행하는 중심 문서는 프로젝트 안의 로컬 Markdown으로 유지한다. truth는 항상 한 곳에만 있고, state.json과 원격 body는 파생물이므로 어긋나도 복구 가능하다.
