//! GPCM (`gpcm.gs.nintendowifi.net`, TCP 29900): GameSpy's presence server.
//! A console logs in with its NAS token and gets the profile of its game
//! (docs/login.md). On a profile's first login DWC also stores its login id
//! in the profile's `lastname`, then logs in again.
//!
//! Friends (docs/login.md): a request (`addbuddy`) is kept for the login. Two
//! logins that asked for each other become mutual friends, get each other's
//! status, and the offline one when either leaves or deletes the other
//! (`delbuddy`); their matchmaking messages (`\bm\1\`) pass between them.
//! The CPU buddy accepts every request at once. The rest comes later: those
//! commands are accepted and ignored.

mod message;

use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, SocketAddr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use md5::{Digest, Md5};
use rand::distr::{Alphanumeric, SampleString};
use socket2::{SockRef, TcpKeepalive};
use sqlx::PgPool;
use time::OffsetDateTime;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, tcp::OwnedWriteHalf},
    sync::{Notify, mpsc},
};
use tracing::{debug, info, trace, warn};

use crate::{
    accounts::{self, Profile},
    activity::{Activity, Via},
    nas::{TicketError, Tickets},
    reports,
};
use message::{Message, split};

/// A client sends short messages: more than this unread means it is not one.
const MAX_BUFFER: usize = 0x4000;
/// The game sends nothing while it idles, and the server writes to a login
/// only to pass on a friend's message, so a console that went to sleep or lost
/// its network would stay logged in. After this long without traffic the
/// system probes it with TCP keepalives…
const KEEPALIVE_IDLE: Duration = Duration::from_secs(60);
/// …this often…
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
/// …and ends the login when this many go unanswered.
const KEEPALIVE_PROBES: u32 = 3;
/// DWC's partner id: Nintendo.
const PARTNER_ID: u32 = 11;
/// The game's friend roster holds 64 friend codes (`DWC_InitFriendsMatch` in
/// `NetworkSession.cpp`).
const MAX_FRIENDS: usize = 64;
/// `\bm\` kinds: a message between friends, a friend request accepted, a
/// friend's status.
const BM_MESSAGE: u32 = 1;
const BM_AUTH: u32 = 4;
const BM_STATUS: u32 = 100;
/// A match command between friends is `GPCM3vMAT`, the command byte, then its
/// values (`DWCi_SendGPBuddyMsgCommand`).
const MATCH_COMMAND_AT: usize = "GPCM3vMAT".len();
/// The CPU buddy's status: online (`|s|1`), with the status data the game
/// sends from its menus as `locstring` (`SC2` and 1).
const CPU_BUDDY_STATUS: &str = "|s|1|ss||ls|U0MyAQ**|ip|0|p|0|qm|0";
/// The status friends get when a login ends.
const OFFLINE_STATUS: &str = "|s|0|ss|Offline|ls||ip|0|p|0|qm|0";

/// GPCM's state: who is logged in where.
pub struct Gpcm {
    pool: PgPool,
    tickets: Arc<Tickets>,
    /// Where the match commands relayed are told.
    activity: Arc<Activity>,
    /// The connection logged in to each profile, to log out by a new login.
    sessions: Mutex<HashMap<i32, Session>>,
    next_connection: AtomicU64,
}

struct Session {
    connection: u64,
    /// Where the console logged in from: gamestats2 takes its scores only
    /// from there.
    address: IpAddr,
    kick: Arc<Notify>,
    logged_in_at: OffsetDateTime,
    /// Messages to the connection from other logins: friends' answers and
    /// statuses.
    outbox: mpsc::UnboundedSender<Message>,
    /// The profiles this login has asked to add (`addbuddy`). DWC asks again
    /// at every login for each friend code of its roster that is not a buddy.
    friends: HashSet<i32>,
    /// The friends online who have asked to add this profile too.
    mutual: HashSet<i32>,
    /// What mutual friends get from the last `\status\`.
    status: Option<String>,
    /// That `\status\`'s DWC status, for `/admin`.
    dwc_status: Option<u8>,
    /// The login ticket (`lt`) the login got. The game's own use of it is
    /// SAKE, which this game does not have; the port shows it to the profile
    /// service to prove which login is its own (docs/profiles.md).
    login_ticket: String,
}

