# dsh-phone-api

The DeepSeek Harness side of Kallisti's DSH transport: a host-plane plugin that
exposes a small HTTP + SSE surface (`/phone/v1/*`) for the app.

## Install

1. Copy this directory somewhere stable, e.g. `~/.dsh/plugins/dsh-phone-api`.
2. Symlink it into your web profile:
   `ln -s ~/.dsh/plugins/dsh-phone-api ~/.dsh/profiles/web/node_modules/dsh-phone-api`
3. Add it to `~/.dsh/profiles/web/cordis.patch.yml`:

   ```yaml
   - insert:
       - id: dsh-phone-api
         name: 'dsh-phone-api'
   ```

4. Set `KALLISTI_DSH_TOKEN` in the environment DSH starts with, then restart DSH.
   With no token set, every request returns 401.
5. Put DSH behind TLS at `/dsh` (tailnet or reverse proxy).

Check it: `curl -H "Authorization: Bearer $KALLISTI_DSH_TOKEN" https://<host>/dsh/phone/v1/health`

## Notes

- Each route is registered once. DSH's web server throws on a duplicate route,
  which leaves the plugin half-mounted.
- `config` edits the web profile patch; DSH's HMR watcher applies it live.
  Saves are validated and the previous file is backed up (last 20).
- `yaml` is resolved through the running DSH install, so there is nothing
  extra to `npm install`.
