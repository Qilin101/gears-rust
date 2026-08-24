// The repository traits take `toolkit_db::secure::DBRunner` and `DomainError`
// wraps `toolkit_db::DbError`, which DE0301 flags. This is the convention across
// gears, not a deviation pending repair. `unknown_lints` covers plain rustc runs,
// where the dylint lint is not registered.
#![allow(unknown_lints)]
#![allow(de0301_no_infra_in_domain)]

pub mod cache;
pub mod error;
pub mod inheritance;
pub mod local_client;
pub mod repo;
pub mod service;
