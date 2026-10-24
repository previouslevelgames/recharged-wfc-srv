use std::{
    env,
    error::Error,
    io::{self, IsTerminal},
    net::SocketAddr,
    num::NonZeroUsize,
};

use recharged_wfc_srv::App;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tokio::net::{TcpListener, UdpSocket};
use tracing::info;
use tracing_appender::{
    non_blocking::WorkerGuard,
    rolling::{RollingFileAppender, Rotation},
};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// Logs to stderr and, with `LOG_DIR`, to one file per day (UTC) there. The
/// guard flushes the file when dropped.
fn init_tracing() -> Result<Option<WorkerGuard>, Box<dyn Error>> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let stderr = fmt::layer()
        .with_writer(io::stderr)
        .with_ansi(io::stderr().is_terminal());

    let (file, guard) = match var("LOG_DIR") {
        Some(dir) => {
            let appender = RollingFileAppender::builder()
                .rotation(Rotation::DAILY)
                .filename_prefix(recharged_wfc_srv::LOG_FILE_PREFIX)
                .filename_suffix(recharged_wfc_srv::LOG_FILE_SUFFIX)
                .max_log_files(retention_days()?.get())
                .build(&dir)
                .map_err(|error| format!("LOG_DIR {dir}: {error}"))?;
            let (writer, guard) = tracing_appender::non_blocking(appender);
            (
                Some(fmt::layer().with_writer(writer).with_ansi(false)),
                Some(guard),
            )
        }
        None => (None, None),
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(stderr)
        .with(file)
        .init();
    Ok(guard)
}

/// How many daily files `LOG_DIR` keeps: `LOG_RETENTION_DAYS`, or 14.
fn retention_days() -> Result<NonZeroUsize, Box<dyn Error>> {
    match var("LOG_RETENTION_DAYS") {
        Some(days) => days
            .parse()
            .map_err(|error| format!("LOG_RETENTION_DAYS: {error}").into()),
        None => Ok(NonZeroUsize::new(14).unwrap()),
    }
}

/// The variable `name`, unless it is unset or empty.
fn var(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let _logs = init_tracing()?;

    let admin = recharged_wfc_srv::admin::Config::from_env()?;
    let profiles = recharged_wfc_srv::profiles::Config::from_env()?;

    // DATABASE_URL in local development; the container provides PG* instead.
    let database = match env::var("DATABASE_URL") {
        Ok(url) => url.parse()?,
        Err(_) => PgConnectOptions::new(),
    }
    // Names the server's connections in pg_stat_activity and on /admin.
    .application_name("recharged-wfc-srv");
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect_with(database)
        .await?;
    sqlx::migrate!().run(&pool).await?;

    let http = TcpListener::bind(("0.0.0.0", port("PORT", 8080)?)).await?;
    let gpcm = TcpListener::bind(("0.0.0.0", port("GPCM_PORT", 29900)?)).await?;
    let master = UdpSocket::bind(("0.0.0.0", port("MASTER_PORT", 27900)?)).await?;
    let browser = TcpListener::bind(("0.0.0.0", port("SERVER_BROWSER_PORT", 28910)?)).await?;
    let natneg = UdpSocket::bind(("0.0.0.0", port("NATNEG_PORT", 27901)?)).await?;
    info!(
        "recharged-wfc-srv listening on {} (HTTP), {} (GPCM), {}/udp (master), {} (server browser) and {}/udp (NAT negotiation) (revision {})",
        http.local_addr()?,
        gpcm.local_addr()?,
        master.local_addr()?,
        browser.local_addr()?,
        natneg.local_addr()?,
        recharged_wfc_srv::REVISION
    );

    match &admin {
        Some(config) => info!("Admin dashboard at {}/admin", config.public_url),
        None => info!(
            "Admin dashboard disabled: set {} to enable it",
            recharged_wfc_srv::admin::VARIABLES.join(", ")
        ),
    }

    match &profiles {
        Some(config) => info!(
            "Profiles at {}/api/v1 (sign-in: {})",
            config.public_url,
            match (config.discord.is_some(), config.test_sign_in) {
                (true, true) => "Discord and test names",
                (true, false) => "Discord",
                _ => "test names only",
            }
        ),
        None => info!(
            "Profiles disabled: set {} to enable them",
            recharged_wfc_srv::profiles::VARIABLES.join(", ")
        ),
    }

    let app = App::new(pool);
    let router = app.router(admin, profiles);
    // HTTP stops gracefully on a signal; the others stop with it.
    tokio::select! {
        result = axum::serve(http, router.into_make_service_with_connect_info::<SocketAddr>())
            .with_graceful_shutdown(shutdown()) => result?,
        result = app.serve_gpcm(gpcm) => result?,
        result = app.serve_master(master) => result?,
        result = app.serve_server_browser(browser) => result?,
        result = app.serve_natneg(natneg) => result?,
    }
    Ok(())
}

/// The port in the variable `name`, or `default`.
fn port(name: &str, default: u16) -> Result<u16, Box<dyn Error>> {
    match env::var(name) {
        Ok(port) => port
            .parse()
            .map_err(|error| format!("{name}: {error}").into()),
        Err(_) => Ok(default),
    }
}

async fn shutdown() {
    let interrupt = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to listen for Ctrl+C");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to listen for SIGTERM")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = interrupt => {},
        () = terminate => {},
    }
    info!("Shutting down");
}
