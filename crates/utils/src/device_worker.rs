use std::future::Future;
use tokio::sync::oneshot;

/// Owns one OS thread and one Tokio reactor. Dropping the owner cancels the
/// worker, including its sockets and any device operations spawned on it.
pub(crate) struct DeviceWorker {
    stop: Option<oneshot::Sender<()>>,
    finished: oneshot::Receiver<()>,
}

impl DeviceWorker {
    pub(crate) fn spawn<F, Fut>(name: String, work: F) -> std::io::Result<Self>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + 'static,
    {
        let (stop, stopped) = oneshot::channel();
        let (finished, completion) = oneshot::channel();
        std::thread::Builder::new().name(name).spawn(move || {
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => {
                    runtime.block_on(async {
                        tokio::select! {
                            _ = stopped => {},
                            _ = work() => {},
                        }
                    });
                    drop(runtime);
                }
                Err(error) => log::error!("Could not start device runtime: {error}"),
            }
            let _ = finished.send(());
        })?;
        Ok(Self {
            stop: Some(stop),
            finished: completion,
        })
    }

    pub(crate) async fn shutdown(mut self) {
        self.stop.take();
        let _ = (&mut self.finished).await;
    }
}

impl Drop for DeviceWorker {
    fn drop(&mut self) {
        self.stop.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn devices_have_distinct_threads_and_shutdown_cancels_their_tasks() {
        let parent = std::thread::current().id();
        let (first_tx, first_rx) = oneshot::channel();
        let (second_tx, second_rx) = oneshot::channel();
        let first = DeviceWorker::spawn("test-device-1".into(), move || async move {
            first_tx.send(std::thread::current().id()).unwrap();
            std::future::pending::<()>().await;
        })
        .unwrap();
        let second = DeviceWorker::spawn("test-device-2".into(), move || async move {
            second_tx.send(std::thread::current().id()).unwrap();
            std::future::pending::<()>().await;
        })
        .unwrap();
        let (first_id, second_id) = tokio::join!(first_rx, second_rx);
        let (first_id, second_id) = (first_id.unwrap(), second_id.unwrap());
        assert_ne!(first_id, parent);
        assert_ne!(second_id, parent);
        assert_ne!(first_id, second_id);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            tokio::join!(first.shutdown(), second.shutdown());
        })
        .await
        .expect("device threads failed to shut down");
    }
}
