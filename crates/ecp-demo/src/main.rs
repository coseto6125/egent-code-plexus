use ecp_demo::app::{router, AppState};
use ecp_demo::Config;
use std::net::SocketAddr;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::from_env()?;
    let state = Arc::new(AppState::from_config(&config)?);
    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!(
        "ecp-demo {} listening on {addr} (bin={}, repos={})",
        env!("CARGO_PKG_VERSION"),
        config.bin.display(),
        config.repos_dir.display()
    );
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    Ok(())
}
