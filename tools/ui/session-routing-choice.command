#!/bin/bash
# 추가 패키지·네트워크·계정 없이 조작하는 터미널 UI 시안. 실제 앱 실행기가 아니다.
# macOS Bash 3.2는 read -n을 byte 단위로 처리하므로 UTF-8을 명시적으로 조립한다.
set -u
export LC_ALL=C

usage() {
  printf '%s\n' 'MindPlayer 터미널 UI 시안' \
    '사용: bash tools/ui/session-routing-choice.command [--entry list|existing|new]' \
    '      [--scenario ready|missing|denied|unsupported]' \
    '목록: 방향키 / j,k 선택, Enter 재개, n 새 세션, x 안전 종료 시연, q 종료' \
    '선택: 방향키 / j,k 선택, c 외부 전송 동의, Enter 열기, Esc 취소' \
    '세션: 원문 입력 + Enter, Backspace 삭제, Tab 시연 다음 단계' \
    '      Ctrl-X 목록 복귀, Ctrl-G 시안 Trace, Ctrl-C 종료' \
    '실제 API·CLI 호출 없음. 키/계정/사용자 세션 접근 및 디스크 저장 없음.'
}

entry=list
scenario=ready
while (( $# )); do
  case $1 in
    --help|-h) usage; exit 0 ;;
    --entry|--scenario)
      if (( $# < 2 )); then usage >&2; exit 2; fi
      case $1 in --entry) entry=$2 ;; --scenario) scenario=$2 ;; esac
      shift 2 ;;
    *) usage >&2; exit 2 ;;
  esac
done
case $entry in list|existing|new) ;; *) usage >&2; exit 2 ;; esac
case $scenario in ready|missing|denied|unsupported) ;; *) usage >&2; exit 2 ;; esac
if [[ ! -t 0 || ! -t 1 ]]; then
  printf '%s\n' '대화형 터미널(TTY)이 필요합니다. --help로 조작법을 볼 수 있습니다.' >&2
  exit 2
fi

