pub mod models;
pub mod schema;
pub mod timestamp;

pub use models::*;
pub use schema::*;

pub use diesel;
pub use diesel_async;

use diesel::prelude::*;
use diesel_async::pooled_connection::deadpool::Pool;
use diesel_async::pooled_connection::AsyncDieselConnectionManager;
use diesel_async::AsyncPgConnection;
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};

pub type DBPool = Pool<AsyncPgConnection>;
pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("../migrations");

/// Connect to postgres and get a pool
pub fn get_pool() -> Result<DBPool, Box<dyn std::error::Error + Send + Sync>> {
    tracing::info!("Connecting to database...");
    let pg_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pg_config = AsyncDieselConnectionManager::<diesel_async::AsyncPgConnection>::new(&pg_url);
    let pg_pool = Pool::builder(pg_config).build()?;
    tracing::info!("Created Database pool.");
    Ok(pg_pool)
}

/// Run any pending migrations
pub fn run_migrations() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing::info!("Running migrations...");
    let pg_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let mut connection = PgConnection::establish(&pg_url)
        .unwrap_or_else(|_| panic!("Error connecting to {}", pg_url));
    connection.run_pending_migrations(MIGRATIONS)?;
    tracing::info!("Migrations complete.");
    Ok(())
}
