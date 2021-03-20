use zkstate;
use std::env;
use serde::{Serialize, Deserialize};
use std::time::Duration;
use std::sync::Arc;

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Testing {
    my_field: String
}

struct NoopWatcher;

impl zookeeper::Watcher for NoopWatcher {
    fn handle(&self, _ev: zookeeper::WatchedEvent) {}
}

fn zk_server_urls() -> String {
    let key = "ZOOKEEPER_SERVERS";
    match env::var(key) {
        Ok(val) => val,
        Err(_) => "localhost:2181".to_string(),
    }
}


fn update<T: FnOnce(&mut Testing)>(i: Testing, c: T) {

    let mut inner = i;
    println!("{:?}", &inner);
    c(&mut inner);
    println!("{:?}", &inner);

}

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let zk_urls = zk_server_urls();
    let zk = zookeeper::ZooKeeper::connect(&*zk_urls, Duration::from_millis(2500), NoopWatcher).unwrap();
    let zka = Arc::new(zk);

    let dir = "/testing-zkstruct".to_string();

    let c = zkstate::ZkState::new(zka.clone(),dir, Testing { my_field: "yoyo".to_string() })?;

    c.update_handler(|msg| {
        println!("got stage change message: {:?}", msg);
    });

    let th = c.clone();
    std::thread::spawn(move || {
        let th = th;
        loop {
            println!("my data is {:?} and I have {} changes pending", th.read().unwrap(), th.metadata().0);
            std::thread::sleep(Duration::from_secs(1));
        }
    });

    std::thread::sleep(Duration::from_secs(5));

    c.update(|mut p| {
       p.my_field = "hello world again".to_string()
    });

    std::thread::sleep(Duration::from_secs(45));

    Ok(())
}