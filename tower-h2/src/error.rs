pub(crate) fn reason_from_dyn_error(err: &(dyn std::error::Error + 'static)) -> h2::Reason {
    let mut cause = Some(err);
    while let Some(err) = cause {
        if let Some(h2_err) = err.downcast_ref::<h2::Error>() {
            return h2_err.reason().unwrap_or(h2::Reason::INTERNAL_ERROR);
        }
        cause = err.source();
    }

    // unknown error
    h2::Reason::INTERNAL_ERROR
}
