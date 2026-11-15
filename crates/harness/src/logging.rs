//! Where the core's `tracing` events go. Without a subscriber they go
//! nowhere, so every binary that wants them calls [`init`] once at startup.

pub use tracing::Level;

/// The environment variable that sets the level: `error`, `warn`, `info`,
/// `debug` or `trace`, in any letter case.
pub const LEVEL_VAR: &str = "PS_LEARN_LOG";

/// Writes `tracing` events at `default` and above to stderr, one line each,
/// unless [`LEVEL_VAR`] names another level. A plugin's `log.debug` is a
/// `debug` event with target `plugin`. A second call, or a subscriber the
/// host installed first, leaves things as they are.
pub fn init(default: Level) {
    let level = std::env::var(LEVEL_VAR)
        .ok()
        .and_then(|name| name.trim().parse::<Level>().ok())
        .unwrap_or(default);
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_max_level(level)
        .try_init();
}
