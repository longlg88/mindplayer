# MindPlayer 전용 Codex UI

일반 Codex 설치는 변경하지 않는다. upstream `rust-v0.162.1`의 정확한 revision
`092d3acd6bec3e3a14bdc7e7a2810ab628ab759d`에 `queue.patch`만 적용한다.
MindPlayer가 여는 UI·probe·app-server에 같은 전용 바이너리를 사용한다. 일반 PATH
Codex는 교체하지 않으며 로그인·권한·Jev 판단 기준은 유지한다.
이 CLI에는 같은 디렉터리의 `codex-code-mode-host`도 반드시 필요하다. 모델의
`tool_mode=code_mode_only`에서는 CLI만 설치하면 도구를 사용할 수 없다.

실행 중 일반 Enter 입력은 네이티브 큐에 각각 보관한다. 이전 턴이 정상 완료되면
하나씩 새 `turn/start`로 제출하므로 Jev가 매 입력의 모델과 effort를 다시 선택한다.
idle `!shell`, 승인 창, Esc는 네이티브 의미를 유지한다. 실패·중단 뒤 큐는 자동
실행하지 않는다. 기존 실행 세션에는 소급 적용하지 않는다.
upstream의 `turn/start` 자체도 active 턴을 steer할 수 있어 UI 사전 확인만으로는
부족하다. 전용 backend는 `mindplayerRequireNewTurn` 입력에 기존 Core의
`start_turn_if_idle`을 사용해 경합을 원자적으로 거절한다. 자동 재시도는 없다.
도구 출력과 원래 helper/steering 요청에는 이 경로를 강제하지 않는다.

`mindplayer-native-routing-v4`는 기존 큐·취소 계약에 세션별 유지 토글을 더한다.
MindPlayer가 설정한 `MINDPLAYER_PER_PROMPT_CHOICE=1`에서만 입력줄의 F6 Jev/
미사용 토글을 활성화한다. 새 세션과 같은 pane의 새 thread는 `MINDPLAYER_JEV_DEFAULT`가
정확히 `jev`이면 ON, 그 밖에는 OFF로 시작한다. 이 값은 로컬 중계가 Jev 키를 읽었을 때만
설정한다. F6로 고른 ON/OFF는 다음 F6까지 유지된다.
전송 시 선택은 각 UserMessage에 결합되므로 이후 토글로 대기열을 다시 쓰지 않는다.
F7 미리보기와 previewId 계약은 제거했다. 환경변수의 기존 이름은 입력별 메타데이터
전달을 뜻하며 매번 선택을 강제한다는 뜻이 아니다. 선택 메타데이터는 로컬 중계가
소비·제거하며 backend에 전달하지 않는다.
전역 Codex나 Claude에는 이 선택을 추가하지 않는다.

판단 중에도 터미널 이벤트 루프를 유지한다.
실제 턴 ID가 생기기 전 Esc는 입력 시도 ID로 `mindplayer/turn/cancel`을 요청한다.
이미 backend에 제출됐다면 취소했다고 꾸미지 않고, 실제 ACK의 턴 ID를 받아
그 턴만 중단한다. 취소한 문장을 다시 직접 제출하면 새 입력 ID로 다시 판단한다.
자동 goal 계속 실행은 새 사용자 입력이 아니며 별도 Jev 판단으로 표시하지 않는다.

모델과 effort는 함께 적용한다. `Applied for this turn: …` 안내는 Jev 선택이나
`turn/start` ACK가 아니라 Core의 적용 설정과 실제 `turn/started`의 상관관계를
확인한 뒤 표시한다. 같은 모델에서 effort만 바뀌거나 같은 설정이 재적용된 턴도
확인할 수 있게 하고, 같은 턴의 알림은 중복 표시하지 않는다. 이 안내는 실행 설정 확인이지 제공자의 서빙 모델
신원 증명이 아니다. 자동 라우팅은 사용자 전역 `config.toml`을 수정하지 않는다.
이전 v1 실행 파일은 이 계약을 충족하지 않으므로 새 앱에서 거절한다.

## 재현

별도 upstream checkout과 Rust 1.95.0이 필요하다. 사용자 Codex나 기본 Rust를
교체할 필요는 없다. upstream의 기존 의존성만 사용하며 새 의존성을 추가하지 않는다.

