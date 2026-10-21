//! One tokio runtime for all networking. GTK has its own loop on the
//! main thread; tokio futures run here and are only awaited from there.
//! A tokio `JoinHandle` can be awaited in any executor.

use std::future::Future;
use std::sync::OnceLock;

use tokio::runtime::Runtime;
use tokio::task::JoinHandle;

pub fn runtime() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("blick-net")
            .enable_all()
            .build()
            .expect("tokio runtime failed to start")
    })
}

/// Starts `fut` on the tokio runtime. A panic in the task becomes an error.
pub async fn spawn<T, F>(fut: F) -> anyhow::Result<T>
where
    T: Send + 'static,
    F: Future<Output = anyhow::Result<T>> + Send + 'static,
{
    runtime().spawn(fut).await?
}

pub fn spawn_handle<T, F>(fut: F) -> JoinHandle<T>
where
    T: Send + 'static,
    F: Future<Output = T> + Send + 'static,
{
    runtime().spawn(fut)
}
