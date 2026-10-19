# Firefly DB

SQLite 连接与版本检查、UTC 微秒时间戳转换、自定义标量函数、写锁忙等重试、维护和在线备份。SQLx、回调函数和备份工具共享内置 SQLite。

主程序持有全局连接池与业务迁移定义，通过 `connect(url, PoolConfig, &migrator)` 取得连接池。`PoolConfig` 包含连接数、获取及空闲超时和时区；每个新连接都会注册对应时区的 SQL 函数。`functions::install(connection, timezone)` 也可单独调用。

`initialize(url, &migrator)` 预先建库，要求新建的绝对路径；`optimize(&pool)` 执行维护；`backup::backup(source, target)` 在线备份，验证完整性及外键并生成独立文件。业务迁移保留在根目录 `migrations_sqlite/`，库接收调用方的迁移对象。

```sh
cargo test --locked -p firefly-db
```

回归覆盖时间戳解码、取消事务回滚、写锁重试、新库权限、拒绝覆盖及接管未知数据库、自定义迁移、连接池时区隔离、SQL 函数替换和 WAL 备份。应用表结构回归保留在 `src/db/schema_tests.rs`。
