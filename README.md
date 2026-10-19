# NeuroVector Backend

NeuroVector API 서버입니다.

- **Rust 2024 + axum 0.8**
- PostgreSQL(pgvector) · Redis · Kafka 연동
- 장애물 감지 이벤트 저장 및 Kafka 발행

## 요구 사항

- Rust 1.85+ (`rustup` 권장)
- platform 레포의 Docker 스택 (PostgreSQL/Redis/Kafka)

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

## API

| 메서드 | 경로 | 설명 |
| --- | --- | --- |
| `GET` | `/` | 서비스 정보 |
| `GET` | `/health` | 라이브니스 |
| `GET` | `/health/ready` | PostgreSQL/Redis 준비 상태 |
| `GET` | `/api/v1/detections?limit=50` | 감지 목록 (최신순, 최대 200) |
| `POST` | `/api/v1/detections` | 감지 1건 저장 + Kafka 발행 |
| `GET` | `/api/v1/detections/{id}` | 감지 1건 조회 |

### POST 예시

```bash
curl -X POST http://localhost:8081/api/v1/detections \
  -H 'Content-Type: application/json' \
  -d '{
    "drone_id": "drone-001",
    "label": "tree",
    "confidence": 0.92,
    "latitude": 37.5665,
    "longitude": 126.9780
  }'
```

## 데이터베이스

`migrations/0001_create_detections.sql` 을 적용하세요.

```bash
psql "$DATABASE_URL" -f migrations/0001_create_detections.sql
```

## 환경 변수

`.env.example` 참고. 주요 값:

| 변수 | 기본값 |
| --- | --- |
| `LISTEN_ADDR` | `0.0.0.0:8081` |
| `DATABASE_URL` | `postgres://neurovector:neurovector@localhost:5432/neurovector` |
| `REDIS_URL` | `redis://:neurovector@localhost:6379/0` |
| `KAFKA_BROKERS` | `localhost:9092` |
| `KAFKA_TOPIC_EVENTS` | `neurovector.events` |

## 개발

```bash
cargo build
cargo clippy --all-targets
cargo fmt
```
