//! Prints the session events nibwardend locks on (phase2.md §4.3), for checking a desktop
//! by hand: run it, lock the screen, unlock, then stop it with Ctrl+C.
//!
//!     cargo run -p nibwarden-platform --example session-events

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        if let Err(e) = nibwarden_platform::session_events::watch(tx).await {
            eprintln!("cannot watch session events: {e}");
            std::process::exit(1);
        }
    });
    eprintln!("watching; lock the screen now (Ctrl+C to stop)");
    while let Some(event) = events.recv().await {
        println!("{event:?}");
    }
}
