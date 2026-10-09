# hidane.dev

Static site for https://hidane.dev, served from Cloudflare Workers (static assets plus a tiny Worker
for the `www` redirect and `install.sh`). Tooling runs on [bun](https://bun.sh).

```sh
bun install          # installs wrangler
bun run dev          # http://localhost:8787
bun run check        # wrangler deploy --dry-run
bunx wrangler login  # once per machine
bun run deploy       # creates the custom-domain DNS records on first deploy
```

- `public/` — the site. `index.html` (English), `ja/index.html`, `404.html`, `install.sh` (placeholder that exits 1 until the first release), `og.png`, `favicon.svg`, `_headers`.
- `src/index.js` — the Worker.
- `wrangler.jsonc` — custom domains `hidane.dev` and `www.hidane.dev`.

Design source: the hidane.dev canvas (dark "sumi + ember", light "kinari + charcoal", IBM Plex Sans JP / IBM Plex Mono).
