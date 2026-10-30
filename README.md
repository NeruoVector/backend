# NeuroVector Backend

NeuroVector API 서버입니다.

- **Rust 2024 + axum 0.8**
- PostgreSQL(pgvector) · Redis · Kafka 연동
- Mosquitto MQTT 수신 (감지/텔레메트리/상태)
- MediaMTX 연동 (SRT 영상 수신, WebRTC/WHEP 중계, 녹화 조회)
- 경로 계획·드론 교차 검증·창문 바깥 수령 · 배송 일정 · 장기 위험 예측 · 비행 분석
- 재고 · 주문 · 더미 결제
- Kafka 기반 비동기 주문 큐 · 주문/결제 이벤트 관리
- Redis cache-aside (주문/재고/관리자 조회)
- 관리자 세션 로그인 (Argon2 비밀번호 + HttpOnly 쿠키)
- Swagger UI (`/swagger-ui`, `/api-docs/openapi.json`)

## 요구 사항

- Rust 1.85+ (`rustup` 권장)
- platform 레포의 Docker 스택 (PostgreSQL/Redis/Kafka/Mosquitto/MediaMTX)

## PostgreSQL 드라이버

데이터 접근은 `tokio-postgres`와 `deadpool-postgres`를 사용합니다. SQLx 의존성은 없습니다.
`DATABASE_URL`은 PostgreSQL 연결 URL이며 `DATABASE_MAX_CONNECTIONS`는 양수여야 합니다.
풀 생성은 지연 연결이지만 서버 시작 시 연결을 확인하며, 연결 생성·풀 대기는 각각 30초로 제한합니다.
준비된 SQL은 연결별로 캐시하고, 행은 이름으로 디코딩합니다. 트랜잭션은 커밋하지 않고
드롭하면 롤백되며, 완료 후 연결을 풀로 반환합니다.

TLS는 rustls와 운영체제 인증서 저장소를 사용하여 인증서와 호스트 이름을 검증합니다.
암호화가 필요한 배포에서는 `DATABASE_URL`에 `?sslmode=require`를 지정하세요.
기본 `prefer`는 TLS를 지원하지 않는 서버에서 평문 연결을 허용합니다.
사설 CA는 `SSL_CERT_FILE=/path/to/ca.pem` 또는 운영체제 신뢰 저장소에 등록하세요.
연결 옵션은 tokio-postgres 형식을 따릅니다 (`disable`, `prefer`, `require` 지원).
SQLx 전용 URL 옵션은 그대로 사용하지 마세요.

일반 검증:

```bash
cargo fmt --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
```

별도 테스트 DB에 마이그레이션을 적용한 뒤 드라이버 원자성·타입 검증을 실행할 수 있습니다.
실제 서비스 DB를 지정하지 마세요.

```bash
TEST_DATABASE_URL=postgres://user:password@127.0.0.1:5432/test_db cargo test postgres_codecs_transactions_errors_and_pool_reuse -- --ignored
TEST_DATABASE_URL=postgres://user:password@127.0.0.1:5432/test_db cargo test processes_create_order_job_idempotently
```

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

API 문서는 Swagger UI 에서 확인할 수 있습니다.