/// What `/admin` shows of a profile's login.
pub struct Presence {
    pub logged_in_at: OffsetDateTime,
    /// DWC's status from the last `\status\`: 1 online, 2 playing, 3 to 6
    /// matchmaking.
    pub status: Option<u8>,
    /// Friend codes asked for in this login, and how many of them are mutual.
    pub friend_requests: usize,
    pub mutual_friends: usize,
}

impl Session {
    fn new(
        connection: u64,
        address: IpAddr,
        kick: Arc<Notify>,
        outbox: mpsc::UnboundedSender<Message>,
        login_ticket: String,
    ) -> Self {
        Self {
            connection,
            address,
            kick,
            logged_in_at: OffsetDateTime::now_utc(),
            outbox,
            friends: HashSet::new(),
            mutual: HashSet::new(),
            status: None,
            dwc_status: None,
            login_ticket,
        }
    }

    /// Records a request to add `friend`. Whether it is new.
    fn add_friend(&mut self, friend: i32) -> bool {
        if self.friends.len() >= MAX_FRIENDS && !self.friends.contains(&friend) {
            return false;
        }
        self.friends.insert(friend)
    }
}

impl Gpcm {
    pub fn new(pool: PgPool, tickets: Arc<Tickets>, activity: Arc<Activity>) -> Self {
        Self {
            pool,
            tickets,
            activity,
            sessions: Mutex::default(),
            next_connection: AtomicU64::new(1),
        }
    }

    /// Who is logged in, by profile.
    pub fn presence(&self) -> HashMap<i32, Presence> {
        let sessions = self.sessions.lock().unwrap();
        sessions
            .iter()
            .map(|(&profile_id, session)| {
                let presence = Presence {
                    logged_in_at: session.logged_in_at,
                    status: session.dwc_status,
                    friend_requests: session.friends.len(),
                    mutual_friends: session.mutual.len(),
                };
                (profile_id, presence)
            })
            .collect()
    }

    /// Where `profile_id` is logged in from, if it is.
    pub fn logged_in_from(&self, profile_id: i32) -> Option<IpAddr> {
        let sessions = self.sessions.lock().unwrap();
        sessions.get(&profile_id).map(|session| session.address)
    }

    /// Whether `profile_id` is logged in with the login ticket
    /// `login_ticket`.
    pub fn logged_in_with(&self, profile_id: i32, login_ticket: &str) -> bool {
        let sessions = self.sessions.lock().unwrap();
        sessions
            .get(&profile_id)
            .is_some_and(|session| session.login_ticket == login_ticket)
    }

    pub async fn serve(self: Arc<Self>, listener: TcpListener) -> std::io::Result<()> {
        loop {
            let (stream, peer) = listener.accept().await?;
            let gpcm = self.clone();
            tokio::spawn(async move {
                if let Err(error) = gpcm.connection(stream, peer).await {
                    debug!(target: "gpcm", %peer, %error, "connection failed");
                }
            });
        }
    }

