# 경로 계획·드론 교차 검증·창문 수령

## 처리 흐름

1. 백엔드가 직선, 요청에 포함된 대안, 금지구역을 우회하는 제한된 가시성 그래프 후보를 만든다.
2. 각 후보의 좌표·고도·구간 거리·총 거리·예상 에너지·금지구역 교차를 검사한다.
3. 유효 후보 중 3차원 이동 거리가 가장 짧은 후보를 저장한다.
4. 드론의 독립 C++ 검증기가 동일한 후보들을 기체 자체 제한으로 다시 검사한다.
5. 드론의 내용 지문·선택 후보·거리·소요 시간·에너지 계산이 백엔드와 일치하면 계획이 `verified`가 된다.
6. 배송이 `dispatched` 또는 `in_flight`로 바뀔 때 유효 기간·드론·경로·현재 금지구역·제한을 다시 확인한다.

선택 기준은 **생성·제공된 후보 안에서의 최단 3차원 거리**다. 전 세계 장애물·풍속·지도 정보를
활용한 전역 최적 경로가 아니다. 가시성 그래프는 다각형 정점 바깥의 8m 오프셋을 후보로 삼고
각 연결 선분을 전체 금지구역과 검사한다. 복잡한 오목 다각형 등에서는 유효 경로를 찾지 못할 수 있다.
그 경우 잘못된 직선 경로를 승인하지 않고 실패하며 명시적인 대안을 받아 다시 계획한다.

계획 요청은 최대 254개 입력 지점, 대안 13개, 총 후보 16개로 제한한다. 자동 계획은 최대 64개
활성 금지구역 및 전체 126개 다각형 정점을 지원한다. 날짜 변경선을 가로지르는 선분은 지원하지 않는다.
모든 고도는 같은 기준점/좌표계의 미터 값이어야 한다. 현재 에너지 모델은 일정한 평균 소비전력에
비행 시간과 수령 대기 시간을 곱한다. 풍속·기체별 수직 속도·수령 하드웨어 모델은 포함하지 않는다.

## API

| 메서드 | 경로 | 동작 |
| --- | --- | --- |
| POST | `/api/v1/routes/plan` | 경로 후보 생성·검증, 경로 및 계획 저장 |
| GET | `/api/v1/routes/plans/{id}` | 후보·모델 제한·검증 정보·`plan_text` 조회 |
| POST | `/api/v1/routes/plans/{id}/cross-check` | 독립 드론 검증 결과 비교 |

DB에 `migrations/0007_mission_plans.sql`을 적용해야 한다. 이번 구현 작업에서는 실제 DB에 적용하지 않았다.
기존 `/routes` 생성 API도 유지하지만, 기존 경로만으로 배송을 출발시킬 수는 없다. 계획 API가 만든
`route_id`로 배송을 생성하고 드론 교차 검증을 완료해야 한다. 계획의 기본 유효 기간은 600초이며
요청에서 30~1800초로 지정한다. 출발 직전에 생성·검증하고, 만료 또는 경로/제한/구역 변경 시
새 계획의 `route_id`로 다시 예약한다. 비행 중의 동적 장애물은 기체 회피 판단 계층이 별도로 처리한다.

## 창문 밖 정지·수령

실내 주소 문자열만으로 창문을 추측하지 않는다. `window_target`에 실제 창문 좌표·고도와
**건물 바깥쪽을 향하는 방위각**을 명시한다. 0°는 북쪽, 90°는 동쪽, 180°는 남쪽이다.
입력 경로의 마지막 지점은 `window_target.window`와 일치해야 한다.

최종 경로는 창문 지점을 제거하고 `approach_distance_m` 바깥 접근 지점과 `standoff_m` 바깥
정지 지점을 순서대로 붙인다. 이격은 최소 2m, 접근 거리는 이격보다 크고 최대 1000m다.
대기 시간은 1~3600초이며 에너지에 반영한다. 실제 기체의 더 엄격한 제한은 드론이 별도로 검사한다.
후보 전체가 창문 정면의 외부 이격 평면 안쪽으로 들어가지 않도록 검사하므로 건물 반대편에서
출발하는 경로는 거절될 수 있다. 건물 전체 외벽 지도·접근 가능 창문·수령인 확인을 자동 인식하지 않는다.

예시: 남쪽에서 접근해 남향 창문 밖 3m에 정지한다.

```json
{
  "name": "window-handoff",
  "drone_id": "drone-001",
  "waypoints": [
    {"lat": 37.499, "lon": 127.0, "altitude_m": 30.0},
    {"lat": 37.5, "lon": 127.0, "altitude_m": 30.0}
  ],
  "cruise_speed_mps": 5.0,
  "max_altitude_m": 120.0,
  "window_target": {
    "window": {"lat": 37.5, "lon": 127.0, "altitude_m": 30.0},
    "outward_bearing_deg": 180.0,
    "standoff_m": 3.0,
    "approach_distance_m": 15.0,
    "hold_seconds": 30.0
  },
  "expires_in_seconds": 600
}
```

## 드론 검증기 연결

위 JSON을 `request.json`으로 저장하고 다음 순서로 처리한다.

```bash
curl --fail-with-body -sS -H 'Content-Type: application/json' \
  --data-binary @request.json http://localhost:8081/api/v1/routes/plan > plan.json
jq -jr '.plan_text' plan.json > plan.txt
plan_fingerprint=$(jq -r '.route_fingerprint' plan.json)
plan_id=$(jq -r '.id' plan.json)

# 이 값들은 서버 응답이 아니라 실제 기체의 신뢰할 수 있는 설정에서 가져온다.
# DRONE_MAX_POWER_W, DRONE_CRUISE_POWER_W를 먼저 설정해야 한다.
nv_route_check --drone-id drone-001 --fingerprint "$plan_fingerprint" \
  --max-power-w "$DRONE_MAX_POWER_W" --min-power-w "$DRONE_CRUISE_POWER_W" \
  < plan.txt > cross-check.json && \
curl --fail-with-body -sS -H 'Content-Type: application/json' \
  --data-binary @cross-check.json \
  "http://localhost:8081/api/v1/routes/plans/$plan_id/cross-check"
```

`plan_text`는 마지막 개행을 포함한 정확한 원문을 전달한다. `jq -r`은 개행을 하나 더 붙일 수 있으므로
`jq -jr`을 사용한다. FNV-1a 지문은 내용 변경 감지용이며 서명이나 드론 신원 인증이 아니다.
현재 드론 HAL에는 HTTP/MQTT 임무 통신 계층이 없어 이 CLI를 통신 브리지에서 호출해야 한다.
실기체 운용에서는 해당 브리지에 드론 인증과 전송 보호를 연결해야 한다.

HAL의 기본 최대 전력 120W와 서버의 기존 평균 소비전력 기본값 300W는 서로 다르다.
검증기는 이를 자동으로 무시하지 않는다. 실제 기체에 맞게 서버 `ROUTE_POWER_W`와 드론의
신뢰할 수 있는 최대/최소 소비전력 설정을 일치시켜야 한다. 더 큰 서버 한도를 승인받기 위해
기체 한도를 임의로 올리는 흐름은 구현하지 않았다.

드론 검증은 정적 임무 검증이며 실제 자동 비행·수령 장치·장애물 관측 및 모터 제어는
[드론 회피 계층](../../drone/docs/avoidance.md)과 상위 비행 제어기에 연결해야 한다.
