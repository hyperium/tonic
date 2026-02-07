mod pb {
    tonic::include_proto!("skip_debug");
}

static_assertions::assert_not_impl_all!(pb::Output: std::fmt::Debug);

fn main() {}
