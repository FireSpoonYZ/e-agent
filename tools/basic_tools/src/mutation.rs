use std::{
    collections::HashMap,
    future::Future,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

use e_agent_tool::Result;
use tokio::sync::Mutex as AsyncMutex;

static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<AsyncMutex<()>>>>> = OnceLock::new();

pub async fn run<T, F, Fut>(path: &str, operation: F) -> Result<T>
where
    F: FnOnce(PathBuf) -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let absolute = std::path::absolute(Path::new(path))?;
    let lock = {
        let mut locks = LOCKS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        locks
            .get(&absolute)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| {
                let lock = Arc::new(AsyncMutex::new(()));
                locks.insert(absolute.clone(), Arc::downgrade(&lock));
                lock
            })
    };

    let guard = lock.clone().lock_owned().await;
    let result = operation(absolute.clone()).await;
    drop(guard);

    if Arc::strong_count(&lock) == 1 {
        let mut locks = LOCKS
            .get()
            .unwrap()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if locks
            .get(&absolute)
            .is_some_and(|entry| entry.ptr_eq(&Arc::downgrade(&lock)))
        {
            locks.remove(&absolute);
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

    use super::run;

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
