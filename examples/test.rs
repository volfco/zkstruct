use zkstate;
use std::env;
use serde::{Serialize, Deserialize};
use std::time::Duration;
use std::sync::Arc;

#[derive(Serialize, Deserialize, Debug)]
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

fn main() {
    env_logger::init();

    let zk_urls = zk_server_urls();
    let zk = zookeeper::ZooKeeper::connect(&*zk_urls, Duration::from_millis(2500), NoopWatcher).unwrap();
    let zka = Arc::new(zk);

    let dir = "/testing-zkstruct".to_string();

    let c = zkstate::ZkState::new(zka.clone(),dir, Testing { my_field: "yoyo".to_string() });

    let mut t = Testing { my_field: "hello world".to_string() };
    update(t, |input|{
        input.my_field = "updated value".to_string()
    });


}