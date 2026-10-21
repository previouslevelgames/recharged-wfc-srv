//! Registration and login as the port does them (docs/login.md): NAS
//! `acctcreate` and `login` over HTTP, GPCM over TCP with friend requests,
//! and the master server's availability check and heartbeats over UDP.

use std::{collections::HashMap, net::SocketAddr};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use md5::{Digest, Md5};
use recharged_wfc_srv::App;
use sqlx::PgPool;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const GSBRCD: &str = "R4QEabcdefg";
const CLIENT_CHALLENGE: &str = "0123456789abcdefghijklmnopqrstuv";

#[sqlx::test]
async fn a_new_console_registers_logs_in_and_keeps_its_profile(pool: PgPool) {
    let server = Server::start(pool).await;

    let reply = server.nas(&[("action", b"acctcreate")]).await;
    assert_eq!(reply["returncd"], "002");
    assert_eq!(reply["datetime"].len(), 14);
    let user_id: i64 = reply["userid"].parse().unwrap();
    assert!((0..1 << 43).contains(&user_id));

    let (token, challenge) = server.nas_login(user_id).await;
    assert_eq!(challenge.len(), 8);

    // The first login of a Mii slot: DWC finds no login id in the profile,
    // stores one, checks it, and logs in again.
    let mut client = Client::connect(server.gpcm).await;
    let login = client.log_in(&token, &challenge).await;
    let profile_id = login["profileid"].clone();
    assert!(profile_id.parse::<i32>().unwrap() >= 1_000_000_000);
    assert_eq!(login["userid"], user_id.to_string());

    let profile = client.get_profile(&profile_id).await;
    assert_eq!(profile["lastname"], "");
    assert!(!profile["sig"].is_empty());
    client
        .send(&format!(
            r"\updatepro\\sesskey\{}\lastname\loginid123\partnerid\11\final\",
            login["sesskey"]
        ))
        .await;
    assert_eq!(
        client.get_profile(&profile_id).await["lastname"],
        "loginid123"
    );
    client.send(r"\logout\\final\").await;
    assert!(client.closed().await);

    // The same token again: the same profile.
    let mut client = Client::connect(server.gpcm).await;
    let again = client.log_in(&token, &challenge).await;
    assert_eq!(again["profileid"], profile_id);
    client
        .send(r"\status\1\sesskey\1\statstring\\locstring\bG9j\final\")
        .await;
    assert_eq!(client.get_profile(&profile_id).await["loc"], "bG9j");
}

#[sqlx::test]
async fn a_new_login_disconnects_the_old_one(pool: PgPool) {
    let server = Server::start(pool).await;
    let (token, challenge) = server.nas_login(42).await;

    let mut first = Client::connect(server.gpcm).await;
    first.log_in(&token, &challenge).await;
    let mut second = Client::connect(server.gpcm).await;
    second.log_in(&token, &challenge).await;

    let error = first.receive().await;
    assert_eq!(error["error"], "");
    assert_eq!(error["err"], "6");
    assert!(first.closed().await);
}

#[sqlx::test]
async fn friend_requests_get_no_answer(pool: PgPool) {
    let server = Server::start(pool).await;
    let (token, challenge) = server.nas_login(42).await;
    let mut client = Client::connect(server.gpcm).await;
    let login = client.log_in(&token, &challenge).await;

    // A friend code, the player's own and a broken one: at any GP error DWC
    // would go offline.
    for friend in ["1000000001", &login["profileid"], "x"] {
        client
            .send(&format!(
                r"\addbuddy\\sesskey\{}\newprofileid\{friend}\reason\\final\",
                login["sesskey"]
            ))
            .await;
    }
    client.send(r"\ka\\final\").await;
    let reply = client.receive().await;
    assert!(reply.contains_key("ka"), "{reply:?}");
}

#[sqlx::test]
async fn the_cpu_buddy_accepts_at_once(pool: PgPool) {
    let server = Server::start(pool).await;
    let (token, challenge) = server.nas_login(42).await;
    let mut client = Client::connect(server.gpcm).await;
    let login = client.log_in(&token, &challenge).await;

    client
        .send(&format!(
            r"\addbuddy\\sesskey\{}\newprofileid\999999999\reason\\final\",
            login["sesskey"]
        ))
        .await;
    let accepted = client.receive().await;
    assert_eq!(
        (accepted["bm"].as_str(), accepted["f"].as_str()),
        ("4", "999999999")
    );
    let status = client.receive().await;
    assert_eq!(
        (status["bm"].as_str(), status["f"].as_str()),
        ("100", "999999999")
    );
    assert!(status["msg"].starts_with("|s|1|"), "{status:?}");

    // DWC then asks for the buddy's profile.
    let buddy = client.get_profile("999999999").await;
    assert_eq!(buddy["userid"], "0");
}

#[sqlx::test]
async fn a_login_needs_the_token_and_its_proof(pool: PgPool) {
    let server = Server::start(pool).await;
    let (token, challenge) = server.nas_login(42).await;

    // A proof from the wrong NAS challenge.
    let mut client = Client::connect(server.gpcm).await;
    client.send_login(&token, "WRONGCHA").await;
    assert_eq!(client.receive().await["err"], "256");
    assert!(client.closed().await);

    // A token this server did not sign.
    let mut client = Client::connect(server.gpcm).await;
    client
        .send_login("NDSAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", &challenge)
        .await;
    assert_eq!(client.receive().await["err"], "266");
    assert!(client.closed().await);

    // Anything else before logging in.
    let mut client = Client::connect(server.gpcm).await;
    client
        .send(r"\getprofile\\profileid\1000000000\id\2\final\")
        .await;
    assert_eq!(client.receive().await["err"], "2");
}

#[sqlx::test]
async fn nas_refuses_logins_without_their_fields(pool: PgPool) {
    let server = Server::start(pool).await;
    let reply = server.nas(&[("action", b"login"), ("userid", b"42")]).await;
    assert_eq!(reply["returncd"], "103");
    assert!(!reply.contains_key("token"));
}

#[tokio::test]
async fn the_game_is_available() {
    let master = Master::start().await;
    let reply = master.send(b"\x09\0\0\0\0mschargedwii\0").await;
    assert_eq!(reply, b"\xFE\xFD\x09\0\0\0\0");
}

#[tokio::test]
async fn a_logged_in_game_is_challenged() {
    let master = Master::start().await;
    let reply = master
        .send(b"\x03\x01\x02\x03\x04statechanged\x001\x00gamename\x00mschargedwii\x00\x00\x00")
        .await;
    assert!(
        reply.starts_with(b"\xFE\xFD\x01\x01\x02\x03\x04"),
        "{reply:?}"
    );
    // 6 letters, options 00, then the address the heartbeat came from.
    let port = master.client.local_addr().unwrap().port();
    let challenge = String::from_utf8(reply[7..].to_vec()).unwrap();
    assert_eq!(challenge[6..], format!("007F000001{port:04X}\0"));
}

/// The master server on a local UDP port, and a client socket.
struct Master {
    address: SocketAddr,
    client: tokio::net::UdpSocket,
}

impl Master {
    async fn start() -> Self {
        use tokio::net::UdpSocket;

        // The master server keeps nothing in the database.
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy_with(sqlx::postgres::PgConnectOptions::new());
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = socket.local_addr().unwrap();
        tokio::spawn(async move { App::new(pool).serve_master(socket).await });
        Self {
            address,
            client: UdpSocket::bind("127.0.0.1:0").await.unwrap(),
        }
    }

    async fn send(&self, packet: &[u8]) -> Vec<u8> {
        self.client.send_to(packet, self.address).await.unwrap();
        let mut reply = [0; 1500];
        let len = self.client.recv(&mut reply).await.unwrap();
        reply[..len].to_vec()
    }
}

/// The server on local ports: HTTP for NAS, TCP for GPCM.
struct Server {
    http: SocketAddr,
    gpcm: SocketAddr,
}

impl Server {
    async fn start(pool: PgPool) -> Self {
        let app = App::new(pool);
        let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gpcm = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = Self {
            http: http.local_addr().unwrap(),
            gpcm: gpcm.local_addr().unwrap(),
        };
        let router = app.router(None);
        tokio::spawn(async move { axum::serve(http, router).await });
        tokio::spawn(async move { app.serve_gpcm(gpcm).await });
        server
    }

    /// `POST /ac` as DWC sends it, Host header twice included. Returns the
    /// reply's fields, decoded.
    async fn nas(&self, fields: &[(&str, &[u8])]) -> HashMap<String, String> {
        let body: Vec<String> = fields
            .iter()
            .map(|(key, value)| format!("{key}={}", nas_base64(value)))
            .collect();
        let body = body.join("&");
        let request = format!(
            "POST /ac HTTP/1.1\r\nHost: naswii.nintendowifi.net\r\nHost: naswii.nintendowifi.net\r\n\
             User-Agent: RVL SDK/1.0\r\nHTTP_X_GAMECD: R4QE\r\n\
             Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        );
        let mut stream = TcpStream::connect(self.http).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 200 "), "{response}");

        let (_, body) = response.split_once("\r\n\r\n").unwrap();
        let body = body.strip_suffix('\0').expect("ends with a NUL");
        body.split('&')
            .map(|pair| {
                let (key, value) = pair.split_once('=').unwrap();
                let value = STANDARD
                    .decode(value.replace('.', "+").replace('-', "/").replace('*', "="))
                    .unwrap();
                (key.to_owned(), String::from_utf8(value).unwrap())
            })
            .collect()
    }

    /// A NAS login of `user_id` to [`GSBRCD`]. Returns the token and the
    /// challenge.
    async fn nas_login(&self, user_id: i64) -> (String, String) {
        let name: Vec<u8> = "Mario".encode_utf16().flat_map(u16::to_be_bytes).collect();
        let reply = self
            .nas(&[
                ("action", b"login"),
                ("gsbrcd", GSBRCD.as_bytes()),
                ("userid", format!("{user_id:013}").as_bytes()),
                ("ingamesn", &name),
                ("sdkver", b"001000"),
                ("gamecd", b"R4QE"),
                ("makercd", b"01"),
                ("unitcd", b"1"),
                ("macadr", b"0223456789ab"),
                ("lang", b"01"),
                ("region", b"01"),
            ])
            .await;
        assert_eq!(reply["returncd"], "001");
        assert_eq!(reply["locator"], "gamespy.com");
        (reply["token"].clone(), reply["challenge"].clone())
    }
}

fn nas_base64(value: &[u8]) -> String {
    STANDARD
        .encode(value)
        .replace('+', ".")
        .replace('/', "-")
        .replace('=', "*")
}

fn md5_hex(data: &str) -> String {
    Md5::digest(data)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A GP client: sends raw messages and reads them back as fields.
struct Client {
    stream: TcpStream,
    buffer: Vec<u8>,
    server_challenge: String,
}

impl Client {
    async fn connect(address: SocketAddr) -> Self {
        let mut client = Self {
            stream: TcpStream::connect(address).await.unwrap(),
            buffer: Vec::new(),
            server_challenge: String::new(),
        };
        let hello = client.receive().await;
        assert_eq!((hello["lc"].as_str(), hello["id"].as_str()), ("1", "1"));
        client.server_challenge = hello["challenge"].clone();
        client
    }

    async fn send(&mut self, message: &str) {
        self.stream.write_all(message.as_bytes()).await.unwrap();
    }

    async fn receive(&mut self) -> HashMap<String, String> {
        loop {
            if let Some(end) = self.buffer.windows(7).position(|w| w == b"\\final\\") {
                let message = String::from_utf8(self.buffer[1..end].to_vec()).unwrap();
                self.buffer.drain(..end + 7);
                let parts: Vec<&str> = message.split('\\').collect();
                return parts
                    .chunks(2)
                    .map(|pair| (pair[0].to_owned(), pair.get(1).unwrap_or(&"").to_string()))
                    .collect();
            }
            let mut chunk = [0; 1024];
            let read = self.stream.read(&mut chunk).await.unwrap();
            assert!(read > 0, "closed while waiting for a message");
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }

    /// Whether the server closed the connection.
    async fn closed(&mut self) -> bool {
        let mut chunk = [0; 64];
        matches!(self.stream.read(&mut chunk).await, Ok(0) | Err(_))
    }

    /// GameSpy's proof for this connection, with `first`'s challenge first.
    fn proof(&self, nas_challenge: &str, token: &str, first: &str, second: &str) -> String {
        let password = md5_hex(nas_challenge);
        md5_hex(&format!(
            "{password}{}{token}{first}{second}{password}",
            " ".repeat(48)
        ))
    }

    async fn send_login(&mut self, token: &str, nas_challenge: &str) {
        let response = self.proof(
            nas_challenge,
            token,
            CLIENT_CHALLENGE,
            &self.server_challenge.clone(),
        );
        self.send(&format!(
            r"\login\\challenge\{CLIENT_CHALLENGE}\authtoken\{token}\partnerid\11\response\{response}\firewall\1\port\0\productid\10927\gamename\mschargedwii\namespaceid\16\sdkrevision\3\id\1\final\"
        ))
        .await;
    }

    /// Logs in and checks the server's proof. Returns `\lc\2`'s fields.
    async fn log_in(&mut self, token: &str, nas_challenge: &str) -> HashMap<String, String> {
        self.send_login(token, nas_challenge).await;
        let reply = self.receive().await;
        assert_eq!(reply.get("lc").map(String::as_str), Some("2"), "{reply:?}");
        let proof = self.proof(
            nas_challenge,
            token,
            &self.server_challenge.clone(),
            CLIENT_CHALLENGE,
        );
        assert_eq!(reply["proof"], proof);
        reply
    }

    async fn get_profile(&mut self, profile_id: &str) -> HashMap<String, String> {
        self.send(&format!(
            r"\getprofile\\sesskey\1\profileid\{profile_id}\id\5\final\"
        ))
        .await;
        let reply = self.receive().await;
        assert_eq!(reply["profileid"], profile_id, "{reply:?}");
        assert_eq!(reply["id"], "5");
        reply
    }
}
