use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};

//Only testing the load of the compiled GRPC
pub fn generate_rnd_string(string_size: usize) -> Result<String, Box<dyn std::error::Error>> {
    let rand_name: String = thread_rng()
        .sample_iter(&Alphanumeric)
        .take(string_size)
        .collect();

    Ok(rand_name)
}
