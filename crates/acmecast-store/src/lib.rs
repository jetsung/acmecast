//! 持久化层：SeaORM 实体、迁移与仓储。
//!
//! 支持 SQLite、MySQL、PostgreSQL 三种方言，业务逻辑在三方言上行为等价。

pub mod db;
pub mod entity;
pub mod error;
pub mod filestore;
pub mod migration;
pub mod repository;

pub use db::{connect, ping};
pub use error::Error;
pub use filestore::{CertFilePaths, FileStore};
pub use migration::{Migrator, migrate, migrate_with};
pub use repository::{
    CertInput, CertPage, CertQuery, CertRepository, CertSort, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE,
    Pipeline, PipelineInput, PipelineRepository, PipelineStep, PipelineStepInput, PipelineSummary,
    SaveOutcome,
};
