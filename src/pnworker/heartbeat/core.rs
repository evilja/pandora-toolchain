use std::collections::HashMap;
use std::fmt::Debug;
use std::future::Future;
use tokio::sync::mpsc::{Receiver, Sender, channel};
use tokio::task::JoinHandle;
use tokio::time::{Duration, Instant, sleep};
use crate::pnworker::core::Stage;
use crate::pnworker::messages::MessagePayload;

// CommData is the uniform heartbeat type all ShrineLayers emit back.
// (job_id, message payload, optional stage)
pub type CommData = (u64, MessagePayload, Option<Stage>);


// Worker is the key type for the Shrine's HashMap.
#[derive(Debug, Eq, Hash, PartialEq, Clone)]
pub enum Worker {
    Download,
    Encode,
    Upload,
    Probe,
}

// HeartStatus is returned by /hearts command.
pub struct HeartStatus {
    pub worker: Worker,
    pub alive: bool,
    pub last_beat_secs: u64,
    pub reboot_count: u32,
}

// The factory signature: given a Receiver<M> and Sender<CommData>, produce a JoinHandle.
// Stored as Box<dyn Fn(...)> so the Shrine can reboot without external help.
type Factory<M> = Box<dyn Fn(Receiver<M>, Sender<CommData>, Sender<()>) -> JoinHandle<()> + Send>;

struct TypedLayer<M> {
    sender: Sender<M>,
    return_receiver: Receiver<CommData>,
    pulse_receiver: Receiver<()>,      // dedicated heartbeat
    last_heartbeat: Instant,
    thread: JoinHandle<()>,
    reboot_count: u32,
    send_capacity: usize,
    recv_capacity: usize,
    factory: Factory<M>,
}

impl<M: Send + 'static> TypedLayer<M> {
    fn new<F, Fut>(
        worker_fn: F,
        send_capacity: usize,
        recv_capacity: usize,
    ) -> Self
    where
        F: Fn(Receiver<M>, Sender<CommData>, Sender<()>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let (tx_pulse, rx_pulse): (Sender<()>, Receiver<()>) = channel(1);
        let (tx_m, rx_m): (Sender<M>, Receiver<M>) = channel(send_capacity);
        let (tx_h, rx_h): (Sender<CommData>, Receiver<CommData>) = channel(recv_capacity);

        let factory: Factory<M> = Box::new(move |rx, tx, pulse| {
            tokio::spawn(worker_fn(rx, tx, pulse))
        });

        let thread = (factory)(rx_m, tx_h, tx_pulse);
        println!("[ShrineLayer] New offering");
        TypedLayer {
            sender: tx_m,
            return_receiver: rx_h,
            pulse_receiver: rx_pulse,
            last_heartbeat: Instant::now(),
            thread,
            reboot_count: 0,
            send_capacity,
            recv_capacity,
            factory,
        }
    }

    fn is_alive(&self) -> bool {
        !self.thread.is_finished()
    }

    fn heartbeat_expired(&self, limit_secs: u64) -> bool {
        self.last_heartbeat.elapsed().as_secs() > limit_secs
    }

    fn try_recv(&mut self) -> Option<CommData> {
        match self.return_receiver.try_recv() {
            Ok(msg) => {
                self.last_heartbeat = Instant::now(); // CommData also counts as heartbeat
                Some(msg)
            }
            Err(_) => None,
        }
    }

    fn abort(&self) {
        self.thread.abort();
    }

    async fn join_dead(&mut self) {
        let old = std::mem::replace(&mut self.thread, tokio::spawn(async {}));
        old.abort();
        let _ = tokio::time::timeout(Duration::from_millis(100), old).await;
        while self.return_receiver.try_recv().is_ok() {}
    }

    async fn reboot(&mut self) {
        self.join_dead().await;

        let (tx_pulse, rx_pulse): (Sender<()>, Receiver<()>) = channel(1);
        let (tx_m, rx_m): (Sender<M>, Receiver<M>) = channel(self.send_capacity);
        let (tx_h, rx_h): (Sender<CommData>, Receiver<CommData>) = channel(self.recv_capacity);

        self.thread = (self.factory)(rx_m, tx_h, tx_pulse);
        self.sender = tx_m;
        self.return_receiver = rx_h;
        self.pulse_receiver = rx_pulse;
        self.last_heartbeat = Instant::now();
        self.reboot_count += 1;
    }
}

