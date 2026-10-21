//! Eine tokio-Runtime für alles Netzwerk. GTK hat seinen eigenen Loop im
//! Hauptthread; tokio-Futures laufen hier und werden von dort nur abgewartet.
//! Ein `JoinHandle` von tokio lässt sich in jedem Executor awaiten.

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
            .expect("tokio-Runtime ließ sich nicht starten")
    })
}

/// Startet `fut` auf der tokio-Runtime. Ein Panic im Task wird zum Fehler.
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
