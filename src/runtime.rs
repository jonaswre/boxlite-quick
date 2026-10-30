use boxlite::BoxliteRuntime;
use boxlite::runtime::options::BoxliteOptions;

/// The one place boxq opens the BoxLite runtime, which takes the `BOXLITE_HOME` lock.
///
/// Callers hold the runtime only long enough to find, create, start, stop, remove or list a
/// Box, then drop it before any long-lived work (jonaswre/boxlite-quick#8). Waiting on a busy
/// lock instead of failing belongs here too, and arrives with the Sandbox lifecycle.
pub fn open() -> anyhow::Result<BoxliteRuntime> {
    Ok(BoxliteRuntime::new(BoxliteOptions::default())?)
}
