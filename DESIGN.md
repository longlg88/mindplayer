# Design

## 세션 사무실 보기 — 2026-10-23 / 배경 시안 5개, 미승인

산출물은 [AI 사무실 배경 시안](docs/plans/2026-10-23-office-view-gallery.ko.html)이다.
검증 결과는 [검증 기록](docs/plans/2026-10-23-office-view-gallery.ko.validation.json)에 둔다.
실제 TUI, 데스크톱 앱, 버전, 설치본, 사용자 세션과 계정은 수정하지 않는다.

- 사용자 결정(2026-10-23):
  - 표면은 MindPlayer가 여는 브라우저 창이다. 이 창은 읽기 전용이다.
  - 분위기는 사용자가 지정한 드라마의 합성체, 홀로그램, 의식 공간이다. 캐릭터와 이름은 MindPlayer 고유다.
  - 공개 레포이므로 참고한 작품의 이름, 로고, 닮은 얼굴을 넣지 않는다.
- 이전의 TUI 문자 셀 5안(사무실을 놓는 자리 비교)은 품질 기준(팀원 시안 수준)에 못 미쳐 이 시안으로 대체했다.
- 결정 축은 배경 하나다. 시안마다 배치, 대표 소품, 움직임이 다르다.
  - 01 합성 연구소: 유리벽 연구소와 콘솔 줄.
  - 02 의식의 섬: 공허에 뜬 섬과 빛의 다리.
  - 03 궤도 정거장: 행성이 보이는 통창과 반응로.
  - 04 기억 아카이브: 큐브 서가 벽과 모노리스.
  - 05 자가 조립 공장: 컨베이어, 로봇 팔, 용광로.
- 시안 5개가 같은 것: 캐릭터 4종(agent 색), 머리 위 상태 기호(`pane_dot`), 상단 칩, 대기열과 N, 카드, 확대·이동.
- 데이터는 category, agent, `SessionStatus`, `title`, `last_prompt_at`, `convo_log`의 마지막 답변, handoff 연결을 쓴다.
  - 잠(새 규칙): idle이고 마지막 프롬프트 뒤 10분이 지났으면 잔다. kiro와 cursor는 자지 않는다.
  - 방 이동(도구별): claude와 kiro는 `convo_log`에 도구 이름이 있다. codex는 도구 호출을 저장하지 않으므로 새 수집이 필요하다.
  - 카드의 마지막 답변: `convo_log` turn을 읽는 코드는 새로 필요하다. cursor는 turn이 0개다.
  - 대기열과 N: `status_rank` 순서(ended·done 다음 blocked)다. N을 누를 때마다 다음 세션으로 넘어간다.
- 경계: 카드에 답장·멈춤·키 전송 버튼이 없다. hook 설치, tmux `send-keys`, 파일 서버를 쓰지 않는다.
  실시간 연결은 미구현 계획이다(127.0.0.1의 읽기 전용 GET).
- Visual language의 "no decorative art" 원칙과 충돌한다. 구현 전에 예외 승인이 필요하다.
- 검증: `docs/tests/office-view-gallery.test.cjs`가 배치 계약, 겹침 대조 실험, N과 대기열의 일치,
  읽기 전용 카드, 전환, 화면 폭, 외부 요청을 확인한다. HTML 검증은 실시간 연결의 증거가 아니다.
- 미결정: 고를 배경, 도구 기록 수집 여부, 캐릭터 이름과 매핑, 창을 여는 키, 24개를 넘는 배치.

## 세션별 F6 유지 토글 · 계정 표시 — 2026-10-19 / 최신 승인

- 터미널 앱만 0.55.16으로 갱신·검증·로컬 설치한다. 데스크톱과 로그인/계정은 변경하지 않는다.
- Codex 세션은 Jev OFF로 시작한다. F6 한 번으로 ON, 같은 세션의 후속 입력·편집에도 유지,
  다시 F6으로 OFF다. 선택만으로 전송하지 않으며 Enter가 현재 모드를 실제 입력 ID에 복사한다.
- 큐에 넣은 입력은 넣을 때의 모드를 유지한다. 이후 토글·다른 pane·다른 세션이 바꾸지 않는다.
  디스크 저장이나 새 세션 상속은 추가하지 않는다. Claude는 Jev UI/호출 없이 native다.
- F7 맥락 미리보기와 전용 RPC/토큰 수명을 제거한다. 기존 bounded 판단 맥락/정책은 유지한다.
- split/zoom pane의 기존 아래쪽 테두리에 그 pane에 결합된 Codex 계정 레이블을 표시한다.
  전역 활성 계정을 추정하지 않고, 표시 레이블을 인증 성공이나 조직 신원으로 주장하지 않는다.
- 대안 3개씩 비교·정리 순서·회귀/실행 검증은
  [구현 계획](docs/plans/2026-10-19-session-jev-toggle.md)에 기록한다.

## 매 프롬프트 Jev 선택 — 2026-10-19 / 이전 승인, 위 최신 UX로 대체

- 최신 범위 수정: **Codex만 Jev 적용**. Claude Jev 연동을 제거하고 Claude 세션에는
  Jev 선택·맥락·판단 표시를 내보내지 않는다. Claude 로그인·계정·네이티브 실행은 유지한다.
  아래 기존 공통-provider 승인과 Claude 장애 보류는 이 결정으로 대체한다.

- 최신 사용자 승인: **1번 입력줄 토글**을 기본으로 하고 **4번의 ‘Jev에 보낼 맥락 확인’**을 기본 닫힌 보조 기능으로 결합한다. 터미널 앱 구현·버전 상승·로컬 설치까지 승인되었으며 데스크톱 앱은 제외한다.
- 첫 입력뿐 아니라 같은 세션의 두 번째·세 번째 및 큐에 넣은 입력도 각각 미선택으로 시작한다. 선택은 전송이 아니며 Enter가 이번 입력만 확정한다. 미선택 Enter는 모델 요청을 보내지 않는다.
- Jev 미사용은 해당 입력에 대해 외부 판단을 호출하지 않고 현재 네이티브 model/effort를 보존한다. 직전 Jev 선택을 초기 모델로 되돌리지 않는다.
- 선택은 실제 입력/요청 식별자에 결합한다. pane-wide ‘다음 요청’ 플래그, 텍스트 일치나 근거 없는 FIFO 대응으로 큐의 선택을 추측하지 않는다. 취소·실패·late callback은 다른 입력의 선택이 되지 않는다.
- 맥락 확인은 같은 요청에서 실제 직렬화할 bounded snapshot을 보여준다. 기존 전체 대화·첨부·도구 원문이 전달된다고 꾸미지 않으며, 민감정보 자동 제거를 보장하지 않는다. 미사용 시 Jev 전송은 없다.
- 네이티브 편집/명령/승인/Escape, busy queue와 명시적 steer, Ctrl-G Trace, 4+3 pane 구성과 전체 폭 quota footer를 보존한다. 실패 시 자동 재시도·native 우회는 없다.
- 검증은 실제 터미널 입력과 native consumer/queue의 식별자 소비까지 포함한다. 합성 HTML·파일 해시·빌드 성공만으로 완료나 설치 동작을 주장하지 않는다.

이전 구현 보류 기록(최신 Codex-only 지시로 해소): 설치된 Claude 2.1.303의 입력 이벤트에는 큐에
넣은 입력을 미래 실행 턴과 연결할 식별자가 없다. 네이티브 큐를 보존하면서 승인된
입력 시점 선택을 적용했다는 증거가 없으므로, 텍스트/FIFO로 대응시키지 않는다.
Claude 입력·큐를 MindPlayer가 소유하도록 범위를 넓힐지, 실행 직전 선택으로 UX를
바꿀지 사용자 결정이 필요하다. 기존 설치본 0.55.14와 시작 동작은 유지했다.
부분 코드·RED/GREEN 범위는 [구현 기록](docs/plans/2026-10-19-per-prompt-jev-implementation.md)에 둔다.

아래 5안은 승인 전 비교 기록이며, 구현 범위·기본값은 위 최신 결정이 우선한다.

## 프롬프트를 보낼 때 Jev 선택 5안 — 2026-10-19 / 비교 기록

산출물: [프롬프트별 Jev 선택 HTML 5안](docs/plans/2026-10-19-prompt-jev-gallery.ko.html).
최신 반복 지시는 같은 세션의 **모든 새 프롬프트 입력마다** 사용/미사용을
새로 선택하는 **시안 비교**다. 첫 입력뿐 아니라 두 번째·세 번째·이후 입력에도
필요하며 시작/세션 기본값·이전 선택 상속은 금지한다. 기존 시작 설정표의 제품 승인을
철회하거나 이 5안의 실제 구현을 승인한 것으로 해석하지 않는다.

- 근거 1차: 기존 DESIGN/시작 설정표/세션 선택 갤러리와 실제 렌더러 팔레트.
  2차: main.rs의 Ctrl-G/X/W/Z/Q와 native 입력 전달, rubric.mjs의 대화 제외·
  8KB 제한, service.mjs의 unknown/truncated fallback 소비를 대조했다.
- 목표: 같은 세션의 매 입력에서 새 선택·전송 범위·최종 실행 상태를 구분한다.
  비목표: 실제 앱/데스크톱/바이너리/계정/세션/가격 정책/인증/설치 변경.
- 사용자: 여러 Codex/Claude 세션을 오가는 작업자. 흐름은 세션 진입 → 초안 →
  이번 선택/맥락 범위 확인 → 판단 또는 native 전송 → 수락 → 실행 → 종료.
- 비교: 01 입력줄 토글(발견성/권장), 02 전송 키 분리(빠름/키 기억 부담),
  03 전송 직전 선택창(명확함/한 단계 추가), 04 맥락 미리보기(참조 검수/폭 필요),
  05 입력 시작 시 선택(작성 전에 결정/입력 전 단계 추가)을 비교한다.
  5안 모두 매 입력마다 새 선택이며 추천은 승인/제품 기본값이 아니다.
- 시각: 기존 terminal 색상·고정 문자 셀·직선·선택 행을 사용한다. 문서의
  비교/세션/체험 버튼은 터미널 밖에 둔다. 원래 4+3 pane/전체 폭 usage와
  Ctrl-G Trace 등을 유지하고 선택 세션 확대 장면으로 비교한다.
- 판단 정확도는 선택 위치만으로 개선되지 않는다. 같은 세션의 이번 입력+
  직전 문답+작업 요약을 명시적으로 외부 전송하는 **미구현 제안**을 함께 표시한다.
  원문 도구 출력·파일·비밀은 기본 제외, 요약의 민감정보 필터는 별도 필요하다.
- 동작: 모든 안에서 새 입력은 미선택이고 이전 선택을 상속하지 않는다.
  02만 F7/F8가 명시적 선택+전송이며 Enter는 미전송이다. 나머지는 선택만으로
  전송하지 않는다. 03의 선택창도 매번 미선택이고 Esc는 초안 복귀다.
  05는 매번 F7 사용/F8 미사용으로 선택 후 작성·Enter 전송, F6는 선택 해제다.
  오류/취소는 초안 보존·자동 우회/재시도 없음. 입력 완료 뒤 바로 타이핑하거나
  붙여넣기/조합 종료/체험용 후속 입력을 넣어도 새 프롬프트의 선택은 비어 있어야 한다.