    async fn connection(
        self: Arc<Self>,
        stream: TcpStream,
        peer: SocketAddr,
    ) -> std::io::Result<()> {
        if let Err(error) = keep_alive(&stream) {
            warn!(target: "gpcm", %peer, %error, "no keepalive");
        }
        let (mut reader, writer) = stream.into_split();
        let (outbox, mut inbox) = mpsc::unbounded_channel();
        let mut connection = Connection {
            id: self.next_connection.fetch_add(1, Ordering::Relaxed),
            peer: peer.ip(),
            challenge: uppercase(10),
            writer,
            outbox,
            login: None,
            ended: "closed",
        };
        debug!(target: "gpcm", %peer, "connected");
        connection
            .send(
                &Message::new("lc", 1)
                    .with("challenge", &connection.challenge)
                    .with("id", 1),
            )
            .await?;

        let mut buffer = Vec::new();
        let mut chunk = [0; 2048];
        let result = loop {
            let kick = connection.login.as_ref().map(|login| login.kick.clone());
            let read = tokio::select! {
                read = reader.read(&mut chunk) => read,
                () = async { kick.unwrap().notified().await }, if kick.is_some() => {
                    connection.ended = "replaced";
                    break connection.fail(Error::FORCED_DISCONNECT, None).await;
                }
                Some(message) = inbox.recv() => match connection.send(&message).await {
                    Ok(()) => continue,
                    Err(error) => {
                        connection.ended = "lost";
                        break Err(error);
                    }
                },
            };
            let read = match read {
                Ok(0) => break Ok(()),
                Ok(read) => read,
                Err(error) => {
                    connection.ended = "lost";
                    break Err(error);
                }
            };
            buffer.extend_from_slice(&chunk[..read]);
            if buffer.len() > MAX_BUFFER {
                break connection.fail(Error::PARSE, None).await;
            }
            match self.handle_all(&mut connection, split(&mut buffer)).await {
                Ok(true) => continue,
                Ok(false) => break Ok(()),
                Err(error) => break Err(error),
            }
        };

        if let Some(login) = &connection.login {
            let profile_id = login.profile.profile_id;
            {
                let mut sessions = self.sessions.lock().unwrap();
                if sessions.get(&profile_id).map(|session| session.connection)
                    == Some(connection.id)
                {
                    let session = sessions.remove(&profile_id).unwrap();
                    for friend in session.mutual {
                        if let Some(friend) = sessions.get_mut(&friend)
                            && friend.mutual.remove(&profile_id)
                        {
                            let _ = friend.outbox.send(buddy_message(
                                BM_STATUS,
                                profile_id,
                                OFFLINE_STATUS,
                            ));
                        }
                    }
                }
            }
            info!(target: "gpcm", profile_id, how = connection.ended, "logged out");
            match reports::left(&self.pool, profile_id, connection.ended).await {
                Ok(matches) => {
                    for match_id in matches {
                        info!(target: "reports", match_id, profile_id, how = connection.ended, "left WFC during the match");
                    }
                }
                Err(error) => warn!(target: "reports", %error, "database error"),
            }
        }
        debug!(target: "gpcm", %peer, "disconnected");
        result
    }

