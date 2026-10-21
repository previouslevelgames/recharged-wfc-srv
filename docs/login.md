# Login: NAS registration and login, then GPCM

What the port's client does to go online, and what the server answers. Traced
from the port's `online` branch (`ab2a9494`) and the prepared decomp tree (`P/`
below: `build/release/prepared/mscharged-decomp/patched/`). WiiLink's
[wfc-server](https://github.com/WiiLink24/wfc-server) (`f0825bf`) is the
reference for the server's side.

| File | Role |
|---|---|
| `P/src/RVL_SDK/dwc/dwc_login.c` | the login sequence, first login of a profile |
| `P/src/RVL_SDK/dwc/dwc_auth_interface.c` | NAS requests and replies, `DWC_AUTHDATA` |
| `P/src/RVL_SDK/gamespy/GP/gpiConnect.c` | GPCM login, proofs |
| `P/src/RVL_SDK/gamespy/GP/gpiInfo.c` | `getprofile` replies |
| `P/src/Game/NetworkSession.cpp` | the game: `DWC_InitFriendsMatch`, `DWC_LoginAsync` |

## Reaching the server

The port sends every `*.nintendowifi.net` name to one IPv4 address, `[online]
server` in its ini (online:`src/net/so.c`), over plain HTTP: the `online`
branch patches out TLS. The services are told apart by port:

| Port | Service | Names |
|---|---|---|
| TCP 80 | NAS `/ac`, gamestats2 | `naswii.`, `gamestats2.gs.` |
| TCP 29900 | GPCM | `gpcm.gs.` |
| UDP 27900 | QR2 master: availability check, heartbeats | `mschargedwii.available.gs.`, `mschargedwii.master.gs.` |
| TCP 28910 | Server browser: the master's sessions | `mschargedwii.ms11.gs.` |

The server listens on `PORT` (8080), `GPCM_PORT` (29900), `MASTER_PORT`
(27900) and `SERVER_BROWSER_PORT` (28910); map host port 80 to `PORT`.

## The sequence

`DWC_LoginAsync` (`dwc_main.c`, `dwc_login.c`):

1. **Availability check**, UDP 27900: `09 00 00 00 00 mschargedwii\0`. The
   server answers `FE FD 09 00 00 00 00`, flags 0: up. Without an answer DWC
   carries on after two 2-second timeouts.
2. **Registration**, NAS `acctcreate`: only without `/shared2/DWC_AUTHDATA`
   (32 bytes, the userid). The client stores the userid there, then logs in.
3. **NAS `login`**: a token and a challenge.
4. **GPCM login** with the token.
5. **First login of a Mii slot**: `getprofile`, `updatepro` with DWC's login
   id as `lastname`, `getprofile` again, `logout`, and a second GPCM login with
   the same token.
6. `\status\`, QR2 heartbeats ([below](#qr2-udp-27900)) and gamestats2.

## NAS: `POST /ac`

A form; every value is Base64 with the alphabet `A-Za-z0-9.-` and `*` for
padding. DWC sends `Host: naswii.nintendowifi.net` twice.

| Field | `acctcreate` | `login` | |
|---|---|---|---|
| `action` | ✓ | ✓ | |
| `userid` | | ✓ | `%013llu`, below 2^43 |
| `gsbrcd` | | ✓ | `R4QE` (the disc's code) and 7 characters of the Mii slot's id |
| `ingamesn` | | ✓ | the Mii name, UTF-16BE (`unitcd` 1) |
| `gamecd`, `sdkver`, `makercd`, `unitcd`, `macadr`, `lang`, `region`, `devtime`, `csnum`, `cfc` | ✓ | ✓ | unchecked |

`macadr`, `csnum` and `cfc` are the same on every port install
(`0223456789ab`, `LU637607941`, `2601200516012010`): nothing is keyed on them.

The reply is `key=value&…` in key order, each value in the same Base64, ended
by a NUL; status 200 always, as the client retries anything else.

| Key | `acctcreate` | `login` |
|---|---|---|
| `returncd` | `002` | `001`; `103` for a missing or bad field |
| `datetime` | UTC `YYYYMMDDhhmmss`: the game's clock for seasons | same |
| `retry` | `0` | `0` |
| `userid` | the new account's | |
| `challenge` | | 8 letters (DWC keeps 8) |
| `locator` | | `gamespy.com` |
| `token` | | `NDS` and Base64 (DWC keeps 255 characters) |

**Registration** makes an `accounts` row with a random userid. A `login` with a
userid this server did not make registers it: a console keeps its userid.

**The token** carries the login (userid, gsbrcd, challenge, Mii name) signed
with HMAC-SHA256 under a key of the process, so the server keeps nothing. It
stays good for 10 minutes and can be used more than once (step 5); a restart
invalidates it.

## GPCM: TCP 29900

Messages are `\key\value\…\final\`; the first key is the command.

On connect: `\lc\1\challenge\<10 letters>\id\1\final\`. The client logs in:

```
\login\\challenge\<32 characters>\authtoken\<token>\partnerid\11\response\<md5>\firewall\1\port\0\productid\10927\gamename\mschargedwii\namespaceid\16\sdkrevision\3\id\1\final\
```

With `P` = `md5_hex(NAS challenge)`, all MD5 in lowercase hex:

```
response = md5_hex(P + 48 spaces + token + client challenge + server challenge + P)
proof    = md5_hex(P + 48 spaces + token + server challenge + client challenge + P)
```

The server checks the token and `response`, logs in to the profile of the
token's userid and gsbrcd (making it on the first login), and answers:

```
\lc\2\sesskey\<n>\proof\<proof>\userid\<userid>\profileid\<pid>\uniquenick\<nick>\lt\<24 characters>\id\1\final\
```

A login to a profile logged in elsewhere disconnects the other connection
(error 6).

| Command | Answer |
|---|---|
| `getprofile` | `\pi\` with `profileid`, `nick`, `userid`, `email`, `sig`, `uniquenick`, `firstname`, `lastname`, `loc`; another profile's names are masked as `000000000R4QE0000000` |
| `updatepro` | none; `firstname` and `lastname` are stored |
| `status` | none; `locstring` is kept for `getprofile`, and mutual friends get the status ([below](#mutual-friends)) |
| `ka` | `\ka\\final\` |
| `logout` | the server closes the connection |
| `addbuddy` | none; `newprofileid` is kept for the login ([below](#friend-requests)). Both players of a [mutual](#mutual-friends) request, and the [CPU buddy](#the-cpu-buddy), get `\bm\4\` and a status |
| `delbuddy` | none; the request ends, and a mutual friend gets the player's offline status ([below](#mutual-friends)) |
| `bm` | none; `\bm\1\` to a mutual friend is passed on ([below](#matchmaking-messages)) |
| `authadd`, … | none: not served yet |

Errors are `\error\\err\<n>\errmsg\<text>\fatal\\final\` (with the request's `\id\`
when there is one), then the
server closes the connection:

| `err` | When |
|---|---|
| 1 | a message that does not parse, or more than 16 KiB unread |
| 2 | anything but `login`, `ka` and `logout` before logging in |
| 4 | the database failed |
| 6 | another connection logged in to the profile |
| 256 | no token, challenge or response, or a wrong `response` |
| 266 | a token this server process did not sign |
| 268 | a token older than 10 minutes |
| 2561 | `getprofile` of an unknown profile |

### Friend requests

The game's roster holds 64 friend codes; a friend code is a profile id with
a checksum. At every login DWC asks to add each friend code that is not a
buddy yet, by its profile id (`DWCi_UpdateFriendReq` in
`P/src/RVL_SDK/dwc/dwc_friend.c`):

```
\addbuddy\\sesskey\<n>\newprofileid\<pid>\reason\\final\
```

The server keeps the requests of each login in memory, up to 64, and answers
none of them but mutual ones and the CPU buddy's. DWC takes any GP error, even
one that is not fatal, as a network error and goes offline
(`DWCi_GPErrorCallback` in `dwc_main.c`), so a bad `newprofileid` or the
player's own is only logged.

A friend code is the profile id and, above it, 7 bits of a CRC-8 (polynomial
0x07) of the profile id and the game code, both little-endian
(`DWC_Acc_CreateFriendKey` in `dwc_account.c`; not the MD5 WiiLink uses for
other Wii games). DWC checks it with the player's own game code, so the same
profile has one code in the US release (`R4QE`) and another in the European
one (`R4QP`). Profile 1000000000 is `0868-9934-5920` in the US release.

### Mutual friends

A request is mutual when its friend is online and has asked for the player
too, in its own login. Each of the two then gets the other's acceptance and,
once the other has sent one, its status:

```
\bm\4\f\<friend>\msg\\final\
\bm\100\f\<friend>\msg\|s|<status>|ss|<statstring>|ls|<locstring>|ip|0|p|0|qm|0\final\
```

A later `\status\` goes to the player's mutual friends the same way (values
without `|`), and when a login ends they get it as offline:
`|s|0|ss|Offline|ls||ip|0|p|0|qm|0`. Deleting a friend from the roster ends it
too: DWC sends `\delbuddy\\sesskey\<n>\delprofileid\<pid>\final\` for a buddy
of GP (`DWC_DeleteBuddyFriendData` in `dwc_friend.c`; one still waiting is
only cleared from the save), and the friend gets the offline status. The
friendship lasts as long as both logins; it starts again at the next login,
where DWC asks again for every friend that is not a buddy of GP. WiiLink keeps
friends in memory the same way.

`status` is DWC's (1 online, 2 playing, 3 to 6 matchmaking), and `locstring`
the game's own status data: `U0MyAQ**` (`SC2` and 1) in its menus.

### Matchmaking messages

DWC's matchmaking talks to a friend through GPCM: a friend who accepts an
invitation (the host's status data names it) asks the host for a place.

```
\bm\1\sesskey\<n>\t\<friend>\msg\GPCM3vMAT<command byte><values, / between>\final\
```

`3` is DWC's matchmaking version, the command byte one of
`DWC_MATCH_COMMAND_*` (`dwc_match.h`: 0x01 reservation, 0x02 OK, 0x03 deny,
0x04 wait, 0x05 cancel, 0x06 the address, …) and the values its numbers. The
port's first reservation was `\x01` and `3/16781740/33007`. The server passes
the message to the friend as `\bm\1\f\<player>\msg\<msg>\final\`, only between
mutual friends and only for messages that start with `GPCM`; it drops any
other without an error.

### The CPU buddy

A profile to add before a second console is online: profile 999999999 of
account 1, Mii name `CPU`. Its friend code is `1770-9365-9135` in the US
release and `0009-9999-9999` in the European one.

Nothing logs in as it yet (a headless client of the port is to do so later),
so the server accepts every request for it at once, then sends its status:
online, with the status data the game sends from its menus.

```
\bm\4\f\999999999\msg\\final\
\bm\100\f\999999999\msg\|s|1|ss||ls|U0MyAQ**|ip|0|p|0|qm|0\final\
```

On `\bm\4\` DWC asks for the profile (`getprofile`) and makes the friend code a
buddy (`DWCi_GPGetInfoCallback_RecvAuthMessage` in `dwc_friend.c`); the status
makes it a buddy of GP, so DWC does not ask again in this login.

## Accounts and profiles

`accounts` has one row per userid. `profiles` has one per userid and gsbrcd,
so each Mii slot is its own profile. Profile ids count from 1,000,000,000 and
fit the 31 bits DWC's friend codes take; the ids below are kept for the
server's own profiles, such as the [CPU buddy](#the-cpu-buddy).
`unique_nick` is the userid in base32 (`0-9a-v`) followed by the gsbrcd.

## QR2: UDP 27900

`P/src/RVL_SDK/gamespy/qr2/qr2.c` is the client. Its packets start with a
type and a 4-byte instance key; the server's start with `FE FD`, the type and
the key.

Once logged in, DWC sends a heartbeat (`03`): `name\0value\0` fields up to an
empty name (`localip0`, `localport`, `natneg`, `statechanged`, `gamename`,
`publicip`, `publicport`, `numplayers`, `maxplayers`, then DWC's own: the
first login sends them as `unknown`, the profile id first; once matchmaking
is set up they are named, below). Without
an answer it tries four times, 10 seconds apart, then shows error 94020
(`e_qrnochallengeerror`: 90000 after login, 4000 QR2, 20 server).

The server answers every heartbeat with a challenge:

```
FE FD 01 <key> <6 letters> 00 <IP %08X><port %04X> \0
```

`00` are the backend options (no query challenges); the address is where the
heartbeat came from, which the game takes as its public address. The game
answers with `01 <key>` and the challenge encrypted with its secret key, then
sends a heartbeat a minute and a keepalive (`08 <key>`) every 20 seconds,
which the server echoes. `statechanged` 2 means the game stops: no answer.

| Packet | Answer |
|---|---|
| `09 00000000 mschargedwii\0` | `FE FD 09 00000000`: up |
| `03` heartbeat | `FE FD 01` challenge |
| `01` challenge response | none (not checked) |
| `08` keepalive | `FE FD 08 <key>` |
| `07` client message ack | none; the [message](#messages-to-a-host) is delivered |

### Sessions

The server keeps each game's last heartbeat as its session, by the address
it comes from: a new instance key there is a new game. Its answer to a
challenge lists it, and it goes with `statechanged` 2 or after 90 seconds
without a heartbeat or keepalive. A game looking for a match asks the server
browser for these. A host's heartbeat once a friend reserved a place:

```
localip0=192.168.1.131 localport=51021 natneg=1 statechanged=1 gamename=mschargedwii
publicip=-1408172031 publicport=44719 numplayers=0 maxplayers=1
dwc_pid=1000000000 dwc_mtype=2 dwc_mresv=1000000001 dwc_mver=3 dwc_eval=1
```

`publicip` is the address from the challenge as a signed 32-bit number
(`AC110001`, 172.17.0.1), `dwc_pid` the profile, `dwc_mtype` DWC's match type
(2 hosting a group, 3 joining one), `dwc_mresv` the profile that reserved a
place or that the game asks for, `dwc_mver` the matchmaking version.

## Server browser: TCP 28910

`P/src/RVL_SDK/gamespy/serverbrowsing/sb_serverlist.c` is the client. A friend
who got a place in a host's group ([matchmaking messages](#matchmaking-messages))
asks for the host's [session](#sessions) by profile. Each request starts with
its length (2 bytes, big-endian, itself included) and its type; a list request
(type `00`):

```
00 8d 00 01 03 00000000 mschargedwii\0 mschargedwii\0 <challenge, 8 bytes>
dwc_pid = 1000000000\0 \numplayers\maxplayers\dwc_pid\dwc_mtype\dwc_mresv\dwc_mver\dwc_eval\0
00000080 06000000
```

That is: protocol 1, encoding 3, game version 0, the game listed and the
game asking, the client's challenge, the filter, the fields and the options
(`80`: a limit on the results follows). The reply, before encryption:

| Bytes | |
|---|---|
| 4 + 2 | the client's public IP and port |
| 1, then per field `00` name `\0` | the fields (all strings), without the addresses |
| `00` | no popular values |
| per session: flags, public IP and port, `localip0` and `localport`, `00000000`, then per field `FF` value `\0` | flags `7E` (keys, every address, and `04` for `natneg`) |
| `00 FF FF FF FF` | the end |

It is encrypted as the client decrypts it (`sb_crypt.c`, `InitCryptKey`): a
20-byte header (byte 0 `05 ^ EC`, bytes 1 and 2 the backend's flags, `00`, byte
6 `0D ^ EA`, the rest random), then the reply under GOACrypt, keyed with the
client's challenge mixed with the secret key `B4LdGW` and header bytes 7 to 19.
Only a lookup by `dwc_pid` finds sessions yet; other filters (matchmaking
with anybody) get an empty list.

### Messages to a host

With the host's address the friend sends it messages through the server
browser (type `02`: the session's public IP and port, then the message):

```
00 25 02 ac110001 d79b SBCM 03000000 06 08 5ba3 ac110001 01ca9a3b c0a80183 2adf 0000
```

`SBCM` and version 3 are DWC's, then the match command (`06`: the sender's
addresses and profile, `DWC_MATCH_COMMAND_TELL_ADDR`) and its data; NAT
negotiation sends its cookie the same way (`FD FC 1E 66 6A B2` first). The
client gets no answer. The master server sends the message to the session's
game as a client message, `FE FD 06 <key> <message key, 4 bytes> <message>`,
again each second until the game acks it (`07 <key> <message key>`), for at
most 10 seconds; the game drops one it has already had.

## Not served yet

Matchmaking with anybody (the server browser's other filters), NAT
negotiation and GPSP. NAS
`svcloc` and `/pr` are not used by the game.
