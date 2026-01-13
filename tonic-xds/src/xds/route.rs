pub(crate) struct RouteInput<'a> {
    pub authority: &'a str,
    pub headers: &'a http::HeaderMap,
}

#[derive(Clone)]
pub(crate) struct RouteDecision {
    pub cluster: String,
}