# `pg_webtransport`

## Talk Postgres over WebTransport


TODO:
- [x] create a test page/script to run WebTransport in the browser
- [] encode and decode Postgres messages using a protocol library (pg-protocol? or rust-postgres-types?)
- [] package the client into a nice interface (perhaps a Connection that could be pooled)
- [] come up with a more concrete auth example (NGINX + kratos cookie/session auth + X-Postgres-User header)
