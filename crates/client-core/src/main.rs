#[tokio::main]
async fn main() {
    if let Err(error) = skinos_core::daemon::serve_with_adapter(std::sync::Arc::new(
        skinos_core::operations::MvpOperations::default(),
    ))
    .await
    {
        eprintln!("SkinOs backend stopped: {error}");
        std::process::exit(1);
    }
}