- 접근성: 키보드 포커스·명시적 라벨/상태·색상 외 문자 표시·동작 영역 이탈 Tab.
  좁은 브라우저는 비교를 쌓고 터미널은 내부 스크롤로 셀 비율을 보존한다.
  애니메이션에 의존하지 않으며 키/Fn/IME/실제 스크린리더는 미검증이다.
- 기술: standalone HTML/CSS/JS, 기존 문자 격자 패턴 재사용, 의존성/네트워크/
  저장소 없음. 모든 판정·모델·진행·사용량은 합성 예시다.
- 검증 목표: 고유 5안, 실제 브라우저 키 이벤트, native/Jev 분기, 취소/실패/
  후속 요청, 세션 격리, 4+3/footer, desktop/mobile, 오류·외부 전송 없음.
- 검증: 선택 없이 전송되는 기존 05의 RED(1 !== 0)를 먼저 포착했다.
  새 브라우저 결과는 passed=32, failed=0, 키 이벤트 160회, 오류·외부 요청 0건이다.
  5안 × 두 합성 세션에서 버튼 없이 직접 연속 세 프롬프트를 입력해
  매번 새 선택 전 미전송을 검증했고, 붙여넣기/조합 종료 핸들러도 합성 이벤트로
  확인했다. 5개 두 번째 입력 화면과 좁은 화면을 검수했다.
  HTML 검증은 실제 CLI/TTY·실제 IME·모델 호출·판단 정확도 증거가 아니다.
- 미결정: 선택할 안/조합, 실제 provider별 입력 어댑터, 전송 요약의 출처/한도/
  redaction/갱신, Fn·IME 충돌. 세션 기본값은 미결정이 아니라 제외다.
  HTML 승인은 구현 승인이 아니다.

## 세션 × 할 일 / 카테고리 20안 — 2026-10-18 / 비교만, 미승인

산출물은 [10 + 10 HTML 시안](docs/plans/2026-10-18-session-tasks-category-gallery.ko.html)이다.
실제 TUI·데스크톱 앱·버전·설치본·사용자 세션과 계정은 수정하지 않는다.
앞선 시작 설정표·Trace·네이티브 입력·4+3 실행 화면 승인은 계속 유효하다.

### 근거와 범위

- 소스: `ui.rs`의 목록·카테고리 popup·pane 배치, `app/session_list.rs`의
  picker·assign_category, `main.rs` 키 처리, core `state.rs` 소속과
  `app/handoff_sync.rs`의 실제 소비 경로를 읽었다.
- 관측: 목록과 실행 pane은 교대하는 전체 폭 화면이다. t는 분류, 좌우/l은
  트리, u는 계정이다. 카테고리는 맥락 동기화에도 영향을 줄 수 있다.
- 코드상 접힌 목적지는 이동 뒤 펼쳐지지 않아 대상 행을 따라가지 못할 수 있다.
  실제 사용자 사건의 원인 재현이나 제품 수정 완료라는 뜻은 아니다.
- 가정: “세로로 반”은 목록 본문의 좌우 50:50이다. 실행 pane을 줄이지 않는다.
  업무 연결은 카테고리와 독립적인 다대다 관계이며, 전부 합성 데이터다.

### 대안 비교

A01 연결 하이라이트, A02 업무 번호 페어링, A03 마감 타임라인, A04 우선순위
스윔레인, A05 집중 업무 핀, A06 업무별 체크리스트, A07 연관 세션 필터,
A08 미연결 수신함, A09 대기·차단 큐, A10 다대다 연결 표를 비교한다.
A01은 발견성, 타임라인은 마감, 행렬은 다중 연결 검수에 유리하나 밀도가 높다.
A01을 첫 비교안으로 열되 제품 기본값 승인으로 간주하지 않는다.

B01 상단 레일, B02 고정 분류 탐색, B03 큰 구획, B04 고정 열,
B05 번호·설명 목적지 선택, B06 위치 경로, B07 이동 전후 대조,
B08 이동 내역+복구, B09 전체 분류 찾기, B10 이동 후 따라가기를 비교한다.
B02 상시 발견성 + B07 반영 전 확인 + B08 오분류 복구의 조합을 권고한다.
큰 구획은 공간, 고정 열은 폭, 경로만의 안은 발견성에 한계가 있다.

렌더러는 정적 이미지 / 웹 폼 / HTML 문자 격자를 비교했다. 조작 가능성과
터미널 제약을 함께 드러내는 문자 격자를 택하며 새 의존성은 추가하지 않는다.

### 조작·접근성·상태 계약

- 112×34 셀, A는 각 56열, 전체 폭 사용량 하단. 기존 4-top/3-bottom 실행
  화면과 Ctrl-G Trace는 유지한다. 색상 외에 ID·↔·경로·텍스트를 쓴다.
- A의 F6 포커스, F7 연결/해제, 오른쪽 Space 수동 완료는 시안용 신규 키다.
  l/u와 Tab의 기존 의미는 침범하지 않는다. Enter는 합성 재개 안내만 한다.
- B의 T → Enter 전후 확인 → Enter 반영, Esc 무변경 취소, F8 복구,
  G 전체에서 위치 찾기를 시연한다. B05/07/08은 선택/확인/이동 후에서 시작한다.
- 연결·카테고리 이동·수동 업무 완료는 서로 다른 동작이다. 세션 응답이 끝나도
  업무는 완료되지 않는다. 제목·계정 레이블 대신 안정 ID로 연관한다.
- 반영 전 대상 수·출발/도착·도착 분류 동기화 상태를 표시한다. 소속 복구는
  이미 공유한 맥락의 회수가 아니며 미분류와 카테고리 삭제를 구분한다.
- 문서 컨트롤은 터미널 밖에만 둔다. Tab으로 빠져나오고 포커스가 보인다.
  설명은 닫힌 disclosure에 둔다. 좁은 브라우저에서는 문서만 재배치하고
  터미널 내부 가로 스크롤로 비율을 유지한다. 작은 실제 터미널은 미검증이다.

### 조사·검증·열린 결정

공개 근거 1차는 fzf 시작 안내/명령 참조와 tmux 공식 가이드/매뉴얼,
2차는 WAI 현재 위치·포커스·색상·상태·오류 예방 설명과 WCAG 2.2의 대조다.
동등 폭 업무 관리가 upstream 기능이라고 주장하지 않는다. W3C 현재 위치는
AAA 패턴 참고이며 모든 편집에 확인창이 의무인 것처럼 표현하지 않는다.
링크는 HTML의 닫힌 근거에 둔다.

검증 목표는 각 10개 고유 화면, 셀 폭/50:50/footer, 연결·완료의 상태 분리,
이동 취소·반영·복구·찾기, ID·제목 보존, 실제 키 이벤트, 좁은 브라우저,
외부 요청/브라우저 오류 없음이다. 실제 Rust 실행·인증·설치 증거는 아니다.

최종 HTML 브라우저 검증은 363 통과/0 실패, 실제 키 이벤트 699회,
27장 캡처, 1440/1280/768/390/320 폭으로 완료했다. 외부·provider 요청과
브라우저 오류는 0이며 브라우저·격리 서버는 정상 종료했다. HTML 해시는
검증 전후 동일하고 앱 소스 123개 집계 해시도 이전 설치 기준과 같다.
공통 문자 격자를 재사용하고 외부 의존성을 추가하지 않았다. 최종 파일은
기본 브라우저로 열었다. 상세 증거와 미검증 항목은
[검증 기록](docs/plans/2026-10-18-session-tasks-category-gallery.ko.validation.json)에 둔다.

미결정: 선택할 안/조합, 업무 입력·편집·기한 설정, 저장/재시작 복구,
끊어진 링크·삭제·대량 연결, 파생 세션 분류 범위, 긴 이름/다수 목적지 스크롤,
충돌·저장 실패·오래된 복구 덮어쓰기 방지, 터미널/Fn/IME/스크린리더 검증.

## 07 실행 설정표 — 2026-10-17 / 최신 사용자 선택

이번 요청은 기존 04 하단 시작바를 **07 실행 설정표**로 교체한다. 기존 범위
선택·스캔·목록·pane·usage footer를 유지하고 첫 PTY 실행 전 한 번만 확정한다.
근거는 시작 화면 갤러리 07, 실제 `startup_routing.rs`/`ui.rs`와 최신 사용자 요청이다.

- 비교: 04 하단바는 작지만 적용 범위가 불명확하고, 별도 확인창은 조작이 중복된다.
  선택한 07 설정표는 이번 실행의 범위·Jev 방식·전송 여부를 문자 셀 행으로 보여준다.
  새 웹 위젯·설정 저장·세션별 질문·실행 중 전환은 추가하지 않는다.
- Jev 행에 외부 전송 대상과 실행 범위를 명시하고 Enter로 선택을 확정하는 동작에
  동의를 통합한다. 별도 `c` 체크는 제거한다. 미사용은 외부 전송 없이 즉시 시작하며,
  Jev는 준비 확인 성공 후 시작한다. 실패·취소 시 미사용으로 자동 우회하지 않는다.
- 입력 시도는 문자열이 아니라 요청 ID와 세대별 수명으로 구분한다. 판단 전 중단한
  입력을 그대로 다시 보내도 새 판단이어야 하며, 이전 응답이 새 턴에 적용되면 안 된다.
- 모델과 effort는 함께 적용한다. 선택·전송·설정 적용·실행을 구분하고, 네이티브
  변경 안내의 부재만으로 적용 실패를 단정하거나 수락 ACK를 실행 증거로 표시하지 않는다.
- 계정별 usage 수치는 같은 로그인이어도 각각 유지하고 공유 로그인 경고를 붙인다.
  이는 독립 계정 복구가 아니다. 인증은 사용자가 실제로 별도 로그인해야 하며 임의
  복사·덮어쓰기·기존 기본 계정 변경은 하지 않는다.
- 검증: 취소 후 동일 입력 재판단·늦은 응답 무효화·모델/effort 일치·동일 로그인
  게이지 유지·07 키보드/작은 화면·미사용 완전 우회·설치 실행 파일 PTY를 확인한다.

## 4번 하단 시작바 구현 — 2026-10-17 / 이전 선택, 07로 대체

사용자가 시작 화면 갤러리 **04 하단 시작바**와 선택 이후 전체 UI의 실제 개발을
요청했다. 아래는 이전 구현 기록이며 현재 계약은 위 07 실행 설정표다.

- 기존 작업 범위 선택·스캔은 유지한다. 첫 세션 목록은 읽기 전용 미리보기이며
  하단 시작바에서 이번 실행의 Jev 사용/미사용을 한 번 확정하기 전에는 새 PTY를
  열지 않는다. 목록 전체를 새 대시보드나 별도 입력창으로 대체하지 않는다.
- 선택 전 키보드·마우스·붙여넣기로 세션 조작을 우회할 수 없어야 한다. 미사용은
  Node·Jev 키·전용 Codex UI 없이 원래 CLI를 실행한다. Jev를 선택한 경우에만
  로컬 준비를 비동기 확인하고, 별도 외부 전송 동의 뒤 시작한다. 준비 확인은
  실제 API 인증 성공을 보증하지 않으며 실패 이유는 고정된 안전 문구만 표시한다.
- 선택 이후 목록/라이브 상단에 실행 모드를 읽기 전용으로 표시한다. 기존/새/계정
  새 로그인/handoff/멀티 선택의 새 실행 모두 같은 모드를 상속한다. 실행 중인
  pane 재연결·초안·작업 상태는 유지하며 세션별 선택창·실행 중 모드 변경은 없다.
