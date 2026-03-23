pub(crate) mod bootstrap;
pub(crate) mod resource;
// TODO: remove dead_code once routing is wired into the client layer
#[allow(dead_code)]
pub(crate) mod routing;
pub(crate) mod uri;
pub(crate) mod xds_manager;
