# The roster page

`index.html` is `Ziglax/nocturnal-roster`'s page, unchanged except for one
constant: `SCRIPT_URL` now points at `/roster/data.json` on this host instead
of a Google Apps Script. The bot writes that file from the ledger on boot and
after every `/roster` command (`roster.output_path` in `nocturnal.yaml`), in
exactly the shape the Apps Script produced: `values`, `notes`, `links`, a
`styleDict` and per-cell `styleIndex`, `headerHeights`.

The look is `deploy/roster-theme.json` — the sheet's style dictionary and
which style each row kind and column used, captured once from the live sheet
on 2026-08-31. It is compiled into the binary. Re-theme by editing it.

Caddy serves the directory:

    handle_path /roster/* {
        root * /var/www/roster
        file_server
    }

The guild site itself (`deploy/site/index.html`) is served at `/`, with Perses under `/perses/`; its data is `/data/*` behind the Perses login.

`/var/www/roster` is owned `nocturnal:caddy`, mode 0750: the bot writes,
Caddy reads, nobody else. The page is public, as the Google-hosted one was.
Putting it behind the Discord login is a separate step: Discord is not an
OIDC provider, so oauth2-proxy cannot front it as-is; caddy-security or a
small session service in the bot are the two honest options.
