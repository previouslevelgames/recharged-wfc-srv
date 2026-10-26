//! Registration and login as the port does them (docs/login.md): NAS
//! `acctcreate` and `login` over HTTP, GPCM over TCP with friend requests,
//! the master server's availability check and heartbeats over UDP, and the
//! server browser's lookup of a host and its messages to one over TCP, NAT
//! negotiation over UDP, and a Recharged build's match reports over HTTP
//! (docs/reports.md).

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
async fn friends_who_add_each_other_see_each_other(pool: PgPool) {
    let server = Server::start(pool).await;
    let mut alice = Player::log_in(&server, 42).await;
    let mut bob = Player::log_in(&server, 43).await;
    alice.status(1, "QWxpY2U*").await;
    bob.status(1, "Qm9i").await;

    // Alice asks first: Bob has not asked for her, so nothing comes back.
    alice.add(&bob.profile_id).await;
    alice.client.send(r"\ka\\final\").await;
    assert!(alice.client.receive().await.contains_key("ka"));

    // Bob asks too: each gets the other's acceptance and status.
    bob.add(&alice.profile_id).await;
    let (alice_id, bob_id) = (alice.profile_id.clone(), bob.profile_id.clone());
    for (player, friend, locstring) in [
        (&mut bob, &alice_id, "QWxpY2U*"),
        (&mut alice, &bob_id, "Qm9i"),
    ] {
        let accepted = player.client.receive().await;
        assert_eq!((accepted["bm"].as_str(), &accepted["f"]), ("4", friend));
        let status = player.client.receive().await;
        assert_eq!((status["bm"].as_str(), &status["f"]), ("100", friend));
        assert_eq!(
            status["msg"],
            format!("|s|1|ss||ls|{locstring}|ip|0|p|0|qm|0")
        );
    }

    // A new status goes to the friend; leaving sends the offline one.
    alice.status(2, "UGxheQ**").await;
    assert!(bob.client.receive().await["msg"].starts_with("|s|2|ss||ls|UGxheQ**|"));
    bob.client.send(r"\logout\\final\").await;
    let offline = alice.client.receive().await;
    assert_eq!(offline["f"], bob_id);
    assert!(
        offline["msg"].starts_with("|s|0|ss|Offline|"),
        "{offline:?}"
    );
}

#[sqlx::test]
async fn a_deleted_friend_sees_the_player_offline(pool: PgPool) {
    let server = Server::start(pool).await;
    let mut alice = Player::log_in(&server, 42).await;
    let mut bob = Player::log_in(&server, 43).await;
    alice.add(&bob.profile_id).await;
    bob.add(&alice.profile_id).await;
    // Each gets the other's acceptance; neither has sent a status.
    assert_eq!(bob.client.receive().await["bm"], "4");
    assert_eq!(alice.client.receive().await["bm"], "4");

    let (alice_id, bob_id) = (alice.profile_id.clone(), bob.profile_id.clone());
    for friend in ["x", &bob_id] {
        let message = format!(
            r"\delbuddy\\sesskey\{}\delprofileid\{friend}\final\",
            alice.sesskey
        );
        alice.client.send(&message).await;
    }
    let offline = bob.client.receive().await;
    assert_eq!(offline["f"], alice_id);
    assert!(
        offline["msg"].starts_with("|s|0|ss|Offline|"),
        "{offline:?}"
    );

    // Neither answer nor status comes after that.
    alice.status(1, "U0MyAQ**").await;
    for player in [&mut alice, &mut bob] {
        player.client.send(r"\ka\\final\").await;
        let reply = player.client.receive().await;
        assert!(reply.contains_key("ka"), "{reply:?}");
    }
}

#[sqlx::test]
async fn matchmaking_messages_pass_between_mutual_friends(pool: PgPool) {
    let server = Server::start(pool).await;
    let mut alice = Player::log_in(&server, 42).await;
    let mut bob = Player::log_in(&server, 43).await;
    alice.add(&bob.profile_id).await;
    bob.add(&alice.profile_id).await;
    assert_eq!(bob.client.receive().await["bm"], "4");
    assert_eq!(alice.client.receive().await["bm"], "4");

    // A reservation, as the port's DWC sent it: command byte 1, then the
    // values.
    let reservation = "GPCM3vMAT\u{1}3/16781740/33007";
    for (to, msg) in [
        ("1000000099", reservation),
        (bob.profile_id.as_str(), "not DWC's"),
        (bob.profile_id.as_str(), reservation),
    ] {
        let message = format!(r"\bm\1\sesskey\{}\t\{to}\msg\{msg}\final\", alice.sesskey);
        alice.client.send(&message).await;
    }
    let relayed = bob.client.receive().await;
    assert_eq!(relayed["bm"], "1");
    assert_eq!(relayed["f"], alice.profile_id);
    assert_eq!(relayed["msg"], reservation);

    // Nothing comes back to the sender.
    alice.client.send(r"\ka\\final\").await;
    assert!(alice.client.receive().await.contains_key("ka"));
}

#[sqlx::test]
async fn a_logged_in_game_reports_its_series(pool: PgPool) {
    let server = Server::start(pool).await;
    let mut player = Player::log_in(&server, 42).await;
    let report = serde_json::json!({
        "profile_id": player.profile_id.parse::<i32>().unwrap(), "release": "R4QE",
        "event": "start", "series": 1_792_000_000_i64, "mode": "friends", "best_of": 1,
        "side": "home", "mii": "Mario", "opponent_mii": "Luigi",
        "captains": {"home": "mario", "away": "luigi"},
        "sidekicks": {"home": [], "away": []}, "game": null, "reason": null,
    })
    .to_string();

    // From the address of its GPCM login: this test's.
    let (status, body) = server.report(&report).await;
    assert_eq!(status, 200, "{body}");
    let answer: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(answer["match_id"].as_i64().is_some(), "{body}");

    player.client.send(r"\logout\\final\").await;
    assert!(player.client.closed().await);
    let (status, body) = server.report(&report).await;
    assert_eq!(
        (status, body.as_str()),
        (
            403,
            r#"{"error":"profile not logged in from this address"}"#
        )
    );
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

#[tokio::test]
async fn the_server_browser_finds_a_hosts_session() {
    let master = Master::start().await;
    master
        .send(b"\x03\x01\x02\x03\x04localip0\x00192.168.1.131\x00localport\x0051021\x00natneg\x001\x00statechanged\x001\x00gamename\x00mschargedwii\x00dwc_pid\x001000000000\x00dwc_mtype\x002\x00\x00\x00")
        .await;

    // The port's request for its host's session, and one for nobody.
    let lookup = b"\x00\x8d\x00\x01\x03\x00\x00\x00\x00mschargedwii\x00mschargedwii\x00Y];98goldwc_pid = 1000000000\x00\\numplayers\\maxplayers\\dwc_pid\\dwc_mtype\\dwc_mresv\\dwc_mver\\dwc_eval\x00\x00\x00\x00\x80\x06\x00\x00\x00";
    let found = master.list(lookup).await;
    let nobody = master
        .list(&lookup.map(|byte| if byte == b'0' { b'1' } else { byte }))
        .await;
    // The encryption's header: 5 bytes, then 13 more of key.
    for reply in [&found, &nobody] {
        assert_eq!((reply[0] ^ 0xEC, reply[6] ^ 0xEA), (5, 13), "{reply:?}");
    }
    // The host's session, 17 bytes and its 7 fields, is in the first only.
    assert!(
        found.len() > nobody.len() + 17,
        "{} {}",
        found.len(),
        nobody.len()
    );
}

#[tokio::test]
async fn the_server_browser_passes_messages_on_until_acked() {
    let master = Master::start().await;
    master
        .send(b"\x03\x01\x02\x03\x04localip0\x00192.168.1.131\x00localport\x0051021\x00statechanged\x001\x00gamename\x00mschargedwii\x00dwc_pid\x001000000000\x00\x00\x00")
        .await;

    // The port's message to its host: a match command (SBCM), command 6.
    let message: &[u8] = b"SBCM\x03\x00\x00\x00\x06\x08\x5b\xa3\xac\x11\x00\x01\x01\xca\x9a\x3b\xc0\xa8\x01\x83\x2a\xdf\x00\x00";
    let host = master.client.local_addr().unwrap();
    let mut request = vec![0, 0, 2, 127, 0, 0, 1];
    request.extend_from_slice(&host.port().to_be_bytes());
    request.extend_from_slice(message);
    request[1] = request.len() as u8;
    let mut stream = TcpStream::connect(master.browser).await.unwrap();
    stream.write_all(&request).await.unwrap();

    // FE FD 06, the instance key, a message key and the message; again
    // until the game acks it.
    let mut packet = [0; 1500];
    let mut keys = Vec::new();
    for _ in 0..2 {
        let len = master.client.recv(&mut packet).await.unwrap();
        assert_eq!(packet[..7], *b"\xFE\xFD\x06\x01\x02\x03\x04");
        assert_eq!(&packet[11..len], message);
        keys.push(packet[7..11].to_vec());
    }
    assert_eq!(keys[0], keys[1]);
    let mut ack = b"\x07\x01\x02\x03\x04".to_vec();
    ack.extend_from_slice(&keys[0]);
    master.client.send_to(&ack, master.address).await.unwrap();
    let again = tokio::time::timeout(
        std::time::Duration::from_millis(1500),
        master.client.recv(&mut packet),
    )
    .await;
    assert!(again.is_err(), "sent again after the ack");
}

#[tokio::test]
async fn nat_negotiation_gives_each_game_the_others_address() {
    use tokio::net::UdpSocket;

    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy_with(sqlx::postgres::PgConnectOptions::new());
    let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let natneg = server.local_addr().unwrap();
    tokio::spawn(async move { App::new(pool).serve_natneg(server).await });

    let packet = |kind: u8, body: &[u8]| {
        let mut packet = b"\xFD\xFC\x1E\x66\x6A\xB2\x03".to_vec();
        packet.push(kind);
        packet.extend_from_slice(&0xC0FF_EE00u32.to_be_bytes());
        packet.extend_from_slice(body);
        packet
    };
    async fn recv(socket: &UdpSocket) -> Vec<u8> {
        let mut packet = [0; 512];
        let len = socket.recv(&mut packet).await.unwrap();
        packet[..len].to_vec()
    }

    // Each game: its game socket's INIT, then its negotiation socket's three.
    let mut games = Vec::new();
    for index in 0..2u8 {
        let game = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let negotiate = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        for port_type in 0..4u8 {
            let socket = if port_type == 0 { &game } else { &negotiate };
            let init = packet(0, &[port_type, index, 1, 127, 0, 0, 1, 0, 0, b'm', 0]);
            socket.send_to(&init, natneg).await.unwrap();
        }
        games.push((game, negotiate));
    }

    // INIT_ACKs to each socket, and once both games are known, a CONNECT
    // with the other game's game socket to each negotiation socket.
    for (index, (game, negotiate)) in games.iter().enumerate() {
        let ack = recv(game).await;
        assert_eq!((ack[7], ack[12], ack[13]), (1, 0, index as u8));
        let other = games[1 - index].0.local_addr().unwrap();
        let (mut acks, mut connects) = (Vec::new(), 0);
        while acks.len() < 3 || connects == 0 {
            let packet = recv(negotiate).await;
            match packet[7] {
                1 => acks.push(packet[12]),
                5 => {
                    assert_eq!(packet[12..16], [127, 0, 0, 1]);
                    assert_eq!(packet[16..18], other.port().to_be_bytes());
                    assert_eq!(packet[19], 0, "no error");
                    connects += 1;
                }
                kind => panic!("packet type {kind}"),
            }
        }
        acks.sort();
        assert_eq!(acks, [1, 2, 3]);
    }
    // Only the first acks; the second gets its CONNECT again.
    let first = &games[0].1;
    first.send_to(&packet(6, &[0, 0]), natneg).await.unwrap();
    assert_eq!(recv(&games[1].1).await[7], 5);

    // The report, acked.
    let mut report = vec![1, 0, 1, 2, 0, 0, 0, 1, 0, 0, 0];
    report.extend_from_slice(&[0; 50]);
    first.send_to(&packet(0x0D, &report), natneg).await.unwrap();
    let mut ack = recv(first).await;
    while ack[7] == 5 {
        ack = recv(first).await;
    }
    assert_eq!(ack[7], 0x0E);
}

/// The master server on a local UDP port with its server browser on a TCP
/// port, and a client socket.
struct Master {
    address: SocketAddr,
    browser: SocketAddr,
    client: tokio::net::UdpSocket,
}

impl Master {
    async fn start() -> Self {
        use tokio::net::UdpSocket;

        // The master server and the server browser keep nothing in the
        // database.
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy_with(sqlx::postgres::PgConnectOptions::new());
        let app = App::new(pool);
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let browser = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let master = Self {
            address: socket.local_addr().unwrap(),
            browser: browser.local_addr().unwrap(),
            client: UdpSocket::bind("127.0.0.1:0").await.unwrap(),
        };
        let browsing = app.clone();
        tokio::spawn(async move { app.serve_master(socket).await });
        tokio::spawn(async move { browsing.serve_server_browser(browser).await });
        master
    }

    /// A list request to the server browser; the encrypted reply.
    async fn list(&self, request: &[u8]) -> Vec<u8> {
        let mut stream = TcpStream::connect(self.browser).await.unwrap();
        stream.write_all(request).await.unwrap();
        let mut reply = vec![0; 4096];
        let len = stream.read(&mut reply).await.unwrap();
        reply.truncate(len);
        reply
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
        let router = app.router(None, None);
        tokio::spawn(async move {
            axum::serve(
                http,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
        });
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

    /// `POST /api/v1/wfc/reports` as the port sends it, to the server's
    /// address rather than a host name. Returns the status and the body.
    async fn report(&self, report: &str) -> (u16, String) {
        let request = format!(
            "POST /api/v1/wfc/reports HTTP/1.1\r\nHost: 2.31.48.136\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{report}",
            report.len()
        );
        let mut stream = TcpStream::connect(self.http).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        let (head, body) = response.split_once("\r\n\r\n").unwrap();
        let status = head.split(' ').nth(1).unwrap().parse().unwrap();
        (status, body.to_owned())
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

/// A logged-in GP client of its own account.
struct Player {
    client: Client,
    profile_id: String,
    sesskey: String,
}

impl Player {
    async fn log_in(server: &Server, user_id: i64) -> Self {
        let (token, challenge) = server.nas_login(user_id).await;
        let mut client = Client::connect(server.gpcm).await;
        let login = client.log_in(&token, &challenge).await;
        Self {
            client,
            profile_id: login["profileid"].clone(),
            sesskey: login["sesskey"].clone(),
        }
    }

    async fn status(&mut self, status: u8, locstring: &str) {
        let message = format!(
            r"\status\{status}\sesskey\{}\statstring\\locstring\{locstring}\final\",
            self.sesskey
        );
        self.client.send(&message).await;
    }

    async fn add(&mut self, profile_id: &str) {
        let message = format!(
            r"\addbuddy\\sesskey\{}\newprofileid\{profile_id}\reason\\final\",
            self.sesskey
        );
        self.client.send(&message).await;
    }
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
