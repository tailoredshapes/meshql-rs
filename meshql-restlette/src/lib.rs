pub mod routes;

pub use routes::{
    build_restlette_router, build_restlette_router_ext, PostCreateFn, SideEffectContext,
    ValidatorContext, ValidatorFn,
};
// Re-export for backward compatibility — AuthContext now lives in meshql-core.
pub use meshql_core::AuthContext;
