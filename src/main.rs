use std::{
    env,
    error::Error,
    io::{self, IsTerminal},
};

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
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

    // DATABASE_URL in local development; the container provides PG* instead.
    let database = match env::var("DATABASE_URL") {
        Ok(url) => url.parse()?,
        Err(_) => PgConnectOptions::new(),
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect_with(database)
        .await?;
    sqlx::migrate!().run(&pool).await?;

    let port: u16 = env::var("PORT")
        .unwrap_or_else(|_| "8080".to_owned())
        .parse()?;
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    info!(
        "recharged-wfc-srv listening on {} (revision {})",
        listener.local_addr()?,
        recharged_wfc_srv::REVISION
    );

    axum::serve(listener, recharged_wfc_srv::router(pool))
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
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