    /// Handles a read's messages. `Ok(false)` once the connection is done.
    async fn handle_all(
        &self,
        connection: &mut Connection,
        messages: Vec<Result<Message, ()>>,
    ) -> std::io::Result<bool> {
        for message in messages {
            let Ok(message) = message else {
                connection.fail(Error::PARSE, None).await?;
                return Ok(false);
            };
            trace!(target: "raw::gpcm", peer = %connection.peer, %message, "message");
            if !self.handle(connection, message).await? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Handles a message. `Ok(false)` once the connection is done.
    async fn handle(&self, connection: &mut Connection, message: Message) -> std::io::Result<bool> {
        let id = message.get("id").unwrap_or("1").to_owned();
        match message.command() {
            "ka" => connection
                .send(&Message::new("ka", ""))
                .await
                .map(|()| true),
            "logout" => {
                connection.ended = "logout";
                Ok(false)
            }
            "login" => match self.log_in(connection, &message).await {
                Ok(()) => Ok(true),
                Err(error) => connection.fail(error, Some(&id)).await.map(|()| false),
            },
            _ if connection.login.is_none() => connection
                .fail(Error::NOT_LOGGED_IN, Some(&id))
                .await
                .map(|()| false),
            "getprofile" => {
                let reply = self.profile_info(connection, &message, &id).await;
                match reply {
                    Ok(reply) => connection.send(&reply).await,
                    Err(error) => connection.fail(error, Some(&id)).await,
                }
                .map(|()| true)
            }
            "updatepro" => {
                let login = connection.login.as_mut().unwrap();
                let (firstname, lastname) = (message.get("firstname"), message.get("lastname"));
                if let Err(error) =
                    accounts::set_names(&self.pool, login.profile.profile_id, firstname, lastname)
                        .await
                {
                    warn!(target: "gpcm", %error, "database error");
                    return connection
                        .fail(Error::DATABASE, Some(&id))
                        .await
                        .map(|()| false);
                }
                if let Some(firstname) = firstname {
                    login.profile.firstname = firstname.to_owned();
                }
                if let Some(lastname) = lastname {
                    login.profile.lastname = lastname.to_owned();
                }
                Ok(true)
            }
            "status" => {
                let login = connection.login.as_mut().unwrap();
                login.locstring = message.get("locstring").unwrap_or_default().to_owned();
                self.set_status(connection, &message);
                Ok(true)
            }
            "addbuddy" => {
                if self.add_friend(connection, &message) == Some(accounts::CPU_BUDDY) {
                    connection.accept_as_cpu_buddy().await?;
                }
                Ok(true)
            }
            "delbuddy" => {
                self.delete_friend(connection, &message);
                Ok(true)
            }
            "bm" => {
                self.relay_message(connection, &message);
                Ok(true)
            }
            command => {
                debug!(target: "gpcm", command, "not implemented yet");
                Ok(true)
            }
        }
    }

    /// `\login\`: checks the NAS token and the client's proof, then answers
    /// with the profile and the server's proof.
    async fn log_in(&self, connection: &mut Connection, message: &Message) -> Result<(), Error> {
        let token = message.get("authtoken").filter(|token| !token.is_empty());
        let (Some(token), Some(client_challenge), Some(response)) =
            (token, message.get("challenge"), message.get("response"))
        else {
            return Err(Error::LOGIN);
        };
        let ticket = self.tickets.redeem(token).map_err(|error| match error {
            TicketError::Invalid => Error::BAD_PREAUTH,
            TicketError::Expired => Error::EXPIRED_TICKET,
        })?;
        let proof = |first: &str, second: &str| proof(&ticket.challenge, token, first, second);
        if response != proof(client_challenge, &connection.challenge) {
            return Err(Error::LOGIN);
        }

        let profile = accounts::profile_login(
            &self.pool,
            ticket.user_id,
            &ticket.gsbrcd,
            ticket.ingamesn.as_deref(),
            &connection.peer.to_string(),
        )
        .await
        .map_err(|error| {
            warn!(target: "gpcm", %error, "database error");
            Error::DATABASE
        })?;
        let sesskey = rand::random_range(10_000_000..300_000_000);
        let login_ticket = Alphanumeric.sample_string(&mut rand::rng(), 24);
        let reply = Message::new("lc", 2)
            .with("sesskey", sesskey)
            .with("proof", proof(&connection.challenge, client_challenge))
            .with("userid", profile.user_id)
            .with("profileid", profile.profile_id)
            .with("uniquenick", &profile.unique_nick)
            .with("lt", &login_ticket)
            .with("id", message.get("id").unwrap_or("1"));

        // Take over the profile before answering: once the client has the
        // answer, a login elsewhere must find this one.
        info!(target: "gpcm", profile_id = profile.profile_id, user_id = profile.user_id, peer = %connection.peer, "logged in");
        let kick = Arc::new(Notify::new());
        let previous = self.sessions.lock().unwrap().insert(
            profile.profile_id,
            Session::new(
                connection.id,
                connection.peer,
                kick.clone(),
                connection.outbox.clone(),
                login_ticket,
            ),
        );
        if let Some(previous) = previous.filter(|previous| previous.connection != connection.id) {
            previous.kick.notify_one();
        }
        connection.login = Some(Login {
            profile,
            kick,
            locstring: String::new(),
        });
        connection.send(&reply).await.map_err(|_| Error::NETWORK)
    }

    /// `\addbuddy\`: a friend code of the game's roster, as `newprofileid`.
    /// Kept for the login. Returns the friend, `None` for a bad request: no
    /// error goes back, as DWC takes any GP error, even one that is not fatal,
    /// as a network error and goes offline.
    fn add_friend(&self, connection: &Connection, message: &Message) -> Option<i32> {
        let profile = &connection.login.as_ref().unwrap().profile;
        let profile_id = profile.profile_id;
        let newprofileid = message.get("newprofileid");
        let Some(friend) = newprofileid
            .and_then(|friend| friend.parse::<i32>().ok())
            .filter(|&friend| friend > 0 && friend != profile_id)
        else {
            debug!(target: "gpcm", profile_id, newprofileid, "bad friend request");
            return None;
        };
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions
            .get_mut(&profile_id)
            .filter(|session| session.connection == connection.id)?;
        if session.add_friend(friend) {
            let game = profile.gsbrcd.get(..4).unwrap_or_default();
            let friend_code = accounts::friend_code(friend, game);
            info!(target: "gpcm", profile_id, friend, friend_code, "friend request");
        } else if !session.friends.contains(&friend) {
            debug!(target: "gpcm", profile_id, friend, "too many friend requests");
            return Some(friend);
        }
        let status = session.status.clone();

        // Mutual once the friend, online, has asked for this profile too.
        // Each then gets the other's acceptance and status.
        let Some(other) = sessions
            .get_mut(&friend)
            .filter(|other| other.friends.contains(&profile_id))
        else {
            return Some(friend);
        };
        other.mutual.insert(profile_id);
        let _ = other.outbox.send(buddy_message(BM_AUTH, profile_id, ""));
        if let Some(status) = &status {
            let _ = other
                .outbox
                .send(buddy_message(BM_STATUS, profile_id, status));
        }
        let other_status = other.status.clone();
        let session = sessions.get_mut(&profile_id).unwrap();
        session.mutual.insert(friend);
        let _ = session.outbox.send(buddy_message(BM_AUTH, friend, ""));
        if let Some(status) = &other_status {
            let _ = session
                .outbox
                .send(buddy_message(BM_STATUS, friend, status));
        }
        info!(target: "gpcm", profile_id, friend, "mutual friends");
        Some(friend)
    }

    /// `\delbuddy\`: a friend deleted from the roster, as `delprofileid`;
    /// DWC sends it only for a buddy of GP. The request ends, and a mutual
    /// friend sees the player offline. Never answered, like `addbuddy`.
    fn delete_friend(&self, connection: &Connection, message: &Message) {
        let profile_id = connection.login.as_ref().unwrap().profile.profile_id;
        let delprofileid = message.get("delprofileid");
        let Some(friend) = delprofileid.and_then(|friend| friend.parse::<i32>().ok()) else {
            debug!(target: "gpcm", profile_id, delprofileid, "bad friend deletion");
            return;
        };
        let mut sessions = self.sessions.lock().unwrap();
        let Some(session) = sessions
            .get_mut(&profile_id)
            .filter(|session| session.connection == connection.id)
        else {
            return;
        };
        session.friends.remove(&friend);
        session.mutual.remove(&friend);
        info!(target: "gpcm", profile_id, friend, "friend deleted");
        if let Some(other) = sessions.get_mut(&friend)
            && other.mutual.remove(&profile_id)
        {
            let _ = other
                .outbox
                .send(buddy_message(BM_STATUS, profile_id, OFFLINE_STATUS));
        }
    }

    /// `\bm\1\`: a message to the mutual friend `t`. DWC's matchmaking sends
    /// its commands this way (`GPCM3vMAT`, a command byte and its values: a
    /// reservation for a place in the friend's group and the answers). The
    /// friend gets it from the player (`f`); anything else is dropped without
    /// an error, which DWC would take as a network error.
    fn relay_message(&self, connection: &Connection, message: &Message) {
        let profile_id = connection.login.as_ref().unwrap().profile.profile_id;
        let to = message.get("t").and_then(|to| to.parse::<i32>().ok());
        let msg = message.get("msg").unwrap_or_default();
        if message.get("bm") != Some("1") || !msg.starts_with("GPCM") {
            debug!(target: "gpcm", profile_id, kind = message.get("bm"), "message dropped");
            return;
        }
        let sessions = self.sessions.lock().unwrap();
        let friend = sessions
            .get(&profile_id)
            .filter(|session| session.connection == connection.id)
            .zip(to)
            .filter(|(session, to)| session.mutual.contains(to))
            .and_then(|(_, to)| sessions.get(&to).map(|friend| (to, friend)))
            .filter(|(_, friend)| friend.mutual.contains(&profile_id));
        let Some((to, friend)) = friend else {
            debug!(target: "gpcm", profile_id, to, "message to no mutual friend dropped");
            return;
        };
        let _ = friend
            .outbox
            .send(buddy_message(BM_MESSAGE, profile_id, msg));
        debug!(target: "gpcm", profile_id, to, "message relayed");
        // `GPCM3vMAT`, then the command byte.
        if let Some(&command) = msg.as_bytes().get(MATCH_COMMAND_AT) {
            self.activity
                .command(Some(profile_id), Some(to), command, Via::Gpcm);
        }
    }

    /// `\status\`: what the game is doing, for mutual friends. The values go
    /// into GP's `|key|value` form, so they lose any `|`.
    fn set_status(&self, connection: &Connection, message: &Message) {
        let profile_id = connection.login.as_ref().unwrap().profile.profile_id;
        let value = |key| message.get(key).unwrap_or_default().replace('|', "");
        let status = format!(
            "|s|{}|ss|{}|ls|{}|ip|0|p|0|qm|0",
            value("status"),
            value("statstring"),
            value("locstring")
        );
        let mut sessions = self.sessions.lock().unwrap();
        let Some(session) = sessions
            .get_mut(&profile_id)
            .filter(|session| session.connection == connection.id)
        else {
            return;
        };
        session.status = Some(status.clone());
        session.dwc_status = message.get("status").and_then(|status| status.parse().ok());
        for friend in session.mutual.clone() {
            if let Some(friend) = sessions
                .get(&friend)
                .filter(|friend| friend.mutual.contains(&profile_id))
            {
                let _ = friend
                    .outbox
                    .send(buddy_message(BM_STATUS, profile_id, &status));
            }
        }
    }

    /// `\getprofile\`: the profile asked for, all of it only for its owner.
    async fn profile_info(
        &self,
        connection: &Connection,
        message: &Message,
        id: &str,
    ) -> Result<Message, Error> {
        let login = connection.login.as_ref().unwrap();
        let profile_id: i32 = message
            .get("profileid")
            .and_then(|profile_id| profile_id.parse().ok())
            .ok_or(Error::BAD_PROFILE)?;
        let (profile, own) = if profile_id == login.profile.profile_id {
            (login.profile.clone(), true)
        } else {
            let profile = accounts::profile(&self.pool, profile_id)
                .await
                .map_err(|error| {
                    warn!(target: "gpcm", %error, "database error");
                    Error::DATABASE
                })?
                .ok_or(Error::BAD_PROFILE)?;
            (profile, false)
        };
        // Others see the game code, not the account.
        let masked = format!("000000000{}0000000", &profile.gsbrcd[..4]);
        let (nick, user_id, lastname) = if own {
            (
                profile.unique_nick.clone(),
                profile.user_id,
                profile.lastname.clone(),
            )
        } else {
            (masked.clone(), 0, masked)
        };
        Ok(Message::new("pi", "")
            .with("profileid", profile.profile_id)
            .with("nick", &nick)
            .with("userid", user_id)
            .with("email", format!("{nick}@nds"))
            .with("sig", uppercase(32).to_lowercase())
            .with("uniquenick", &nick)
            .with("firstname", &profile.firstname)
            .with("lastname", lastname)
            .with("pid", PARTNER_ID)
            .with("lon", "0.000000")
            .with("lat", "0.000000")
            .with("loc", if own { login.locstring.as_str() } else { "" })
            .with("id", id))
    }
}

/// A client's connection.
struct Connection {
    id: u64,
    peer: IpAddr,
    /// Sent on connect; both login proofs include it.
    challenge: String,
    writer: OwnedWriteHalf,
    /// For the session, so other logins can send to this connection.
    outbox: mpsc::UnboundedSender<Message>,
    login: Option<Login>,
    /// How it ended, for a match it leaves (`reports::left`): `closed` until
    /// something else is known.
    ended: &'static str,
}

struct Login {
    profile: Profile,
    /// Notified when another connection logs in to the profile.
    kick: Arc<Notify>,
    /// From the last `\status\`.
    locstring: String,
}

impl Connection {
    async fn send(&mut self, message: &Message) -> std::io::Result<()> {
        trace!(target: "raw::gpcm", peer = %self.peer, %message, "reply");
        self.writer.write_all(message.to_string().as_bytes()).await
    }

    /// Nothing logs in as the CPU buddy yet, so it accepts every request
    /// itself (`\bm\4\`) and is online. DWC then asks for its profile and
    /// makes the friend code a buddy (`DWCi_GPGetInfoCallback_RecvAuthMessage`
    /// in `dwc_friend.c`); the status makes it a GP buddy, which DWC asks no
    /// more of in this login.
    async fn accept_as_cpu_buddy(&mut self) -> std::io::Result<()> {
        let profile_id = self.login.as_ref().map(|login| login.profile.profile_id);
        info!(target: "gpcm", profile_id, "the CPU buddy accepts");
        let from = accounts::CPU_BUDDY;
        self.send(&buddy_message(BM_AUTH, from, "")).await?;
        self.send(&buddy_message(BM_STATUS, from, CPU_BUDDY_STATUS))
            .await
    }

    /// Sends `error`; the client then gives up the connection.
    async fn fail(&mut self, error: Error, id: Option<&str>) -> std::io::Result<()> {
        warn!(target: "gpcm", peer = %self.peer, code = error.code, error = error.message, "refused");
        let mut message = Message::new("error", "")
            .with("err", error.code)
            .with("errmsg", error.message)
            .with("fatal", "");
        if let Some(id) = id {
            message = message.with("id", id);
        }
        self.send(&message).await
    }
}

/// A GameSpy error. All of these end the connection.
#[derive(Clone, Copy, Debug)]
struct Error {
    code: u16,
    message: &'static str,
}

impl Error {
    const PARSE: Self = Self::new(0x0001, "There was an error parsing an incoming request.");
    const NOT_LOGGED_IN: Self = Self::new(
        0x0002,
        "This request cannot be processed because you are not logged in.",
    );
    const DATABASE: Self = Self::new(
        0x0004,
        "This request cannot be processed because of a database error.",
    );
    const NETWORK: Self = Self::new(0x0005, "There was an error connecting a network socket.");
    const FORCED_DISCONNECT: Self = Self::new(
        0x0006,
        "This profile has been disconnected by another login.",
    );
    const LOGIN: Self = Self::new(0x0100, "There was an error logging in to the GP backend.");
    const BAD_PREAUTH: Self = Self::new(
        0x010A,
        "There was an error validating the pre-authentication.",
    );
    const EXPIRED_TICKET: Self = Self::new(
        0x010C,
        "The login ticket had expired and could not be used.",
    );
    const BAD_PROFILE: Self = Self::new(0x0A01, "The profile info was requested on is invalid.");

    const fn new(code: u16, message: &'static str) -> Self {
        Self { code, message }
    }
}

/// A login proof: from the client (`response`) with its challenge first,
/// from the server (`proof`) with the server's first.
fn proof(nas_challenge: &str, token: &str, first: &str, second: &str) -> String {
    let password = md5_hex(nas_challenge);
    md5_hex(format!(
        "{password}{}{token}{first}{second}{password}",
        " ".repeat(48)
    ))
}

/// `\bm\<kind>\f\<from>\msg\<msg>\final\`: a message from a buddy.
fn buddy_message(kind: u32, from: i32, msg: &str) -> Message {
    Message::new("bm", kind).with("f", from).with("msg", msg)
}

fn md5_hex(data: impl AsRef<[u8]>) -> String {
    Md5::digest(data)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Makes the system end the connection of a console that stopped answering
/// ([`KEEPALIVE_IDLE`]).
fn keep_alive(stream: &TcpStream) -> std::io::Result<()> {
    let socket = SockRef::from(stream);
    socket.set_tcp_keepalive(
        &TcpKeepalive::new()
            .with_time(KEEPALIVE_IDLE)
            .with_interval(KEEPALIVE_INTERVAL)
            .with_retries(KEEPALIVE_PROBES),
    )?;
    // No probe goes out while a friend's message waits for its ack: the
    // same limit then ends the login, instead of minutes of retransmission.
    #[cfg(target_os = "linux")]
    socket.set_tcp_user_timeout(Some(KEEPALIVE_IDLE + KEEPALIVE_INTERVAL * KEEPALIVE_PROBES))?;
    Ok(())
}

fn uppercase(len: usize) -> String {
    (0..len)
        .map(|_| char::from(rand::random_range(b'A'..=b'Z')))
        .collect()
}

#[cfg(test)]
impl Gpcm {
    /// A login of `profile_id` from `address` with no connection behind it,
    /// for other modules' tests. Its login ticket is `ticket-<profile_id>`.
    pub(crate) fn fake_login(&self, profile_id: i32, address: IpAddr) {
        let session = Session::new(
            0,
            address,
            Arc::new(Notify::new()),
            mpsc::unbounded_channel().0,
            format!("ticket-{profile_id}"),
        );
        self.sessions.lock().unwrap().insert(profile_id, session);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn presence_tells_who_is_logged_in() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy_with(sqlx::postgres::PgConnectOptions::new());
        let gpcm = Gpcm::new(pool, Arc::default(), Arc::default());
        let mut session = Session::new(
            1,
            IpAddr::from([10, 0, 0, 1]),
            Arc::new(Notify::new()),
            mpsc::unbounded_channel().0,
            String::new(),
        );
        session.add_friend(1_000_000_001);
        session.mutual.insert(1_000_000_001);
        session.dwc_status = Some(2);
        gpcm.sessions.lock().unwrap().insert(1_000_000_000, session);

        let presence = gpcm.presence();
        let online = &presence[&1_000_000_000];
        assert_eq!(online.status, Some(2));
        assert_eq!((online.friend_requests, online.mutual_friends), (1, 1));
        assert!(!presence.contains_key(&1_000_000_001));
        assert_eq!(
            gpcm.logged_in_from(1_000_000_000),
            Some(IpAddr::from([10, 0, 0, 1]))
        );
        assert_eq!(gpcm.logged_in_from(1_000_000_001), None);
    }

    #[test]
    fn friend_requests_are_kept_once_up_to_the_roster_size() {
        let mut session = Session::new(
            1,
            IpAddr::from([10, 0, 0, 1]),
            Arc::new(Notify::new()),
            mpsc::unbounded_channel().0,
            String::new(),
        );
        assert!(session.add_friend(1_000_000_001));
        assert!(!session.add_friend(1_000_000_001));
        for friend in 1..MAX_FRIENDS as i32 {
            assert!(session.add_friend(friend));
        }
        assert!(!session.add_friend(2_000_000_000));
        assert_eq!(session.friends.len(), MAX_FRIENDS);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn logins_are_probed_when_idle() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        keep_alive(&stream).unwrap();

        let socket = SockRef::from(&stream);
        assert!(socket.keepalive().unwrap());
        assert_eq!(socket.tcp_keepalive_time().unwrap(), KEEPALIVE_IDLE);
        assert_eq!(socket.tcp_keepalive_interval().unwrap(), KEEPALIVE_INTERVAL);
        assert_eq!(socket.tcp_keepalive_retries().unwrap(), KEEPALIVE_PROBES);
        assert_eq!(
            socket.tcp_user_timeout().unwrap(),
            Some(Duration::from_secs(90))
        );
    }

    #[test]
    fn proofs_follow_gamespy() {
        // printf '%s' ABCDEFGH | md5sum
        assert_eq!(md5_hex("ABCDEFGH"), "4783e784b4fa2fba9e4d6502dbc64f8f");
        // p=4783e784b4fa2fba9e4d6502dbc64f8f; printf '%s' "$p$(printf '%48s')NDStokenclientserver$p" | md5sum
        assert_eq!(
            proof("ABCDEFGH", "NDStoken", "client", "server"),
            "6f6368922e17ce1b214fe6f2063f7958"
        );
    }
}
