# Contributing

Thanks for helping bring the online modes of Mario Strikers Charged back. This
repository is the server. Problems with the game itself, the launcher or
controllers belong in
[Recharged's issues](https://github.com/previouslevelgames/recharged/issues).

## Reporting a server problem

Open an [issue](https://github.com/previouslevelgames/recharged-wfc-srv/issues)
with:

- **The version**: the admin dashboard shows it, `/status` the revision.
- **How the server runs**: Unraid or Docker Compose, one server or several on
  one database, relay on or off.
- **What happened**: when (UTC), what the players did, and the friend codes
  involved.
- **The log of that day**: from the dashboard's **Logs** page or the logs
  folder. For protocol problems, `RUST_LOG=info,raw=trace` adds raw dumps.

Logs contain players' IP addresses and friend codes: remove what the problem
does not need. Never post `.env` files, passwords, OAuth secrets or tokens.

## Ideas

Ideas are welcome as issues: say what they would do for players or for whoever
runs a server. Before you start on a larger change, open an issue first so we
can agree on the design. Help with the admin dashboard's frontend and design is
especially welcome.

## Pull requests

We review every pull request with care and expect the same care in return.

- Open it against `main`; `stable` only takes release pull requests.
- One change per pull request, with what it does, why, and how you tested it.
- `just check` passes: formatting, clippy and the tests.
- A change to what the server answers the game has been tried against the real
  game, not only in tests.
- Commits follow the repository's style, e.g. `fix(relay): ...`.

### AI tools

Using AI tools is fine; we use them too (see the
[disclaimer](README.md#disclaimer-ai-assistance)). What counts is that you own
the result: you have read and understood every line, you can explain it, and
you have tested it.

We reject pull requests built on carefree vibe coding: code that was obviously
not read or checked, changes nobody tested, unrelated edits, invented protocol
behavior, or descriptions that do not match the code. We close them without a
detailed review. If the idea behind one is good, we may implement it ourselves.
