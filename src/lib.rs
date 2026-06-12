pub mod commands;
pub mod gh;
pub mod gitx;
pub mod resolve;
pub mod state;
pub mod store;
pub mod sync;
pub mod task;
pub mod ui;
pub mod util;

use anyhow::Result;
use gitx::Repo;
use store::Store;

/// Shared command context: the discovered repo and its resolved store.
pub struct Ctx {
    pub repo: Repo,
    pub store: Store,
}

impl Ctx {
    /// Discover the repo from cwd and resolve its store.
    /// Errors if not in a git repo or the repo is not initialized (`rein init`).
    pub fn load() -> Result<Ctx> {
        let cwd = std::env::current_dir()?;
        let repo = Repo::discover(&cwd)?;
        let store = Store::resolve(&repo)?;
        Ok(Ctx { repo, store })
    }
}
