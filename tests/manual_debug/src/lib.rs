pub mod pb {
    tonic::include_proto!("test");

    impl std::fmt::Debug for ManualDebug {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "ManualDebug manual implementation")
        }
    }
}
