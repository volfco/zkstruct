use zookeeper::{ZooKeeper, WatchedEvent, WatchedEventType, ZkError, ZkResult};
use std::sync::{Arc, RwLock};
use serde::{Serialize};
use serde::de::DeserializeOwned;
pub use treediff::{value::Key, diff, tools::ChangeType};
use std::time::{Instant, Duration};
use std::{thread, fmt};
use std::sync::RwLockReadGuard;
use serde_json::Value;
use crossbeam_channel::Receiver;
use std::fmt::Debug;

const MAX_TIMING_DELTA: i64 = 30000; // ms
const LOCK_POLL_INTERVAL: u64 = 5; // ms
const LOCK_POLL_TIMEOUT: u64 = 1000; // ms

#[derive(Debug)]
pub enum ZkStructError {
    /// Timed out when trying to lock the struct for writing
    LockAcquireTimeout,
    StaleRead,
    /// The expected version of the object does not match the remote version.
    StaleWrite,

    ZkError(ZkError),
    Poisoned
}
impl From<ZkError> for ZkStructError {
    fn from(error: ZkError) -> ZkStructError {
        ZkStructError::ZkError(error)
    }
}

#[derive(Debug)]
struct InternalState {
    /// Path of the ZkStruct Dir
    zk_path: String,

    /// Known epoch of the local object. Compared to the remote epoch
    epoch: i32,
    /// Last time the inner object was sync
    timings: chrono::DateTime<chrono::Utc>,
    
    emit_updates: bool,

    chan_rx: crossbeam_channel::Receiver<Change<Key, serde_json::Value>>,
    chan_tx: crossbeam_channel::Sender<Change<Key, serde_json::Value>>,
}

#[derive(Clone)]
pub struct ZkState<T: Serialize + DeserializeOwned + Send + Sync> {
    /// ZooKeeper client
    zk: Arc<ZooKeeper>,
    id: String,

