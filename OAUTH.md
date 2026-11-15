# 카카오 · 네이버 · 구글 고객 로그인

고객 로그인 화면의 기존 세 소셜 버튼은 서버에서 설정 완료된 공급자만 활성화합니다. 인증 코드는 서버가 토큰으로 교환하며, 성공하면 기존 고객 세션 쿠키를 발급하고 홈 화면으로 돌아옵니다. 일반 비밀번호 로그인과 관리자 로그인은 유지됩니다.

## 환경변수

Docker Compose는 프로젝트 루트 `.env`를 사용합니다. Rust 서버를 직접 실행할 때는 실행 디렉터리의 `.env` 또는 프로세스 환경변수를 사용합니다. 루트와 `backend/.env.example`에 다음 항목이 있습니다. 실제 루트 `.env`에는 누락된 키만 빈 값으로 추가했으며 기존 값을 덮어쓰지 않았습니다.

현재 서버의 루트 `.env`에는 세 공급자별 `CLIENT_ID`, `CLIENT_SECRET`, `REDIRECT_URI` 입력란이 있습니다. 빈 운영 주소와 localhost 기본 콜백은 `https://neurovector-sim.forge-lang.org` 기준으로 채웠으며, 이미 입력된 인증정보와 별도 운영 주소는 보존했습니다. 각 공급자 콘솔에 해당 콜백 주소를 동일하게 등록하세요. 일반 `KAKAO_API_KEY`, `NAVER_API_KEY`, `GOOGLE_API_KEY` 항목은 OAuth 설정으로 읽지 않습니다. 실제 인증정보를 입력한 뒤 backend 컨테이너를 재생성해야 환경변수가 반영됩니다. 비밀키를 `VITE_` 환경변수나 브라우저 저장소에 넣지 마세요.

```dotenv
OAUTH_FRONTEND_URL=https://your-shop.example

# 카카오 CLIENT_ID = REST API 키. 시크릿 사용 설정 시 함께 입력.
KAKAO_CLIENT_ID=
KAKAO_CLIENT_SECRET=
KAKAO_REDIRECT_URI=https://your-shop.example/api/v1/auth/oauth/kakao/callback

NAVER_CLIENT_ID=
NAVER_CLIENT_SECRET=
NAVER_REDIRECT_URI=https://your-shop.example/api/v1/auth/oauth/naver/callback

GOOGLE_CLIENT_ID=
GOOGLE_CLIENT_SECRET=
GOOGLE_REDIRECT_URI=https://your-shop.example/api/v1/auth/oauth/google/callback
```

- `OAUTH_FRONTEND_URL`은 브라우저가 사용하는 사이트 원점입니다. 경로·쿼리·해시를 넣지 않습니다.
- 콜백은 공급자 콘솔에 **완전히 동일하게** 등록합니다. 프런트엔드와 같은 스킴·호스트를 사용하고 `/api`를 백엔드로 프록시합니다. localhost에서는 포트가 달라도 됩니다.
- 개발용 HTTP는 `localhost`, `127.0.0.1`, `[::1]`에만 허용합니다. 운영에는 HTTPS와 `ADMIN_COOKIE_SECURE=true`를 사용하세요. OAuth 쿠키는 HTTPS 원점에서 자동으로 Secure 처리됩니다.
- Google·Naver는 ID와 시크릿이 모두 필요합니다. Kakao는 REST API 키가 필요하며 시크릿 사용 설정을 켰다면 시크릿도 입력합니다.
- ID/시크릿이 모두 비어 있으면 해당 공급자는 비활성화됩니다. 콜백 예시만 있는 템플릿을 복사해도 일반 로그인은 작동합니다. 일부 자격증명만 넣으면 서버가 설정 오류로 기동을 거부합니다.
- 시크릿을 `VITE_` 변수나 프런트엔드 `.env`에 넣지 마세요. 브라우저에는 공급자 활성화 여부만 전달합니다.