- Swagger UI: `http://localhost:8081/swagger-ui`
- OpenAPI JSON: `http://localhost:8081/api-docs/openapi.json`

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
| `POST` | `/api/v1/routes/plan` | 우회 후보 계획·창문 바깥 정지 경로 생성 |
| `GET` | `/api/v1/routes/plans/{id}` | 드론 전달용 계획 조회 |
| `POST` | `/api/v1/routes/plans/{id}/cross-check` | 드론 독립 검증 결과 비교 |
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
| `GET` | `/api/v1/inventory` | 재고 목록 (공개) |
| `GET` | `/api/v1/inventory/{id}` | 재고 조회 (공개) |
| `POST` | `/api/v1/inventory` | 상품 등록 (관리자 로그인 필요) |
| `PATCH` | `/api/v1/inventory/{id}` | 상품/재고 수정 (관리자 로그인 필요) |
| `POST` | `/api/v1/inventory/{id}/restock` | 재고 추가 (관리자 로그인 필요) |
| `GET/POST` | `/api/v1/orders` | 주문 목록/동기 생성 |
| `POST` | `/api/v1/orders/queue` | Kafka 기반 비동기 주문 생성 (202) |
| `GET` | `/api/v1/orders/jobs/{id}` | 비동기 주문 작업 상태 |
| `GET` | `/api/v1/orders/{id}` | 주문 상세 (품목/결제 포함) |
| `PATCH` | `/api/v1/orders/{id}/status` | 주문 상태 전환 |
| `GET/POST` | `/api/v1/orders/{id}/payments` | 결제 내역/더미 결제 |
| `GET` | `/api/v1/payments/{id}` | 결제 조회 |
| `POST` | `/api/v1/payments/{id}/refund` | 더미 환불 |
| `GET` | `/api/v1/catalog` | 활성 상품과 판매자/분류/이미지/중량 목록 |
| `GET` | `/api/v1/merchants` | 공개 판매자 목록 |
| `GET` | `/api/v1/auth/providers`, `/api/v1/auth/{provider}/login` | 설정된 Google/Kakao/Naver OAuth 로그인 시작 |
| `GET` | `/api/v1/auth/{provider}/callback` | OAuth state 검증 및 고객 세션 생성 |
| `GET` | `/api/v1/auth/me` | 현재 고객 세션 조회 |
| `POST` | `/api/v1/auth/logout` | 고객 세션 폐기 및 쿠키 삭제 |
| `GET/POST` | `/api/v1/student-verification` | 내 학생 인증 상태 조회 / 증빙 이미지 제출 |
| `GET` | `/api/v1/account/orders` | 고객 주문 내역 (고객 세션 필요) |
| `POST` | `/api/v1/orders/{id}/handoff` | `{ "pin": "123456" }` 으로 배송 인수 확인 |
| `GET/PUT` | `/api/v1/merchant/profile` | 판매자 프로필 조회/수정 |
| `GET/POST` | `/api/v1/merchant/categories` | 내 상품 분류 조회/생성 |
| `PATCH/DELETE` | `/api/v1/merchant/categories/{id}` | 내 상품 분류 수정/삭제 |
| `GET/POST` | `/api/v1/merchant/products` | 내 재고 상품 조회/생성 |
| `PUT/DELETE` | `/api/v1/merchant/products/{id}` | 내 재고 상품 수정/삭제 |

관리자용 고객 API 는 `/api/v1/admin` 세션이 필요합니다.

| 메서드 | 경로 | 설명 |
| --- | --- | --- |
| `GET` | `/api/v1/admin/student-verifications` | 학생 인증 제출 목록 |
| `GET` | `/api/v1/admin/student-verifications/{customer_id}/evidence` | 학생 증빙 원본 이미지 조회 |
| `POST` | `/api/v1/admin/student-verifications/{customer_id}/review` | `{"status":"approved"}` 또는 `{"status":"rejected","reason":"..."}` 검토 결과 저장 |
| `GET` | `/api/v1/admin/customers` | 학생 인증 상태를 포함한 고객 목록 |
| `POST` | `/api/v1/admin/customers/{id}/merchant` | 해당 고객에게 판매자 권한 부여 및 기본 프로필 생성 |

학생 인증 evidence 는 로그인한 고객의 API 응답에 포함되지 않으며, 관리자 세션으로 보호된
별도 이미지 경로에서만 읽을 수 있습니다. 상품/카테고리/프로필 변경은 로그인한 판매자 계정에
속한 행만 수정합니다.

학생 증빙 업로드는 `multipart/form-data` 의 `school`, `student_number`, `image` 필드를 받습니다.
관리자는 `/api/v1/admin/student-verifications` 에서 제출 내용을 검토하고, evidence 경로에서
세션 보호된 이미지 원본을 볼 수 있습니다. 판매자 관리와 고객 PIN 주문은 고객 OAuth 세션이
필요하며, 판매자 변경은 항상 해당 고객 소유 상품에만 적용됩니다.

동기 주문은 `handoff_pin` 을 선택적으로 받을 수 있습니다. PIN 주문은 고객 세션이 필요하고,
PIN 은 Argon2 해시로 저장되며 주문 응답, 큐 payload, Kafka 이벤트에는 포함되지 않습니다.
저장 주소는 `spot_id` 와 함께 보낼 수 있습니다. 주문의 주소/좌표/실내 위치는 선택한 주소에서
다시 읽어 같은 트랜잭션 안에 스냅샷으로 저장합니다. 비동기 주문 큐는 PIN 또는 저장 주소를
받지 않습니다. PIN 주문은 `in_transit` 상태이고, 연결된 배송 정류장이 있으면 `arrived` 된
후에만 완료할 수 있습니다. 기존 상태 API 의 `delivered` 변경도 DB trigger 가 PIN 확인 없이
완료되지 않도록 차단합니다.

상품 데이터의 `weight_grams` 는 기본 0 입니다. 자동 배송 배정에는 양수 중량을 등록해야 합니다.

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

### 관리자 세션 로그인