    inner: Arc<RwLock<T>>,
    state: Arc<RwLock<InternalState>>
}
impl<T: Serialize + DeserializeOwned + Send + Sync + 'static> ZkState<T> {
    pub fn new(zk: Arc<ZooKeeper>, zk_path: String, initial_state: T) -> anyhow::Result<Self> {
        let instance_id = uuid::Uuid::new_v4();
        log::debug!("starting zkstate");

        let (chan_tx, chan_rx) = crossbeam_channel::unbounded();
        let r = Self {
            id: instance_id.to_string(),
            zk,
            inner: Arc::new(RwLock::new(initial_state)),
            state: Arc::new(RwLock::new(InternalState {
                zk_path,
                epoch: 0,
                timings: chrono::Utc::now(),
                emit_updates: true,
                chan_rx,
                chan_tx
            }))
        };
        r.initialize()?;
        Ok(r)
    }

    pub fn expect(zk: Arc<ZooKeeper>, zk_path: String) -> anyhow::Result<Self> {
        let instance_id = uuid::Uuid::new_v4();
        log::debug!("starting zkstate");

        // block, waiting for our initial state to show up in zookeeper
        let (l_tx, l_rx) = crossbeam_channel::unbounded();
        let raw_data = zk.get_data_w(format!("{}/payload", &zk_path).as_str(), move |_| {
            let _ = l_tx.send(());
        });

        let data;
        if let Ok(inner) = raw_data {
            data = inner.0;
        } else {
            let _ = l_rx.recv();
            data = zk.get_data(format!("{}/payload", &zk_path).as_str(), false)?.0;
        }

        let (chan_tx, chan_rx) = crossbeam_channel::unbounded();
        let r = Self {
            id: instance_id.to_string(),
            zk,
            inner: Arc::new(RwLock::new(serde_json::from_slice(data.as_slice()).unwrap())),
            state: Arc::new(RwLock::new(InternalState {
                zk_path,
                epoch: 0,
                timings: chrono::Utc::now(),
                emit_updates: true,
                chan_rx,
                chan_tx
            }))
        };
        r.initialize()?;
        Ok(r)
    }

    fn initialize(&self) -> ZkResult<()> {
        let path = format!("{}/payload", &self.state.read().unwrap().zk_path);
        // if the path doesn't exist, let's make it and populate it
        if self.zk.exists(path.as_str(), false).unwrap().is_none() {
            log::debug!("{} does not exist, creating", &path);
            self.zk.create(&self.state.read().unwrap().zk_path, vec![], zookeeper::Acl::open_unsafe().clone(), zookeeper::CreateMode::Persistent)?;


            // we need to populate it
            let data = self.inner.read().unwrap();
            let inner = serde_json::to_vec(&*data).unwrap();
            self.zk.create(path.as_str(), inner, zookeeper::Acl::open_unsafe().clone(), zookeeper::CreateMode::Persistent)?;
        }
        log::debug!("{} exists, continuing initialization", &path);

        state_change(self.zk.clone(), self.inner.clone(), self.state.clone());

        // Create a thread that will ensure consistency in the background.
        // TODO make the interval configurable
        let zk = self.zk.clone();
        let state = self.state.clone();
        let inner = self.inner.clone();
        thread::spawn(move || {
            let zk = zk;
            let inner = inner;
            let state = state;
            loop {
                thread::sleep(Duration::from_secs(5)); // TODO make this configurable

                let handle = state.read().unwrap();
                let path = format!("{}/payload", handle.zk_path);

                if let Some(meta) = zk.exists(path.as_str(), false).unwrap() {
                    if handle.epoch != meta.version {
                        log::warn!("the remote epoch has drifted. local: {}. remote: {}", handle.epoch, meta.version);
                        state_change(zk.clone(), inner.clone(), state.clone());
                    }
                    drop(handle);
                    state.write().unwrap().timings = chrono::Utc::now();
                }
            }
        });

        Ok(())
    }

    /// Method to be invoked to handle state change notifications
    pub fn update_handler<M: Fn(Change<Key, serde_json::Value>) + Send + 'static>(&self, closure: M) -> Result<(), ZkStructError> {
        let chan_handle = self.state.read().unwrap().chan_rx.clone();
        thread::spawn(move || {
            let rx = chan_handle;
            loop {
                let message = rx.recv().unwrap();
                closure(message);
            }
        });
        Ok(())
    }

    /// Return a reference to the Crossbeam Receiver to get Change notifications
    pub fn get_update_channel(&self) -> Receiver<Change<Key, Value>> {
        self.state.read().unwrap().chan_rx.clone()
    }

    /// Update the shared object using a closure.
    ///
    /// The closure is passed a reference to the contents of the ZkState. Once the closure returns,
    /// the shared state in Zookeeper is committed and the write locks released.
    pub fn update<M: FnOnce(&mut T)>(&self, closure: M) -> Result<(), ZkStructError> {
        let path = format!("{}/payload", &self.state.read().unwrap().zk_path);

        // acquire write lock for the internal object
        let mut inner = self.inner.write().unwrap();
        let state = self.state.write().unwrap();

        // get write lock from zookeeper to prevent anyone from modifying this object while we're
        // committing it
        let latch_path = format!("{}/write_lock", &state.zk_path);
        let latch = zookeeper::recipes::leader::LeaderLatch::new(self.zk.clone(), self.id.clone(), latch_path);
        latch.start()?;

        let mut total_time = 0;
        loop {
            if latch.has_leadership() { break; }
            thread::sleep(Duration::from_millis(LOCK_POLL_INTERVAL));
            if total_time > LOCK_POLL_TIMEOUT {
                return Err(ZkStructError::LockAcquireTimeout)
            } else {
                total_time += LOCK_POLL_INTERVAL;
            }
        }

        // pre change
        let a = serde_json::to_value(&*inner).unwrap();

        // at this point, we should have an exclusive lock on the object so we execute the closure
        closure(&mut inner);

        // post change
        let b = serde_json::to_value(&*inner).unwrap();

        emit_updates(&a, &b, &state);

        let raw_data = serde_json::to_vec(&*inner).unwrap();
        let update_op = self.zk.set_data(path.as_str(), raw_data, Some(state.epoch));
        match update_op {
            Ok(_) => {}
            Err(err) => {
                if err == ZkError::BadVersion {
                    return Err(ZkStructError::StaleWrite)
                }
                return Err(ZkStructError::ZkError(err))
            }
        }

        drop(inner); // drop the write lock on the inner object
        drop(state); // drop the write lock on the internal state object

        if let Err(inner) = latch.stop() {
            return Err(ZkStructError::ZkError(inner));
        }

        Ok(())
    }

    /// Returns a Result<RwLockReadGuard<T>>
    pub fn read(&self) -> Result<RwLockReadGuard<'_, T>, ZkStructError> {
        let state = self.state.read().unwrap();
        let delta = (chrono::Utc::now() - state.timings).num_milliseconds();
        if delta > MAX_TIMING_DELTA {
            log::error!("attempted to read stale data. data is {}ms old, limit is {}ms", &delta, MAX_TIMING_DELTA);
            return Err(ZkStructError::StaleRead)
        }
        log::debug!("reading internal data. data is {}ms old, limit is {}ms. epoch is {}", &delta, MAX_TIMING_DELTA, state.epoch);

        match self.inner.read() {
            Ok(inner) => Ok(inner),
            Err(_) => Err(ZkStructError::Poisoned)
        }
    }

    /// Consistent Read.
    ///
    /// Preforms a check to make sure the local version is the same as the remote version before
    /// returning. If there is a mismatch, will preform a sync and return the latest object
    pub fn c_read(&self) { unimplemented!() }

    /// Dirty Read.
    ///
    /// Returns the local data, not failing if the data is too old
    // TODO condense this code, with c_read, and read to remove some code reuse
    pub fn d_read(&self) -> Result<RwLockReadGuard<'_, T>, ZkStructError> {
        let state = self.state.read().unwrap();
        let delta = (chrono::Utc::now() - state.timings).num_milliseconds();
        if delta > MAX_TIMING_DELTA {
            log::warn!("attempted to read stale data. data is {}ms old, limit is {}ms", &delta, MAX_TIMING_DELTA);
        } else {
            log::debug!("dirty reading internal data. data is {}ms old, limit is {}ms. epoch is {}", &delta, MAX_TIMING_DELTA, state.epoch);
        }

        match self.inner.read() {
            Ok(inner) => Ok(inner),
            Err(_) => Err(ZkStructError::Poisoned)
        }
    }

    pub fn metadata(&self) -> (usize, i32) {
        return (self.state.read().unwrap().chan_rx.len(), 0)
    }

    /// Return the ID of this ZkState instance
    pub fn get_id(&self) -> &String {
        &self.id
    }
}
impl<T: Serialize + DeserializeOwned + Send + Sync + Debug + 'static> fmt::Debug for ZkState<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "ZkState{{ id: {}, inner: {:?}, state: {:?} }}", self.id, &*self.inner.read().unwrap(), &*self.state.read().unwrap())
    }
}

