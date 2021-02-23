pub mod google {
    pub mod protobuf {
        tonic::include_proto!("google.protobuf");
    }
}

pub fn grok() {
    let _empty = crate::google::protobuf::Empty {};
}
