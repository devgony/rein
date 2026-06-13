시나리오 A — 혼자 로컬에서 (가장 기본)

rein new "settings cleanup" # inbox에 초안 생성 (id와 경로 출력)
rein open settings-cleanup # $EDITOR로 열어 Goal/Tasks/Validation 작성
rein start settings-cleanup # inbox → active, current가 이 task로 설정됨

이후 Claude Code에게 맡기면 LLM이 이렇게 진행한다:

rein current --path # task 문서 위치 찾기 (skill의 진입점)
rein check <item-id> # 항목 완료 체크
rein log "구현 메모" # Agent Log에 append
rein fail <item-id> --reason "…" # 막혔을 때 blocker 기록

끝나면:

rein done # active → done/YYYY-MM/ (current 자동 해제)

체크리스트 항목의 <!-- task:... --> ID는 직접 안 붙여도 된다 — issue/push/pull 시점에 자동 부여된다. 로컬 전용으로만 쓸 거면 직접 붙이거나 그냥 텍스트로 둬도 된다(단 check는 ID가 필요).

시나리오 B — 병렬 worktree (Claude Code 멀티 에이전트)

rein new "feat a"
rein new "feat b"
rein start feat-a --worktree # ../proj-wt/feat-a + 브랜치 rein/feat-a 생성
rein start feat-b --worktree # ../proj-wt/feat-b

각 worktree가 자기 task에 바인딩되므로, 에이전트는 자기 worktree cwd에서 그냥 명령을 치면 된다:

cd ../proj-wt/feat-a && rein current # → feat-a의 task (cwd로 자동 resolve)
cd ../proj-wt/feat-b && rein check x # → feat-b 문서만 변경, 교차 오염 없음

부모 세션에서 정리할 땐 명시적으로:

rein done feat-a # dirty면 거부됨 → 커밋 후 재시도 (worktree도 제거)
rein cancel feat-b --force # 버리는 경우

메인 repo에서 task 없이 rein check를 치면 active가 2개 이상일 때 에러로 막힌다(의도된 가드) — --task <id>를 쓰거나 해당 worktree에서 실행하면 된다.

시나리오 C — GitHub 공유 inbox

rein issue settings-cleanup # GitHub issue 발행 (rein label, 마커 wrap)
rein pull-inbox # 팀원이 만든 rein label issue들 가져오기
rein pull # 원격에서 issue body가 수정됐을 때 반영
rein push # 로컬 변경을 issue/PR managed section에 반영

충돌 나면 (push가 conflict로 실패):

rein open <task> # conflicts/ 백업 참고해서 로컬 문서 정리
rein push --resolved # 로컬 기준으로 강제 push

PR 연결:

rein start feat-a --worktree --draft-pr # 시작하면서 draft PR 생성, 또는
rein attach-pr 123 # 기존 PR 연결
rein push # PR body managed section 갱신 (Agent Log는 <details>로 접힘)

보조 도구

rein ui # TUI 대시보드: j/k 이동, Tab status 전환, Enter 편집, s/d/p, / 필터
rein status # store 위치, 현재 task, status별 카운트, active의 branch/worktree
rein list # 전체 목록 (--status inbox|active|done|canceled)
rein root # store 경로 — rg $(rein root), code $(rein root) 등에 활용
rein doctor # state/ 깨졌을 때 재생성, frontmatter drift 수정
rein init --skill # .claude/skills/run-rein-task/SKILL.md 스캐폴드 (LLM 실행 규칙)

가장 짧은 한 사이클은 new → start → (LLM: check/log) → done이고, 공유가 필요할 때만 B/C를 얹는 구조다.
