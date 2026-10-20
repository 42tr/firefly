# Firefly Process Control

Unix process-group ownership for cancellable shell and CLI work. Callers start
children with `process_group(0)`, use `terminate()` for a graceful stop, and
use `kill()` to disarm and forcefully stop the group. The guard also kills the
group when dropped.

```sh
cargo test --locked -p firefly-process-control
```
