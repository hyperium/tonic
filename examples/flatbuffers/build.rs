fn main() {
    tonic_build::butte::compile_fbs("fbs/helloworld/helloworld.fbs").unwrap();
}
