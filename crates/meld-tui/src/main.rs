use std::io;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, EnvFilter};

fn init_logging() -> Option<WorkerGuard> {
    let repo_path = std::env::current_dir().ok()?.join(".meld");
    let _ = std::fs::create_dir_all(&repo_path);
    let log_path = repo_path.join("meld.log");

    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .ok()?;
    let (non_blocking, guard) = tracing_appender::non_blocking(file);

    let env_filter = EnvFilter::try_from_env("RUST_LOG").unwrap_or_else(|_| {
        EnvFilter::new(
            "meld=debug,meld_tui=debug,meld_engine=debug,meld_adapter=debug,sqlx=warn",
        )
    });

    let _ = fmt::Subscriber::builder()
        .with_writer(non_blocking)
        .with_env_filter(env_filter)
        .with_ansi(false)
        .with_target(false)
        .try_init();

    Some(guard)
}

fn main() -> Result<(), io::Error> {
    let _guard = init_logging();

    meld_tui::app::run()
}