// TypedShrine supervises worker layers and deliberately does not replay interrupted work.
// M is the WorkerMsg enum shared by every typed worker layer.
//
// Usage:
//   let mut shrine: TypedShrine<WorkerMsg> = TypedShrine::new();
//   shrine.layer(Worker::Download, pn_dloadworker, 5, 50);
//   shrine.send(&Worker::Download, WorkerMsg::Download(data)).await?;
//   while let Some((worker, comm)) = shrine.receive(500).await { ... }
pub struct TypedShrine<M: Send + 'static> {
    layers: HashMap<Worker, TypedLayer<M>>,
}

impl<M: Send + Clone + 'static> TypedShrine<M> {
    pub fn new() -> Self {
        TypedShrine {
            layers: HashMap::new(),
        }
    }

    pub async fn drain_heartbeats(&mut self) {
        for layer in self.layers.values_mut() {
            while layer.pulse_receiver.try_recv().is_ok() {
                layer.last_heartbeat = Instant::now();
            }
        }

        // Detect and reboot dead layers regardless of job state
        let dead: Vec<Worker> = self.layers.iter()
            .filter(|(_, l)| !l.is_alive() || l.heartbeat_expired(160))
            .map(|(w, _)| w.clone())
            .collect();

        for worker in &dead {
            self.reboot(&worker).await;
        }
    }
    // Register a worker with its factory function.
    // worker_fn must be Fn (not FnOnce) so it can be called again on reboot.
    pub fn layer<F, Fut>(
        &mut self,
        worker: Worker,
        worker_fn: F,
        send_capacity: usize,
        recv_capacity: usize,
    ) where
        F: Fn(Receiver<M>, Sender<CommData>, Sender<()>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.layers.insert(
            worker,
            TypedLayer::new(worker_fn, send_capacity, recv_capacity),
        );
    }

    pub async fn send(&mut self, worker: &Worker, msg: M) -> Result<(), String> {
        if !self.layers.contains_key(worker) {
            return Err(format!("[Shrine] {:?} layer not found", worker));
        }
        for attempt in 0..2 {
            if let Some(layer) = self.layers.get(worker) {
                if !layer.is_alive() || layer.heartbeat_expired(160) {
                    eprintln!("[Shrine] {:?} dead/expired — auto-rebooting before send", worker);
                    self.reboot(worker).await;
                }
            }
            match self.layers.get(worker) {
                Some(layer) => match layer.sender.send(msg.clone()).await {
                    Ok(()) => {
                        if attempt > 0 {
                            eprintln!("[Shrine] {:?} send recovered after auto-reboot", worker);
                        }
                        println!("[Shrine] Message sent to: {worker:?}");
                        return Ok(());
                    }
                    Err(_) if attempt == 0 => {
                        eprintln!("[Shrine] {:?} send hit closed channel — rebooting and retrying", worker);
                        self.reboot(worker).await;
                        continue;
                    }
                    Err(_) => {
                        return Err(format!("[Shrine] {:?} channel still closed after reboot", worker));
                    }
                },
                None => return Err(format!("[Shrine] {:?} layer vanished", worker)),
            }
        }
        Err(format!("[Shrine] {:?} send exhausted retries", worker))
    }

    // Poll all layers for a worker message, waiting up to timeout_ms.
    // A short sleep prevents the empty poll path from busy-spinning a runtime thread.
    pub async fn receive(&mut self, timeout_ms: u64) -> Option<(Worker, CommData)> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);

        loop {
            // Collect any pending heartbeats
            for (worker, layer) in self.layers.iter_mut() {
                if let Some(msg) = layer.try_recv() {
                    println!("[Shrine] Message sent from: {worker:?}");
                    return Some((worker.clone(), msg));
                }
                if Instant::now() >= deadline {
                    return None;
                }
            }

            // Detect dead or heartbeat-expired layers
            let dead: Vec<Worker> = self.layers.iter()
                .filter(|(_, l)| !l.is_alive() || l.heartbeat_expired(160))
                .map(|(w, _)| w.clone())
                .collect();

            for worker in &dead {
                self.reboot(worker).await;
            }

            if Instant::now() >= deadline {
                return None;
            }

            sleep(Duration::from_millis(1)).await;
        }
    }
    pub async fn force_reboot(&mut self, worker: &Worker) {
        self.reboot(worker).await;
    }
    pub async fn force_reboot_all(&mut self) {
        let workers: Vec<Worker> = self.layers.keys().cloned().collect();
        for worker in &workers {
            self.reboot(worker).await;
        }
    }
    pub fn reboot_epoch(&self, worker: &Worker) -> u32 {
        self.layers
            .get(worker)
            .map(|layer| layer.reboot_count)
            .unwrap_or(0)
    }
    // Rebooting intentionally starts an empty layer; interrupted jobs are not replayed.
    async fn reboot(&mut self, worker: &Worker) {
        if let Some(layer) = self.layers.get_mut(worker) {
            eprintln!(
                "[Shrine] {:?} dead or expired — rebooting (count: {})",
                worker,
                layer.reboot_count + 1
            );
            layer.reboot().await;
        }

    }
    pub async fn kill(&mut self) {
        let workers: Vec<Worker> = self.layers.keys().cloned().collect();
        for worker in &workers {
            if let Some(layer) = self.layers.get_mut(worker) {
            eprintln!(
                "[Shrine] {:?} Killed",
                worker,
            );
            layer.abort();
        }
        }
    }

    // /hearts — returns status of all layers for the Discord command.
    pub fn hearts(&self) -> Vec<HeartStatus> {
        self.layers.iter().map(|(worker, layer)| HeartStatus {
            worker: worker.clone(),
            alive: layer.is_alive() && !layer.heartbeat_expired(160),
            last_beat_secs: layer.last_heartbeat.elapsed().as_secs(),
            reboot_count: layer.reboot_count,
        }).collect()
    }
}

