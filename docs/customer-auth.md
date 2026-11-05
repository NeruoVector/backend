# Customer account API

All endpoints use `/api/v1`. Account sessions are server-side PostgreSQL records with SHA-256 hashed random tokens and a seven-day expiry. The browser receives only a separate `nv_customer_session` HttpOnly, SameSite=Strict cookie, with Secure enabled by the production cookie setting. No SMS verification or OAuth identity is asserted.

- `POST /auth/register`: `{ "username": "test_user", "password": "eight-or-more", "name": "고객", "phone": "01012345678" }`; 201 and `{id,username,name,phone}`. Usernames normalize to lowercase. Duplicate username or normalized phone returns 409. Registration creates a session.
- `POST /auth/login`: `{username,password}`; 200 and the same customer object. Incorrect credentials return 401.
- `GET /auth/me`: customer object or 401.
- `POST /auth/logout`: 204, revokes the token and clears its cookie.
- `GET /auth/orders`: `{items,total}`, latest 100 customer-owned orders. Each row includes its persistent four-digit `pickup_code`.
- `GET /auth/orders/{id}`: regular order detail, strict ownership. Other owners and legacy guest orders return 404.
- `GET /auth/reviews`: `{items,total,average_rating}`, latest 100 reviews of customer-owned orders.

Use same-origin requests and cookies. Mutations reject an Origin header belonging to a different host. Authenticated `POST /orders` assigns ownership from the session, never from a supplied phone. Public order lists contain only legacy guest orders. Customer-owned order details, payment routes and per-order review routes require the owning session; public store reviews/photos remain public. The asynchronous queue currently accepts guest orders only; account orders use `POST /orders`.

Order detail includes `pickup_code`, exactly four numeric digits including leading zeroes, described in the UI as the pickup confirmation password. New order numbers end in that code while retaining a random unique prefix. Existing order numbers remain unchanged and receive a randomly generated, persisted pickup code. Migration 0011 repairs the initial legacy backfill once, tracked by a transactional migration marker. Neither backfill derives a code from a public order ID; repeated migrations preserve existing codes, and the repair excludes new-format order numbers. This code is unrelated to an account password and is not a login credential.
