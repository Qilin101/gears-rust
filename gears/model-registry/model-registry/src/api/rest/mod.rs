//! REST API layer: DTOs, error mapping, handlers, and routes.

pub mod dto;
pub mod error;
pub mod handlers;
pub mod routes;

#[cfg(test)]
#[path = "dto_test.rs"]
mod dto_test;
#[cfg(test)]
#[path = "error_test.rs"]
mod error_test;
