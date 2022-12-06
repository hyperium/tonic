pub mod gen {
    pub mod google {
        pub mod protobuf {
            #![allow(clippy::derive_partial_eq_without_eq)]
            tonic::include_proto!("google.protobuf");
        }
    }

    pub mod test {
        #![allow(clippy::derive_partial_eq_without_eq)]
        tonic::include_proto!("test");
    }
}

pub fn grok() {
    let _any = crate::gen::google::protobuf::Any {
        type_url: "foo".to_owned(),
        value: Vec::new(),
    };
}
