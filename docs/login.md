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
| UDP 27900 | QR2 master: availability check | `mschargedwii.available.gs.` |

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
6. `\status\`, then QR2 heartbeats (not served yet: DWC drops the session
   when no challenge arrives within about 40 seconds) and gamestats2.

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
| `addbuddy`, `authadd`, `delbuddy`, `bm`, … | none: not served yet |

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

## Accounts and profiles

`accounts` has one row per userid. `profiles` has one per userid and gsbrcd,
so each Mii slot is its own profile. Profile ids count from 1,000,000,000 and
fit the 31 bits DWC's friend codes take; `unique_nick` is the userid in base32
(`0-9a-v`) followed by the gsbrcd.

## Not served yet

QR2 heartbeats and matchmaking, friends (`addbuddy`, status messages), GPSP,
the server browser and NAT negotiation. NAS `svcloc` and `/pr` are not used by
the game.
