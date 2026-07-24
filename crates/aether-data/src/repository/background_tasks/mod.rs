mod memory;
mod mysql;
mod postgres;
mod sqlite;

#[allow(unused_imports)]
pub(crate) use aether_data_contracts::repository::background_tasks::{
    BackgroundTaskKind, BackgroundTaskListQuery, BackgroundTaskReadRepository,
    BackgroundTaskRepository, BackgroundTaskStatus, BackgroundTaskSummary,
    BackgroundTaskWriteRepository, StoredBackgroundTaskEvent, StoredBackgroundTaskRun,
    StoredBackgroundTaskRunPage, UpsertBackgroundTaskEvent, UpsertBackgroundTaskRun,
    BACKGROUND_TASK_WORKER_BOOT_RUN_ID_PREFIX,
};

pub use memory::InMemoryBackgroundTaskRepository;
pub use mysql::MysqlBackgroundTaskRepository;
pub use postgres::SqlxBackgroundTaskRepository;
pub use sqlite::SqliteBackgroundTaskRepository;
