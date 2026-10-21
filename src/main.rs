use std::{
    env,
    error::Error,
    io::{self, IsTerminal},
};

use recharged_wfc_srv::App;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tokio::net::{TcpListener, UdpSocket};
use tracing::info;
use tracing_subscriber::EnvFilter;

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .with_ansi(io::stderr().is_terminal())
        .init();
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    init_tracing();

    let admin = recharged_wfc_srv::admin::Config::from_env()?;

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
    info!(
        "recharged-wfc-srv listening on {} (HTTP), {} (GPCM) and {}/udp (master) (revision {})",
        http.local_addr()?,
        gpcm.local_addr()?,
        master.local_addr()?,
        recharged_wfc_srv::REVISION
    );

    match &admin {
        Some(config) => info!("Admin dashboard at {}/admin", config.public_url),
        None => info!(
            "Admin dashboard disabled: set {} to enable it",
            recharged_wfc_srv::admin::VARIABLES.join(", ")
        ),
    }

    let app = App::new(pool);
    let router = app.router(admin);
    // HTTP stops gracefully on a signal; the others stop with it.
    tokio::select! {
        result = axum::serve(http, router).with_graceful_shutdown(shutdown()) => result?,
        result = app.serve_gpcm(gpcm) => result?,
        result = app.serve_master(master) => result?,
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
