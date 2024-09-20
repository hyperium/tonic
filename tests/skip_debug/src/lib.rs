pub mod pb {
    tonic::include_proto!("test");

    // Add a dummy impl Debug to the skipped debug implementations to avoid
    // missing impl Debug errors and check debug is not implemented for Output.
    impl std::fmt::Debug for Output {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Output").finish()
        }
    }
}
