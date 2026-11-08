//! Build-time entry point used by pgrx to discover this extension's SQL
//! entities and generate its install script. PostgreSQL does not run this
//! binary while maintaining a view; the background worker lives in `worker`.

::pgrx::pgrx_embed!();
