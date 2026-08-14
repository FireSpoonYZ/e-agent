use std::{
    collections::HashMap,
    future::Future,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

use e_agent_extension::Result;
use tokio::sync::Mutex as AsyncMutex;

static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<AsyncMutex<()>>>>> = OnceLock::new();
static BASE_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Directory every relative tool path resolves against.
///
/// ponytail: captured on first use instead of at module import, so a Python
/// `os.chdir()` before the very first tool call still wins; hook the extension
/// initializer if that ever matters.
pub fn base_dir() -> Result<&'static Path> {
    if let Some(dir) = BASE_DIR.get() {
        return Ok(dir);
    }
    let dir = std::env::current_dir()?;
    Ok(BASE_DIR.get_or_init(|| dir))
}

/// Expand a leading `~` and resolve the result against [`base_dir`].
pub fn resolve(path: &str) -> Result<PathBuf> {
    let expanded = expand_tilde(path);
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        base_dir()?.join(expanded)
    };
    Ok(std::path::absolute(absolute)?)
}

fn expand_tilde(path: &str) -> PathBuf {
    let rest = match path.strip_prefix('~') {
        Some("") => "",
        Some(rest) if rest.starts_with(['/', '\\']) => &rest[1..],
        _ => return PathBuf::from(path),
    };
    match std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        Some(home) => PathBuf::from(home).join(rest),
        None => PathBuf::from(path),
    }
}

pub async fn run<T, F, Fut>(path: &str, operation: F) -> Result<T>
where
    F: FnOnce(PathBuf) -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let absolute = resolve(path)?;
    // Symlink aliases must share one lock, so key on the canonical path when it exists.
    let key = tokio::fs::canonicalize(&absolute)
        .await
        .unwrap_or_else(|_| absolute.clone());
    let lock = {
        let mut locks = LOCKS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        locks.get(&key).and_then(Weak::upgrade).unwrap_or_else(|| {
            let lock = Arc::new(AsyncMutex::new(()));
            locks.insert(key.clone(), Arc::downgrade(&lock));
            lock
        })
    };

    let guard = lock.clone().lock_owned().await;
    let result = operation(absolute).await;
    drop(guard);

    if Arc::strong_count(&lock) == 1 {
        let mut locks = LOCKS
            .get()
            .unwrap()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if locks
            .get(&key)
            .is_some_and(|entry| entry.ptr_eq(&Arc::downgrade(&lock)))
        {
            locks.remove(&key);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;

    use super::{base_dir, resolve, run};

    #[test]
    fn resolves_relative_and_tilde_paths_against_a_fixed_base() {
        let base = base_dir().unwrap();
        assert_eq!(resolve("sub/file.txt").unwrap(), base.join("sub/file.txt"));
        assert!(resolve("~/file.txt").unwrap().is_absolute());
        assert!(!resolve("~/file.txt").unwrap().starts_with("~"));
    }

    #[test]
    fn serializes_operations_for_the_same_path() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let active = Arc::new(AtomicUsize::new(0));
            let first_active = active.clone();
            let first = tokio::spawn(run("mutation-lock-test", move |_| async move {
                assert_eq!(first_active.fetch_add(1, Ordering::SeqCst), 0);
                tokio::time::sleep(Duration::from_millis(50)).await;
                first_active.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            }));
            let second_active = active.clone();
            let second = tokio::spawn(run("mutation-lock-test", move |_| async move {
                assert_eq!(second_active.fetch_add(1, Ordering::SeqCst), 0);
                tokio::time::sleep(Duration::from_millis(50)).await;
                second_active.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            }));
            first.await.unwrap().unwrap();
            second.await.unwrap().unwrap();
        });
    }
}
