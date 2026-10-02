//! Egyetlen háttérszálas dekódoló, megszakítható, prioritásos feladatsorral.
//!
//! A UI a `set_jobs` hívással mindig a teljes kívánt listát adja át. Ami kimarad belőle,
//! az törlődik; ha épp az fut, a dekóder a következő ellenőrzési pontnál megszakad.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};

use super::decode::{DecodeError, Target};

#[derive(Debug, Clone)]
pub struct Job {
    pub id: u64,
    pub path: PathBuf,
    pub target: Target,
    /// Kisebb szám = sürgősebb.
    pub prio: u8,
}

/// A platform dekódoló függvénye. `T` a UI-szálnak átadott, megjelenítésre kész eredmény.
pub type DecodeFn<T> = dyn Fn(&Job, &dyn Fn() -> bool) -> Result<T, DecodeError> + Send + Sync;

struct Queue {
    jobs: Vec<Job>,
    /// Lefutott feladatok, amelyek eredményét a UI még nem vette át. A kívánt listában
    /// ilyenkor még szerepelnek – újra lefuttatni viszont tiszta pazarlás lenne.
    done: Vec<u64>,
}

struct Shared {
    queue: Mutex<Queue>,
    cv: Condvar,
    running: AtomicU64,
    cancel: AtomicBool,
}

pub struct Worker<T> {
    shared: Arc<Shared>,
    pub results: Receiver<(u64, Result<T, DecodeError>)>,
}

impl<T: Send + 'static> Worker<T> {
    /// `thread_init` a szál elején fut (pl. COM inicializálás), `notify` minden eredmény után.
    pub fn spawn(thread_init: fn(), decode: Box<DecodeFn<T>>, notify: Box<dyn Fn() + Send>) -> Worker<T> {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue { jobs: Vec::new(), done: Vec::new() }),
            cv: Condvar::new(),
            running: AtomicU64::new(0),
            cancel: AtomicBool::new(false),
        });
        let (tx, rx) = channel();
        let s = shared.clone();
        std::thread::Builder::new()
            .name("decoder".into())
            .stack_size(1 << 20)
            .spawn(move || {
                thread_init();
                run(s, decode, tx, notify)
            })
            .expect("dekódoló szál indítása");
        Worker { shared, results: rx }
    }

    pub fn set_jobs(&self, mut jobs: Vec<Job>) {
        jobs.sort_by_key(|j| j.prio);
        // A futó feladat azonosítóját a zár alatt olvassuk: a szál is a zár alatt állítja be.
        let mut q = self.shared.queue.lock().unwrap();
        let running = self.shared.running.load(Ordering::Acquire);
        if running != 0 && !jobs.iter().any(|j| j.id == running) {
            self.shared.cancel.store(true, Ordering::Release);
        }
        // Amit a UI már nem kér, arra az elkészült eredmény sem kell többé.
        q.done.retain(|id| jobs.iter().any(|j| j.id == *id));
        jobs.retain(|j| j.id != running && !q.done.contains(&j.id));
        q.jobs = jobs;
        self.shared.cv.notify_one();
    }
}

fn run<T>(
    s: Arc<Shared>,
    decode: Box<DecodeFn<T>>,
    tx: Sender<(u64, Result<T, DecodeError>)>,
    notify: Box<dyn Fn() + Send>,
) {
    loop {
        let job = {
            let mut q = s.queue.lock().unwrap();
            while q.jobs.is_empty() {
                q = s.cv.wait(q).unwrap();
            }
            let job = q.jobs.remove(0);
            s.running.store(job.id, Ordering::Release);
            s.cancel.store(false, Ordering::Release);
            job
        };
        let cancel = || s.cancel.load(Ordering::Acquire);
        let result = decode(&job, &cancel);
        {
            // A zár alatt: különben egy közbeeső `set_jobs` még futónak hinné a feladatot,
            // utána viszont már nem látná elkészültnek – és újraindítaná.
            let mut q = s.queue.lock().unwrap();
            s.running.store(0, Ordering::Release);
            // Megszakított feladat nem hoz eredményt: azt szabad újra kérni.
            if !matches!(result, Err(DecodeError::Cancelled)) {
                q.done.push(job.id);
            }
        }
        if tx.send((job.id, result)).is_err() {
            return;
        }
        notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::Duration;

    fn job(id: u64) -> Job {
        Job { id, path: PathBuf::from("a.jpg"), target: Target::Full, prio: 0 }
    }

    /// A UI addig is a kívánt listában tartja a feladatot, amíg az eredményét át nem veszi
    /// (pl. a mappalistázás végén). Ilyenkor tilos újra lefuttatni.
    #[test]
    fn elkeszult_feladat_nem_fut_ujra() {
        let runs = Arc::new(AtomicU64::new(0));
        let r = runs.clone();
        let w: Worker<u64> = Worker::spawn(
            || {},
            Box::new(move |j, _| Ok(r.fetch_add(1, Ordering::SeqCst) + j.id)),
            Box::new(|| {}),
        );

        w.set_jobs(vec![job(1)]);
        assert_eq!(w.results.recv_timeout(Duration::from_secs(5)).unwrap().0, 1);

        w.set_jobs(vec![job(1)]);
        assert!(
            matches!(w.results.recv_timeout(Duration::from_millis(200)), Err(RecvTimeoutError::Timeout)),
            "a már elkészült feladat újrafutott"
        );

        // Ha a UI leveszi a listáról, az elkészült eredményről is elfelejtkezünk.
        w.set_jobs(Vec::new());
        w.set_jobs(vec![job(1)]);
        assert_eq!(w.results.recv_timeout(Duration::from_secs(5)).unwrap().0, 1);
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }
}
