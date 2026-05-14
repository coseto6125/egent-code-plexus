# Docker Compose fixture

Three services: `web` (built from `./app`), `db` (postgres:15 image), `redis` (redis:7-alpine image).

Expected symbols:
- Class nodes: `web`, `db`, `redis`
- Imports: `./app` (build), `postgres:15` (image), `redis:7-alpine` (image)
- Const nodes: `APP_ENV`, `SECRET_KEY`, `POSTGRES_DB`, `POSTGRES_USER`, `POSTGRES_PASSWORD`
- Call edges: `web → db`, `web → redis` (via depends_on)
