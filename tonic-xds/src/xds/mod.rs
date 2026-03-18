pub(crate) mod bootstrap;
// TODO: remove dead_code once cache is wired into the client layer
#[allow(dead_code)]
pub(crate) mod cache;
pub(crate) mod resource;
// TODO: remove dead_code once routing is wired into the client layer
#[allow(dead_code)]
pub(crate) mod routing;
pub(crate) mod uri;
pub(crate) mod xds_manager;
