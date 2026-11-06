# 사용자 기능 API

기본 경로는 `/api/v1`입니다. Swagger UI(`/swagger-ui`)에 요청/응답 스키마와 인증 방식이 등록되어 있습니다.

| 기능 | 메서드 / 경로 | 동작 |
| --- | --- | --- |
| 사용자 등록 | `POST /users` | 아이디/비밀번호 저장, 사용자 세션 발급 |
| 로그인 | `POST /users/session` | 비밀번호 확인 후 세션 발급 |
| 현재 세션 | `GET /users/session` | 사용자 ID, 아이디, 만료 시각 |
| 로그아웃 | `DELETE /users/session` | 현재 세션 폐기, 쿠키 삭제 |
| 학교 검색 | `GET /schools?q=&region=&limit=&offset=` | 학교 이름/코드 부분 검색, 지역 정확 일치 |
| 학교 코드 조회 | `GET /schools/{code}` | 코드로 학교 조회, 미등록 코드는 404 |
| 학교 등록/수정 | `PUT /admin/schools/{code}` | 관리자 전용 학교 목록 관리 |
| 상품 리뷰 조회 | `GET /reviews?item_id={uuid}&limit=&offset=` | 해당 상품 리뷰, 최신순 |
| 주문 리뷰 조회 | `GET /reviews?phone=&limit=` | `item_id` 생략 시 주문 리뷰 `{items,total,average_rating}` 응답 |
| 리뷰 작성 | `POST /reviews` | 사용자 로그인 필요, 사용자/상품당 1개 |
| 리뷰 수정/삭제 | `PUT /reviews/{id}`, `DELETE /reviews/{id}` | 작성자만 가능, 타인 리뷰는 404 |
| 알림 발송 | `POST /admin/notifications` | 관리자 전용, 지정 사용자 알림함에 저장 |
| 알림 조회 | `GET /notifications?limit=&offset=` | 로그인한 사용자 본인 알림만 조회 |
| 읽음 처리 | `POST /notifications/{id}/read` | 본인 알림만 가능, 최초 읽은 시각 유지 |
| 휴대폰 인증 요청 | `POST /phone-verifications` | 형식만 구현, 501 |
| 휴대폰 인증 확인 | `POST /phone-verifications/confirm` | 형식만 구현, 501 |
| 결제수단 저장 | `POST /payment-methods` | 사용자 로그인 필요, 형식만 구현, 501 |

## 적용 및 인증

주문 리뷰(0009), 고객 인증·카드·수령 확인·OAuth(0010~0014) 마이그레이션 이후 `migrations/0015_customer_apis.sql`을 적용합니다. Compose의 migrate 서비스는 이 파일도 순서대로 적용합니다.

사용자 등록/로그인은 `{"username":"student01","password":"a-long-password"}`를 받습니다. 등록 시 username은 앞뒤 공백 제거 후 3~80자, 비밀번호는 8~128자입니다. 비밀번호는 기존 관리자 인증과 같은 동시 실행 제한을 사용하는 Argon2로 해시합니다.

사용자 쿠키는 `nv_user_session`이며 관리자 쿠키와 분리됩니다. HttpOnly, SameSite=Strict, Path=/, 유효 기간 24시간이며 DB에는 토큰의 SHA-256 해시만 저장합니다. 기존 `ADMIN_COOKIE_SECURE=true` 설정은 사용자 쿠키에도 적용됩니다. 만료된 세션은 인증되지 않습니다. 만료 레코드는 필요에 따라 `DELETE FROM neurovector.user_sessions WHERE expires_at <= now()`로 정리할 수 있습니다.

브라우저에서 쿠키를 사용하는 요청은 `credentials: 'include'`를 설정합니다. 관리자 쿠키로 사용자 API를 인증하거나 사용자 쿠키로 관리자 API를 인증할 수 없습니다.

## 요청 예시

```bash
# 사용자 등록 및 쿠키 저장
curl -c user-cookies.txt -X POST http://localhost:8081/api/v1/users \
  -H 'Content-Type: application/json' \
  -d '{"username":"student01","password":"a-long-password"}'

# 세션 조회
curl -b user-cookies.txt http://localhost:8081/api/v1/users/session

# 리뷰 작성: item_id는 기존 inventory_items의 UUID
curl -b user-cookies.txt -X POST http://localhost:8081/api/v1/reviews \
  -H 'Content-Type: application/json' \
  -d '{"item_id":"<상품 UUID>","rating":5,"content":"배송이 좋았습니다."}'

# 학교 등록: 기존 관리자 로그인으로 발급한 쿠키 사용
curl -b admin-cookies.txt -X PUT http://localhost:8081/api/v1/admin/schools/LOCAL-001 \
  -H 'Content-Type: application/json' \
  -d '{"name":"학교 이름","region":"서울"}'

# 학교 검색
curl -G http://localhost:8081/api/v1/schools --data-urlencode 'q=학교'

# 알림 발송
curl -b admin-cookies.txt -X POST http://localhost:8081/api/v1/admin/notifications \
  -H 'Content-Type: application/json' \
  -d '{"user_id":"<사용자 UUID>","title":"배송 안내","body":"배송이 완료되었습니다."}'
```

학교 목록은 초기에는 비어 있으며 관리자 API로 등록한 데이터를 조회합니다. 외부 학교 코드 서비스와 동기화하거나 공식 코드 데이터를 기본 제공하지 않습니다. 알림은 서비스 내 알림함으로 발송되며 SMS/이메일/모바일 푸시 연동은 포함하지 않습니다. 리뷰는 상품 기준이며 구매 인증은 수행하지 않습니다.

목록의 기본 limit은 50, 최대 200이고 offset은 0 이상입니다. 리뷰 평점은 1~5, 내용은 공백 제거 후 1~2000자입니다. 알림 제목은 1~100자, 본문은 1~4000자입니다.

## 형식만 구현한 API

휴대폰 인증 요청은 `{"phone":"01012345678"}`, 확인은 `{"verification_id":"<UUID>","code":"123456"}` 형식입니다. 결제수단 저장은 `{"provider":"PG 이름","billing_token":"PG 발급 토큰"}` 형식이며 카드번호/CVC 필드는 받지 않습니다.

유효한 형식으로 호출하면 HTTP 501과 `{"error":"not_implemented","feature":"...","message":"..."}`를 반환합니다. 실제 문자 발송, 인증 확인, 결제 토큰 저장은 수행하지 않습니다.

## 검증

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt -- --check

# 별도의 빈 PostgreSQL DB에서 HTTP/DB 통합 시나리오 실행
TEST_CUSTOMER_DATABASE_URL=postgres://user:password@localhost:5432/isolated_test \
  cargo test customer_apis_enforce -- --ignored --nocapture
```

통합 테스트는 infra의 확장/스키마 초기화와 필요한 마이그레이션을 실행한 뒤 임시 포트의 실제 HTTP 서버를 구동합니다. 해당 DB에 테스트 데이터를 남기므로 운영 DB를 사용하지 마세요. 사용자 등록/로그인/만료/로그아웃, 관리자와 사용자 권한 분리, 리뷰 저장/중복/수정/삭제, 타인 데이터 접근 차단, 학교 검색, 알림 발송/읽음, 미구현 API의 501 응답을 확인합니다.

상품 리뷰는 `nv_user_session`으로 작성·수정·삭제하며 기존 작성자 권한을 유지합니다. 주문 리뷰는 별도의 `nv_customer_session` 및 주문 소유권 검사를 유지합니다. `item_id`가 있으면 상품 리뷰 조회가 우선하며, 잘못된 UUID는 조회 오류로 처리합니다.