saved_stty=$(stty -g) || exit 2
cleanup() {
  stty "$saved_stty" 2>/dev/null || :
  printf '\033[0m\033[?25h\033[?1049l'
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
stty -echo -icanon -ixon min 1 time 0 || exit 2
printf '\033[?1049h\033[?25l\033[2J'

screen=list
selected=0
choice=0
consent=0
opening_new=0
active=0
trace=0
notice='기존 세션은 Enter, 새 세션은 n으로 열어 보세요.'
session_names=('리뷰 세션' '설계 검토' '실행 중인 세션')
session_ids=('demo-existing-1' 'demo-existing-2' 'demo-live-1')
session_modes=(native native jev)
session_live=(0 0 1)
session_steps=(input input running)
session_prompts=('' '' '이전 작업 (가상 예시)')
session_drafts=('' '' '')
session_queues=('' '' '')
session_queue_counts=(0 0 0)
session_turns=(0 0 1)
session_traces=('아직 제출 없음' '아직 제출 없음' '기존 실행에 복귀 (시연)')
rows=30
cols=100
key=''
clipped=''
clipped_width=0
char_size=1

# C locale의 byte 위치를 사용한다. 입력 문자열을 포맷/명령으로 평가하지 않는다.
utf8_size() {
  local code
  printf -v code '%d' "'$1"
  code=$((code & 255))
  char_size=1
  if (( code >= 194 && code <= 223 )); then char_size=2
  elif (( code >= 224 && code <= 239 )); then char_size=3
  elif (( code >= 240 && code <= 244 )); then char_size=4
  fi
}

clip_text() {
  local text=$1 limit=$2 offset=0 width size
  clipped=''
  clipped_width=0
  while (( offset < ${#text} )); do
    utf8_size "${text:offset:1}"
    size=$char_size
    width=1
    # 한글/CJK는 2칸. 그 외 비ASCII는 보수적으로 2칸 잡아 우측 넘침을 막는다.
    if (( size > 1 )); then width=2; fi
    if (( clipped_width + width > limit )); then break; fi
    clipped+="${text:offset:size}"
    clipped_width=$((clipped_width + width))
    offset=$((offset + size))
  done
}

draw_line() {
  local row=$1 text=$2 color=${3:-0}
  if (( row < 1 || row > rows )); then return; fi
  clip_text "$text" "$((cols - 1))"
  printf '\033[%s;1H\033[2K\033[%sm%s\033[0m' "$row" "$color" "$clipped"
}

mode_name() {
  if [[ ${session_modes[active]} == jev ]]; then mode_label='Jev 사용'; else mode_label='Jev 미사용'; fi
}

render() {
  local size row index marker state_label mode_label phase cursor_col start visible_count
  size=$(stty size 2>/dev/null) || size='30 100'
  # WINCH가 IFS='' read를 중단해도 그 동적 IFS를 이어받지 않는다.
  IFS=' ' read -r rows cols <<< "$size"
  if (( rows < 1 || cols < 1 )); then rows=30; cols=100; fi
  printf '\033[?25l\033[H'
  # 지워진 이전 프레임을 남기지 않고 현재 크기 안에서만 그린다.
  for (( row=1; row<=rows; row++ )); do printf '\033[%s;1H\033[2K' "$row"; done
  draw_line 1 'MindPlayer | 터미널 UI 프로토타입' '1;34'
  draw_line 2 'UI 시연 · 실제 API 호출 없음 · 설치본 아님' '33'
  if (( rows < 19 || cols < 48 )); then
    draw_line 4 '창을 48열 x 19행 이상으로 넓혀주세요.' '33'
    draw_line "$((rows - 1))" 'Ctrl-C 종료 / 크기 변경 시 자동 다시 그리기' '2'
    return
  fi
  case $screen in
    list)
      draw_line 4 '세션 목록 (합성 데이터 · 메모리에서만 유지)' '1'
      visible_count=$((rows - 11))
      start=0
      if (( selected >= visible_count )); then start=$((selected - visible_count + 1)); fi
      for (( index=start; index<${#session_names[@]}; index++ )); do
        row=$((6 + index - start))
        if (( row >= rows - 6 )); then break; fi
        marker=' '
        state_label='종료됨'
        if (( session_live[index] )); then state_label='열린 창'; fi
        if (( index == selected )); then marker='>'; fi
        if (( index == selected )); then
          draw_line "$row" "$marker codex  ${session_names[index]}  [$state_label]" '7'
        else
          draw_line "$row" "$marker codex  ${session_names[index]}  [$state_label]"
        fi
      done
      draw_line "$((rows - 5))" "$notice" '33'
      draw_line "$((rows - 2))" '방향키/j,k 선택  Enter 열기  n 새 세션  x 안전 종료 시연  q 종료' '36'
      ;;
    choice)
      if (( opening_new )); then state_label='새 세션 (아직 생성되지 않음)'; else state_label="${session_names[selected]} / 같은 세션 재개"; fi
      draw_line 4 "열기 전: $state_label" '1;34'
      draw_line 5 '모델 판단 방식 (이 실행에 적용)' '1'
      marker='  ( )'
      if (( choice == 0 )); then marker='> (x)'; fi
      draw_line 7 "$marker Jev 사용 안 함" '32'
      draw_line 8 '      기존 CLI 그대로 / Jev 호출·전송 없음' '2'
      marker='  ( )'
      if (( choice == 1 )); then marker='> (x)'; fi
      draw_line 10 "$marker Jev 사용" '33'
      draw_line 11 '      매 새 입력의 모델 + effort 판단 (시연)' '2'
      if (( choice )); then
        marker='[ ]'
        if (( consent )); then marker='[x]'; fi
        draw_line 13 "c $marker TypeSafe Jev 외부 전송 동의 (설정 시 Gateway 경유)"
        draw_line 14 '현재 입력 + 제한된 직전 판단 + 후보/기준 (실제 전송 없음)' '2'
      else
        draw_line 13 '미사용은 Jev 키 불필요. CLI 기능/설정은 그대로.' '2'
      fi
      draw_line "$((rows - 5))" "$notice" '33'
      draw_line "$((rows - 2))" '방향키/j,k 선택  c 동의  Enter 열기  Esc 취소  q 종료' '36'
      ;;
    session)
      mode_name
      phase=${session_steps[active]}
      draw_line 4 "${session_names[active]} | codex | $mode_label" '1;34'
      draw_line 5 "같은 세션: ${session_ids[active]} / 업무 계정 (시연)" '2'
      if (( trace )); then
        draw_line 7 'Trace (시안 내 상태 전이 · 실제 실행 증거 아님)' '1;36'
        draw_line 9 "${session_traces[active]}"
        draw_line 11 "턴: ${session_turns[active]} / 대기 입력: ${session_queue_counts[active]}"
        draw_line 13 'Ctrl-G로 닫기 / 실제 요청·로그·토큰 조회 없음' '2'
      else
        draw_line 7 "입력: ${session_prompts[active]:-(아직 제출 없음)}"
        case $phase in
          input) state_label='프롬프트를 입력하세요' ;;
          judging) state_label='Jev 판단 중 (시연)' ;;
          requested) state_label='요청 수락 (시연) / 예시 모델 + medium' ;;
          running) state_label='실행 중 (시연)' ;;
          done) state_label='완료 (시연)' ;;
        esac
        draw_line 9 "$state_label" '1;33'
        if [[ $phase == running ]]; then draw_line 11 '도구 실행/결과 출력 영역 (가상 상태)' '2'; fi
        draw_line 13 "대기 입력: ${session_queue_counts[active]} / 제출: ${session_turns[active]}회"
        draw_line "$((rows - 6))" "> ${session_drafts[active]}"
      fi
      draw_line "$((rows - 5))" "$notice" '33'
      draw_line "$((rows - 2))" '입력+Enter  Tab 다음 단계(시연)  Ctrl-X 목록  Ctrl-G Trace  Ctrl-C 종료' '36'
      ;;
  esac
  draw_line "$((rows - 3))" '계정별 사용량: 실제 조회 없음 / 전체 폭 footer 유지' '2'
  draw_line "$rows" '키·계정·사용자 세션 접근 없음 / 입력·선택 디스크 저장 없음' '2'
  if [[ $screen == session ]] && (( trace == 0 )); then
    clip_text "${session_drafts[active]}" "$((cols - 4))"
    cursor_col=$((3 + clipped_width))
    printf '\033[%s;%sH\033[?25h' "$((rows - 6))" "$cursor_col"
  fi
}

