# Google Calendar & Tasks setup

Second Brain uses **three OAuth scopes**:

| Scope | What it allows | Why |
|---|---|---|
| `calendar.freebusy` | See *when* you are busy in your calendars (no titles, no details) | Never schedule over your meetings |
| `calendar.app.created` | Create one secondary calendar and manage events **only in calendars this app created** | Work blocks live in a dedicated "Second Brain" calendar |
| `tasks` | Read and write Google Tasks | Your tasks on your phone and in the Calendar "Tasks" view |

For the calendar, Google itself prevents the app from reading or modifying your
meetings and personal events. Even a bug or a prompt injection cannot touch them.

Google offers no narrower Tasks scope, so `tasks` technically covers all your task
lists. The app only ever reads or writes the lists **it created** (ids stored in
the database): one list per project (e.g. "2nd brain") plus a default list
("Second Brain", `calendar.tasks_list_name`) for tasks without a project. Set
`calendar.tasks_sync = false` to turn it off.

## Optional: manage your own events (`--events`)

`sb calendar auth --events` additionally grants `calendar.events`: read and delete
events in **your own** calendars. It is off by default (`calendar.events_access`)
because it removes Google's guarantee above. Uses:

```sh
sb calendar events --from 2026-10-12 --to 2026-11-15 --match cours   # list only
sb calendar delete --from 2026-10-12 --to 2026-11-15 --match cours   # shows what would go
sb calendar delete ... --yes                                         # deletes, saves a backup
sb calendar restore data/backups/deleted-events-<time>.json          # recreate them
```

Blocked periods (no extra scope needed) are all-day events in the Second Brain
calendar that also keep work sessions off those days:

```sh
sb calendar block "Internship" --from 2026-10-12 --to 2026-11-15 --weekdays
sb calendar block "Vacation" --from 2026-10-10 --to 2026-10-25
```

Delete the event in Google Calendar to lift the block.

## Tasks sync (two-way)

| You do | Result |
|---|---|
| `sb task add "review exam notes +inbox"`, a `- [ ]` in the journal | appears in the Google list "inbox" |
| `sb task done/edit/cancel` | ticked / updated / removed on Google |
| tick, untick, rename or re-date it on your phone | applied locally at the next sync (logged, `sb undo`) |
| delete it on your phone | cancelled locally |
| add a task in one of these lists on your phone | imported into the matching project |

Sync runs after every `sb task …` and `sb ingest` that touched tasks, before
`sb tasks` / `sb today` (to pick up what you ticked on the phone), on
`sb calendar sync`, and every `autonomy.calendar_sync_minutes` in the daemon. Tasks
with a due date also show on that day in Google Calendar.

## Connect (one command)

```sh
sb calendar auth
```

The first time, a guided setup opens the right Google Cloud pages in your browser.
Google requires every app, including a personal one, to have its own OAuth client.
You do this **once** (~3 min):

1. Create a project (any name).
2. Enable the Google Calendar API **and the Google Tasks API**.
3. Consent screen: *Get started*, app name, your email, audience **External**.
4. *Audience → Add users*: add **your own Gmail address** as a test user.
5. *Clients → Create client → Desktop app → Download JSON*.

Drag the downloaded `client_secret_*.json` into the terminal and press Enter. It
is stored in `data/secrets/google_client.json` (mode 600). Your browser then opens
the Google sign-in page: click **Allow**. The token is stored in
`data/secrets/google_token.json` (mode 600, never logged), the dedicated calendar
is created, and your busy times are read. Done.

Already connected before Tasks support? Enable the Google Tasks API
(https://console.cloud.google.com/apis/library/tasks.googleapis.com) and run
`sb calendar auth` once more to grant it.

After that, the daemon and `sb plan --apply` refresh access silently. You only
redo `sb calendar auth` if access is revoked. Use `sb calendar auth --reset` to
change the OAuth client.

> While the consent screen is in **Testing** status, Google expires refresh tokens
> after 7 days. To avoid re-running `sb calendar auth` weekly, click *Publish app*
> on the *Audience* page. For your own unverified app, Google then shows an
> "unverified app" warning during sign-in that you can accept (*Advanced → Go to …*).

Advanced: instead of the wizard you can set `calendar.client_id` in `config.toml`
and `SB_GOOGLE_CLIENT_SECRET` in the environment. To pause calendar use, set
`calendar.enabled = false`.

### Why not just a "Sign in with Google" link, or an MCP server?

- A one-click link only works if the *developer* registers the app with Google, and
  for public use passes Google's verification for Calendar scopes. `sb` is a personal
  tool with no central server, so each user's own client is the reliable option. It
  also means nobody else's project holds a token to your calendar.
- A Calendar MCP server or connector gives a *model* access to your calendar during
  a chat. `sb` deliberately keeps the model away from calendar writes: the Rust
  planner computes times and writes them through narrow OAuth scopes, with no
  tokens spent per calendar operation. An MCP route would cost tokens for every sync
  and usually grants broader access to all your events.

## Use

```sh
sb calendar sync          # pull busy time + your edits of agent events (no writes)
sb plan                   # dry-run: proposed work blocks
sb plan --apply           # store sessions and create events in "Second Brain"
```

Autonomy is controlled by `[autonomy] calendar_writes`:

- `dry-run` (default): the daemon syncs busy time and *proposes* plan changes (one
  notification per distinct proposal). You apply with `sb plan --apply`.
- `auto`: the daemon applies plan changes and pushes them to the dedicated calendar.

## How changes are handled

| You do this in Google Calendar | Second Brain does |
|---|---|
| Add a meeting overlapping a work block | next sync detects new busy time → cancels just that block and re-places the work |
| Delete a work block | session cancelled; that time is remembered as declined and not reused |
| Move a work block | the session follows your move |
| Nothing | nothing (sync is incremental: free/busy fingerprint + events sync token) |

Every change is in `sb log` and plan applications are undoable with `sb undo <id>`.

## Troubleshooting

- `token refresh failed (400): invalid_grant` → token expired/revoked: `sb calendar auth`.
- `port 8765 busy` → change `redirect_port`.
- Network down → sync errors are logged and retried; planning keeps working from
  the last known busy times.
- To disconnect: delete `data/secrets/google_token.json` and remove the app at
  <https://myaccount.google.com/permissions>.
