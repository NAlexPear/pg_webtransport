# `pg_webtransport`

## Postgres over `WebTransport` from the Browser

TODO:
- [x] create a test page/script to run WebTransport in the browser
- [x] handle Postgres connection startup sequence messages
- [ ] run connection startup sequence on the proxy instead of the client
- [ ] package the Client into an interface that passes messages between JS and WASM efficiently
- [ ] demo the secure version of this (NGINX + kratos cookie/session auth + X-Postgres-User header)
