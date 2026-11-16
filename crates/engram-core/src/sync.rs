//! Poison-tolerant locking.
//!
//! A panic while a lock is held poisons it, and `.lock().unwrap()` then turns
//! that one panic into a panic on every later request — a long-lived core
//! would stay broken until restarted. Everything behind these locks is either
//! store-backed (the store commits or rolls back on its own) or a cache that
//! is safe to read half-updated, so the guard is recovered instead.

use std::ffi::{OsStr, OsString};
use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// `Mutex::lock` that recovers a poisoned guard.
pub trait MutexExt<T: ?Sized> {
    fn locked(&self) -> MutexGuard<'_, T>;
}

impl<T: ?Sized> MutexExt<T> for Mutex<T> {
    fn locked(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// `RwLock::read` / `write` that recover a poisoned guard.
pub trait RwLockExt<T: ?Sized> {
    fn reading(&self) -> RwLockReadGuard<'_, T>;
    fn writing(&self) -> RwLockWriteGuard<'_, T>;
}

impl<T: ?Sized> RwLockExt<T> for RwLock<T> {
    fn reading(&self) -> RwLockReadGuard<'_, T> {
        self.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn writing(&self) -> RwLockWriteGuard<'_, T> {
        self.write().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Test support: the process environment is global, so a test that points
/// `ENGRAM_HOME` (or any variable) elsewhere holds this guard. It serializes
/// such tests and restores every variable it touched on drop, panics included.
#[doc(hidden)]
pub struct EnvGuard {
    saved: Vec<(&'static str, Option<OsString>)>,
    _lock: MutexGuard<'static, ()>,
}

impl EnvGuard {
    pub fn lock() -> Self {
        static LOCK: Mutex<()> = Mutex::new(());
        Self {
            saved: Vec::new(),
            _lock: LOCK.locked(),
        }
    }

    pub fn set(&mut self, key: &'static str, value: impl AsRef<OsStr>) {
        self.remember(key);
        // SAFETY: every writer holds LOCK, and tests that write the
        // environment run their threads under that same guard.
        unsafe { std::env::set_var(key, value) };
    }

    pub fn remove(&mut self, key: &'static str) {
        self.remember(key);
        // SAFETY: as in `set`.
        unsafe { std::env::remove_var(key) };
    }

    fn remember(&mut self, key: &'static str) {
        if !self.saved.iter().any(|(k, _)| *k == key) {
            self.saved.push((key, std::env::var_os(key)));
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, old) in self.saved.drain(..).rev() {
            // SAFETY: still under LOCK.
            unsafe {
                match old {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn a_poisoned_mutex_still_hands_out_its_guard() {
        let m = Arc::new(Mutex::new(1));
        let m2 = m.clone();
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("poison it");
        })
        .join();
        assert!(m.is_poisoned());
        *m.locked() += 1;
        assert_eq!(*m.locked(), 2);
    }

    #[test]
    fn a_poisoned_rwlock_still_reads_and_writes() {
        let l = Arc::new(RwLock::new(String::from("a")));
        let l2 = l.clone();
        let _ = std::thread::spawn(move || {
            let _g = l2.write().unwrap();
            panic!("poison it");
        })
        .join();
        l.writing().push('b');
        assert_eq!(*l.reading(), "ab");
    }

    #[test]
    fn env_guard_restores_what_it_touched() {
        const KEY: &str = "ENGRAM_SYNC_TEST_VAR";
        {
            let mut env = EnvGuard::lock();
            env.set(KEY, "one");
            env.set(KEY, "two");
            assert_eq!(std::env::var(KEY).as_deref(), Ok("two"));
        }
        assert!(std::env::var_os(KEY).is_none());
    }
}
