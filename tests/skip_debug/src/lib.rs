pub mod pb {
    tonic::include_proto!("test");
}

#[cfg(test)]
static_assertions::assert_not_impl_all!(pb::Output: std::fmt::Debug);