```sh
git clone --depth 1 --branch rust-v0.162.1 https://github.com/openai/codex.git /absolute/private/source
node tools/native-codex/build.mjs --source /absolute/private/source --output /absolute/private/staging --cargo /absolute/rust/bin/cargo
```

빌드 도구는 소스 revision·변경 내역을 검사하고 네이티브 회귀 테스트를 실행한 뒤,
`dev-small` 프로필로 UI를 포함한 CLI를 빌드한다. 호스트는 동일한 고정 소스·잠금
의존성으로 빌드하거나, `--code-mode-host /absolute/official/0.162.1/codex-code-mode-host`
옵션으로 공식 npm 패키지 0.162.1의 기존 실행 파일을 명시적으로 제공해 복사한다.
후자는 소스 빌드로 기록하지 않고 원본 경로·SHA-256·prebuilt 출처를 후보 영수증에
별도로 보관한다. 전역 설치는 수정하지 않으며 PATH에서 호스트를 자동 탐색하지 않는다.
호스트의 upstream V8 아카이브를 내려받을 수 없는 경우에도 호스트를 생략한
후보를 활성화하지 않는다. 후보 manifest와 SHA-256 경로의 실행 파일 쌍만 생성하며
**자동 활성화하지 않는다**. 최적화한 배포 빌드가 아닌 로컬
검증용 빌드다. `--mindplayer-ui-contract`는 전용 패치의 식별자이며, 기능 동작
검증이나 서빙 모델의 증거를 대체하지 않는다.

실제 PTY 회귀 테스트는 `MINDPLAYER_TEST_NATIVE_UI=1` 및
`MINDPLAYER_NATIVE_CODEX=/absolute/private/codex`로 실행한다. 실제 Jev·추론 검증은
별도 opt-in이며 API 사용량을 소비한다. 합성 backend 결과와 실제 추론 기록을
혼동하지 않는다. 정상 완료·개별 입력·승인·중단 검증을 통과한 후보만 로컬
`~/.mindplayer/native-codex/active.json`으로 활성화한다. 이전 manifest와 MindPlayer
실행 파일은 교체 전에 보관한다.

활성 manifest에는 후보 영수증 전체를 복사하지 않는다. 검증한 CLI를
`builds/<binarySha256>/codex`, 호스트를 그 형제 경로인
`builds/<binarySha256>/codex-code-mode-host`에 설치한다. `schemaVersion: 1`을
유지하되 `schemaVersion`, `sourceRevision`, `contract`, `version`, `binarySha256`,
`binary`, `codeModeHostSha256`, `codeModeHost` 여덟 필드만 `active.json`에 기록한다.
`codeModeHostSha256`은 CLI가 아닌 호스트 자체의 SHA-256이고, `codeModeHost` 경로의
디렉터리 해시는 CLI의 `binarySha256`이다. 기존 여섯 필드 manifest는 거절한다.
디렉터리·실행 파일은 0700, manifest는 0600으로 유지한다. 두 실행 파일 모두
소유자·일반 파일·실행 권한·크기 제한·심볼릭 링크 없는 경로·해시를 검사한 뒤에만
통합 probe·backend·UI를 실행한다. 누락·변조 시 전역 Codex나 다른 호스트로
대체하지 않는다. 빌드·패치·호스트 출처·테스트 메타데이터는 별도 후보 영수증으로
보관한다. 해시·계약 확인이나 합성 호스트 테스트는 실제 도구 실행의 증거가 아니며,
활성화 전 실제 네이티브 도구 경로로 CLI·호스트 호환성을 별도 검증한다.

비교 후 제외한 방식:

- Enter를 queue에만 바인딩: idle `!shell` 제출이 망가진다.
- 중계기에서 `turn/steer` 응답 보류: 네이티브 UI 이벤트 루프와 승인·Esc를 막는다.
- 비정상적인 review/compact 오류 반환: 실제 backend 상태와 다르고 입력이 합쳐질 수 있다.

네이티브 UI 자체의 큐를 사용하면 입력 소유권과 원래 승인 경로를 그대로 유지할 수
있다. 대신 upstream 버전 업그레이드 시 패치·프로토콜·실제 PTY 검증을 다시 해야 한다.
원본 Codex 코드는 upstream Apache-2.0 라이선스를 따른다.
