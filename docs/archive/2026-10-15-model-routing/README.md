# 폐기한 모델 선택 시안

2026-10-15 사용자 요청으로 Sense HTML 2개와 직접 연결된 이전 Jev HTML 2개를
현재 문서 경로에서 제외했다. 영구 삭제하지 않고 원문을 그대로 옮겼으므로 복구 가능하다.
새 대안 비교: [PIM 없는 모델 선택](../../model-routing-options.ko.html).

| 원래 경로 (`docs/` 기준) | 보관 파일 | 이동 전후 SHA-256 |
| --- | --- | --- |
| `sense-model-topology.ko.html` | [원문](sense-model-topology.ko.html) | `9f078af245813950d06c3e0e57cc2863c87b84e2df37a84386da274b07696ae5` |
| `sense-model-ui.ko.html` | [원문](sense-model-ui.ko.html) | `f931bdcf155cd899602f0ef4ee5b0224908b9bf094a687dc0482ca6a59b710bd` |
| `jev-execution-topology.html` | [원문](jev-execution-topology.html) | `32ad7e3394867ea6890320ef627432975e0126f4df79c6b0c8f9bb80ef660cad` |
| `jev-model-auto-simple.ko.html` | [원문](jev-model-auto-simple.ko.html) | `6a923bfac42dcb91b4f3fb02e83f14d960741a2463313965e4f932f1d4187f63` |

복구하려면 원하는 보관 파일을 같은 이름의 `docs/` 경로로 이동한다. 기존 파일이
있으면 덮어쓰지 말고 비교한다. 두 Sense 파일을 함께 복구하면 상호 링크도 유지된다.
보관 원문을 현재 구현 결정이나 실제 동작 검증으로 사용하지 않는다.

범위 밖인 완료 표시·Trace·observer HTML, PIM 저장소와 과거 검증 JSON은 유지했다.
과거 검증 JSON의 파일 경로와 상태는 당시 기록이며 현재 경로·구현 상태를 뜻하지 않는다.