관리자 콘솔과 관리자 API 는 세션 쿠키 기반 로그인을 사용합니다. 최초 기동 시
`ADMIN_USERNAME` / `ADMIN_PASSWORD` 계정이 `admin_users` 에 생성되며, 비밀번호는
Argon2 해시로 저장됩니다. 비밀번호를 바꿔 재기동하면 기존 세션은 모두 폐기됩니다.

| 메서드 | 경로 | 설명 |
| --- | --- | --- |
| `POST` | `/api/v1/admin/auth/login` | `{"username":"admin","password":"..."}` 로그인, HttpOnly 세션 쿠키 발급 |
| `GET` | `/api/v1/admin/auth/me` | 현재 로그인한 관리자와 세션 만료 시각 |
| `POST` | `/api/v1/admin/auth/logout` | 세션 폐기 및 쿠키 삭제 |

세션 쿠키는 `HttpOnly`, `SameSite=Strict`, `Path=/` 로 설정됩니다. HTTPS 배포에서는
`ADMIN_COOKIE_SECURE=true` 로 설정하세요. `GET /api/v1/inventory` 와 스토어 주문
API 는 공개되지만, 재고 변경과 아래 관리자 API 는 모두 로그인 세션이 필요합니다.

```bash
# 로그인 후 쿠키를 파일에 저장
curl -c cookies.txt -X POST http://localhost:8081/api/v1/admin/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"neurovector-admin"}'

# 관리자 API 호출
curl -b cookies.txt http://localhost:8081/api/v1/admin/orders/stats
```

### Kafka 주문 큐 / 관리자 API

`POST /api/v1/orders/queue` 는 주문 요청을 `order_jobs` 테이블에 저장한 뒤
`KAFKA_TOPIC_ORDERS` (`neurovector.orders`) 로 `job_id` 를 발행합니다. 백그라운드
`order-worker` 가 메시지를 소비해 동기 API 와 동일한 트랜잭션으로 주문/재고를 처리합니다.

- 작업 상태: `queued → processing → completed` 또는 `failed`/`dead`/`cancelled`
- 컨슈머 오프셋은 `kafka_consumer_offsets` 에 저장되어 재시작 후 이어서 처리합니다.
- worker 는 job 상태를 원자적으로 선점(claim)하므로 메시지가 재처리되어도 주문이 중복 생성되지 않습니다.
- 이벤트는 `event_id` 를 기본키로 사용해 재처리 시 중복 적재를 무시합니다.
- Kafka 발행에 실패하면 job 은 `queued` 로 남고 응답의 `kafka_published=false` 로 표시됩니다.
  이후 republisher 가 주기적으로 재발행하며, 관리자 retry API 로 즉시 재시도할 수도 있습니다.
- 주문/결제 상태 변경은 `order_events` 에 적재되고 같은 토픽으로 이벤트가 발행됩니다.
- 아래 관리자 API 는 모두 유효한 관리자 세션 쿠키가 필요합니다.

| 메서드 | 경로 | 설명 |
| --- | --- | --- |
| `GET` | `/api/v1/admin/orders?status=&phone=&q=&from=&to=&limit=&offset=` | 관리자 주문 검색/페이지네이션 |
| `GET` | `/api/v1/admin/orders/stats` | 상태별 주문 수/매출 통계 |
| `GET` | `/api/v1/admin/order-jobs?status=&limit=` | 주문 작업 목록 |
| `POST` | `/api/v1/admin/order-jobs/{id}/retry` | 실패/대기 작업 재발행 |
| `POST` | `/api/v1/admin/order-jobs/{id}/cancel` | 대기/실패 작업 취소 |
| `GET` | `/api/v1/admin/order-events?order_id=&event_type=&limit=` | 주문/결제 이벤트 감사 로그 |
| `GET` | `/api/v1/admin/queue/stats` | 작업 상태/이벤트 수/컨슈머 오프셋 |
| `GET` | `/api/v1/admin/cache/stats` | Redis 캐시 설정/키 수 |

비동기 주문 예시:

```bash
curl -X POST http://localhost:8081/api/v1/orders/queue \
  -H 'Content-Type: application/json' \
  -d '{
    "customer_name": "홍길동",
    "customer_phone": "010-1234-5678",
    "delivery_address": "서울시 강남구",
    "items": [{"item_id": "<inventory-uuid>", "quantity": 1}]
  }'
```

### Redis 캐시

주문 목록/상세, 재고 목록/상세, 관리자 주문 목록/통계에 cache-aside 를 적용합니다.
기본 TTL 은 `CACHE_TTL_SECONDS` (30초) 이며, 주문/재고 변경 시 관련 prefix 를
`SCAN` 으로 무효화합니다. Redis 장애 시 캐시는 미스로 처리되고 DB 조회로 폴백합니다.
응답 헤더 `X-Cache: HIT|MISS` 로 캐시 상태를 확인할 수 있습니다.

