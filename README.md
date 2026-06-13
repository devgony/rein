# rein

LLM task journal + shared inbox manager.

**rein**(고삐)은 harness를 채운 말에게 명령을 전달하는 인터페이스다 — LLM에게 맡길 작업 문서를 로컬 Markdown으로 관리하고, LLM이 안전하게 실행·체크하며, 필요할 때만 GitHub issue/PR과 연결한다.

- truth는 로컬 store의 Markdown 문서다. GitHub는 source of truth가 아니라 공유·리뷰 표면이다.
- 상태 변경은 Markdown 직접 편집이 아니라 CLI mutation 명령(`check`/`log`/`fail`)으로 한다 — LLM이 문서를 깨뜨리지 않는다.
- 설계 배경과 의사결정은 `PLAN.md` 참고.

## 설치

```sh
cargo install --path .
```

git이 필요하고, GitHub 연동(`issue`/`pull`/`push` 등)에는 `gh` CLI가 필요하다.

## 핵심 개념

- **store**: repo별 로컬 store(`~/.local/share/rein/<key>/`). 키는 `rein init`이 `git config rein.store`에 발급하는 UUID라 worktree·디렉토리 이동에 영향받지 않고, repo 밖에 있어 커밋 사고가 없다. `REIN_ROOT`로 위치 override 가능.
- **상태 = 디렉토리 위치**: `inbox/` → `active/` → `done/YYYY-MM/`, 그리고 `canceled/`. frontmatter의 `status`는 파생값이다.
- **item ID**: 체크리스트 항목의 `<!-- task:N -->`는 한 번 부여되면 고정되는 안정 정수다(줄 번호가 아님). 도구가 문서를 만지는 시점마다 자동 부여하고, `rein check <N>`이 이 번호를 쓴다.
- **task resolution 순서**: `--task <id>` → worktree 포인터 → `REIN_TASK` → store의 `current` 파일.

## 기본 워크플로우

### A. 혼자 로컬에서 (가장 기본)

```sh
rein new "settings cleanup"   # inbox에 초안 생성 (id·경로 출력)
rein open settings-cleanup    # $EDITOR로 Goal/Tasks/Validation 작성
rein start settings-cleanup   # inbox → active, current가 이 task로 설정
```

이후 Claude Code에 맡기면 LLM은 skill 규칙을 따라 진행한다:

```sh
rein todo                     # 남은 unchecked 항목 목록 (skill 진입점)
rein check <item-id>          # 항목 완료 체크
rein log "구현 메모"          # Agent Log에 append
rein fail <item-id> --reason "…"   # 막혔을 때 blocker 기록
```

끝나면:

```sh
rein done                     # active → done/YYYY-MM/ (current 자동 해제)
```

체크리스트의 `<!-- task:... -->` ID는 직접 안 붙여도 된다 — 도구가 자동 부여한다(`check`에는 ID가 필요).

### B. 병렬 worktree (Claude Code 멀티 에이전트)

```sh
rein start feat-a --worktree  # ../proj-wt/feat-a + 브랜치 rein/feat-a 생성
rein start feat-b --worktree
```

각 worktree가 자기 task에 바인딩되므로 에이전트는 자기 cwd에서 그냥 명령을 친다:

```sh
cd ../proj-wt/feat-a && rein current   # → feat-a (cwd로 자동 resolve)
cd ../proj-wt/feat-b && rein check x   # → feat-b 문서만 변경, 교차 오염 없음
```

정리는 부모 세션에서 명시적으로(`rein done feat-a` / `rein cancel feat-b --force`). 메인 repo에서 task 없이 mutation을 치면 active가 2개 이상일 때 가드로 막힌다 — `--task`를 쓰거나 해당 worktree에서 실행한다.

### C. GitHub 공유 inbox / PR

```sh
rein issue settings-cleanup   # GitHub issue 발행 (rein label, 마커 wrap)
rein pull-inbox               # rein label issue들 가져오기 (idempotent)
rein pull                     # 원격 issue body 변경 반영
rein push                     # 로컬 변경을 issue/PR managed section에 반영
```

원격 body는 `rein:begin`/`rein:end` 마커 사이의 managed section만 갱신하고 바깥의 사람 글은 보존한다. 충돌은 3-way hash로 감지해 `conflicts/`에 백업하고, 정리 후 `rein push --resolved`로 강제 push한다. PR은 `rein start … --draft-pr` 또는 `rein attach-pr <n>`로 연결하고 `rein push`로 갱신한다(Agent Log는 `<details>`로 접힘).

## TUI (`rein ui`)

여러 프로젝트의 task를 한 화면에서 본다. 현재 repo에서 실행하면 그 프로젝트로 scope가 미리 잡히고, `P`로 다른 프로젝트를 고른다.

| key | 동작 |
| --- | --- |
| `j`/`k` | 이동 |
| `Tab` | status 전환 (all/inbox/active/done/canceled) |
| `P` | 프로젝트 선택 (project > task 계층) |
| `Enter` | `$EDITOR`로 편집 |
| `n` | 새 task 생성 |
| `s` | start (inbox → active) |
| `m` | 임의 상태로 이동 (i/a/d/c) |
| `d` | done |
| `p` | issue 발행 또는 push |
| `/` | 필터 (프로젝트명도 매칭) |
| `q` | 종료 |

편집은 항상 `$EDITOR`에 위임한다 — TUI 안에 Markdown 에디터를 두지 않는다.

## 명령 요약

```text
rein init [--skill]                  store 생성 + git config rein.store 발급 (--skill: SKILL.md 스캐폴드)
rein new <title> [--shared]          inbox에 task 초안 생성
rein list [--status <s>]             task 목록
rein todo [--all] [--task <id>]      resolved task의 unchecked 항목 (--all: 전체+상태)
rein open [task]                     $EDITOR로 열기 (인자 없으면 fuzzy picker)
rein current [--path]                resolved task 출력 (조회 전용)
rein use <task>                      task 바인딩 전환 (worktree 포인터 / current 파일)
rein move <task> <status>            임의 상태로 이동 (부작용 없는 단순 relocation)
rein start <task> [--worktree] [--branch <b>] [--draft-pr]
rein check / uncheck <item-id> [--task <id>]
rein log <text> [--task <id>]
rein fail <item-id> --reason <text> [--task <id>]
rein issue <task> | pull-inbox | pull | push [--resolved]
rein attach-issue <n> | attach-pr <n>
rein done [task] [--keep-worktree]
rein cancel [task] [--keep-worktree] [--force]
rein doctor                          state/ 재생성, frontmatter drift 수정
rein status | root | ui
```

## LLM 연동 (Claude skill)

```sh
rein init --skill   # .claude/skills/run-rein-task/SKILL.md 스캐폴드
```

skill은 `rein todo`로 남은 항목을 받아 실행하고, 상태 변경은 `rein check`/`log`/`fail`로만 한다(Markdown 직접 편집 금지). 자세한 규칙은 스캐폴드된 SKILL.md에 있다.
