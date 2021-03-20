
# zkstruct
Simple way to share a struct with zookeeper. You can subscribe to changes and handle data changes.

**This is a prototype, and should not be considered stable or be used for production purposes.**
You're going to do that anyways, but FYI.


## overview
ZkStruct is designed to provide a struct that can act as a shared state. 

We're building a little agent that will monitor a list of processes on the host. The agent uses ZkStruct for its state.
An external actor can come along and modify that state object (either using ZkStruct or something else) to update the
list of monitored processes. Let's assume this is a `Vec<String>`, where each String is a glob of the process we want
to match. 

This external actor modifies our Vec from `vec!["systemd*", "rsync*"]` to `vec!["systemd*", "rsync*", "firefox*"]`. The
library will register the change, either from a Watcher event or refresh event, and update the internal state struct.
The library will do a diff on the local state, emit all changes to a channel you can subscribe to, and then update the
local state. 


## reads
Reads using `.read()` are not guaranteed to be fresh, and might be out of date. There is an eventual guarantee that the 
data will be consistent, as reads will fail if the object has not been verified consistent within a certain time 
(default is 30 seconds).

Reads using `.c_read()` are guaranteed to be fresh, as the local object version is compared to the zookeeper version. 
This will, of course, be slower as the client needs to request data from zookeeper and compare it before it will be 
returned, but you can be sure that the data being returned is correct.

## writes
There are two write guarantees:
1. You will always update the latest version of the object. If the local version of the object does not match the
version in zookeeper, the update request will fail with an error.
2. Only one update operation can take place at a time, either locally or among clients. An exclusive write lock is used 
both internally, and in zookeeper to prevent any unexpected updates or data loss. 

# Update Stream
Any changes, either local or remote, can be consumed and acted on. All clients can subscribe to a stream of changes to 
the local object. This can be done using the `.update_handler` method to attach a closure that will be caused everytime
there is change registered.

There are 4 change types:
1. Added
2. Modified
3. Removed
4. Unchanged

Each Enum will come with the names of keys updated, and `serde_json::Value` body that contains the values added or
removed. Modified changes will return the old and new values. Unchanged results are not enqueued and therefore not
returned. 

```rust
shared_state.update_handler(|msg| {
    println!("got stage change message: {:?}", msg);
});
```

Changes are emitted before the local object is updated, but the local object can be updated before all messages in the 
channel can be consumed. 

# zookeeper structure
```text
/<dir>                  Where the Data will be stored
/<dir>/payload          JSON Payload
/<dir>/listeners        Ephemeral records with the IDs of every listener active
```