기본 Compose 프런트엔드가 `http://localhost:5173`이면 `.env.example`의 콜백을 사용합니다. 학교 배포처럼 포트를 바꾼 경우 실제 공개 주소/포트로 네 URL을 바꾸세요. Vite 개발 서버는 `NV_BACKEND=http://localhost:8081`로 로컬 백엔드를 지정할 수 있습니다.

## 공급자 콘솔

1. **카카오**: 카카오 로그인을 활성화하고 REST API 키를 입력합니다. 위 Kakao 콜백을 Redirect URI로 등록합니다. 닉네임을 제공하려면 프로필 닉네임 동의 항목도 설정합니다. 이름이 없으면 기본 표시 이름을 사용합니다.
2. **네이버**: 네이버 로그인 API를 사용할 애플리케이션을 등록하고 서비스 URL과 Callback URL을 설정합니다. Client ID/Secret을 입력합니다. 이름/별명이 없더라도 안정적인 사용자 ID로 로그인합니다.
3. **구글**: OAuth 동의 화면을 설정하고 웹 애플리케이션 클라이언트를 만듭니다. 승인된 리디렉션 URI에 위 Google 콜백을 등록합니다. 테스트 모드면 테스트 사용자를 추가합니다. `openid profile` 범위와 S256 PKCE를 사용합니다.

공식 명세: [카카오 REST 로그인](https://developers.kakao.com/docs/en/kakaologin/rest-api), [네이버 인증](https://developers.naver.com/docs/login/api/api.md), [네이버 프로필](https://developers.naver.com/docs/login/profile/profile.md), [구글 웹 서버 OAuth](https://developers.google.com/identity/protocols/oauth2/web-server).

## 데이터와 보안

`0014_customer_oauth.sql` 마이그레이션이 필요합니다. 공급자와 공급자 사용자 ID의 조합으로 계정을 식별하며 이메일·이름·전화번호가 같다고 기존 계정에 자동 연결하지 않습니다. 내부 임의 비밀번호 자격증명은 사용자에게 노출되지 않습니다. 프로필 전화번호를 요구하거나 임의 전화번호를 만들지 않습니다. 연락처가 없는 신규 소셜 고객은 기존 주문 폼에서 연락처를 입력합니다. 실제 전화번호의 고유성은 유지됩니다.

`state`는 브라우저의 HttpOnly/SameSite=Lax 쿠키와 대조하며 DB에는 해시만 저장합니다. 공급자별로 바인딩되고 10분 후 만료되며 한 번만 소비됩니다. Google PKCE 검증자는 서버에만 저장합니다. 토큰은 브라우저나 localStorage에 보내지 않고 프로필 조회 후 보관하지 않습니다. 앱 로그아웃은 기존 앱 세션을 폐기하며 공급자 사이트의 로그인 세션까지 종료하지는 않습니다.

## 적용 및 확인

환경변수와 공급자 콘솔 설정을 채운 뒤 기존 배포 절차로 마이그레이션을 적용하고 backend/frontend를 빌드·배포합니다. 이 구현 작업은 실행 중인 서비스나 운영 DB 스키마를 변경하지 않습니다.

- `GET /api/v1/auth/oauth/providers`: 세 공급자의 `provider`, `enabled`만 반환합니다.
- `GET /api/v1/auth/oauth/{kakao|naver|google}/start`: 공급자 인증 페이지로 이동합니다.
- `GET /api/v1/auth/oauth/{provider}/callback`: 코드/상태를 검증하고 앱 로그인 세션을 발급합니다.
- 공급자 승인을 거절하면 로그인 화면에 취소 안내가 나타납니다. 상태가 만료되면 다시 시작할 수 있습니다.

실제 공급자 앱 키와 테스트 계정이 없으므로 공급자 사이트에서 승인하는 최종 로그인은 키 설정 후 확인해야 합니다. HTTP 공급자 경계 테스트와 격리 PostgreSQL 테스트로 코드 교환, 계정 식별, 세션, 상태 만료·재사용 방지를 검증합니다.
