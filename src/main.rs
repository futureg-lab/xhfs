use tracing_subscriber::EnvFilter;

pub mod block;
pub mod disk;

fn main() {
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("brutefs=DEBUG"))
        .unwrap();

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .without_time()
        .init();

    tracing::debug!("Hello world");
}