## MQTT

Mosquitto 브로커는 `platform` compose 의 `mosquitto` 서비스입니다. 기본 계정은
`neurovector` / `neurovector` (`.env` 에서 변경) 이며, 드론별 계정은
`MQTT_EXTRA_USERS=drone-001:secret,drone-002:secret` 형식으로 추가할 수 있습니다.

| 토픽 | 페이로드 |
| --- | --- |
| `neurovector/drones/{drone_id}/detections` | `{"label":"person","confidence":0.91,"latitude":37.5,"longitude":127.0,"distance_m":12.4}` |
| `neurovector/drones/{drone_id}/telemetry` | `{"latitude":37.5,"longitude":127.0,"altitude_m":30,"speed_mps":8,"battery_pct":82,"status":"in_flight"}` |
| `neurovector/drones/{drone_id}/status` | `{"online":true,"status":"ready"}` |

HTTP `POST /api/v1/detections` 는 기존 연동 호환을 위해 그대로 유지됩니다.
`distance_m` 은 선택 필드이며, vision 서비스가 단안 카메라로 추정한 거리(미터)를
전달합니다. 값이 없으면 `null` 로 저장됩니다.

텔레메트리는 `status` 값이 `in_flight`/`armed` 등이면 비행 세션을 자동 시작하고,
`landed`/`completed`/`idle`/`failed` 등이면 세션을 종료합니다.

## 데이터베이스

```bash
psql "$DATABASE_URL" -f migrations/0001_create_detections.sql
psql "$DATABASE_URL" -f migrations/0002_routes_deliveries_flights_orders.sql
psql "$DATABASE_URL" -f migrations/0003_order_queue_cache.sql
psql "$DATABASE_URL" -f migrations/0004_admin_auth.sql
psql "$DATABASE_URL" -f migrations/0005_detection_distance.sql
psql "$DATABASE_URL" -f migrations/0006_detection_priority.sql
psql "$DATABASE_URL" -f migrations/0007_mission_plans.sql
psql "$DATABASE_URL" -f migrations/0008_commerce_accounts.sql
psql "$DATABASE_URL" -f migrations/0009_spots_dispatch.sql
psql "$DATABASE_URL" -f migrations/0010_commerce_handoff_guard.sql
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
| `CACHE_ENABLED` | `true` |
| `CACHE_TTL_SECONDS` | `30` |
| `KAFKA_BROKERS` | `localhost:9092` |
| `KAFKA_TOPIC_EVENTS` | `neurovector.events` |
| `KAFKA_TOPIC_ORDERS` | `neurovector.orders` |
| `ADMIN_USERNAME` / `ADMIN_PASSWORD` | `admin` / `neurovector-admin` |
| `ADMIN_SESSION_TTL_SECONDS` | `86400` |
| `ADMIN_COOKIE_SECURE` | `false` |
| `AUTH_PUBLIC_ORIGIN` | `http://localhost:5173` (public web origin; `/api/v1` is proxied to backend) |
| `AUTH_FRONTEND_ORIGIN` | `http://localhost:5173` |
| `AUTH_COOKIE_SECURE` | `false` (HTTPS 배포 시 `true`) |
| `GOOGLE_CLIENT_ID` / `GOOGLE_CLIENT_SECRET` | provider console credentials |
| `KAKAO_CLIENT_ID` / `KAKAO_CLIENT_SECRET` | Kakao REST API key / client secret |
| `NAVER_CLIENT_ID` / `NAVER_CLIENT_SECRET` | Naver application credentials |
| `VITE_GOOGLE_MAPS_API_KEY` | frontend build argument (optional) |
| `CORS_ALLOWED_ORIGINS` | `http://localhost:5173,http://127.0.0.1:5173` |
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

## 이번 최적화 및 경로 연동

[최적화 설명](docs/optimization.md)에 주문 SQL 배치, Redis 연결 재사용·전체 작업 시간 제한,
캐시 무효화, 비밀번호 계산 분리의 변경과 범위를 기록했습니다.

[임무 계획 연동](docs/missions.md)에 창문 좌표·외부 접근·정지 지점, 후보 경로 선택,
드론 검증기 실행과 교차 검증 API 사용법이 있습니다. 배송을 `dispatched`/`in_flight`로
바꾸려면 해당 경로의 유효한 드론 검증이 필요합니다. 기존 경로만 있는 배송은 먼저 새 임무
계획을 생성하고 그 `route_id`로 예약해야 합니다. 마이그레이션은 배포 시 별도로 적용하세요.