fn handle_zk_watcher<T: Serialize + DeserializeOwned + Send + Sync + 'static>(ev: WatchedEvent, zk: Arc<ZooKeeper>, inner: Arc<RwLock<T>>, state: Arc<RwLock<InternalState>>) {
    if let WatchedEventType::NodeDataChanged = ev.event_type {
        state_change(zk, inner, state)
    }
}

#[derive(PartialEq, Debug)]
pub enum Change<K, V> {
    /// The Value was removed
    Removed(Vec<K>, V),
    /// The Value was added
    Added(Vec<K>, V),
    /// No change was performed to the Value
    Unchanged(),
    /// The first Value was modified and became the second Value
    Modified(Vec<K>, V, V),
}

/// Pull the full state from ZooKeeper, compare it to the current inner, Enqueue Changes, and then
/// Update the inner field with the state
fn state_change<T: Serialize + DeserializeOwned + Send + Sync + 'static>(zk: Arc<ZooKeeper>, inner: Arc<RwLock<T>>, state: Arc<RwLock<InternalState>>) {
    let start = Instant::now();

    let path = format!("{}/payload", &state.read().unwrap().zk_path);
    let movable = (zk.clone(), inner.clone(), state.clone());

    let raw_obj = zk.get_data_w(path.as_str(), move |ev| {
        let movable = movable.clone();
        handle_zk_watcher(ev, movable.0, movable.1, movable.2);
    }).unwrap();

    // explicitly hold on to the handle while we compare the diff. it might take a bit for big objects
    // and we do not want to allow for something else to update the object while we're in the process
    // of updating.
    let mut a_handle = inner.write().unwrap();
    let mut state = state.write().unwrap();
    let b: serde_json::Value = serde_json::from_slice(&*raw_obj.0).unwrap();

    // only do a delta if we want to emit updates
    if state.emit_updates {
        let a: serde_json::Value = serde_json::to_value(&*a_handle).unwrap();
        emit_updates(&a, &b, &state);
    }

    *a_handle = serde_json::from_value(b).unwrap();
    state.epoch = raw_obj.1.version;
    state.timings = chrono::Utc::now();

    drop(a_handle); // drop the write handle for the internal object
    drop(state);    // drop the write handle for the state object

    log::debug!("took {}ms to handle state change", start.elapsed().as_millis());
}

fn emit_updates(a: &serde_json::Value, b: &serde_json::Value, state: &InternalState) {
    let mut delta = treediff::tools::Recorder::default();
    diff(a, b, &mut delta);

    let mut ops = (0, 0, 0, 0);
    for change in delta.calls {
        let op = match change {
            ChangeType::Added(k, v) => {
                ops.0 += 1;
                Change::Added(k.clone(), v.clone())
            },
            ChangeType::Removed(k, v) => {
                ops.1 += 1;
                Change::Removed(k.clone(), v.clone())
            },
            ChangeType::Modified(k, a, v) => {
                ops.2 += 1;
                Change::Modified(k.clone(), a.clone(), v.clone())
            },
            ChangeType::Unchanged(_, _) => {
                ops.3 += 1;
                Change::Unchanged()
            },
        };
        if op != Change::Unchanged() {
            let _insert = state.chan_tx.send(op);
        }
    }
    log::debug!("{} added, {} removed, {} modified, {} noop", ops.0, ops.1, ops.2, ops.3);

}