start_choice() {
  choice=0
  consent=0
  if (( opening_new == 0 )) && [[ ${session_modes[selected]} == jev ]]; then choice=1; fi
  screen=choice
  notice='시연 준비: 설정 있음으로 가정 / 실제 인증·잔액은 검사하지 않음'
  case $scenario in
    missing) notice='Jev 키 없음 (시연) / 미사용 경로는 선택 가능' ;;
    denied) notice='정책이 외부 전송 금지 (시연) / 미사용 경로는 선택 가능' ;;
    unsupported) notice='이 CLI의 Jev 통합 미지원 (시연) / 미사용 경로는 선택 가능' ;;
  esac
}

open_session() {
  if (( choice )); then
    case $scenario in
      missing) notice='열기 차단: Jev 키 없음 / 직접 미사용으로 바꿔주세요'; return ;;
      denied) notice='열기 차단: 정책이 외부 전송 금지'; return ;;
      unsupported) notice='열기 차단: Jev 미지원'; return ;;
    esac
    if (( consent == 0 )); then notice='열기 차단: c로 외부 전송 동의가 필요합니다'; return; fi
  fi
  if (( opening_new )); then
    selected=${#session_names[@]}
    session_names[selected]="새 세션 $((selected - 2))"
    session_ids[selected]="demo-new-$((selected - 2))"
    session_steps[selected]=input
    session_prompts[selected]=''
    session_drafts[selected]=''
    session_queues[selected]=''
    session_queue_counts[selected]=0
    session_turns[selected]=0
    session_traces[selected]='아직 제출 없음'
  fi
  active=$selected
  session_modes[active]=native
  if (( choice )); then session_modes[active]=jev; fi
  session_live[active]=1
  session_steps[active]=input
  trace=0
  screen=session
  notice='원문을 직접 입력하세요. Tab은 시연 단계만 진행합니다.'
}

submit_prompt() {
  local prompt=${session_drafts[active]} phase=${session_steps[active]}
  if [[ ! $prompt || $prompt != *[![:space:]]* ]]; then return; fi
  session_drafts[active]=''
  if [[ $phase != input && $phase != done ]]; then
    session_queues[active]+="$prompt"$'\n'
    session_queue_counts[active]=$((session_queue_counts[active] + 1))
    notice='실행 중 입력: 개별 큐에 추가 (시연). 이전 입력과 병합하지 않음.'
    return
  fi
  session_prompts[active]=$prompt
  session_turns[active]=$((session_turns[active] + 1))
  if [[ ${session_modes[active]} == jev ]]; then
    session_steps[active]=judging
    session_traces[active]="턴 ${session_turns[active]}: 새 입력 -> Jev 판단 (시연)"
  else
    session_steps[active]=running
    session_traces[active]="턴 ${session_turns[active]}: 네이티브 실행 (시연) / Jev 판단 없음"
  fi
  notice='실제 전송 없음 / Tab으로 다음 상태를 확인하세요.'
}

advance() {
  case ${session_steps[active]} in
    judging) session_steps[active]=requested; session_traces[active]='Jev 선택 -> 모델 + effort 요청 수락 (시연)' ;;
    requested) session_steps[active]=running; session_traces[active]='요청 수락 -> 턴 시작 (시연)' ;;
    running) session_steps[active]=done; session_traces[active]='턴 완료 (시연)'; notice='완료. 새 프롬프트 입력 또는 Tab으로 다음 대기 입력 진행.' ;;
    done)
      if (( session_queue_counts[active] )); then
        # 입력 중인 초안은 큐 전환으로 덮어쓰지 않는다.
        local draft=${session_drafts[active]} queue=${session_queues[active]}
        session_drafts[active]=${queue%%$'\n'*}
        session_queues[active]=${queue#*$'\n'}
        session_queue_counts[active]=$((session_queue_counts[active] - 1))
        submit_prompt
        session_drafts[active]=$draft
      fi
      ;;
  esac
}

