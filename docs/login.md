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

The server listens on `PORT` (8080), `GPCM_PORT` (29900) and `MASTER_PORT`
(27900); map host port 80 to `PORT`.

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
| `status` | none; `locstring` is kept for `getprofile` |
| `ka` | `\ka\\final\` |
| `logout` | the server closes the connection |
| `addbuddy` | none; `newprofileid` is kept for the login. For the [CPU buddy](#the-cpu-buddy): `\bm\4\`, then its status |
| `authadd`, `delbuddy`, `bm`, … | none: not served yet |

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
none of them but the CPU buddy's ([below](#the-cpu-buddy)). DWC takes any GP
error, even one that is not fatal, as a network error and goes offline
(`DWCi_GPErrorCallback` in `dwc_main.c`), so a bad `newprofileid` or the
player's own is only logged.

A friend code is the profile id and, above it, 7 bits of a CRC-8 (polynomial
0x07) of the profile id and the game code, both little-endian
(`DWC_Acc_CreateFriendKey` in `dwc_account.c`; not the MD5 WiiLink uses for
other Wii games). DWC checks it with the player's own game code, so the same
profile has one code in the US release (`R4QE`) and another in the European
one (`R4QP`). Profile 1000000000 is `0868-9934-5920` in the
US release.

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
`publicip`, `publicport`, `numplayers`, `maxplayers`, then DWC's own fields as
`unknown`, the profile id first: DWC names them only for matchmaking). Without
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

## Not served yet

Server lists and matchmaking, mutual friends and their status messages, GPSP,
the server browser and NAT negotiation. NAS `svcloc` and `/pr` are not used by
the game.
