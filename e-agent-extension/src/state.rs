//! Per-session extension state.
//!
//! The host owns session identity and passes it with every native call. Session
//! ids are never exposed as model-visible tool parameters.
//
// ponytail: one process-global current-session slot per cdylib, which matches
// the executor's serialized program execution; move to per-call context if
// independent sessions ever execute extension code concurrently.

use std::sync::{
    OnceLock,
    atomic::{AtomicU64, Ordering},
};

use dashmap::{DashMap, mapref::one::RefMut};
use serde::{Deserialize, Serialize};

/// Process-local identity of one host session.
#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
pub struct SessionId(pub u64);

impl SessionId {
    /// Allocate a stable host session id, optionally restoring one persisted in the file name.
    pub fn from_persisted(value: u64) -> Self {
        Self(value)
    }

    /// Allocate the next process-local session id.
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

static CURRENT: AtomicU64 = AtomicU64::new(0);

/// Set the session whose state stateful tools should use.
pub fn set_current_session(session: SessionId) {
    CURRENT.store(session.0, Ordering::SeqCst);
}

/// Clear the current session, restoring the default slot.
pub fn clear_current_session() {
    CURRENT.store(0, Ordering::SeqCst);
}

/// The session whose state stateful tools currently use.
pub fn current_session() -> SessionId {
    SessionId(CURRENT.load(Ordering::SeqCst))
}

/// One in-memory state object per session, owned by a single extension.
pub struct SessionStates<S> {
    states: OnceLock<DashMap<SessionId, S>>,
}

impl<S> Default for SessionStates<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> SessionStates<S> {
    pub const fn new() -> Self {
        Self {
            states: OnceLock::new(),
        }
    }
}

impl<S: Default + Send + Sync + 'static> SessionStates<S> {
    fn states(&self) -> &DashMap<SessionId, S> {
        self.states.get_or_init(DashMap::new)
    }

    /// Borrow the current session's state, creating it on first use.
    ///
    /// The returned guard keeps the entry locked, so a stateful tool must not
    /// call another stateful tool of the same extension while holding it.
    pub fn current(&'static self) -> RefMut<'static, SessionId, S> {
        self.states().entry(current_session()).or_default()
    }

    /// Remove one session's state; other sessions keep theirs.
    pub fn drop_session(&self, session: SessionId) {
        self.states().remove(&session);
    }
}

/// The current-session slot and state maps are process wide, so tests that
/// touch them must not run concurrently.
#[cfg(test)]
pub(crate) fn test_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::{SessionId, SessionStates, clear_current_session, set_current_session, test_guard};

    #[test]
    fn keeps_state_per_session_and_drops_one_session() {
        let _guard = test_guard();
        static STATES: SessionStates<Vec<u8>> = SessionStates::new();
        let first = SessionId::next();
        let second = SessionId::next();
        assert_ne!(first, second);

        set_current_session(first);
        STATES.current().push(1);
        STATES.current().push(2);
        assert_eq!(*STATES.current(), vec![1, 2]);

        set_current_session(second);
        assert!(STATES.current().is_empty());
        STATES.current().push(9);

        set_current_session(first);
        assert_eq!(*STATES.current(), vec![1, 2]);

        STATES.drop_session(first);
        assert!(STATES.current().is_empty());
        set_current_session(second);
        assert_eq!(*STATES.current(), vec![9]);
        clear_current_session();
    }
}
