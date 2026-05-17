pub mod meta;
pub mod overlay;
pub mod state;
pub use meta::SessionMeta;
pub use overlay::{DirtyEntry, DirtyFiles};
pub use state::{SessionState, StaleReason};
