# NeuroVector Backend

NeuroVector API 서버입니다.

- **Rust 2024 + axum 0.8**
- PostgreSQL(pgvector) · Redis · Kafka 연동
- Mosquitto MQTT 수신 (감지/텔레메트리/상태)
- MediaMTX 연동 (SRT 영상 수신, WebRTC/WHEP 중계, 녹화 조회)
- 경로 검증 · 배송 일정 · 장기 위험 예측 · 비행 분석
- 재고 · 주문 · 더미 결제

## 요구 사항

- Rust 1.85+ (`rustup` 권장)
- platform 레포의 Docker 스택 (PostgreSQL/Redis/Kafka/Mosquitto/MediaMTX)

## 시작하기

```bash
cp .env.example .env
cargo run
```

기본 주소는 `http://localhost:8081` 입니다.

```bash
curl http://localhost:8081/health
curl http://localhost:8081/health/ready
```

## API 요약

### 감지 / 텔레메트리

| 메서드 | 경로 | 설명 |
| --- | --- | --- |
| `GET` | `/api/v1/detections?limit=50` | 감지 목록 (최신순, 최대 200) |
| `POST` | `/api/v1/detections` | 감지 1건 저장 + Kafka 발행 (HTTP 호환) |
| `GET` | `/api/v1/detections/{id}` | 감지 1건 조회 |
| `GET` | `/api/v1/flights/status` | 드론별 최신 상태 |
| `GET` | `/api/v1/flights/status/{drone_id}` | 특정 드론 최신 상태 |
| `POST` | `/api/v1/flights/telemetry/{drone_id}` | 텔레메트리 1건 저장 (테스트용) |
| `GET` | `/api/v1/flights` | 비행 세션 목록 |
| `GET` | `/api/v1/flights/{id}` | 비행 세션 조회 |
| `GET` | `/api/v1/flights/{id}/telemetry` | 비행 텔레메트리 |
| `GET` | `/api/v1/flights/{id}/analysis` | 비행 분석 (거리/배터리/고도/이상 징후) |
| `POST` | `/api/v1/flights/{id}/end` | 비행 세션 수동 종료 |

### 경로 / 배송 / 위험

| 메서드 | 경로 | 설명 |
| --- | --- | --- |
| `GET` | `/api/v1/routes` | 경로 목록 |
| `POST` | `/api/v1/routes` | 경로 생성 (검증 통과 시) |
| `POST` | `/api/v1/routes/validate` | 경로 검증만 수행 |
| `GET` | `/api/v1/routes/{id}` | 경로 조회 |
| `GET` | `/api/v1/routes/{id}/validation` | 저장된 경로 재검증 |
| `DELETE` | `/api/v1/routes/{id}` | 경로 삭제 |
| `GET/POST` | `/api/v1/routes/no-fly-zones` | 비행 금지 구역 목록/생성 |
| `DELETE` | `/api/v1/routes/no-fly-zones/{id}` | 비행 금지 구역 삭제 |
| `GET` | `/api/v1/deliveries?status=&drone_id=&from=&to=` | 배송 일정 목록 |
| `POST` | `/api/v1/deliveries` | 배송 일정 생성 |
| `GET` | `/api/v1/deliveries/{id}` | 배송 상세 |
| `PATCH` | `/api/v1/deliveries/{id}/status` | 배송 상태 전환 |
| `GET` | `/api/v1/risk/predict?route_id=&drone_id=&days=` | 장기 위험 예측 |
| `GET` | `/api/v1/risk/hotspots?days=&limit=` | 감지 핫스팟 |

경로 검증은 웨이포인트 수/좌표/고도, 구간 길이, 총 거리, 배터리 소모 추정,
비행 금지 구역 교차 여부를 규칙 기반으로 검사합니다. 위험 예측은 최근 감지의
신뢰도·라벨 심각도·최신성(반감기 30일)을 가중해 0~100 점수를 계산합니다.

### 미디어

| 메서드 | 경로 | 설명 |
| --- | --- | --- |
| `GET` | `/api/v1/streams` | MediaMTX 활성 경로 |
| `GET` | `/api/v1/streams/{drone_id}` | SRT publish/read URL + 활성 여부 |
| `POST` | `/api/v1/streams/{drone_id}/whep` | WHEP offer 프록시 (Content-Type: application/sdp) |
| `DELETE` | `/api/v1/streams/{drone_id}/whep/{session_id}` | WHEP 세션 종료 |
| `GET` | `/api/v1/recordings?drone_id=` | 녹화 목록 |
| `GET` | `/api/v1/recordings/{drone_id}/{filename}` | 녹화 파일 다운로드 |

