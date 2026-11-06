# Nodes: several servers as one pool

**A plan,** being built. What exists is several servers sharing one database
([install.md](install.md#several-servers-on-one-database)): they share every
player, profile, score and match, but each server knows only its own players
online, so all of the game's traffic goes to one of them. Built of phase 1 so
far: the nodes' names and leases (with their players and games), the NAS
login tokens' shared key, the GPCM logins' rows, by which the HTTP of any
node finds a game's login on another, and the RANKED pairings.

The goal, with two or more server containers, the **nodes**:

- **One pool.** Every node takes players, and players on different nodes see
  each other online, as friends and in matchmaking, and play each other.
- **Updates nobody notices.** One node is drained, updated and taken back, then
  the next one.
- **A node may die.** The others carry on as the pool without it. Its players
  are dropped, log in again and land on another node.

It is not about a database that survives on its own: the database stays one
container, and without it nothing works, as today. Nor about nodes far apart:
the nodes are assumed close to each other (one host, or one network), as
packets between them are on a match's path when the relay carries it.

## The pieces

```
 players ──▶ front server ──── one tunnel per node ────▶ node 1  ─┐
            (binding by IP,                              node 2  ─┼─▶ database
             agent)          ◀── desired state, health ──  …     ─┘   (shared state,
                                                                     leases, messages)
 browsers, launcher ──▶ Cloudflare ──▶ HTTP proxy ──▶ any node
 owners ──▶ /admin → Nodes: drain, enable, off
```

- **Live state** is in each node's memory, as today, and what other nodes need
  of it is in the database: each node keeps its own rows, under a lease.
- **Traffic** is bound to nodes on the front server, in the kernel, by a small
  agent.
- **Control** is a page of `/admin`, **Nodes**; the agent reads its decisions
  from the nodes.

## The front: binding players to nodes

The master server and NAT negotiation need every game's real address and port
(they go into the QR2 challenge, the server browser's lists and the CONNECTs).
A proxy, which opens its own connections to the node, hides them. So the front
forwards the game's ports by **destination NAT without source NAT**, as it does
today for one node, and each node is reached through **its own tunnel**, whose
other end sends the node's replies back the same way.

**Binding by address.** The front keeps one set of player addresses per node,
each entry with a timeout that every packet from it renews (nftables dynamic
sets). A packet from an address in a node's set goes to that node; a new
address goes to an active node, taking turns, and joins its set. Every game
port of one address goes to the same node, HTTP 80 included, so a game's NAS
login, GPCM connection, heartbeats and negotiations usually meet on one node.
Not always: some networks send a game's HTTP out by another address than its
GPCM connection (the NAS address of [login.md](login.md#nas-post-ac)), and
carrier-grade NAT can do the same with UDP. The nodes never count on it.

**Flows.** NAT is decided at a flow's first packet; the flow then stays where
it went (connection tracking), whatever the sets say later. A TCP connection
stays on its node until it closes. A UDP flow moves to another node when the
agent deletes its tracking entry: the next packet is a new flow and is bound
again.

**Who sends to a game.** A packet to a game has to leave the front from the
address and port the game's own flow came in to: anything else is dropped by
the game's router, or by the front, whose tracking already holds that pair for
the flow's node. So **only the node of a game's flow sends to that game.**
When another node has something for it, a CONNECT, a client message, a relayed
packet, it asks that node to send it.

**The agent** on the front server:

- Checks every node's `/status` through its tunnel every 2 seconds, by itself,
  without the database. Three misses in a row and the node is **down**.
- Applies each node's state: **active** (gets new addresses), **draining**
  (keeps its flows, gets no new addresses, its addresses are bound again at
  their next new flow), **off** or **down** (its addresses are released and its
  UDP flows deleted, so they move at their next packet).
- Takes a node back only when told (**enable**): a node that restarts by itself
  doesn't get traffic back by itself.
- Reads the desired states from any healthy node (`GET
  /admin/api/nodes/front`, with a token), and reports back the addresses and
  flows bound to each node. Unable to reach any, it keeps the last states it
  had. It never takes the last node out of the pool.

**HTTP behind Cloudflare** (`/admin`, `/sign-in`, `/api/v1`): the tunnel ends
at an HTTP proxy, which sends each request to any healthy node. A proxy is fine
there: the real address comes in `CF-Connecting-IP`, which the server reads
already. This needs phase 1 below, as `/api/v1` asks after GPCM logins.

## Nodes and leases

- **Name.** `NODE_NAME` names a node (default: its host name). `/status`,
  `/admin`, the logs and the minute counts of players online
  (`online_samples.server`, there since 1.2.14) show it.
- **Lease.** At start a node draws an incarnation id and writes a row in
  `nodes`: id, name, revision, internal address, start, heartbeat, counters
  (GPCM connections, players by status, QR2 sessions, negotiations, relays).
  Built so far: everything but the internal address, with two counters:
  players logged in to GPCM and QR2 sessions.
  It renews the heartbeat every 5 seconds. A node whose heartbeat is older than
  15 seconds is dead: the others ignore its rows and remove them. A node that
  stops normally removes its rows itself. A node that restarts has a new id, so
  nothing of its earlier run counts as its own.
- **Desired state** is per name, in `node_states`, and outlives restarts.
- **Messages between nodes** go through PostgreSQL `LISTEN/NOTIFY`, one
  channel per incarnation: small JSON, each with a version, well under the
  8000-byte limit. A request to another node has a short timeout and is
  dropped when that node's lease is gone: no node ever waits on a dead one.
  A node ignores messages it doesn't understand.
- **Relayed packets** are the exception: too many and too urgent for the
  database, they go as UDP straight to the other node's internal address.
- **Jobs.** Periodic work that may run twice (cleanups) runs on every node.
  Work that must run once takes a leader by `pg_try_advisory_lock`: the lock
  dies with its node's connection, and another node takes it over.
- **A pool of one** is no special case. A node alone finds nothing of other
  nodes and behaves as today, with no mode to switch.

## What is in memory today, and what it needs

| State | Today | Shared how | Phase |
|---|---|---|---|
| NAS login tokens | `nas/ticket.rs`: signed, nothing kept but the key | One key for all nodes, kept in the database (`shared_keys`); tokens then also survive a restart | 1, built |
| gamestats2 tokens | `gamestats/token.rs` | A table with their expiry; the per-address limit counts in it | 1 |
| `/admin` sessions, armed merges and deletes | `admin/session.rs`, `admin/maintenance.rs` | Tables | 1 |
| Sign-ins waiting for their browser | `profiles/sign_in.rs` | A table | 1 |
| Rate-limit windows | `profiles/mii_sign_in.rs`, `reports/errors.rs` | Stay per node: a limit holds per node | 1 |
| RANKED pairings | `pairing.rs` | `pairings`: each ready game by set and side, with its time by the database's clock (`READY_TTL`) and whether another game of its player replaced it. One ready call of a set at a time (an advisory lock); every node's server browser lists by them | 1, built |
| Matchmaking activity for `/admin` | `activity.rs` | A table, the latest 1000 events | 1 |
| GPCM logins, for the HTTP side | `gpcm/mod.rs`: the sessions | `logins` (`gpcm/logins.rs`): a row per login and node with the connection, addresses, the login ticket's hash, DWC status and since when, matchmaking, room, friend counts and `off_boards`. Each node writes its rows after each change, a login's before GPCM answers it; the rows go with its lease. Lookups (a login ticket, a request's address, presence, off the boards) take the node's own sessions first, then the rows of the other live nodes. A node without a lease writes and reads none | 1, built |
| Players online now, Overview | each node counts its own | Overview, Players and Monitoring's figure of now count the pool's logins (the rows); the minute samples and the lease's counters stay each node's own, and Monitoring adds the samples up | 1, built |
| GPCM between players | `gpcm/mod.rs`: mutual friends, statuses, `\bm\` messages (match commands), the offline notice, a new login ending the old one, `/admin` disconnecting a banned or deleted player | A message to the node of the other login, which delivers it on its connection | 3 |
| QR2 sessions (rooms) | `master.rs` | A row per game: address, node, instance key, fields, listed, last heard; written when its fields change, its time at most every few seconds. The server browser lists them all; NAT negotiation finds game sockets in them (`heard_at`) | 3 |
| Client messages to a host | `master.rs`: sent and retried until acked | Sent by the node of the host's session; another node asks it to and gets back whether it was acked | 3 |
| NAT negotiations | `natneg.rs`: by cookie, and the pairs that failed | Each INIT in a table by cookie and client index; the node that completes a pair decides, relay or not, and each game's node sends its CONNECTs and takes its acks. The failed pairs in a table | 3 |
| Relayed matches | `relay.rs` | The match is kept by the node that decided it. A packet from a game on another node is passed to the node of its partner, which sends it on from its own socket | 3 |

## Draining a node

1. **Drain** on the Nodes page. The front sends it no new addresses; its
   addresses go to other nodes at their next new flow; flows under way stay.
2. With phase 3, the agent then moves the node's UDP flows at once: the other
   nodes know its rooms, negotiations and relays, so the games carry on within
   a packet. Only GPCM, one TCP connection per player, has to end.
3. A GPCM connection lasts as long as its player is online, often for hours.
   After a grace period, the node closes, a few at a time, the connections of
   players neither in matchmaking nor in a match (DWC status other than 2 to
   6), and those of the others when their match ends. It closes them with an
   error the port knows as "moved": the port logs in again without showing it,
   and lands on another node. Older builds show a network error, and the
   player goes online again.
4. **Drained**: no GPCM connections, QR2 sessions or relays left. The page says
   the node can be updated.
5. Update the container. It starts with the new revision, which the page shows.
   **Enable** it, watch it on the Nodes page and in Monitoring, then do the
   next node.

## Two versions at once

During an update, the release before and the new one run together, on one
database and in one pool:

- **Migrations never break the release before.** They add tables, columns and
  indexes; renaming or dropping waits for a later release, once no node uses
  the old shape. (1.2.14's new primary key of `online_samples` would not pass:
  a node still on 1.2.13 could no longer write its counts.)
- **Messages between nodes** stay readable by the release before. A node
  ignores what it doesn't know.
- **An older node may start on a newer database.** sqlx refuses this today
  (`migration … was previously applied but is missing`). In a pool it must be
  allowed, or a node that restarts mid-update would stay down; the rule above
  keeps it safe.
- **Automatic updates are off for the nodes.** An update is drain, update,
  enable, one node at a time.

## A node that dies

When a node stops without warning, a crash, a container switched off:

- After 6 to 10 seconds the agent finds it down. New addresses and its own go
  to the other nodes; its UDP flows are deleted and move at their next packet.
- After 15 seconds its lease runs out. The other nodes drop its logins, rooms,
  negotiations and relays; its players look offline to their friends until
  they log in again. Its leader locks have gone with its connection, and
  others take its jobs.
- Its players' GPCM connections are gone: their games show an error, and they
  log in again on another node. A match runs between the two games, not
  through the server (unless the relay carries it); whether the game plays on
  once its GPCM connection is gone is to be tried.
- When it comes back, it has a new lease and stays out of the pool until
  someone enables it.

## `/admin` → Nodes

For each node: name, revision, desired and actual state, heartbeat age, GPCM
connections and players by status, QR2 sessions, negotiations, relays, the
addresses and flows the front has bound to it, and while draining, what is
left ("12 players, 3 in a match"). Owners get **Drain**, **Enable** and
**Off**. Monitoring's timeline can show each node's line. The agent's API is
there too: the desired states, and the bindings it reports.

## Phases

Each phase is useful by itself, and nothing of it is thrown away by the next.

1. **HTTP on every node.** Names and leases, the shared state of phase 1 in the
   table above, the summed counters, and a Nodes page that shows. Then
   Cloudflare's HTTP can go to every node, while all game traffic still goes to
   one. Done when the profile service's tests pass with HTTP on one node and
   the game's GPCM login on another: sign-in, binding a WFC profile, RANKED
   ready, the player and friend lists.
2. **The front.** A tunnel per node, the binding, the agent with its health
   checks, the HTTP proxy. All new addresses still go to one node; the agent
   can switch them all to another, which already makes an update one
   reconnect, without waiting for the container.
3. **The game across nodes.** GPCM between players, the QR2 sessions, client
   messages, NAT negotiation and the relay, and the messages between nodes.
   Done when two games on different nodes are friends, play a friend battle, a
   random match, a RANKED set and a relayed match, and when a node switched
   off mid-session leaves the other one working.
4. **Unnoticed updates.** The drain with its grace period, the port's silent
   login after "moved", the Nodes page's buttons, and the update routine.

## Testing

- **Unit tests** start two `App`s, two nodes, in one process on one test
  database, and check each shared piece across them.
- **The container test** (`scripts/test-container.py`) runs two nodes on one
  database, its games on both.
- **Live**, on the test rig: two windows of the port, each bound to another
  local node, for friends, friend battles, negotiations and the relay, and a
  node stopped mid-match.

## Open questions

- In the port: which of DWC's states allow logging in again without the game
  noticing, and which error says "moved".
- Whether the relay's limit (`NATNEG_RELAY_MAX`) holds per node or for the
  pool.
- How long a binding lasts without packets: long enough for a game in its
  menus, short enough for addresses shared by many players to spread.
- Whether QR2 sessions written to the database stay cheap with many games
  (expected: yes, at a few writes per game a minute).