- Jev의 새 입력 판단·모델과 effort 요청·요청 수락·실제 실행·완료·후속 판단·
  실패 미전송·중단·개별 큐와 Trace는 기존 검증된 프로토콜을 유지하고 실제 경로로
  연결한다. Claude/Kiro/Cursor는 원래 CLI다. 4+3 pane/전체 폭 usage/footer/권한 유지.
- 새 의존성·전역 설정 저장·계정/프롬프트 저장 형식·비용 정책·Git push는 범위 밖이다.
  빌드는 `CARGO_INCREMENTAL=0`으로 기존 대용량 incremental을 늘리지 않는다.
- 검증 순서: 순수 상태 회귀 → 렌더/키/모든 spawn 경계 → Node router 전체 회귀 →
  격리 HOME 실제 MindPlayer PTY → 실제 native Codex/Jev 연속 턴과 상태/모델·effort
  증거 교차 확인. 모의 백엔드·HTML 검사만으로 실제 통합 완료를 선언하지 않는다.
- 완료 검증: Rust 814개 통과·7개 opt-in 제외, Node 496개 통과·25개 opt-in 제외,
  fmt/clippy/공백 검사 통과. 최종 설치본으로 Jev 실제 판단 2회와 독립 native 턴
  2회, 미사용 실제 Codex 응답 1회를 별도로 확인했다. 요청·collaboration 설정과
  durable `turn_context`의 모델·effort가 일치한다. 서버 내부 실행 모델 인증이나
  비용 절감의 증거는 아니다. 미사용은 Jev 런타임·전용 UI를 만들지 않았다.
- 로컬 수정본 0.55.10을 설치하고 Ghostty 하위의 실제 MindPlayer 프로세스를
  확인했다. 기존 실행 창은 교체하지 않았다. 새 시작바·상태·spawn 경계와 기존
  native UI를 재사용했으며 새 입력창·세션별 모드 저장은 추가하지 않았다.
  [변경 파일·설치 체크섬·롤백·검증 한계](docs/startup-routing-2026-10-17.validation.json).

## MindPlayer 시작 시 한 번 선택 — 2026-10-17 / 비교 기록, 4번 승인으로 대체됨

최신 요청은 **세션마다 고르지 않고 MindPlayer 실행 시 Jev 사용/미사용을 한 번
선택하는 터미널 UI 10안**이다. [시작 화면 10안](docs/plans/2026-10-17-startup-routing-gallery.ko.html)이
비교 기록이다. 이전 12안의 세션별 선택 경계는 대체되었으며, 이후 4번 구현을 승인했다.

- 비교 대안: 작은 시작창, 전체 화면 목록, 두 칸 선택, 하단 시작바, 왼쪽 메뉴,
  셸 질문형, 실행 설정표, 단계형 시작, 목록 미리보기, 시작 명령 검색.
  모두 88열×29행 문자 격자·터미널 색상·키보드로만 표현한다. 브라우저 양식은 없다.
- 선택은 **현재 MindPlayer 프로세스의 새 실행·기존 세션 재개에 공통 적용**한다.
  Enter 재개와 n 새 세션에서 다시 묻지 않는다. 이미 실행 중인 pane 재연결은
  기존 실행 방식을 유지한다. 프로세스 간 저장·기본값 기억·실행 중 전역 전환은 미승인이다.
- 미사용은 기존 CLI 설정/모델/effort를 유지한다. Jev 사용은 현재 통합 범위인
  Codex의 일반 새 사용자 입력마다 모델과 effort를 함께 판단하는 제안이다.
  Claude/Kiro/Cursor는 원래 CLI로 동작하며 통합된 것처럼 그리지 않는다.
- Jev 선택과 외부 전송 동의는 구분한다. Jev 선택 뒤 필요한 동의를 한 번 확인하고
  준비 불가/전송 금지 시 시작을 차단한다. 자동 미사용 우회나 재전송은 없다.
  키·계정·설정·실제 세션을 읽거나 저장하지 않는다. 키 보유/인증 성공을 주장하지 않는다.
- 01~10은 레이아웃 비교이며 기본 선택은 합성 fixture일 뿐 제품 기본값 승인이 아니다.
  Enter/방향키/Esc 조작과 선택 후 목록·기존/새 세션·판단 흐름을 브라우저에서 시연한다.
  쉘 실행 가능성을 고려한 시안이지 실제 Bash/Rust TUI 실행·설치·Jev 호출 증거가 아니다.
- 첫 대안군(작은 창/셸 질문)은 최소 조작, 두 번째(전체 화면/두 칸/왼쪽 메뉴)는
  발견성, 세 번째(미리보기/하단바/설정표/단계/검색)는 시작 후 동작의 예측성을 비교한다.
  긴 설명·별도 대시보드·매 프롬프트 확인은 추가하지 않는다. 기존 4+3 pane과
  full-width quota footer, 네이티브 입력·권한·Ctrl-G Trace는 그대로 유지한다.
- 아래 기록의 2라운드 공식/upstream 키보드 조사와 실제 `ui.rs` 목록·색상·footer를
  재사용한다. 이번 요청에는 추가 SDK/API·모델/비용 정책·바이너리 변경이 필요하지 않다.
- 검증: `docs/tests/startup-routing-gallery.test.cjs`의 21개 검증 그룹 통과,
  키 입력 931회, 1440/1280/390px에서 페이지 가로 넘침·브라우저 오류·외부 호출 0.
  기존/새 세션 상태·초안 보존과 타이머 분리를 회귀검증했다. 10개 화면은 직접
  이미지로 점검했고 최종 HTML을 기본 브라우저로 열었다. 실제 터미널/IME·Rust
  실행·인증·모델 적용·설치·프로세스 수명 검증을 뜻하지 않는다.

## 터미널 UI 12안 — 2026-10-17 / 이전 세션별 경계, 대체됨

당시 요청은 **터미널에서 구현 가능한 UI 아이디어 최소 10개를 설명문 없이
HTML 화면으로 비교**하는 것이다. 이전 HTML의 브라우저 radio/select/button을
터미널 위젯으로 표현한 방식도 반려되었다. 독립 Bash 데모를 다시 만드는 요청이
아니다. [12안 갤러리](docs/plans/2026-10-17-session-routing-gallery.ko.html)를
이전 UI 비교 기록으로 보존하며, 현재 진입점은 위 시작 화면 10안이다.
어느 안도 승인된 제품 UI로 간주하지 않는다.

- 실제 렌더러 `ui.rs`의 색상·목록 전체 화면·full-width quota footer를 따른다.
  시안 내부는 88열×29행 문자 격자와 ANSI에서 표현 가능한 색상·선택 행·선만 사용한다.
  브라우저 양식 컨트롤·둥근 버튼·그림자·토글 스위치를 TUI로 위장하지 않는다.
- 비교 대안: 중앙 quick-pick, 선택 행 확장, 하단 실행바, 오른쪽 미리보기,
  fuzzy picker, 명령 팔레트, 계정×방식 표, 실행 전 체크시트, 두 실행 키,
  다음 실행 열, 행 옆 메뉴, 실행 대기표. 모두 기존/새 세션에 대해 선택을
  PTY 생성 전 수행하는 비교안이다. 기본값·기억·새 단축키는 미승인이다.
- 제품 설치·실제 호출·인증·비용 절감은 범위 밖이다. 원래 CLI는 Jev를 호출하지 않고
  CLI 설정을 유지한다. Jev는 외부 전송 동의를 별도로 받고 준비 불가 시 명시적으로
  차단한다. 이미 실행 중인 세션의 모드를 몰래 전환하거나 계정을 바꾸지 않는다.
- 각 터미널은 키보드로 선택/취소/예시 입력을 조작할 수 있으나 HTML 시안일 뿐이다.
  일반 제출 뒤 합성 상태는 자동 진행하며 수동 단계 넘기기를 제품 조작으로 만들지
  않는다. 후속 입력·개별 큐·실패 화면과 기존 4+3 배치는 화면으로 보여준다.
- 조사에서는 routing plugin/skill의 설정·명령·statusline과 전용 TUI를 구분한다.
  upstream에 없는 선택창을 그 도구의 실제 UI처럼 묘사하지 않는다. 공개 도구의
  키보드 패턴은 참고일 뿐 MindPlayer의 승인된 신규 단축키가 아니다.

