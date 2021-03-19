
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

There is no strong guarantee of consistency (yet). The library does not depend on Zookeeper Watchers firing to ensure
consistency, but this is the primary way updates are committed. There are background processes that will poll the epoch
of the object and compare to the local version (by default, every 5s) and compare the full state (by default, every 
30s). As it stands, writes are not guaranteed to be committed before they return and reads are not guaranteed to be 
fresh.


# zookeeper structure
```text

/<dir>                  Where the Data will be stored
/<dir>/epoch            Epoch of the payload
/<dir>/payload          JSON Payload
/<dir>/listeners        Empirical records with the IDs of every listener active

```