// Example usage (not compiled, for documentation):
//
// #[derive(Clone)]
// pub enum WorkerMsg {
//     Download(DownloadData),
//     Encode(EncodeData),
//     Upload(UploadData),
// }
//
// async fn pn_dloadworker(mut rx: Receiver<WorkerMsg>, tx: Sender<CommData>) {
//     while let Some(msg) = rx.recv().await {
//         if let WorkerMsg::Download((path, torrent, id)) = msg {
//             // ... do work ...
//             tx.send((id, "Downloading...".into(), Some(Stage::Downloading))).await.ok();
//         }
//         tokio::task::yield_now().await; // cancellation point
//     }
// }
//
// let mut shrine: TypedShrine<WorkerMsg> = TypedShrine::new();
// shrine.layer(Worker::Download, pn_dloadworker, 5, 50);
// shrine.layer(Worker::Encode,   pn_encdeworker, 5, 50);
// shrine.layer(Worker::Upload,   pn_uloadworker, 5, 50);
//
// shrine.send(&Worker::Download, WorkerMsg::Download(data)).await?;
//
// while let Some((worker, (job_id, msg, stage))) = shrine.receive(500).await {
//     println!("[{:?}] job={} msg={} stage={:?}", worker, job_id, msg, stage);
// }
//
// // Discord /hearts handler:
// for status in shrine.hearts() {
//     println!("{:?}: alive={} last_beat={}s reboots={}",
//         status.worker, status.alive, status.last_beat_secs, status.reboot_count);
// }
