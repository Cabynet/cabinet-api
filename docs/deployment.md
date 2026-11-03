# Deployment

`.github/workflows/deploy.yml` runs on every push to `main`: tests (`scripts/test.sh`), builds the image, pushes it to `ghcr.io/cabynet/cabinet-api`, copies `docker/compose.production.yml` and `docker/Caddyfile` to the VPS and restarts the `api` service. Pull requests only run the tests.

## GitHub settings (Settings > Secrets and variables > Actions)

| Name | Kind | Value |
|---|---|---|
| `SSH_HOST`, `SSH_PORT`, `SSH_USER` | secret | VPS address and login |
| `SSH_PRIVATE_KEY` | secret | key authorised on the VPS |
| `DEPLOY_PATH` | variable | directory on the VPS that holds `docker/` |

Nothing about the server is committed to this repository.

## First deployment on the VPS

Needs Docker Compose and Python 3. Copy `docker/init-env.py` to `$DEPLOY_PATH/docker/` and run it there:

```sh
python3 docker/init-env.py   # creates docker/.env (mode 600), refuses to overwrite
```

Then add to `docker/.env`: `R2_ENDPOINT`, `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY`, `R2_BUCKET_NAME`, `API_URL` (https), `FRONTEND_URL`, `API_DOMAIN`, `COOKIE_DOMAIN` (parent domain of web and API), and the OAuth client ids/secrets if used. `PROXY_SIGNING_KEY` is generated for you; the API refuses to start over https without it. Back it up: changing it invalidates cached signed image URLs.

Start everything once by hand, then let CI handle updates:

```sh
docker compose --env-file docker/.env -f docker/compose.production.yml --profile https up -d --wait
```

The API applies its SQL migrations when it starts. Never run `docker compose down -v`; it deletes the database volume.

## Checks

- Health: `curl http://127.0.0.1:4000/v1/health` on the VPS.
- Rollback: set `IMAGE_TAG` in `docker/.env` to an earlier short SHA and run `docker compose ... up -d --wait api`.
