mod memory;
mod mysql;
mod postgres;
mod sqlite;

pub(crate) use aether_data_contracts::repository::routing_profiles::{
    CreateRoutingGroupBindingRecord, CreateRoutingGroupRecord, CreateRoutingGroupVersionRecord,
    RoutingGroupBindingQuery, RoutingGroupBindingSubject, RoutingGroupLookupKey,
    RoutingGroupReadRepository, RoutingGroupWriteRepository, StoredRoutingGroup,
    StoredRoutingGroupBinding, StoredRoutingGroupVersion, UpdateRoutingGroupBindingRecord,
    UpdateRoutingGroupRecord,
};
pub use memory::InMemoryRoutingGroupRepository;
pub use mysql::MysqlRoutingGroupRepository;
pub use postgres::PostgresRoutingGroupRepository;
pub use sqlite::SqliteRoutingGroupRepository;
