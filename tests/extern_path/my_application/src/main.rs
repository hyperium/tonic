use uuid::DoSomething;
mod pb {
    tonic::include_proto!("my_application");
}
fn main() {
    // verify that extern_path to replace proto's with impl's from other crates works.
    let message = pb::MyMessage {
        message_id: Some(::uuid::Uuid {
            uuid_str: "".to_string(),
        }),
        some_payload: "".to_string(),
    };
    dbg!(message.message_id.unwrap().do_it());
}
#[cfg(test)]
#[test]
fn service_types_have_extern_types() {
    // verify that extern_path to replace proto's with impl's from other crates works.
    let message = pb::MyMessage {
        message_id: Some(::uuid::Uuid {
            uuid_str: "not really a uuid".to_string(),
        }),
        some_payload: "payload".to_string(),
    };
    assert_eq!(message.message_id.unwrap().do_it(), "Done");
}