delete_character() {
  local text=${session_drafts[active]} offset code
  offset=$((${#text} - 1))
  while (( offset > 0 )); do
    printf -v code '%d' "'${text:offset:1}"
    code=$((code & 255))
    if (( code < 128 || code >= 192 )); then break; fi
    offset=$((offset - 1))
  done
  if (( offset < 0 )); then offset=0; fi
  session_drafts[active]=${text:0:offset}
}

read_key() {
  local first next suffix rest count code timeout=1 index
  key=''
  IFS= read -r -s -n 1 first || return 1
  case $first in
    $'\033')
      if (( BASH_VERSINFO[0] >= 4 )); then timeout=0.08; fi
      if IFS= read -r -s -n 1 -t "$timeout" next; then
        if [[ $next == '[' || $next == O ]]; then
          if IFS= read -r -s -n 1 -t "$timeout" suffix; then
            case $suffix in A) key=UP ;; B) key=DOWN ;; *) key=IGNORE ;; esac
          else key=IGNORE; fi
        else key=IGNORE; fi
      else key=ESC; fi
      ;;
    '') key=ENTER ;;
    $'\r') key=ENTER ;;
    $'\t') key=TAB ;;
    $'\177'|$'\010') key=BACKSPACE ;;
    $'\030') key=LIST ;;
    $'\007') key=TRACE ;;
    $'\004') key=EOF ;;
    *)
      printf -v code '%d' "'$first"
      code=$((code & 255))
      if (( code >= 32 && code <= 126 )); then key=$first
      elif (( code >= 194 && code <= 244 )); then
        # WINCH renderer도 utf8_size를 쓰므로 입력 길이는 지역 변수로 계산한다.
        if (( code < 224 )); then count=1
        elif (( code < 240 )); then count=2
        else count=3; fi
        rest=''
        IFS= read -r -s -n "$count" -t 1 rest || return 0
        if (( ${#rest} != count )); then return 0; fi
        for (( index=0; index<count; index++ )); do
          printf -v code '%d' "'${rest:index:1}"
          code=$((code & 255))
          if (( code < 128 || code > 191 )); then return 0; fi
        done
        key=$first$rest
      else key=IGNORE; fi
      ;;
  esac
}

handle_key() {
  case $screen in
    list)
      case $key in
        q|EOF) exit 0 ;;
        UP|k) selected=$(((selected + ${#session_names[@]} - 1) % ${#session_names[@]})) ;;
        DOWN|j) selected=$(((selected + 1) % ${#session_names[@]})) ;;
        n) opening_new=1; start_choice ;;
        ENTER)
          if (( session_live[selected] )); then
            active=$selected; screen=session; trace=0
            notice='기존 창에 복귀 (시연) / 모드 변경·중복 실행 없음'
          else opening_new=0; start_choice; fi
          ;;
        x)
          if (( session_queue_counts[selected] )) || [[ ${session_steps[selected]} != input && ${session_steps[selected]} != done ]]; then
            notice='종료 차단: 실행 중 또는 대기 입력 있음. 시연 완료 후 종료하세요.'
          else session_live[selected]=0; notice='안전 종료 (시연). 다시 Enter하면 모드를 선택합니다.'; fi
          ;;
      esac
      ;;
    choice)
      case $key in
        UP|DOWN|j|k) choice=$((1 - choice)) ;;
        c) consent=$((1 - consent)) ;;
        ENTER) open_session ;;
        ESC|LIST) screen=list; notice='열기 취소 / 세션 생성·실행·선택 기록 없음' ;;
        q|EOF) exit 0 ;;
      esac
      ;;
    session)
      case $key in
        LIST|ESC) screen=list; notice='기존 창 유지 / Enter로 복귀, 모드는 변경하지 않음' ;;
        TRACE) trace=$((1 - trace)) ;;
        TAB) if (( trace == 0 )); then advance; fi ;;
        ENTER) if (( trace == 0 )); then submit_prompt; fi ;;
        BACKSPACE) if (( trace == 0 )); then delete_character; fi ;;
        EOF) exit 0 ;;
        UP|DOWN|IGNORE|'') ;;
        *)
          if (( trace == 0 )); then
            if (( ${#session_drafts[active]} + ${#key} <= 4096 )); then session_drafts[active]+=$key
            else notice='시안 입력 한도 4096 bytes. 더 긴 입력은 받지 않습니다.'; fi
          fi
          ;;
      esac
      ;;
  esac
}

case $entry in
  existing) start_choice ;;
  new) opening_new=1; start_choice ;;
esac
trap render WINCH
render
while read_key; do
  handle_key
  render
done
