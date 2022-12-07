include!(concat!(env!("OUT_DIR"), "/uuid.rs"));

pub trait DoSomething {
    fn do_it(&self) -> String;
}

impl DoSomething for Uuid {
    fn do_it(&self) -> String {
        "Done".to_string()
    }
}