조사 교차검증(2026-10-17): 1라운드에서 전날 언급한 routing upstream 5개의
README/설정/화면 계약을 확인하고, 2라운드에서 터미널 도구의 공식 키바인딩과
대조했다. [fzf](https://github.com/junegunn/fzf/blob/ca4c1b8/README.md)는 검색형 목록과
제한 높이 표시, [tmux](https://man.openbsd.org/tmux#choose-tree)는 트리·미리보기,
[Gum](https://github.com/charmbracelet/gum/blob/879f048/README.md)은 작은 선택형 프롬프트,
[OpenCode](https://opencode.ai/docs/keybinds/)는 명령 팔레트·세션 선택 패턴의 근거다.
[effortless](https://github.com/HeyCubit/effortless/blob/df5602e/README.md)는 호스트 내
상태줄/설정 UI이며, [jev-router](https://github.com/dirien/jev-router)의 라이브 화면은
브라우저다. ClaudeRouter와 codex-auto-model-router는 훅/스킬/텍스트 제어가 확인됐지만
독립 TUI는 확인되지 않았다. 이를 전용 TUI가 없다는 단정으로 바꾸지 않는다.
Upstream 앱을 설치·실행하거나 스크린샷을 픽셀 단위로 재현한 조사는 아니다.

단축키 제약: 기존 `←→`는 목록 트리 접기/펼치기이므로 방식 선택은 열기 전
문맥에서만 한다. 제안 F4/F6/F8은 미승인 신규 키다. 검색 중 n/c는 검색어이며
5·6안의 동의는 F4로 분리한다. live Esc는 목록 복귀가 아니라 네이티브 문맥이고
목록 복귀는 Ctrl-X다. 시안의 중단은 예시 상태만 멈추며 대기 입력을 자동 실행하지 않는다.

## 실행 가능한 터미널 시안 — 2026-10-16

**상태: 사용자 반려.** 아래 독립 Bash 시안은 조작 검증용 기록이며 제품 UI의
기준이 아니다. 기존 MindPlayer 화면을 대체하는 별도 화면·긴 안내·수동 상태
진행은 승인되지 않았다. 후속안은 기존 목록·pane·전체 폭 footer와 조작을
보존해야 한다. 반려 사유의 세부 취향은 아직 확인되지 않았으므로 추정하지 않는다.

당시 사용자 정정은 **Bash에서 실행하고 조작하는 UI**였다. 브라우저 컨트롤은
이 요구의 결과물이 아니다. [터미널 시안](tools/ui/session-routing-choice.command)을
`bash tools/ui/session-routing-choice.command`로 실행한다. 기본 macOS Bash 3.2와
설치된 Bash 5에서 실제 PTY 입력·화살표·한글·후속 입력·종료 복원을 검증한다.
실제 Codex/Jev 호출이나 MindPlayer 바이너리 연결 없이 메모리의 합성 세션만 쓴다.

사용자 확인용 창은 **Ghostty**로 연다. macOS에서는 `ghostty -e`를 직접 실행하지
않고, `open -na /Applications/Ghostty.app --args -e /bin/bash <시안의 절대 경로>`를
사용한다. 실행 성공뿐 아니라 Ghostty 하위의 실제 Bash 프로세스를 확인한다.

구현 대안은 (1) **순수 Bash+ANSI 선택**, (2) dialog/whiptail, (3) 기존 Rust TUI에
바로 통합이다. 1은 추가 패키지와 Cargo 캐시 생성 없이 지금 조작할 수 있다.
2는 새 의존성과 호스트별 차이가 생기고, 3은 설계 확인을 넘어 앱 실행 경로를
변경하므로 이번 범위에서 제외한다. 앞 절의 HTML은 방향/근거 참고이며,
이 Bash 시안 역시 반려되었으므로 후속 제품 UI의 기준으로 삼지 않는다.

- 목록 Enter: 기존 세션의 모드 선택. `n`: 새 세션의 같은 선택. 방향키 또는 j/k,
  Enter 확정, Esc 취소. `c`로 전송 동의를 명시하며 준비 불가 때 Jev 시작을 차단한다.
- 세션에서는 원문을 직접 입력하고 Enter로 제출한다. Jev 모드의 판단·요청·실행·
  완료는 **Tab으로 넘기는 합성 상태**다. 미사용은 판단 없이 합성 실행으로 간다.
  일반 글자를 명령으로 평가하지 않는다. 실제 모델명을 확인한 것처럼 표시하지 않는다.
- 작업 중 입력은 시안 내 개별 큐에 남고 완료 후 다음 Tab에서 새 판단한다.
  Ctrl-X는 목록 복귀, Ctrl-G는 시안 Trace다. 활성 세션 Enter는 기존 시안으로
  돌아가며 모드를 바꾸지 않는다. 목록 `x`는 완료/idle·큐 없는 경우만 종료 시연한다.
- 목록 `q` 또는 Ctrl-C로 종료한다. echo·canonical mode·flow control·cursor·대체 화면을
  복원한다. 작은 창/resize도 범위 내로 그린다. macOS Bash 3.2의 단독 Esc 판별은
  최대 1초 지연이며 Bash 5에서는 짧은 timeout을 사용한다.
- 모든 화면에 실제 API 호출 없음 표시. 키·기존 계정·사용자 세션 파일을 읽지 않고
  프롬프트·선택을 디스크에 저장하지 않는다. 실제 앱 통합·설치·비용 검증은 미수행이다.

## 세션을 열기 전 Jev 선택 — 2026-10-16 / 제안, 앱 미구현

당시 사용자 요구는 **기존 세션과 새 세션을 실행하기 전에 Jev 사용/미사용을
선택**하는 것이다. 이전의 선택창 금지·항상 Jev 연결 방침을 이 경계에서만
대체한다. 프롬프트마다 확인창을 띄우지 않으며, 기존 Trace·4+3 pane·전체 폭
사용량 footer·네이티브 입력과 권한은 유지한다.

화면·방향 평가·현재 대비 변경점:
[세션별 Jev 선택 UI](docs/plans/2026-10-16-session-routing-choice.ko.html).
이 문서는 승인 요청용 시안이며 Rust TUI 구현·설치·버전 변경을 뜻하지 않는다.

### 방향 평가와 제품 계약

- 방향은 타당하다. MindPlayer의 세션 관리와 선택적 외부 판단 의존성을 분리한다.
  키 보유는 사용 의사나 외부 전송 동의가 아니다. 비용 절감은 별도 실측 대상이다.
- `Jev 사용 안 함`은 원래 CLI 실행 경로다. MindPlayer의 Jev 중계·전용 UI 의존성·
  Jev 호출/전송을 생략한다. CLI의 기존 모델·effort·subagent·Advisor 설정을
  강제로 바꾸거나 다른 자동 분류기를 대신 켜지 않는다. 미사용은 모델 고정이 아니다.
- `Jev 사용`은 기존 Codex 통합을 재사용한다. 매 일반 새 사용자 턴에서
  모델과 effort를 함께 선택하며, 같은 설정 유지도 유효하다. 명시적 steering·
  도구 응답·승인 응답은 새 판단이 아니다. 실패 시 자동 우회·재전송하지 않는다.
- 현재 구현은 Codex 대화 명령을 자동 wrap한다. 정책의 명시적 전송 금지는
  판단 없이 passthrough한다. 따라서 현재를 무조건 항상 Jev 호출이라고 부르지 않는다.
- 현재 확인된 통합 범위는 Rust TUI의 Codex다. 다른 CLI의 네이티브 열기는
  유지하되 Jev 선택은 미지원으로 표시한다. 별도 Tauri 앱 지원을 주장하지 않는다.

### 권장 UI와 대안

1. **공통 열기 전 선택창 — 권장.** 기존 Enter와 새 세션의 계정/라벨 설정 뒤,
   PTY 생성 전에 같은 두 항목을 보여준다. 추가 조작은 실행 전 한 번뿐이다.
2. 별도 일반 열기/Jev 열기 단축키: 빠르지만 발견성이 낮고 새 세션과 재개가 갈라진다.
3. 전역 자동 켜짐+예외 설정: 조작은 적지만 다른 사용자·혼합 pane·전송 동의가 불명확하다.

새 세션과 선택 기록 없는 기존 세션은 미사용을 기본으로 **제안**한다. 기존 선택이
있으면 재개 전에 미리 선택해 보여주되 자동 실행하지 않는다. Jev가 준비되지 않아도
미사용 경로는 사용할 수 있다. Jev를 골랐는데 준비가 안 되면 이유와 명시적 미사용
변경 동작을 보여준다. 인증·정책·연결·실행 가능 범위는 별개이며 사전 점검이
첫 실제 요청의 인증 성공이나 잔액을 보증하지 않는다.

### 상태·동의·전환 경계

- 제안 저장 키: `provider + 안정적인 accountFingerprint + 실제 threadId`.
  값: `routingMode: native | jev`, 선택 버전. 계정 표시 이름·세션 label은 키가 아니다.
  이름 변경으로 모드가 바뀌지 않아야 한다. 비밀값·프롬프트를 선택 기록에 저장하지 않는다.
- 이번 실행의 모드는 시작 전에 고정한다. 실행/실제 thread 확인 후 다음 재개용
  선택을 기록한다. 취소·실행 실패·새 세션 synthetic ID에는 영구 기록하지 않는다.
- 외부 전송 동의는 별도 버전·범위로 관리한다. 기존 유효 동의는 재사용할 수 있지만
  키나 과거 선택만으로 동의를 추론하지 않는다. 현재 프롬프트+제한된 직전 판단+
  후보/기준을 실제 판단 서비스로 보낸다는 점을 표시한다. 전체 대화를 보내지 않는다.
- 재개는 같은 계정·같은 thread를 유지한다. Jev를 꺼도 CLI가 마지막으로 유지한
  모델/effort를 임의로 원복하지 않는다. 실행 중인 pane 재연결은 새 실행이 아니며
  모드 변경창을 띄워 즉시 전환한 것처럼 표시하지 않는다.
- 모드 변경은 안전하게 종료한 뒤 다시 열 때 적용한다. 진행 중인 작업이나 큐를
  자동 중단·유실·재전송하지 않는다. handoff는 대상의 새 실행 선택이며 전송 동의를
  몰래 상속하지 않는다. 여러 pane은 각기 다른 모드를 가질 수 있다.
- 일괄 열기는 실행되지 않은 각 대상별 선택을 확인하는 후속 설계 범위다.
  전역 설정을 무심코 덮어쓰는 일괄 토글을 이번 시안에 추가하지 않는다.

### 구현 영향·검증·열린 결정

영향 경계는 진입 modal → PendingSpawn → PTY/router 분기 → 선택 상태/실제 ID 이동 →
pane의 모드 표시다. Jev 판단 기준·policy·네이티브 큐·Trace는 재사용한다.
모드와 현재 모델의 수명은 분리한다. 공급자/계정의 사용 제한과 전송 금지 정책은
Jev 선택으로 우회하지 않는다.

HTML 검증 범위: 기존/새 세션의 선택, 취소, 유효 동의, 키 없음, 정책 금지, 미지원,
실행 중 재연결, 두 경로의 후속 입력, 4+3 배치, 키보드, 좁은 화면, 외부 요청 0건.
이 검증은 브라우저 시안 검증이며 실제 Rust launch/resume·인증·Jev 비용·모델 실행의
증거가 아니다. 실제 구현에는 각 분기 회귀 테스트와 설치본 행동 검증이 필요하다.

열린 결정: 기본값/다음 재개까지 기억은 위 권장안이며 미승인이다. 일괄 열기와
실행 중 안전한 모드 전환은 별도 범위다. 원가·cache·성공률 측정 전 자동 라우팅이
더 싸다고 표시하지 않는다. 외부 서비스의 confidence는 정답률이 아니다.

## 실행 중 입력도 개별 새 턴으로 — 2026-10-16 / 0.55.10

사용자가 승인한 범위는 **MindPlayer 전용 네이티브 Codex UI**다. 일반 Codex
설치·기존 실행 pane·Jev 판단 기준·기존 승인 권한은 변경하지 않는다. 전용 UI의
실행 중 일반 Enter는 각각 네이티브 큐에 들어가고, 정상 완료 뒤 하나씩 새
`turn/start`가 되어 Jev 판단과 모델·effort 적용을 함께 거친다. 입력·도구 출력·승인
응답을 중계기에서 가짜 턴으로 바꾸지 않는다. 중단·실패 뒤 큐는 개별 입력으로
유지하고 자동 실행하지 않는다. 사용자는 기존 큐 편집 동작으로 복구할 수 있다.

`turn/steer`에는 모델·effort 변경 필드가 없다. 응답 보류는 UI 이벤트 루프를
막고, queue 전용 Enter 설정은 idle `!shell`을 막으며, review/compact 오류 위장은
실제 상태와 다르고 입력 병합 위험이 있다. 따라서 입력 의미를 아는 네이티브
UI 안에서만 큐 동작을 바꾼다. 이 절은 아래의 일반 Enter steering 설명을 대체한다.

upstream 0.162.1의 고정 revision에 최소 패치를 보관한다. 런타임은 전용 설치
manifest의 revision·계약·SHA-256을 확인하며, 누락·변조 때 일반 Codex로 몰래
우회하지 않는다. 통합 pane의 UI·probe·app-server는 같은 검증된 전용 바이너리를
사용한다. 일반 PATH Codex는 교체하지 않는다. 재현 절차와
유지보수 경계는 [전용 UI 빌드](tools/native-codex/README.ko.md)에 있다.
upstream app-server는 `turn/start`도 active 턴에 합칠 수 있으므로, 선택된 새 입력에는
전용 `mindplayerRequireNewTurn` 표식을 붙이고 기존 Core의 `start_turn_if_idle`로
원자적으로 시작한다. 경합 때 입력·설정을 주입하지 않고 거절하며, 재시도하지 않는다.
UI의 사전 상태 확인만으로 이 보장을 주장하지 않는다. 도구 출력·명시적 steering은
기존 경로를 유지한다. 이는 앞서 검토한 UI-only + 일반 backend 방식을 대체한다.
동일 main 재진입은 확인된 소유권을 유지한다. 사용자가 네이티브 picker에서 고른
main 변경은 전용 UI의 로컬 provenance로 묶고, 해당 표식은 원래 backend에 보내지
않는다. helper·subagent·ephemeral은 제외하며, 확인되지 않은 main은 Jev 우회 실행
대신 제출 전에 멈춘다. 계정 변경으로 무효화한 소유권을 이전 기록만으로 복원하지 않는다.
0.55.10 로컬 설치와 설치본의 실제 Jev·네이티브 추론 검증을 완료했다. 실행 중 입력한
다음 프롬프트까지 두 번의 독립 판단·새 턴을 확인했고, 요청과 durable `turn_context`의
모델·effort가 모두 일치했다. 이는 서버 내부의 실제 서빙 모델 영수증이나 품질·비용
절감 증명은 아니다. 기존 pane에는 소급 적용하지 않는다.
설치 경로·해시·회귀 검증·호스트 출처·남은 한계는
[0.55.10 검증 기록](docs/jev-native-queue-0.55.10.validation.json)에 보관한다.

## 실행 상태 교정 — 2026-10-16 / 0.55.9

기존 모델줄에서 Jev 선택 → 요청 수락 → **턴 시작 알림을 받은 실행 중** →
턴 완료·중단·실패를 구분한다. 요청 수락만으로 실행 중을 표시하지 않고,
정상 실행에 `model/rerouted`를 요구하지 않는다. 요청 모델·effort는 선택값이며,
별도 서버 모델 변경 보고는 부가 정보다. 이를 실제 서빙 모델·effort 영수증으로 부르지 않는다.
기존의 정상 실행에 대한 `실행 모델 미확인` 표시는 폐기한다. 송신 결과 불명 오류는 유지한다.

대안: 문구만 교체하면 ACK/실행 혼동이 남고, reroute를 필수 확인으로 쓰면 정상
실행을 확인할 수 없다. 따라서 pane 상태 v2에서 수락/실행을 분리하고
`executionVerified`를 `serverModelReported`로 대체한다. 늦은 ACK와 이전 턴 알림은
새 턴 상태를 덮지 못한다. 판단 정책·키·계정·권한·입력·화면 배치는 변경하지 않는다.
검증 기록: [0.55.9 실행 상태](docs/jev-routing-lifecycle-0.55.9.validation.json).
이 절은 아래 과거 문서의 실행 미확인 표시와 모델 확인 용어보다 우선한다.

## 최신 판단 경계 — 현재 프롬프트와 제한된 직전 판단, 2026-10-16

승인 설계: [MindPlayer → Jev → 다음 네이티브 턴](docs/report/2026-10-16-jev-decision-boundary-topology.ko.html).
0.55.8 소스에서 기본 카탈로그 Choice에 `mindplayer-capacity-v1` 기준을 고정하고,
현재 프롬프트와 직전 수락된 같은 세션 판단만 전달한다. 전체 대화 수집·paging·새 요약
에이전트 없이 기존 중계/네이티브 UI를 재사용한다. UI·권한·계정·원문 입력은 바꾸지 않는다.

- Jev는 해결 공간·조정 부담·실패 영향·후속 작업·정보 부족을 후보 설명과 함께 판단한다.
  코드로 모델 등급이나 가격 순위를 정하지 않는다. 독립 질문에는 필요한 기준을 각각 제공한다.
- 직전 판단은 네이티브 start ACK 이후에만 보존한다. 완료·중단·실패는 **턴** 결과이며,
  전체 작업 완료나 환경 상태의 증명이 아니다. compact는 원문 가용성을 바꾸지만 이 판단을 지우지 않는다.
- 판단은 로컬 계정·thread 소유권·요청 ID/타입·세대·카탈로그·정책·기준 버전에 묶는다.
  오래된 응답은 적용하지 않고 재시작 때 이전 상태를 모르면 `null`로 보낸다.
- 전체 스냅샷 8 KiB, 전체 요청 32 KiB, 응답 64 KiB, 기본 5초 마감을 유지한다.
  원문 잘림·첨부·모호한 참조는 baseline 유지 근거다. 자체로 이해되는 요청은 unknown history와 구별한다.
- 명시적 실측 품질/비용 정책은 별도 엄격 경로로 유지한다. 설명 기반 선택으로 우회하지 않는다.
  confidence를 정답률로 부르거나 측정하지 않은 최저 비용/품질을 보장하지 않는다.
- 인증·TLS·네트워크·시간 초과·응답 오류는 비밀값 없는 전송 전 원인으로 표시한다.
  자동 재호출·기존 모델로의 몰래 실행·전송 결과 불명 시 재전송은 없다.

이 절은 아래 과거 설계의 **기본 카탈로그 판단 입력 계약**을 대체한다. UI 승인 경계는 유지한다.
설치·동작 증거: [0.55.8 로컬 검증 기록](docs/jev-decision-boundary-0.55.8.validation.json).
실제 TypeSafe Jev의 연속 3회 입력 및 한·영 12개 예시를 확인했다. Codex backend는
합성이며 실제 모델의 실행 품질·effort·비용 절감은 검증하지 않았다.

## 최신 UI 재검토 — 세션 진입부터 매 새 턴, 2026-10-15

상태: **0.55.6 구현·로컬 설치 완료 / 실제 설치본의 연속 3회 Jev 판단·Codex 완료 확인**.
검증: [설치·실행 증거와 남은 한계](docs/jev-turn-routing-0.55.6.validation.json).
미리보기: [세션 진입 → 입력 → 판단 → 실행 → 다음 입력](docs/plans/2026-10-15-jev-turn-ui.ko.html).
이번 시안은 아래의 과거 기술 설계보다 **Jev 사용자 흐름 표현**에서 우선한다.
Trace·완료 테두리의 기존 승인과 계정/창/권한 경계는 바꾸지 않는다.

- 근거: 2026-10-15 22:11 사용자 스크린샷의 단일 집중 창, 세션 목록/4+3
  배치의 기존 소스, 0.55.5 검증 기록. 스크린샷의 오류 원인을 이 문서로
  진단하거나 해결했다고 주장하지 않는다. 스크린샷·키·실제 대화 원문은 임베드하지 않는다.
- 교정: 사용자는 매 입력의 **판단과 실제 모델 적용**을 원한다. 판정만 호출하고
  기존 모델을 유지하는 것을 완성으로 설명하지 않는다. 진입·입력·판단·요청·실행·
  완료·후속 재판정 화면을 각각 보여주고, 요청 모델과 확인된 실행 모델을 구별한다.
- 첫 화면: 현재 화면의 재구성과 목표 화면을 함께 표시한다. 모든 정상 단계의
  작은 화면은 접지 않고 나열하며, 문서용 단계 버튼으로 목표 화면을 확대한다.
- 유지: 사용자의 집중 창, 필요 시 원래 4-top/3-bottom 구성, 전체 폭 quota,
  동일 세션·계정·작업 폴더·입력 위치. 입력은 네이티브 CLI 표면을 그대로 사용한다.
- 변경 제안: 기존 모델줄 한 곳의 읽기 전용 상태. 자동/고정·모델 선택·활성화·
  설정·확인 팝업, 상시 옆 패널, 별도 입력창은 추가하지 않는다. Ctrl-G는 유지한다.
- 세 대안: 모델줄 한 곳(선택: 적은 점유), 매번 대화에 판단 메시지 삽입(제외:
  대화 오염), 별도 상세 패널(제외: 터미널 폭 감소). 문서 탐색 버튼은 제품 UI가 아니다.
- A→B→A→B는 가상 모델 구성의 시연이다. 실제 모델명/가격/품질 순위/절감률을
  가정하지 않는다. 모델 변경 없이 유지된 턴도 사유와 판정 여부를 구분해야 한다.
- 작업 진행 중 도구 출력·승인·현재 턴 추가 지시는 새 턴으로 재판정하거나
  모델을 중간 교체하지 않는다. 다음 새 턴에는 다시 판단한다. 모호한 queued
  입력의 실제 네이티브 동작은 아직 검증 항목이지 완료된 기능이 아니다.
- 오류 처리: 선택 실패는 미전송, 실행 모델 증거 부재는 미확인, 명시 중단은
  중단으로 표시한다. 송신 결과가 불확실하면 미전송이라고 주장하지 않으며
  자동 재전송하지 않는다. 네이티브 입력 이력은 유지하되, 실패 시 편집창 자동
  복구는 실제 네이티브 UI 검증 없이 보장하지 않는다.
- 검증: 자체 HTML/CSS/JS, 기존 Playwright/Chrome만 사용. 정상 10단계·오류/추가
  지시 상태, 키보드, 1440/969/390px, 4+3 및 quota 폭, 외부 요청/비밀 부재를 검사한다.
- 구현 경계: 네이티브 터미널 출력에 ANSI를 주입하지 않는다. pane 소유의 비공개
  상태 스냅샷을 읽고 기존 모델줄에만 렌더링한다. 메뉴·승인·스크롤백·좁은 창에서
  모델줄을 안전하게 확인할 수 없으면 기존 창 테두리를 사용하며 내용 높이는 유지한다.
- 선택 대안 비교: 경험적 품질/비용 정책 완성은 평가·전체 문맥 어댑터가 필요하고,
  작업 분류→고정 모델 매핑은 수동 모델 순위 관리가 필요하다. 현재 구현은 정책
  파일이 없는 경우에만 **현재 카탈로그의 모델+effort를 직접 Jev Choice로 선택**한다.
  이는 설명 기반 휴리스틱이며 구독 비용 최적·품질 비열등성을 입증한 정책이 아니다.
  유효한 경험적 정책은 기존 경로를 우선하고, 존재하지만 잘못된 정책은 우회하지 않는다.
- 새 턴은 완료 문맥으로 다시 판단한다. 실행 중 `turn/steer`·도구 출력은 네이티브
  경로를 유지하고, 새 `turn/start`는 기존 턴이 끝난 뒤 판단한다. `ultra`의 자동
  위임은 새로운 선택 후보에서 제외하고 기존 baseline인 경우만 보존한다.
- 남은 증거 한계: 시작 수락은 실제 서빙 모델 영수증이 아니다. 서버의 턴별 모델
  보고가 없으면 실행 모델은 미확인이다. HTML은 여전히 합성 UI 시안이며 실제
  런타임·비용 절감의 증거가 아니다. 새 SDK·push는 추가하지 않는다.

## Approved direction — 2026-09-21

The user selected **A / full-screen Trace** in `docs/observe-trace-ui.html`.
From a focused live session, **Ctrl-G** replaces the session viewport with its
Trace view; **Ctrl-G** or **Esc** returns to that same live session. The PTY
continues to run and ingest output behind the Trace view. Ctrl-G reuses the
experimental observer command entry point instead of adding a function key
that conflicts with macOS media controls or stealing a new PTY editing chord.
The intermediate command popup and `:observe` command are retired. Read the
selected session's local log; never infer exact file token costs or unrecorded
reasoning. The primary token figure is provider-recorded usage accumulated
across the selected user turn, accompanied by model-call count and latest-call
size so repeated context processing is explicit; the user's literal prompt
text estimate is secondary and explicitly marked `~`. Cached input is a subset
of input, not additive.

The HTML files are illustrative previews, not application surfaces. Earlier
browser-dashboard and separate-window designs below are historical drafts,
superseded by this decision. Trace implementation follows on a new branch after
the account-fix/refresh release.

### Experimental observer review — 2026-09-21

The former `Ctrl-O` sidecar resembled a second session pane and its raw,
line-oriented transcript did not make a prompt → observed tool call/result →
usage sequence scannable. It is superseded by the full-screen Trace and should
not remain as a second observer entry point. The implementation uses a session
identity, then a selectable prompt rail with a local prompt-text token estimate,
provider-recorded request context, and observed execution events. Session-
cumulative totals are omitted from Trace because they obscure the selected
prompt. The agent PTY keeps running behind that view.

## Source of truth

- Status: Active — Codex-only per-prompt input-row choice + optional exact-context preview approved; implementation/verification in progress. Claude is native only. Trace and native queue contracts remain; task/category gallery is unapproved.
- Last refreshed: 2026-10-19 (latest Codex-only implementation scope; install not yet completed)
- Primary product surfaces: Codex input-row routing choice, session list, native panes,
  dedicated session trace window
- Evidence reviewed: README.md, mindplayer-core session/token models, the TUI
  entrypoint and renderer, startup_routing.rs, local_router.rs, spawn.rs,
  startup-check.mjs, actual PTY tests, and the local Codex JSONL event schema.

## Brand

- Personality: dense, calm, operator-oriented, trustworthy.
- Trust signals: show source event type, timestamp, token scope, and whether a
  statement is observed or inferred.
- Avoid: pretending hidden model reasoning is available, decorative dashboards,
  or uncited AI-generated explanations.

## Product goals

- Routing goal: fresh Jev/native choice for every Codex prompt before Enter,
  including queued and repeated text. Optional F7 preview shows the exact
  bounded snapshot bound to that submission; no startup inheritance.
- Routing non-goals: Claude Jev, saved defaults, provider changes, queue ownership
  guessing, silent native fallback, full-history or automatic redaction claims.
- Goals: open one session's trace from the list; explain token usage by turn;
  connect request, observable decision record, tool execution, and result.
- Non-goals: expose private chain-of-thought, rewrite provider transcripts, or
  add a second token-counting source that disagrees with provider logs.
- Success signals: a user can answer “what ran, why did it run, and how many
  tokens did it cost?” without opening the raw JSONL manually.

## Personas and jobs

- Primary persona: an engineer comparing agent sessions and investigating cost,
  latency, or an unexpected action.
- User jobs: inspect a single session, find the expensive turn, verify a tool
  result, and distinguish observed facts from summaries.

## Information architecture

- Primary navigation: session list → dedicated trace window.
- Core routes/screens: mindplayer trace <session-id>; Overview, Timeline, Raw
  log tabs; session switcher in the trace window.
- Content hierarchy: session identity → prompt selection → that prompt's token
  usage → observed execution evidence.

## Design principles

- Evidence before interpretation: every decision summary links to the request,
  tool call, or result that supports it.
- Token scopes stay explicit: Trace foregrounds provider-recorded usage for the
  selected turn, keeps the `~`-marked local estimate of the literal user text
  secondary, and does not show session-cumulative totals.
- Progressive disclosure: the default view is scannable; raw payloads and full
  command output expand on demand.

## Visual language

- Current startup/list/live TUI: fixed character cells, terminal palette,
  keyboard selection, straight rules, no web form controls or pixel-grid cards.
  The following older browser Trace treatment does not redefine the TUI.
- Color: MindPlayer dark terminal palette; blue for navigation, green for
  successful execution, amber for uncertainty, red only for errors.
- Typography: system sans for labels, monospace for ids, commands, paths, and
  event payloads.
- Spacing/layout rhythm: compact 8px grid, restrained borders, one bright focus
  state.
- Shape/radius/elevation: 10px panels, 1px borders, no floating-card shadows.
- Motion: short tab/filter transitions only; respect reduced-motion preference.
- Imagery/iconography: text badges and small inline symbols; no decorative art.

## Components

- Routing implementation: Codex native composer F6 selection, Enter submission,
  optional initially closed F7 context; empty/selected/loading/available/error
  states. Claude sessions have no selector or routing-status wrapper.
  Reuse the native pane's judgment/request/acceptance/execution/outcome model
  line, individual queue, original 4+3 composition, full-width quotas and Trace.
- Existing components to reuse: session identity, agent/status colors, token
  formatting, footer shortcut language.
- New/changed components: trace window shell, token metric cards, token-by-turn
  table, evidence timeline card, raw-event disclosure, source/inference badge.
- Variants and states: loading, unavailable source file, partial transcript,
  malformed event, empty session, active/failed tool call.

## Accessibility

- Target standard: WCAG 2.1 AA intent for the HTML prototype.
- Keyboard/focus behavior: visible focus, logical tab order, Esc closes the
  trace window, / focuses search, and all filters are buttons.
- Contrast/readability: never use color alone for state; every status has text.
- Screen-reader semantics: landmark regions, button labels, table headers, and
  native details disclosures.
- Reduced motion and sensory considerations: no required animation.

## Responsive behavior

- Supported breakpoints/devices: desktop-first; usable down to 760px wide.
- Layout adaptations: the session rail becomes a horizontal selector below the
  header; metric cards wrap; evidence cards become single-column.
- Touch/hover differences: buttons remain full-label controls; hover is optional.

## Interaction states

- Loading: show “reading transcript” with the known file path.
- Empty: explain that the provider did not record token/event details.
- Error: keep aggregate metadata visible and show the failed source read.
- Success: show source timestamp and “observed” labels.
- Disabled: raw payload actions are disabled when the source is unavailable.
- Offline/slow network: this view is local-first; TypeSafe summaries, if ever
  enabled, are optional and must not block raw evidence.

## Content voice

- Tone: factual and concise.
- Terminology: “observed” for JSONL facts, “summary” for derived text,
  “decision record” for an auditable action rationale, “reasoning” only as a
  token category—not as a claim that hidden thought is displayed.
- Microcopy rules: show units and scope beside every number; say “not recorded”
  instead of guessing.

## Implementation constraints

- Framework/styling system: standalone HTML prototype first; production surface
  should follow the existing Rust TUI conventions.
- Design-token constraints: reuse the existing dark palette and compact density.
- Performance constraints: parse only the selected session; cap raw payload
  previews; avoid rereading all sessions when opening the trace.
- Compatibility constraints: Codex, Claude, Kiro, and Cursor have different
  evidence levels; the UI must support partial data.
- Test/screenshot expectations: verify keyboard navigation, narrow layout, and
  source/inference labels before production implementation.

## Open questions

- [ ] Should mindplayer trace <session-id> open a new terminal window through a
  platform-specific launcher, or reuse the current terminal process?
- [ ] Which provider-specific event fields should be normalized in v1?
- [ ] Should derived summaries be local deterministic labels only, or optionally
  call TypeSafe after the evidence view has loaded?

## 현재 설계 초안 — Jev 기반 비용효율 모델 선택, 2026-10-15

상태: **설계 초안 / Jev 연결·비용 정책 미구현 / 사용자 결정 대기**.
최신 요청에 따라 Vercel AI Gateway의 Jev를 판정기로 사용하는 방향을 검토한다.
상세 결정 원장·대안 비교·구현 순서·합격 기준:
[Jev 비용 라우팅 HTML 설계](docs/plans/2026-10-15-jev-cost-routing-design.ko.html).

- 현재 로컬 0.55.3은 Codex 네이티브 UI의 새/resume/handoff를 remote/app-server로
  중계하며 같은 thread의 모델 변경 제어 경로를 검증했다. 아래 일부 입력 재구현
  검토 기록은 이 구현 이전 자료다. Jev 연결이나 실제 비용 절감을 검증한 것은 아니다.
- 권고는 **Jev의 작업 요구 판정 + 코드의 품질 하한/비용 정책**이다. 모델 ID/가격/계정
  허용 여부를 Jev가 임의 생성하지 못하게 한다. catalog 순서를 비용 순위로 쓰지 않는다.
- 기존 UI·동일 제공자/계정·thread·권한을 보존한다. PIM/자체 학습 모델/별도 daemon/
  매 턴 모델 선택 UI를 추가하지 않는다. 타 제공자는 어댑터 검증 전 원본 실행을 유지한다.
- 외부 전송 범위, 품질 우선순위, 후보/청구 방식, 실패 시 동작, 판단 예산/지연,
  제공자 지원 순서는 미결정이다. 권고값은 승인된 운영 설정이 아니다.
- 계정 quota와 모델별 소모량은 다르며 구독 소모를 API 가격으로 환산하지 않는다.
  자료가 없을 때는 기존 검증 모델을 유지하고 절감 효과를 미상으로 기록한다.

## 이전 검토안 — PIM 없이 입력별 모델 선택, 2026-10-15

상태: **대안 비교 초안 / 구현·방식 선택 미승인**.
이번 요청은 Vercel 연결과 PIM 개발·연결을 제외하고, MindPlayer가 매 입력의
실행 모델을 고르는 여러 방법을 HTML로 비교하는 것이다. 아래 과거 Sense의
자체 학습 모델 방향을 이어 구현하지 않는다. 기존 Trace와 완료 표시 결정은 유지한다.
미리보기: [모델 선택 세 가지 의견](docs/model-routing-options.ko.html).

### 유지할 화면 계약

- 위 4개 / 아래 3개 창, 전체 폭 사용량 하단, 선택 창, 입력 위치를 유지한다.
- 첫 화면에서 현재 구조의 재구성과 제안 구조를 함께 보여준다. 스크린샷이나
  실제 실행 결과가 아니라 코드에서 확인한 배치의 HTML 재구성이라고 명시한다.
- 기존 모델 표시줄의 읽기 전용 요청 모델만 강조한다. 모델 선택창·자동/고정
  전환·새 대시보드·사용자 확인 단계를 추가하지 않는다.
- HTML의 시나리오 버튼과 펼침은 문서용이다. 제품 조작이나 실제 판단기가 아니다.
- 한국어, 터미널 색상, 보이는 키보드 포커스, 네이티브 details를 사용한다.
  좁은 화면에서 비교 열은 쌓되 각 터미널 안의 4+3 배치는 보존하고 가로 스크롤한다.

### 세 가지 의견과 추천

| 대안 | 판단 방법 | 장점 | 핵심 한계 |
| --- | --- | --- | --- |
| 1. 로컬 규칙 | 입력·같은 대화의 제공된 맥락·첨부 메타데이터에 명시적인 규칙 적용 | 추가 판정 추론 호출 없음, 재현 가능 | 자연어·후속 지시를 놓치므로 보수적인 기존 모델 유지 필요 |
| 2. 기존 LLM에 매번 질문 | 학습 없이 고정 범용 모델이 작업 유형·맥락 부족 등을 구조화 출력 | 의미 이해를 빠르게 실험 | 매번 별도 호출·사용량·지연, 인증과 원문 전송 범위 확인 필요 |
| 3. 규칙 + 불확실할 때 LLM | 검증된 좁은 규칙으로 먼저 판단하고 나머지는 2안 | 의미 판단과 호출 수 절감의 절충 | 규칙의 오분류·LLM 호출 비율을 실제 데이터로 검증해야 함 |

**추천은 3안이지만 확정은 아니다.** 먼저 2안을 실행 없는 비교 기준으로
평가하고, 품질이 검증된 사례만 3안의 규칙으로 옮긴다. 자체 모델 학습·별도
서비스 저장소는 필요 없지만, MindPlayer의 작은 판단 정책과 실행 연결 코드는 필요하다.
LLM의 확신값은 정답률이 아니며 모델 ID나 권한을 판정기가 임의로 만들게 하지 않는다.
안전성·첨부 지원·계정 제약은 어떤 대안에서도 코드 정책이 우선한다.

### 현재 코드에서 확인한 실행 경계

`ui.rs::compute_pane_rects`의 실제 4+3 배치와 `main.rs`의 `encode_key →
send_to_pty`, `app/pane.rs::paste_to_pty`를 확인했다. 지금은 네이티브 CLI에
바이트를 전달한다. 판정기를 추가하는 것만으로 완성 프롬프트를 가로채거나
현재 대화의 실행 모델을 바꿀 수 없다. 키워드 훅이나 `/model` 자동 타이핑을
통합 완료로 취급하지 않는다.

공통 제안 경로는 완성 입력 이벤트 → 창별 제출 게이트 → 판정 및 모델 정책 →
Codex 턴 어댑터 → 같은 계정·threadId의 새 턴이다. 원래 입력 영역의 모양은
유지하지만 내부 입력 처리와 이벤트 렌더링은 개발이 필요하다. 현재 PTY와
app-server가 같은 대화를 동시에 실행하지 않게 단일 소유권·정확한 재개를 검증한다.

[공식 app-server 문서](https://learn.chatgpt.com/docs/app-server#start-a-turn)의
새 턴에는 모델 지정이 있고, [진행 중 추가 지시](https://learn.chatgpt.com/docs/app-server#steer-an-active-turn)에는
모델 변경이 없다. 후보는 런타임 카탈로그와 계정 정책·첨부 지원·검증된 실행
설정으로 제한한다. 목록 조회를 실제 사용 권한 증명으로 간주하지 않는다.
실행 엔진은 Codex로 고정하며 Claude 등은 별도 검증 전 지원한다고 표시하지 않는다.

판정 입력은 현재 프롬프트와 같은 대화의 필요한 최근 맥락으로 한정한다.
비밀값·다른 계정·다른 창의 원문은 섞지 않는다. 판정 호출은 도구 사용·명령 실행을
금지하는 별도 격리가 필요하며 프롬프트 문구만으로 이를 보장하지 않는다.
응답 스키마 검증과 코드 정책 검증은 별개다.

전송 전 판정 실패는 이미 사용 가능했던 기존 모델을 유지한다. 그것마저 사용할 수
없으면 기존 오류 경로를 따른다. 모델을 판정하지 못했다고 더 약한 모델로 내리지 않는다.
전송 이후 응답이 불명확하면 모델을 바꿔 다시 실행하지 않고 먼저 상태를 확인한다.
원래 도구 승인·샌드박스는 유지한다. 취소·입력 변경·창 재생성·계정 변경 시 낡은
판단은 무효화한다. IME·붙여넣기·여러 줄·첨부·승인·추가 지시는 구별한다.
후보·요청·수락·관측 모델은 다른 상태이며 요청만으로 적용 완료라 표시하지 않는다.

### 인증·측정·아직 결정하지 않은 것

- 판정용 기존 Codex 로그인 경로와 OpenAI API는 별개다. 전자는 지원 모델·
  요금제·추가 사용량을 검증해야 하고, 후자는 별도 API 인증·과금 확인이 필요하다.
  ChatGPT 구독을 임의의 API 호출 권한으로 취급하거나 로그인 토큰을 추출하지 않는다.
- 고정 모델 이름·가격·추론 지연·정확도·절감률을 가정하지 않는다. HTML의 모델
  A/B는 예시이며 실사용 가능한 후보가 하나면 모델 변경 없이 진행한다.
- [ ] 판정 연결 경로와 도구 없는 격리 방식: 구현 전에 선택·검증.
- [ ] 모델별 한국어·후속 지시·첨부 평가: 실행 품질과 판정 포함 종단 지연을 비교.
- [ ] 판정 시간 예산, 입력 큐 상한, 취소와 역압, 실패 시 사용량: 측정 후 결정.
- [ ] 기존 대화 재개, 단일 실행 소유권, 승인·추가 지시·전송 불명 상태: 회귀 검증 필요.
- 제안 HTML은 미리 정한 예시만 바꾼다. 실제 모델 판단·변경·추론 호출·Rust
  동작 검증은 이번 문서 작업에서 하지 않는다.

### 폐기 범위

사용자가 지정한 Sense HTML 2개와 같은 제안 흐름의 Jev HTML 2개는
`docs/archive/2026-10-15-model-routing/`으로 원문 그대로 이동했다.
새 설계의 진입점에서 제외하며 복구 방법·해시는 보관함 README에 기록한다.
완료 표시·Trace·observer HTML과 기존 검증 영수증은 삭제하지 않는다.

## 과거 기록 — Sense 내부 서비스, 2026-10-13 / 2026-10-15 폐기

**아래는 당시 초안의 이력이다. 자체 판정모델 방향과 연결 HTML은 후속 사용자
요청으로 폐기되었으며 현재 구현 지침이 아니다. 위의 PIM 없는 대안 비교가 우선한다.**

상태: **HTML 제안 / 실제 앱 미구현**. 사용자가 요청한 기능은 매번 바뀌는
프롬프트를 자체 Jev가 판정하여, 이미 선택한 Codex 창 안에서 적절한 모델을
쓰게 하는 것이다. Codex와 Claude 중 하나를 고르는 기능이 아니다. 실행 계획,
상시 모니터링, 별도 입력 대시보드로 범위를 넓히지 않는다.

### 실제 화면과 입력 경계

- `ui.rs:886-905`: 목록과 터미널은 별도 화면이며, 터미널에 사이드바가 없다.
- `ui.rs:1759-1792`: 7개 창은 위 4개 / 아래 3개로 빈 칸 없이 배치된다.
- `ui.rs:2304-2408`: 제목에 선택·상태·분류·이름·번호가 있다. 모델 선택 컨트롤은
  없다. 에이전트 본문의 기존 모델줄을 읽기 전용으로 유지하는 제안이다.
- `ui.rs:992-1032`: 기존 팝업은 있지만 Sense를 위한 팝업은 추가하지 않는다.
- `main.rs:1058-1123`, `app/pane.rs:1148-1205`: 단축키 처리 후 입력 바이트는
  선택한 PTY로 전달된다. Enter가 항상 새로운 완성 프롬프트라는 뜻은 아니다.

사용자의 추가 교정: 자동/고정 선택은 필요 없다. **서비스가 항상 자동으로
판단하고 적용하며, 사용자는 기존 창에서 입력하고 응답을 받기만 한다.**
앞선 `jev-model-auto-simple.ko.html`의 선택 UI는 폐기한 제안이다. 파일과 검증
영수증은 과거 산출물로 보존하지만 신규 설계의 기준으로 쓰지 않는다.

### 최소 UI 계약

- 기존 4+3 분할, 전체 폭 사용량 하단, 선택·상태 표현, 입력 위치를 유지한다.
- 제목에 버튼·배지를 추가하지 않는다. 기존 모델 표시줄에 요청 모델을 읽기
  전용으로 보여주며, 실행 증거가 없으면 적용 완료로 표현하지 않는다.
  모델 표시를 누르는 동작도, 새 설정 진입 키도 없다.
- 자동/고정, 모델 선택창, 서비스 활성화 버튼, 입력별 승인 팝업을 만들지 않는다.
  창·제공자·계정·대화 세션은 고정하고 모델 선택만 서비스에 맡긴다.
- 모델 이름은 런타임에서 얻는다. HTML의 ‘빠른 모델 / 정밀 모델’은 설명용
  예시이며 실제 모델 ID나 현재 모델의 성능 평가가 아니다.
- 판단 기록은 기존 기록 경로를 재사용한다. 상시 상세 패널이나 비공개 추론
  표시를 만들지 않는다. 서비스가 요청한 모델과 실제 실행이 확인된 모델을 구분한다.
- Ctrl-G Trace는 그대로 둔다. HTML의 입력 예시 버튼은 브라우저 시연 도구이며
  실제 앱의 설정 버튼이 아니다.
- 과거 시안: `docs/archive/2026-10-15-model-routing/sense-model-ui.ko.html`.
  과거 구성도: `docs/archive/2026-10-15-model-routing/sense-model-topology.ko.html`.

### 대안 비교와 선택

| UI 대안 | 결정 | 이유 |
| --- | --- | --- |
| 항상 자동 판단 + 기존 모델줄에 결과만 표시 | 선택 | 사용자가 입력 이외의 결정을 하지 않는다 |
| 자동/고정 선택 + 설정 팝업 | 제외 | 사용자가 요구하지 않은 선택을 다시 떠넘긴다 |
| 항상 보이는 상세 패널 | 제외 | 터미널 폭을 줄이고 매 입력마다 불필요한 정보가 보인다 |

렌더링도 세 가지를 비교했다. 단독 HTML/CSS와 자체 SVG는 외부 의존성 없이
기존 분할과 입력별 모델 변경 시연을 함께 보여줄 수 있어 선택했다. Graphviz
정적 그림은 UI 시연과 설계 정보를 한 산출물로 다루기 어렵고, 클라이언트 그래프
라이브러리는 네트워크·의존성 비용이 불필요하다. 구성도는 별도 설계 HTML에 둔다.

### 이름과 서비스 배치

이름 제안은 **Sense(센스)**, 코드 경계는 `mindplayer-sense`이다. ‘판단’을
담당하므로 Sense를 선택했다. Pilot은 실행 전체를 지휘한다는 인상을 줘 제외했고,
Switch는 의미 판단 없이 모델만 바꾸는 부품처럼 보여 제외했다. 명칭은 내부
설계 제안이며 상표·외부 서비스명 독점성을 검증한 것은 아니다.

‘서비스’는 MindPlayer 내부의 논리적 모듈을 뜻한다. 처음부터 별도 데몬이나
HTTP 서버를 띄우지 않는다. Rust 실행부에서 백그라운드 작업과 창별 큐를
사용하고, 자체 소형 판정모델의 학습·평가는 Python으로 분리하는 안이다.
추론 라이브러리와 모델 파일 형식은 실제 벤치마크 전까지 미정이다.

| 배치 대안 | 결정 | 비용 |
| --- | --- | --- |
| Rust 프로세스 내부 모듈 + 추론 포트 | 제안 | 기존 앱 수명·창 식별자 재사용, 별도 서버 관리 없음 |
| 로컬 Python 데몬 | 후속 대안 | 모델 실험은 쉬우나 배포·IPC·프로세스 관리 추가 |
| 원격 판단 API | 초기 비교 실험만 | 원문 외부 전송·네트워크·API 운영 필요, 기본 제품 경로로 채택하지 않음 |

### 자체 판단기와 정책의 분리

Jev에서 참고할 것은 자연어와 상태를 좁은 구조화 판단으로 바꾸는 역할이다.
TypeSafe 비공개 가중치나 학습 구조를 복제했다는 뜻이 아니다.

| 판정 대안 | 결정 | 한계 |
| --- | --- | --- |
| 키워드 규칙만 사용 | 제외 | 한국어·후속 지시·실제 맥락을 이해하지 못함 |
| 범용 LLM의 구조화 출력 | 평가 기준선 후보 | 빠른 초기 실험은 가능하나 별도 호출 지연·원문 전송·생성 오류가 있음 |
| 자체 소형 의미 판정모델 + 코드 정책 | 목표안 | 직접 만든 평가셋·모델별 성능 측정·보정이 먼저 필요함 |

판정기는 작업 유형, 추론 요구, 변경 위험, 맥락 의존도를 유한한 타입으로 반환한다.
정책 코드는 해당 창 제공자의 모델 카탈로그, 계정 정책·검증된 설정·지원 입력·최근
거부 정보, 측정된 프로파일을 사용해 모델 ID를 결정한다. `model/list`는 후보
메타데이터이며 실행 권한 증명이 아니다. ‘분석’이라는 단어 하나를 특정 모델에
고정 매핑하지 않는다.
새 모델이 추가되면 카탈로그·프로파일을 갱신하며 판정모델 재학습을 필수로 만들지 않는다.
판정 결과의 확률이 곧 정답률인 것은 아니므로 자체 평가 데이터로 보정해야 한다.

### 제안 구조와 미해결 구현 경계

```text
기존 Codex 창의 완성된 새 입력
  -> 창별 턴 게이트: 고정한 세션·계정·권한의 입력 스냅샷과 큐
  -> Sense 판정기: 작업·난도·위험·맥락 판단
  -> Sense 정책: 같은 제공자의 허용 모델 중 선택
  -> Codex 턴 어댑터: 선택 모델을 새 턴에 지정
  -> 같은 Codex 세션에서 실행
```

이 경로는 제안이지 현재 PTY에 구현된 기능이 아니다. 완성된 프롬프트를
수집하고 모델을 적용하는 턴 어댑터가 필요하다. raw PTY의 Enter 감지만으로
이를 보장할 수 없다. 같은 시각적 입력 영역에서 구조화된 제출 이벤트를 만들고,
어댑터가 app-server 실행·이벤트 렌더링을 소유하는 목표안이다. 기존 CLI의 PTY와
app-server를 같은 대화에 동시에 붙이지 않는다. 신규 창의 실행 소유권과 기존
대화의 정확한 재개·단일 소유권 이전을 각각 검증해야 하며, 이 이전은 아직 구현하지
않았다. Claude는 후속 단계로 제공자 내부 모델 변경·세션 유지 지원을 검증한 뒤
별도 어댑터로 연결한다.

설치본 `codex-cli 0.162.1`의 스키마와 공식 문서에서 `turn/start.model`은 해당
턴과 후속 턴에 지속됨을 확인했다. 따라서 매 새 턴에 명시적으로 지정한다.
`turn/steer`에는 모델 필드가 없다. 초기화·모델 목록 조회만 실제 수행했고,
프롬프트 제출과 대화 변경은 0회였다. Turn 응답에는 실행 모델 영수증 필드가
없다. 후보·요청·제공자 수락·관측 모델·미확인 상태를 분리하며,
`model/rerouted` 통지가 없다는 사실을 실행 증명으로 사용하지 않는다.

승인 응답, 작업 중 추가 지시, 붙여넣기 내부 개행은 새 턴과 구분한다. 모델 변경은
새 턴을 시작하기 전에 적용한다. 진행 중인 턴을 다른 모델로 재시작하거나 실행을
복제하지 않는다. 새 턴 제출과 현재 턴 추가 지시는 입력 제어 이벤트로 구분하며
판정모델이 임의로 구분하지 않는다. 판단은 제공자·비밀값 없는 계정 참조·창 세대·
threadId·활성 턴 ID·입력 버전/해시·카탈로그 버전에 묶는다. 승인·추가 지시·
새 입력·닫기·재생성·세션 재식별 시 오래된 판단을 무효화한다.
판정 시간초과·유효하지 않은 결과는 사용 가능성이 확인된 기존 모델로 진행한다.
사용할 모델이 없으면 기존 오류 경로를 따른다. 판단 시간 예산·큐 상한·취소·
역압 동작은 구현 전 정하고 검증한다. 무한 대기나 입력 손실은 허용하지 않는다.
전송 후 상태가 불명확한 경우에는 새 모델로 중복 실행하지 않고 상태를 확인한다.

최초 맥락은 동일 제공자·계정·threadId의 현재 입력과 필요한 최근 턴으로
제한하고 출처를 기록한다. 분류가 같다는 이유로 다른 창의 대화·델타를 합치지 않는다.
IME·소프트 개행·붙여넣기·첨부·승인·추가 지시·초기 대기 입력을 포함한 제출
계약과 정확한 기존 대화 재개가 구현 승인 조건이다. 모델 선택 승인은 없애지만
원래의 도구 실행·권한 승인은 유지한다.

REA는 공개/허가된 인터페이스와 동작을 조사하는 개발 도구이며 실제 입력
경로의 구성요소가 아니다. 공개 기능 계약 → 직접 만든 평가셋 → 자체 판정모델
학습·평가의 개발 경로에 둔다. 비공개 학습 데이터나 가중치를 얻었다고 주장하지 않는다.
실제 사용자 원문을 학습 데이터로 자동 저장·외부 전송하지 않는다.

### 표현 기준과 검증 범위

- C4 [표기법](https://c4model.com/diagrams/notation): 구성요소의 역할과 화살표
  의미를 짧게 표시하고 기존 부분과 제안 부분을 구분한다.
- W3C [펼침](https://www.w3.org/WAI/ARIA/apg/patterns/disclosure/): 설명과 구성도는
  상세 설명을 네이티브 details로 접어둔다. 구성도는 설계 HTML의 주 화면이다.
- HTML의 예제 입력은 결정론적 시연이다. 실제 Jev 호출, 모델 변경, Rust 앱
  동작을 검증한 증거로 사용하지 않는다. 좁은 브라우저에서도 4+3 분할 자체는
  재배치하지 않고 화면별 가로 스크롤을 허용한다.

## Active — Codex completion visibility / approved A, 2026-10-12

Status: **Active — A implemented in local 0.54.2.** This additive decision
does not change the approved Trace direction or any earlier Trace history.
The standalone HTML remains illustrative; the Rust TUI is the implementation
surface. Preview: `docs/codex-completion-ui-proposal.html`.

Refreshed against the user's actual screenshot (2026-10-12 13:39:39): the
primary surface is the existing full-width seven-pane grid, **four panes above
and three wider panes below**, followed by the existing full-width account
usage/quota footer. The earlier left-list/dashboard mockup is rejected for this
surface. Do not add a left list, result inbox, pane rows, or dependencies.
Screenshot contents cannot establish lifecycle status; mock text, state
assignments, and quota figures are illustrative. The original screenshot is
not embedded.

### Evidence and goal

The user cannot easily distinguish a completed Codex response from an idle
pane. The current `crates/mindplayer-tui/src/ui.rs` uses status-colored spans,
while `agent_hooks.rs` maps `Stop` to `Idle` and `app/pane.rs` prioritizes
confirmed process termination. Generic `Idle` is not completion evidence, and
process exit is not proof of success.

Goal: identify the lifecycle state at a glance without confusing completion
with idle, interruption, process exit, approval blocking, provider identity,
or category text. Completion is a current-turn evidence claim, not a claim
that the task succeeded or passed verification. Non-goals: change the Trace
direction, add a new application surface, change the existing seven-pane
geometry, add rows or dependencies, reorder panes, move focus, or change the
account/session model as part of this design decision.

### Approved direction — A / status borders

- **A is approved.** The pane border carries the lifecycle status color. Keep
  the status visible at a glance without adding a header chip or a new row.
- Focus and zoom use only the existing focus/zoom affordances: the current
  corner treatment and `▸` marker. They do not replace the status border or
  create a second status channel.
- Categories exist only in the title. A label such as the screenshot's `still
  working` remains category/title content and must not be promoted to a
  lifecycle status. Provider tags, pane indexes, handoff labels, and
  HTML-preview markers remain independent title metadata.
- In a narrow pane, render the **status glyph first, then category/title**. If
  the cell budget is insufficient, elide or shorten the category/title before
  dropping the glyph; perform a terminal cell-width check rather than relying
  on character count.
- B is rejected as the final direction: a fixed header chip adds a competing
  status surface and consumes the title/header budget. C is rejected as the
  default: a bottom status strip adds a terminal content row to every pane.

### Geometry and implementation constraints

- Preserve the screenshot's existing **4 top + 3 bottom pane geometry** and
  the existing full-width account usage/quota footer.
- Add zero pane rows and zero dependencies. Do not introduce a left list,
  dashboard, result inbox, or responsive reflow that changes the terminal
  composition.
- Keep existing selection/focus/zoom behavior and category/title content.
  Status borders must not make a selected or zoomed pane lose its existing
  `▸`/corner affordance.
- No HTML or browser changes are required for this implementation. Verification
  covers Rust rendering cells and real pane PTYs.

### Lifecycle semantics and approved palette

| State | Meaning | Foreground |
| --- | --- | --- |
| `idle` | Input is available, but there is no trustworthy current-turn completion evidence. Quietness, a stale `Stop`, or generic inactivity must not become completion. | `#8C94A2` / RGB 140, 148, 162 |
| `working` | The current turn is executing. | `#F0D478` / RGB 240, 212, 120 |
| `completed` | A trustworthy end signal is correlated with the current submitted turn. This means the response ended; it does not mean the task succeeded or passed verification. | `#91D9AE` / RGB 145, 217, 174 |
| `blocked` | The user must approve or otherwise act before execution can continue. | `#FFB783` / RGB 255, 183, 131 |
| `ended` | The PTY/process exited. This is gray and success-unknown, including a process that happened to display a successful-looking final screen. | `#AEB7C6` / RGB 174, 183, 198 |

Explicit error evidence remains the only basis for a failure state; preserve
the Visual language rule that red is reserved for errors. Clear `completed`
when the next turn starts. Explicit interruption invalidates the current turn's
completion correlation; later completion records for that turn cannot restore
green. The pane then uses idle, blocked, or ended according to current evidence.
The agent's terminal text owns the interruption message; a dedicated interrupted
status is outside this patch. Do not reorder panes or move keyboard focus on any
transition.

### Acceptance criteria and implementation handoff

Verify the actual Rust implementation against these criteria:

1. At the screenshot-sized terminal composition, exactly four panes remain in
   the top band and three wider panes remain in the bottom band; the existing
   account usage/quota footer remains full-width and no left list or extra pane
   row appears.
2. A status borders render with the approved palette. Focus and zoom retain
   only their existing corner/`▸` affordances, and category labels remain title
   text rather than status text.
3. Narrow-pane rendering places the status glyph before category/title and
   preserves the glyph under truncation; verify terminal cell width for the
   chosen glyphs. Automatic ASCII fallback remains a future consideration.
4. A current-turn-correlated end signal produces `completed`; generic idle,
   stale/unmatched `Stop`, or screen quietness stays `idle`. Explicit
   interruption clears completion correlation, process exit is gray `ended`,
   and neither is rendered as `failed` or `completed` without its required
   evidence.
5. Starting a new turn clears the prior completion display. Status transitions
   do not reorder panes, move focus, alter zoom, change category text, add rows,
   or add dependencies.
6. Run the existing Rust unit/smoke test suite plus targeted lifecycle tests
   for completion correlation, idle fallback, interruption, process exit, and
   palette/geometry rendering. Perform a terminal smoke check at the supplied
   4+3 layout and a narrow-pane glyph-first case. Browser visual comparison is
   not a required or available verification path for this handoff.

### Implementation decisions and remaining questions

- [x] Correlate Codex `event_msg` / `task_started` and `task_complete` records
  by turn ID, without requiring hooks. Start resumed-session readers at EOF;
  historical completion is not replayed as current completion.
- [x] Invalidate completion correlation on interruption and on a new submitted
  prompt; preserve it when approving/steering an already active turn.
- [ ] Consider a separately labelled interrupted state only as a future,
  explicitly scoped change; do not infer it from silence.
- [ ] Validate ASCII fallbacks for terminals that render status glyphs as wide.
- [ ] Browser visual comparison remains unverified: browser file-URL access
  was blocked; do not treat source/logic validation as screenshot validation.