드론 publish 예시:

```text
srt://<MEDIA_SRT_PUBLIC_HOST>:8890?streamid=publish:drone-001
```

### 재고 / 주문 / 결제 (더미)

| 메서드 | 경로 | 설명 |
| --- | --- | --- |
| `GET/POST` | `/api/v1/inventory` | 재고 목록/등록 |
| `GET/PATCH` | `/api/v1/inventory/{id}` | 재고 조회/수정 |
| `POST` | `/api/v1/inventory/{id}/restock` | 재고 추가 |
| `GET/POST` | `/api/v1/orders` | 주문 목록/생성 |
| `GET` | `/api/v1/orders/{id}` | 주문 상세 (품목/결제 포함) |
| `PATCH` | `/api/v1/orders/{id}/status` | 주문 상태 전환 |
| `GET/POST` | `/api/v1/orders/{id}/payments` | 결제 내역/더미 결제 |
| `GET` | `/api/v1/payments/{id}` | 결제 조회 |
| `POST` | `/api/v1/payments/{id}/refund` | 더미 환불 |

주문 생성 시 재고를 `SELECT ... FOR UPDATE` 로 잠그고 차감합니다. 주문 취소/환불 시
재고를 복구하며, 결제는 외부 PG 없이 `dummy` provider 로 즉시 승인/거절을 시뮬레이션합니다.

```bash
# 성공 결제
curl -X POST http://localhost:8081/api/v1/orders/{id}/payments \
  -H 'Content-Type: application/json' \
  -d '{"method":"dummy_card","simulate":"success"}'

# 실패 결제 (402 Payment Required)
curl -X POST http://localhost:8081/api/v1/orders/{id}/payments \
  -H 'Content-Type: application/json' \
  -d '{"method":"dummy_card","simulate":"failure"}'
```

## MQTT

Mosquitto 브로커는 `platform` compose 의 `mosquitto` 서비스입니다. 기본 계정은
`neurovector` / `neurovector` (`.env` 에서 변경) 이며, 드론별 계정은
`MQTT_EXTRA_USERS=drone-001:secret,drone-002:secret` 형식으로 추가할 수 있습니다.

| 토픽 | 페이로드 |
| --- | --- |
| `neurovector/drones/{drone_id}/detections` | `{"label":"person","confidence":0.91,"latitude":37.5,"longitude":127.0}` |
| `neurovector/drones/{drone_id}/telemetry` | `{"latitude":37.5,"longitude":127.0,"altitude_m":30,"speed_mps":8,"battery_pct":82,"status":"in_flight"}` |
| `neurovector/drones/{drone_id}/status` | `{"online":true,"status":"ready"}` |

HTTP `POST /api/v1/detections` 는 기존 연동 호환을 위해 그대로 유지됩니다.
텔레메트리는 `status` 값이 `in_flight`/`armed` 등이면 비행 세션을 자동 시작하고,
`landed`/`completed`/`idle`/`failed` 등이면 세션을 종료합니다.

## 데이터베이스

```bash
psql "$DATABASE_URL" -f migrations/0001_create_detections.sql
psql "$DATABASE_URL" -f migrations/0002_routes_deliveries_flights_orders.sql
```

`platform` compose 에서는 `migrate` 서비스가 모든 마이그레이션을 순서대로 적용합니다.
마이그레이션은 `IF NOT EXISTS` 를 사용해 반복 실행해도 안전합니다.

## 환경 변수

`.env.example` 참고. 주요 값:

| 변수 | 기본값 |
| --- | --- |
| `LISTEN_ADDR` | `0.0.0.0:8081` |
| `DATABASE_URL` | `postgres://neurovector:neurovector@localhost:5432/neurovector` |
| `REDIS_URL` | `redis://:neurovector@localhost:6379/0` |
| `KAFKA_BROKERS` | `localhost:9092` |
| `MQTT_HOST` / `MQTT_PORT` | `localhost` / `1883` |
| `MEDIA_API_BASE_URL` | `http://localhost:9997` |
| `MEDIA_WHEP_BASE_URL` | `http://localhost:8889` |
| `MEDIA_RECORDINGS_DIR` | `./recordings` |
| `MAX_ALTITUDE_M` | `120` |
| `RISK_LOOKBACK_DAYS` | `90` |

## 개발

```bash
cargo build
cargo test
cargo clippy --all-targets
cargo fmt
```
